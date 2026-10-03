# Blueprint: a Windows app + web client + Google Cloud backend, from one prompt

The architecture ParkScreen (and Teeline) use. Hand the **Prompt** section to Claude Code in an
empty repo, with `gcloud`, `gh` and `firebase` logged in, and fill in the `<…>` placeholders.

## Architecture

| Piece | Where | Deployed by |
|---|---|---|
| Web client (static, Vite/TS) | Firebase Hosting site `<app>` -> `https://<app>.web.app` | push to `main` |
| Backend API / signalling (stateless-ish, WebSockets OK) | Cloud Run service `<app>-server`, single instance if state is in memory | push to `main` |
| Persistent data | Firestore (write-through; load at startup) | n/a |
| Windows app | Signed Velopack installer + update feed on Firebase Hosting site `<app>-releases` | push of a `v*` tag |

Rules that make it work:

1. **No VM, no tunnel.** Everything is Google Cloud managed. Heavy traffic (video, files) goes
   peer to peer or direct, never through the server.
2. **Client and server are on different origins.** Firebase Hosting rewrites to Cloud Run are not
   relied on for WebSockets. The client is built with `VITE_SERVER_URL=<cloud run url>`; the
   Windows app bakes `<APP>_SERVER_URL` in at build time (env override at run time). The server
   enforces `ALLOWED_ORIGINS` (CORS plus WebSocket Origin check); non-browser clients
   authenticate with a key, not cookies. Browser tokens live in `localStorage`.
3. **Cloud Run settings:** `--allow-unauthenticated --max-instances=1 --concurrency=1000
   --timeout=3600`, health check on `/status` (Google's front end swallows `/healthz`).
   WebSockets are cut at 60 minutes, so clients must reconnect with backoff.
4. **Two Hosting sites in one Firebase project** via `firebase.json` hosting targets
   (`<app>` -> `web/client/dist`, `<app>-releases` -> `Releases`) and `.firebaserc` targets.
5. **No long-lived keys anywhere.** GitHub Actions authenticates to GCP with Workload Identity
   Federation (a provider restricted by `assertion.repository=='<Owner>/<Repo>'`, **using the
   exact casing GitHub puts in the token**) and to Azure with OIDC for signing.
6. **Windows release pipeline** (`windows-latest`, GitHub `release` environment): build exe with
   the server URL baked in -> sign exe (Azure Trusted Signing action) -> `vpk pack` (Velopack,
   downloads the previous feed first for delta packages) -> sign the installer -> add release
   notes -> deploy `Releases/` to Hosting. Version comes from the tag.
7. **Tag only after `<APP>_SERVER_URL` is set**, otherwise the exe has no server baked in.

## Repo layout

```
Cargo.toml / package.json      workspace roots
<app>-protocol/                shared message types (single source of truth for client+server+app)
server/                        backend (Rust/axum here; any container works). Dockerfile at root
host|app/                      the Windows app
web/client/                    Vite + TypeScript static client, vitest tests
deploy/gcp-setup.sh            idempotent one-time setup (see below)
deploy/README.md               architecture, limits, setup, release steps
firebase.json  .firebaserc
.github/workflows/ci.yml       tests on PR/push (cargo test, npm test, windows build)
.github/workflows/deploy.yml   push to main: Cloud Run from source, build client, hosting deploy
.github/workflows/release.yml  tag v*: signed Windows release
```

## `deploy/gcp-setup.sh` must (idempotently)

1. Enable APIs: run, cloudbuild, firestore, firebasehosting, artifactregistry, iamcredentials.
2. Create the two Hosting sites (ids are global: check `<id>.web.app` returns 404 first).
3. Grant the Cloud Run runtime identity (default compute SA) `roles/datastore.user`.
4. Create `<app>-deployer` SA with: run.admin, cloudbuild.builds.editor, storage.admin,
   artifactregistry.writer, firebasehosting.admin, serviceusage.serviceUsageConsumer; and
   `iam.serviceAccountUser` on the compute SA (source deploys build and run as it).
5. Create a WIF OIDC provider in an existing pool, with the repo attribute condition, and bind
   `roles/iam.workloadIdentityUser` for that repo's principalSet on the deployer SA.
6. Set GitHub variables (repo **and** `release` environment): `GCP_WORKLOAD_IDENTITY_PROVIDER`,
   `GCP_SERVICE_ACCOUNT`, `FIREBASE_PROJECT_ID`, `GCP_REGION`.
7. Print the manual steps left: run Deploy, then set `<APP>_SERVER_URL` (repo + environment).

Also needed once: Azure variables `AZURE_CLIENT_ID`, `AZURE_TENANT_ID`, `AZURE_SUBSCRIPTION_ID`
(variables, not secrets) and optionally `SIGNING_ENDPOINT/ACCOUNT/PROFILE`; the Azure app's
federated credential must trust the repo's `release` environment.

## Gotchas learned the hard way

- IAM and WIF bindings take 1-2 minutes to propagate: a first deploy straight after setup can
  fail with `iam.serviceAccounts.getAccessToken denied`. Wait and re-run.
- A push to `main` that lands before setup runs will fail Deploy; that is expected, just re-run.
- `workflow_dispatch` runs on the repo's default branch.
- If the repo is renamed or transferred, the WIF condition casing must match the token.
- `firebase-tools` deploy hosting needs `firebasehosting.admin` and the sites to exist first.
- Firestore location is permanent per project; reuse the project's existing database.

## Prompt (copy, fill placeholders, paste)

> Build `<APP>`: `<one-paragraph description of what the app does, what runs on the Windows PC,
> what runs in the browser, and what the server coordinates>`.
>
> Use exactly the architecture in `deploy/BLUEPRINT.md` (copy it into the repo first if absent):
> a Windows app in `<Rust|C#>`, a Vite/TypeScript web client, and a `<Rust|Node>` backend on Cloud
> Run with Firestore persistence, hosted at `https://<app>.web.app` (client) and
> `https://<app>-releases.web.app` (signed Velopack installer and update feed), in GCP project
> `<project>` region `<region>`, GitHub repo `<owner>/<repo>`.
>
> Do all of it, not just the code: shared protocol crate/package, server with `/status`, CORS and
> WebSocket origin checks from `ALLOWED_ORIGINS`, Dockerfile, client reading `VITE_SERVER_URL`,
> app reading baked-in `<APP>_SERVER_URL` with run-time override, tests for each part, CI,
> `deploy.yml`, `release.yml`, `firebase.json`/`.firebaserc` with the two targets,
> `deploy/gcp-setup.sh` and `deploy/README.md` as specified in the blueprint. Reuse the existing
> Azure signing setup (`<signing account/profile>`) and WIF pool `<pool>`.
>
> Then take it live: confirm `gcloud`, `gh` and `firebase` logins, run the setup script, merge to
> `main`, re-run Deploy until green (allow for IAM propagation), set `<APP>_SERVER_URL` on the repo
> and the `release` environment, tag `v0.1.0`, wait for the release workflow, then verify
> end to end: site returns 200, `/status` returns ok, CORS preflight allows only the site origin,
> the release feed lists the version, and `Get-AuthenticodeSignature` on the installer is `Valid`.
> Keep going until all of that is true or you hit something only I can do, and tell me exactly what.
