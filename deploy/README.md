# Hosting ParkScreen on Firebase + Cloud Run

Same pattern as Teeline (`fredle/transcriber`): everything on Google Cloud, no VM.

| Piece | Where | URL |
|---|---|---|
| Web client + `/probe/` | Firebase Hosting site `parkscreen` | https://parkscreen.web.app |
| Signalling + pairing server | Cloud Run service `parkscreen-server` | `https://parkscreen-server-<hash>-<region>.a.run.app` |
| Pairings (hosts, paired cars) | Firestore (`hosts`, `pairings`) | |
| Host agent installer + update feed | Firebase Hosting site `parkscreen-releases` | https://parkscreen-releases.web.app |

Video and touch never touch any of this: they go peer to peer between the PC and the car.

## Why the client and server are on different origins

Firebase Hosting rewrites to Cloud Run are not relied on for WebSockets (support is not
documented), so the page and the host agent connect to the Cloud Run URL directly:

- The page is built with `VITE_SERVER_URL=<cloud run url>` (done by `deploy.yml`).
- The host agent bakes the same URL in at build time (`PARKSCREEN_SERVER_URL`, done by
  `release.yml`); `PARKSCREEN_URL` overrides it at run time.
- The server only accepts browser requests from `ALLOWED_ORIGINS`
  (default `https://parkscreen.web.app`): it answers CORS for `/api/pair/claim` and refuses
  WebSocket upgrades from other sites. Hosts (no `Origin` header) are authenticated by
  their Ed25519 key instead.
- The car's token lives in `localStorage` (a cookie from another origin would be third-party).

## Limits to know about

- **One instance** (`--max-instances=1`): online hosts/cars and pairing codes are held in
  memory. Each WebSocket is one Cloud Run request and an instance takes at most 1000, so
  expect roughly **500 sessions** (a session is a PC socket plus a car socket). To go beyond
  that, move host/car routing out of process memory first (Firestore listeners or Redis pub/sub).
- **60-minute WebSocket cap.** Signalling sockets are dropped and re-established about
  hourly; clients and the host reconnect with backoff and live video is unaffected.
- **Health check** is `/status` on Cloud Run (Google's front end swallows `/healthz`).
- Pairings are read from memory after a load at startup and written through to Firestore,
  so a restart keeps every paired car.

## One-time setup

Easiest: `bash deploy/gcp-setup.sh` does all of this (region `europe-west2`). The manual steps follow.

```bash
PROJECT=leatham-sandbox        # as in .firebaserc; change both if you use another project
gcloud config set project $PROJECT

gcloud services enable run.googleapis.com firestore.googleapis.com cloudbuild.googleapis.com
gcloud firestore databases create --location=europe-west2   # skip if the project already has one

# Cloud Run runs as the default compute service account; let it use Firestore.
PROJECT_NUMBER=$(gcloud projects describe $PROJECT --format='value(projectNumber)')
gcloud projects add-iam-policy-binding $PROJECT \
  --member="serviceAccount:$PROJECT_NUMBER-compute@developer.gserviceaccount.com" \
  --role="roles/datastore.user"

# Hosting sites. "parkscreen" must be free: site ids are global across Firebase. If it is
# taken, pick another id and update .firebaserc, firebase.json targets and ALLOWED_ORIGINS.
firebase hosting:sites:create parkscreen --project $PROJECT
firebase hosting:sites:create parkscreen-releases --project $PROJECT
firebase target:apply hosting parkscreen parkscreen --project $PROJECT
firebase target:apply hosting parkscreen-releases parkscreen-releases --project $PROJECT
```

GitHub (repo → Settings → Variables / Secrets), reusing the Teeline Workload Identity
Federation and signing setup where you like:

| Kind | Name | Used by |
|---|---|---|
| var | `GCP_WORKLOAD_IDENTITY_PROVIDER`, `GCP_SERVICE_ACCOUNT` | both workflows |
| var | `FIREBASE_PROJECT_ID` | both |
| var | `GCP_REGION` (`europe-west2`, set by the script; workflow falls back to `us-central1`) | deploy |
| var | `PARKSCREEN_SERVER_URL` (the Cloud Run URL, after the first deploy) | release |
| var | `SIGNING_ENDPOINT`, `SIGNING_ACCOUNT`, `SIGNING_PROFILE` (default to Teeline's) | release |
| var | `AZURE_CLIENT_ID`, `AZURE_TENANT_ID`, `AZURE_SUBSCRIPTION_ID` (variables, not secrets; copy from Teeline; not set by the script) | release |

The deploy service account needs roles to deploy Cloud Run from source (Cloud Run Admin,
Cloud Build Editor, Service Account User, Storage access for the build bucket) and Firebase
Hosting Admin. The `release` GitHub environment must exist (as in Teeline).

First deploy: push to `main` (or run **Deploy** manually). It prints the Cloud Run URL in the
log; set it as `PARKSCREEN_SERVER_URL` before cutting a host release.

## Local development

```bash
cargo run -p parkscreen-server                 # sqlite://parkscreen.db, same-origin client
cd web/client && npm run dev                   # Vite proxies /api and /ws to :8080
PARKSCREEN_URL=http://127.0.0.1:8080 cargo run -p parkscreen-host -- --pair
```

Server environment: `STORE` (`sqlite` default, `firestore`, `memory`), `DATABASE_URL`,
`FIRESTORE_PROJECT`, `ALLOWED_ORIGINS` (comma separated), `LISTEN` or `PORT`.
`FIRESTORE_EMULATOR_HOST` is honoured for local Firestore.

### TURN (cars and PCs on different networks)

Without TURN the server hands out STUN only, which fails behind symmetric NATs, CGNAT and
UDP-blocking networks. To add a relay, create a Cloudflare Realtime TURN key (Cloudflare
dashboard, Realtime, TURN) and set a repository variable `CF_TURN_KEY_ID` and secret
`CF_TURN_API_TOKEN`; the deploy workflow passes them to Cloud Run (as plain environment
variables, so use a key that only has TURN access). The server mints 6-hour credentials, caches
them for an hour, and sends them to the host and the car when each connects. The startup log
line `ICE servers turn=true` confirms it is on. Leave them unset for STUN only.

## Releasing the host agent

Before tagging, add the version to `web/client/public/release-notes/index.html` (newest first, move
the "Latest" tag) and push to `main` so https://parkscreen.web.app/release-notes/ is current.

Tag `vX.Y.Z`. `release.yml` builds `parkscreen-host.exe` (with the server URL baked in),
signs it with Azure Trusted Signing, packs it with Velopack (deltas against the previous
release), signs the installer and publishes the feed to https://parkscreen-releases.web.app.
This workflow has not run yet.
updates yet (see `UpdateService.cs` in Teeline for the pattern).
