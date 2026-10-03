# parkscreen-host (W1)

Captures one Windows monitor with DXGI Desktop Duplication, encodes it to H.264
(OpenH264) and streams it over WebRTC to any browser on the LAN. See `../PLAN.md` §5.3.

```
parkscreen-host list
parkscreen-host set-mode --monitor 2 1920x1200@60
parkscreen-host serve [--monitor auto|<idx>|<name>] [--fps 60] [--bitrate 12M] [--match-viewport]
```

`serve` prints a `http://<lan-ip>:8765` URL. Open it in Chrome or the Tesla browser (in Park)
and tap **Connect**. Double-click (or two-finger tap) toggles a stats overlay.

Allow `TCP 8765` and `UDP 8766` through Windows Firewall (private networks).
Install a virtual display driver (e.g. Virtual Display Driver) to get a monitor to extend onto;
`--monitor auto` prefers one whose name looks virtual.

Build: `cargo build --release` (needs the MSVC build tools). `RUST_LOG=parkscreen_host=debug` for more logging.
Not yet: cursor, touch input, hardware encoding, tray UI.
