# parkscreen-host

The Windows host agent. It dials out to `parkscreen-server`, pairs with the car, and streams
one monitor to the Tesla browser over WebRTC (H.264). See `../PLAN.md` §5.

```
parkscreen-host [--pair] [--with-input] [--bitrate 12M]
                [--monitor auto|<index>|<name>] [--encoder auto|hardware|software] [--match-viewport]
parkscreen-host list
parkscreen-host set-mode --monitor 2 1920x1200@60
```

- `PARKSCREEN_URL` points the agent at a server (default: the URL baked in at build time,
  else `http://127.0.0.1:8080` for a local `parkscreen-server`). `--pair` prints a pairing code.
- **Capture:** DXGI Desktop Duplication of the chosen monitor, with the pointer composited in
  (`capture_dxgi`, `cursor`). `--monitor auto` prefers a monitor whose name looks virtual.
  Install a virtual display driver (e.g. Virtual Display Driver) to get a monitor to extend onto.
- **Encode:** a Media Foundation hardware encoder (NVENC, Quick Sync, AMF) with OpenH264 as the
  fallback (`encode_mf`, `encode`), BT.709 either way (`convert`). `--encoder` forces one.
- **`--match-viewport`** switches the chosen monitor to the car's screen size when it connects.
  Off by default, because it resizes a real monitor.
- Without Windows (or for tests) the agent streams a test pattern through OpenH264.
- Updates itself (Velopack, `updater.rs`): checks the feed every 4 hours and restarts into a newer version when no car is streaming.
- **Tray:** with no arguments the app runs from the system tray (status, Pair a car…, open website, Start with Windows, Quit), one instance per user, no admin rights needed. `--pair` or `--no-tray` runs in a terminal instead and prints the code.
- Not yet: touch injection on Windows (`NullInput`).

Build from the repo root: `cargo build --release -p parkscreen-host` (needs the MSVC build
tools). `RUST_LOG=parkscreen_host=debug` for more logging. Windows-only modules are in
`capture_dxgi`, `encode_mf` and `windows_media`; the rest is portable and covered by
`cargo test`.
