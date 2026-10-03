# ParkScreen: Implementation Plan

ParkScreen turns a Tesla's touchscreen into an extra monitor for a Windows PC.
A virtual display driver adds a real monitor to Windows, a host agent captures
and encodes that monitor, and a web client in the Tesla browser decodes and
shows it.

> **Use only while parked.** Tesla blocks video in the browser while the car is
> in gear. ParkScreen is built for Park, and we will not work around that
> lockout (see §9).

---

## 1. Goals and non-goals

**Goals (v1)**
- Windows shows a new monitor ("ParkScreen") under *Settings → Display* that
  you can extend to, arrange and set to any supported resolution.
- The Tesla browser shows that monitor full-screen with less than about 100 ms
  of glass-to-glass latency on a good LAN, at 30–60 fps.
- Touch on the Tesla screen controls the PC (tap, drag, scroll).
- Setup takes no more than one installer on the PC and one URL on the car.

**Non-goals (v1)**
- macOS/Linux hosts. The architecture allows them later.
- Audio. The browser can play it, but it is out of scope for v1.
- Use while driving.
- Streaming over the internet when the car and PC are on different networks.
  This is a v2 cloud relay feature (§6.3).

---

## 2. System overview

```
┌───────────────────────── Windows PC ─────────────────────────┐
│                                                              │
│  ┌──────────────────┐   frames    ┌───────────────────────┐  │
│  │ ParkScreen IDD   │───────────▶│ Host Agent             │  │
│  │ (IddCx UMDF      │◀───────────│  - capture             │  │
│  │  virtual monitor)│  mode ctrl  │  - HW H.264 encode    │  │
│  └──────────────────┘             │  - HTTPS/WSS server   │  │
│                                   │  - WebRTC (optional)  │  │
│                                   │  - input injection    │  │
│                                   │  - tray UI / pairing  │  │
│                                   └──────────┬────────────┘  │
└──────────────────────────────────────────────┼───────────────┘
                                               │ LAN / phone hotspot
                                    video ▼    ▲ touch, viewport size
┌──────────────────────────── Tesla ───────────┴───────────────┐
│  Tesla browser (Chromium) → ParkScreen web client            │
│   capability probe → best transport → decode → <canvas>      │
└──────────────────────────────────────────────────────────────┘
```

There are three deliverables:

| Component | Language / stack | Location in repo |
|---|---|---|
| Virtual display driver | C++ / UMDF 2 / IddCx | `driver/` |
| Host agent + tray app | Rust (`windows` crate), Tauri for the UI | `host/` |
| Web client | TypeScript, Vite, no framework (keeps it light for the car's CPU) | `web/` |

---

## 3. The Tesla browser: constraints that drive the design

The Tesla browser is Chromium-based, but its version and features vary with
the car's hardware (Intel Atom MCU vs. AMD Ryzen MCU) and firmware. We cannot
assume WebRTC, WebCodecs or MSE works on every car. So:

1. **Phase 0 is a capability probe** (§8). We ship a test page and run it on
   real cars before committing to a transport.
2. **The client chooses a transport at runtime** from a ranked list and falls
   back automatically.
3. **We need a secure context (HTTPS).** WebCodecs and some other APIs only
   work on HTTPS pages, and an HTTPS page cannot open `ws://` to a LAN IP
   (mixed content). Self-signed certificates bring up a warning that is
   painful to click through on the car. Our fix is in §6.2.

Known screen and viewport facts (to confirm in Phase 0):

| Vehicle | Panel | Notes |
|---|---|---|
| Model 3 / Y | 1920×1200 | Browser viewport is smaller because of browser chrome and the car UI |
| Model S / X (2021+) | 2200×1300 | |
| Cybertruck | 18.5" panel | To be measured |

Because the viewport differs between cars and changes with browser full-screen
mode, the client reports its real pixel size
(`innerWidth × innerHeight × devicePixelRatio`), and the driver adds that exact
mode to the virtual monitor (§4.3). That way frames are 1:1, with no scaling
blur.

---

## 4. Virtual display driver (`driver/`)

### 4.1 Approach
Use the **Indirect Display Driver (IddCx)** model, a user-mode (UMDF) driver.
This is the supported way to make a virtual monitor on Windows 10 1903+ and
Windows 11, and Parsec VDD, Virtual-Display-Driver and Microsoft's
`IddSampleDriver` all use it.

We start from Microsoft's `IddSampleDriver` (MIT) for structure and add:

- One monitor ("ParkScreen"), with optional N monitors later.
- **Dynamic modes**: a custom EDID plus `IddCxMonitorUpdateModes` so the host
  can add the car's exact resolution at runtime.
- Refresh rates of 30 and 60 Hz.
- **Plug and unplug on demand**: the monitor only exists while a car is
  connected. That way Windows does not keep windows on an invisible screen.
- HDR and cursor: v1 is SDR only. Hardware cursor via
  `IddCxMonitorSetupHardwareCursor`, so the cursor is sent as separate
  metadata and composited on the client. This means less re-encoding when only
  the mouse moves.

### 4.2 Getting frames out: two-stage approach

| Stage | How | Pros | Cons |
|---|---|---|---|
| **A (MVP)** | Driver only presents a monitor. Host captures that monitor with **Windows.Graphics.Capture** (falling back to DXGI Desktop Duplication). | Simple driver, all the complex code is in the host, easy to debug | One extra GPU copy |
| **B (optimised)** | Driver's swap-chain thread (`IddCxSwapChainReleaseAndAcquireBuffer`) copies each frame into a **shared D3D11 texture** with a keyed mutex. Host opens the handle and encodes directly. | Lowest latency, exact dirty rectangles, no capture API overhead | More driver code, needs IPC |

We ship A first. B is a performance milestone.

### 4.3 Driver ↔ host control channel
- A named pipe or a device interface with `DeviceIoControl` IOCTLs:
  `PLUG_MONITOR`, `UNPLUG_MONITOR`, `SET_MODES(list)`, `GET_STATUS`.
- In Stage B, also `GET_SHARED_TEXTURE_HANDLE` and a frame-ready event.

### 4.4 Signing and installation (the biggest non-code risk)
- Windows 10/11 needs drivers to be **Microsoft-signed**. The path is an
  **EV code-signing certificate** (about $300–500/year) plus **attestation
  signing** through the Partner Center Hardware Dev Center. Start this
  early because verification takes weeks.
- **Development:** test-signing mode (`bcdedit /set testsigning on`).
- **Fallback for early users:** the host agent can also drive an existing
  signed IddCx driver (e.g. the open-source *Virtual Display Driver* or
  *parsec-vdd*) through the same abstraction (`DisplayBackend` trait). This
  lets us release the host and client before our own driver is signed.
- Installer: WiX/MSIX bundle that installs the driver with `pnputil`/`devcon`,
  installs the host agent as a per-user startup app, and adds a firewall rule
  for the agent's port.

---

## 5. Host agent (`host/`)

A Rust process with a tray icon (Tauri), split into modules:

| Module | Responsibility |
|---|---|
| `display` | `DisplayBackend` trait; `ParkScreenIdd` and `ThirdPartyVdd` implementations; plug/unplug; set modes |
| `capture` | WGC capture of the virtual monitor's `HMONITOR`; dirty-rect and "no change" detection; cursor shape and position |
| `encode` | **Media Foundation H.264 hardware MFT** (NVENC / Quick Sync / AMF are all exposed through MF), and a software fallback (openh264). Also a JPEG encoder for the fallback transport. |
| `transport` | HTTPS + WSS server (axum + rustls); optional WebRTC (`webrtc-rs`) |
| `input` | Turns client touch and pointer events into `InjectSyntheticPointerInput` (real touch) or `SendInput` (mouse mode), mapped to the virtual monitor's desktop coordinates |
| `pairing` | Device pairing, tokens, certificate management (§6) |
| `ui` | Tray menu: status, the connected car, resolution, quality preset, "disconnect", and the URL/PIN to type in the car |

### 5.1 Encoder settings (low latency)
- H.264 **Constrained Baseline or Main**. This is the most widely supported
  profile, and Tesla hardware decode is likely limited to it.
- No B-frames, `LowLatencyMode = TRUE`, a single slice or a small slice count.
- CBR or capped VBR, 8–20 Mbps by default. Quality presets: *Battery/Hotspot*
  (5 Mbps, 30 fps), *Balanced*, *Sharp* (text-optimised: higher QP floor and
  bitrate).
- An IDR when a client joins or on request (lost frame or decoder reset), and
  periodic intra-refresh instead of large keyframes. This avoids bitrate
  spikes on Wi-Fi.
- Skip encoding when nothing on screen has changed (static desktops cost close
  to zero bandwidth).

### 5.2 Adaptive quality
The client reports decode time, dropped frames and its buffer level once per
second. The host changes bitrate, fps and (as a last resort) resolution scale.

---

## 6. Connectivity, security and pairing

### 6.1 Network topologies
1. **Same Wi-Fi**: car on home Wi-Fi in the garage, PC on the same LAN. This
   is the primary case.
2. **Phone hotspot**: laptop and car both on the phone's hotspot. Some phones
   isolate clients; the probe page detects this and explains it.
3. **Laptop as hotspot**: the Windows Mobile Hotspot, with the car joining the
   laptop. This works without any other network, which is great for laptops
   in the car. The host agent can offer to turn it on.
4. **Different networks**: needs the cloud relay (v2, §6.3).

### 6.2 HTTPS on the LAN without certificate warnings
We use the same pattern as Plex's `*.plex.direct`:

- We own a domain, e.g. `parkscreen.direct`. Each host gets a unique ID and a
  **publicly trusted wildcard certificate** for `*.<hostid>.parkscreen.direct`.
  Our backend issues it via ACME DNS-01 and the host fetches it on first run
  and on renewal.
- Our DNS answers `192-168-1-50.<hostid>.parkscreen.direct` with
  `192.168.1.50`.
- The car opens `https://parkscreen.app`. That static page (served from a
  CDN) looks up the host by pairing code and redirects to the host's
  LAN hostname. From then on everything is direct HTTPS/WSS on the LAN, with a
  valid certificate and a secure context.
- **Offline fallback:** the host also serves plain `http://<lan-ip>:port`
  with the JPEG and WebSocket transport, which needs no secure context. This
  is worse quality, but it always works.

Note: some routers block DNS answers that point to private IPs ("DNS rebinding
protection"). The probe detects this and falls back to the IP URL.

### 6.3 Cloud relay (v2)
- Signalling over WSS through our backend, with WebRTC media P2P when
  possible and TURN when not. This only applies if Phase 0 shows that WebRTC
  works in the Tesla browser.
- Costs money (TURN bandwidth), so it would probably be a paid tier.

### 6.4 Pairing and auth
- The tray app shows a **6-digit code**. The user types
  `parkscreen.app` on the car and enters the code. (A QR code does not help
  because the car cannot scan it.)
- Pairing exchanges a long-lived device token, stored in the car browser's
  `localStorage` (and as a cookie), so later visits connect without a code.
- Every WS/WebRTC session is authenticated with that token. The host keeps an
  allow-list of paired cars, which it can revoke from the tray.
- Input injection is off until the user turns it on per paired device. This
  matters because the car can control the PC.

---

## 7. Web client (`web/`)

### 7.1 Transports, ranked
The client runs a probe at startup and picks the first transport that works:

| Rank | Transport | Decode | Needs |
|---|---|---|---|
| 1 | **WebRTC** (H.264 video track + data channel for input and cursor) | Browser's `<video>`, hardware | `RTCPeerConnection` and H.264 in SDP |
| 2 | **WebCodecs over WSS** (raw H.264 Annex-B access units) | `VideoDecoder` → `<canvas>`/WebGL | Secure context and `VideoDecoder` |
| 3 | **MSE over WSS** (fragmented MP4, one frame per fragment) | `<video>` with `MediaSource` | `MediaSource.isTypeSupported('video/mp4; codecs="avc1.42E01F"')` |
| 4 | **JPEG over WS** (dirty tiles) | `createImageBitmap` → canvas | Nothing special; always works |

The host supports all four from the same capture pipeline: the H.264 bitstream
feeds 1–3, and the JPEG tile encoder feeds 4.

MSE adds latency because of buffering. We fight it by using `liveSeekableRange`
and seeking to the live edge when the buffer grows past about 100 ms.

### 7.2 UI
- Full-screen canvas or video, with a black letterbox if the aspect ratio
  differs.
- Touch:
  - **Direct touch mode** (default): taps and drags become touch or mouse
    events.
  - **Trackpad mode**: relative cursor movement, which works better for small
    UI elements.
- A two-finger tap opens a small overlay: connection stats, quality preset,
  mode toggle, keyboard (on-screen keyboard text sent as Unicode `SendInput`),
  disconnect.
- Hints for entering browser full-screen and for the Tesla "Theater"
  shortcut, where these help increase the viewport.
- Keep the screen awake while connected. Use the Wake Lock API if available,
  and otherwise accept that Tesla's own display timeout applies.

### 7.3 Viewport → mode negotiation
When the client connects, and again after a resize, it sends
`{w, h, dpr, refreshHint}`. The host asks the driver to add the mode and set
it, then restarts the encoder at that size, and Windows rearranges the
desktop. A setting lets users choose a lower resolution and a higher Windows
scale factor (e.g. 150%) so text is readable at arm's length.

---

## 8. Roadmap

### Phase 0: Feasibility (1–2 weeks)
- [ ] `web/probe`: a page that reports the user agent and Chromium version,
      viewport and DPR, WebRTC and H.264 support, WebCodecs, MSE codec
      strings, WebGL, Wake Lock, secure-context behaviour, measured WS
      throughput and RTT to a test host, and whether rebinding DNS resolves.
      It uploads an anonymous report.
- [ ] Run it on at least one Intel and one AMD Tesla, in Park.
- [ ] **Decision gate:** pick the default transport. Confirm the
      `parkscreen.direct` certificate approach is needed and works.
- [ ] Start EV certificate and Partner Center registration (long lead time).

### Phase 1: MVP over LAN (4–6 weeks)
- [ ] Host: `ThirdPartyVdd` backend (existing signed driver), WGC capture, MF
      H.264 encode, WSS server, and the transport chosen in Phase 0, plus JPEG
      fallback.
- [ ] Web: player for the chosen transport, plus JPEG fallback.
- [ ] Viewport mode negotiation (if the backend supports custom modes).
- [ ] Manual pairing with a PIN, and the plain HTTP IP fallback.
- **Exit criteria:** extend a desktop to a Model 3 at native viewport
  resolution, 30 fps or more, under 150 ms latency, stable for 1 hour.

### Phase 2: Own driver plus input (4–6 weeks)
- [ ] ParkScreen IddCx driver (Stage A): dynamic modes, plug/unplug, IOCTLs.
- [ ] Touch and mouse input injection, and trackpad mode.
- [ ] Hardware cursor channel.
- [ ] Adaptive bitrate.

### Phase 3: Product polish (3–4 weeks)
- [ ] `parkscreen.direct` DNS and certificate service, and `parkscreen.app`
      landing and pairing page.
- [ ] Installer (signed driver, agent, firewall rule) and auto-update.
- [ ] Tray UI, quality presets, and the laptop-hotspot helper.
- [ ] Attestation-signed driver release.

### Phase 4: Performance and v2
- [ ] Driver Stage B (shared-texture frames, dirty rectangles).
- [ ] Cloud relay (WebRTC with TURN) for different networks.
- [ ] Audio, multiple monitors, HEVC/AV1 where the car can decode them.

---

## 9. Risks and mitigations

| Risk | Impact | Mitigation |
|---|---|---|
| Tesla browser lacks WebRTC, WebCodecs or MSE on some cars | Lower quality | Four-tier transport fallback; Phase 0 probe on real hardware |
| Firmware updates change browser behaviour | Breakage | The probe runs on every connect; telemetry on which transport is chosen; fallback chain |
| Driver signing delay or cost | Can't ship our own driver | Third-party signed VDD backend from day one; start EV and Partner Center in Phase 0 |
| Mixed content and certificate warnings | Poor setup experience | `*.parkscreen.direct` trusted wildcard; plain HTTP JPEG fallback |
| Router DNS-rebinding protection | Hostname doesn't resolve | Detect it, fall back to IP URL, and document the router setting |
| Phone hotspot client isolation | Can't connect | Detect it, recommend the laptop-hotspot mode |
| Weak decode on Intel Atom cars | Stutter | 30 fps / 720p "Battery" preset; adaptive quality |
| Remote input is a security surface | PC takeover | Paired-token auth, input off by default, LAN-only by default, TLS everywhere |
| Use while driving | Safety / legal | Built for Park; we do **not** detect or work around Tesla's driving video lockout. The JPEG fallback also respects that lockout: the client pauses if the page is hidden or blocked. Clear in-app warnings. |

---

## 10. Repository layout (planned)

```
ParkScreen/
├── PLAN.md
├── driver/            # IddCx UMDF driver (Visual Studio + WDK solution)
│   ├── ParkScreenIdd/
│   └── inf/
├── host/              # Rust workspace
│   ├── crates/display/
│   ├── crates/capture/
│   ├── crates/encode/
│   ├── crates/transport/
│   ├── crates/input/
│   └── app/           # Tauri tray app
├── web/
│   ├── probe/         # Phase 0 capability probe
│   └── client/        # player
├── backend/           # parkscreen.app + DNS/cert issuance (Phase 3)
└── installer/         # WiX / MSIX
```

## 11. Open questions
1. Is there a Tesla-side way to keep the browser full-screen or remove the
   browser chrome, to make the most of the panel? (Measure in Phase 0.)
2. Should we support "mirror" mode (duplicating an existing monitor) as well
   as "extend"? It is cheap to add in the host because it is just capturing a
   different `HMONITOR`.
3. Licensing and business model: open-source driver and client, with a paid
   relay?
