#!/usr/bin/env bash
# One-time Google Cloud / Firebase / GitHub setup for ParkScreen, modelled on Teeline.
# Review it, then run it from a shell where `gcloud auth login`, `gh auth login` and
# `npx firebase-tools login` are done. Safe to re-run: existing resources are skipped.
#
#   bash deploy/gcp-setup.sh
set -euo pipefail

PROJECT=leatham-sandbox            # as in .firebaserc
REGION=europe-west2                # where the other Cloud Run services live
GH_REPO=fredle/parkscreen          # GitHub repo that deploys
GH_REPO_ATTR=fredle/ParkScreen     # the casing GitHub puts in the OIDC token
POOL=github-pool                   # existing pool shared with Teeline
PROVIDER=github-parkscreen
SA_NAME=parkscreen-deployer
SA=$SA_NAME@$PROJECT.iam.gserviceaccount.com

gcloud config set project "$PROJECT" >/dev/null
NUMBER=$(gcloud projects describe "$PROJECT" --format='value(projectNumber)')
COMPUTE_SA=$NUMBER-compute@developer.gserviceaccount.com

echo "== APIs"
gcloud services enable run.googleapis.com cloudbuild.googleapis.com firestore.googleapis.com \
  firebasehosting.googleapis.com artifactregistry.googleapis.com iamcredentials.googleapis.com

echo "== Firebase Hosting sites (ids are global; 404 on <id>.web.app means free)"
for site in parkscreen parkscreen-releases; do
  npx --yes firebase-tools@15 hosting:sites:create "$site" --project "$PROJECT" || echo "  $site: exists or taken, check above"
done

echo "== Cloud Run runtime identity may use Firestore"
gcloud projects add-iam-policy-binding "$PROJECT" --condition=None \
  --member="serviceAccount:$COMPUTE_SA" --role=roles/datastore.user >/dev/null

echo "== Deploy service account"
gcloud iam service-accounts describe "$SA" >/dev/null 2>&1 || \
  gcloud iam service-accounts create "$SA_NAME" --display-name="ParkScreen GitHub Actions deployer"
for role in roles/run.admin roles/cloudbuild.builds.editor roles/storage.admin \
            roles/artifactregistry.writer roles/firebasehosting.admin roles/serviceusage.serviceUsageConsumer; do
  gcloud projects add-iam-policy-binding "$PROJECT" --condition=None \
    --member="serviceAccount:$SA" --role="$role" >/dev/null
done
# Deploying from source runs the build and the service as the compute service account.
gcloud iam service-accounts add-iam-policy-binding "$COMPUTE_SA" \
  --member="serviceAccount:$SA" --role=roles/iam.serviceAccountUser >/dev/null

echo "== Workload Identity Federation: only $GH_REPO may impersonate $SA"
gcloud iam workload-identity-pools providers describe "$PROVIDER" --workload-identity-pool="$POOL" --location=global >/dev/null 2>&1 || \
  gcloud iam workload-identity-pools providers create-oidc "$PROVIDER" \
    --workload-identity-pool="$POOL" --location=global \
    --issuer-uri=https://token.actions.githubusercontent.com \
    --attribute-mapping="google.subject=assertion.sub,attribute.repository=assertion.repository" \
    --attribute-condition="assertion.repository=='$GH_REPO_ATTR'"
gcloud iam service-accounts add-iam-policy-binding "$SA" \
  --role=roles/iam.workloadIdentityUser \
  --member="principalSet://iam.googleapis.com/projects/$NUMBER/locations/global/workloadIdentityPools/$POOL/attribute.repository/$GH_REPO_ATTR" >/dev/null

echo "== GitHub repository variables"
gh variable set GCP_WORKLOAD_IDENTITY_PROVIDER --repo "$GH_REPO" \
  --body "projects/$NUMBER/locations/global/workloadIdentityPools/$POOL/providers/$PROVIDER"
gh variable set GCP_SERVICE_ACCOUNT --repo "$GH_REPO" --body "$SA"
gh variable set FIREBASE_PROJECT_ID --repo "$GH_REPO" --body "$PROJECT"
gh variable set GCP_REGION --repo "$GH_REPO" --body "$REGION"
# release.yml also authenticates to GCP inside the `release` environment.
for v in GCP_WORKLOAD_IDENTITY_PROVIDER GCP_SERVICE_ACCOUNT FIREBASE_PROJECT_ID; do
  gh variable set "$v" --env release --repo "$GH_REPO" --body "$(gh variable get "$v" --repo "$GH_REPO")"
done

cat <<EOF

Done. Still manual:
  1. Run the Deploy workflow (it triggers on pushes to main, or run it by hand with workflow_dispatch).
  2. Copy the Cloud Run URL from its log into the PARKSCREEN_SERVER_URL variable:
       gh variable set PARKSCREEN_SERVER_URL --repo $GH_REPO --body https://parkscreen-server-...run.app
       gh variable set PARKSCREEN_SERVER_URL --env release --repo $GH_REPO --body https://parkscreen-server-...run.app
  3. If the Cloud Run service must be public, check that unauthenticated access is allowed:
       gcloud run services get-iam-policy parkscreen-server --region $REGION
EOF
