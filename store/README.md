# Microsoft Store submission assets

- `listing.json`: Store listing text (en-US). `privacy-policy.txt`: the policy pasted into Partner Center; the same text is served at https://parkscreen.web.app/privacy/ (`web/client/public/privacy/`).
- `screenshots/`: 1920x1080 screenshots, `boxart-1x1-1080.png`, `poster-2x3-720x1080.png`. Rendered from `src/*.html` (real captures of parkscreen.web.app plus an honest diagram). The Partner Center uploader only accepted the logos when the file names contained `boxart`/`poster`.
- Package: signed Velopack installer, EXE, x64, silent switch `--silent`, from a versioned immutable URL (`release.yml` publishes `store/<version>/ParkScreenSetup.exe`).
