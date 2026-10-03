# Handoff (2026-10-03)

Where things stand, for whoever picks this up next. Delete this file once it is stale.

## Branch and code
- Branch `claude/funny-volta-40u9d2` has `main` merged in (commit `9485725`) and is pushed.
  The Windows backends (Desktop Duplication, Media Foundation encoder, cursor, BT.709) sit
  behind `main`'s `Capture`, `Encoder`, `DisplayBackend` and `MediaFactory` traits; see
  `host/README.md` and the status note in `PLAN.md` §8.
- Local: `cargo clippy -p parkscreen-host --all-targets --no-deps -- -D warnings` and
  `cargo test -p parkscreen-host` pass (Rust 1.97). CI uses the newest stable, which has
  already added lints that local 1.97 lacks, so check the `host` run on the branch.
- Never run on real hardware: the stream, the hardware encoder, `--match-viewport` and the
  Tesla browser are all untested. The W1/W2 exit criteria in `PLAN.md` are still open.
- Dropped in the merge: the local player page and `:8765` signalling. Use a local
  `parkscreen-server` for offline testing, or restore it from `b80f1fc`.

## Code signing (done, working)
- Azure Trusted Signing account `leatham-signing-account` (West Europe), profile
  `cert-profile`, subject `CN=FBL Consulting Ltd`.
- App registration `parkscreen-release` (client id `043d5bef-479b-47c9-9158-b2497fab715b`)
  has a federated credential for `fredle/ParkScreen` environment `release` and the
  Artifact Signing Certificate Profile Signer role.
- GitHub environment `release` has `AZURE_CLIENT_ID`, `AZURE_TENANT_ID` and
  `AZURE_SUBSCRIPTION_ID` as variables. `release.yml` reads them as `vars.*`, not secrets.
- Tag `v0.1.0` (built from the old tree) produced a signed `parkscreen-host.exe` release.
  Not yet checked by hand: right-click, Properties, Digital Signatures.

## Not done: Firebase and Cloud Run
Nothing exists yet. `parkscreen.web.app` and `parkscreen-releases.web.app` return "Site Not
Found", and there is no `parkscreen-server` Cloud Run service.
- `deploy/gcp-setup.sh` creates it all in project `leatham-sandbox`: the two Hosting sites, a
  `parkscreen-deployer` service account, a `github-parkscreen` provider in the existing
  `github-pool`, and the GitHub variables. **It has not been run.** Read it first.
- Then run the Deploy workflow, set `PARKSCREEN_SERVER_URL` from its log, and only then cut a
  host release (`release.yml` also needs the `GCP_*`, `FIREBASE_PROJECT_ID` and
  `PARKSCREEN_SERVER_URL` variables, and the `parkscreen-releases` site).
- `gcloud auth login` was done in the previous session; it may have expired again.
- Known: `freddie@leatham.com` could not list providers of the `portmon-github` pool, so some
  IAM steps may be refused.

## Other open items
- `server/` has clippy errors on current Rust (`map_or`, a complex type) that `main`'s CI
  doesn't check. Repo moved to `fredle/ParkScreen`; the git remote still uses the old casing.
- Next milestones: W3 (Windows touch injection, `NullInput` today), W4 (our IddCx driver and
  the signing spike), W5 (tray app and installer).
