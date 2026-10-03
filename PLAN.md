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
- Audio in the MVP. It moves to Phase 3 (§7.1).
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
│  └──────────────────┘             │  - HTTP/WS signalling │  │
│                                   │  - WebRTC sender      │  │
│                                   │  - input injection    │  │
│                                   │  - tray UI / pairing  │  │
│                                   └──────────┬────────────┘  │
└──────────────────────────────────────────────┼───────────────┘
                                               │ LAN / phone hotspot
                                    video ▼    ▲ touch, viewport size
┌──────────────────────────── Tesla ───────────┴───────────────┐
│  Tesla browser (Chromium) → ParkScreen web client            │
│   WebRTC (JPEG/WS fallback) → full-screen <video>            │
└──────────────────────────────────────────────────────────────┘
```

There are three deliverables:

| Component | Language / stack | Location in repo |
|---|---|---|
| Virtual display driver | C++ / UMDF 2 / IddCx | `driver/` |
| Host agent + tray app | Rust (`windows` crate), Tauri for the UI | `host/` |
| Web client | TypeScript, Vite, no framework (keeps it light for the car's CPU) | `web/` |

---

## 3. The Tesla browser: working assumptions

We stopped treating browser support as unknown. We design against the
following assumptions, based on public information as of October 2026. A
short check on a real car (Phase 0) confirms them, but it no longer decides
the architecture.

### 3.1 Assumptions

| # | Assumption | Basis | Design consequence |
|---|---|---|---|
| A1 | The browser is **Chromium-based** on every car, and recent firmware tracks a fairly modern Chromium. | Tesla moved to Chromium in 2019; the 2026.26 browser update added new features that need a current engine. | Target evergreen Chromium APIs; no polyfills for old Chrome. |
| A2 | **WebRTC works, including H.264 video receive and data channels**, on AMD Ryzen cars running 2026.26 or later. | 2026.26 officially supports Google Meet, Microsoft Teams, Discord and Slack video calls in the browser. All of these are WebRTC. | **WebRTC is the primary transport.** |
| A3 | WebRTC receive also works on **Intel Atom** cars, but decoding is weaker. Camera and mic features are Ryzen-only. | Existing phone and laptop mirroring products (TeslaStream, CrankWheel, Tesla Display) stream to the car browser over WebRTC; 2026.26 camera/mic is limited to Ryzen. | Intel cars get a **720p30 default** preset. We never rely on camera or mic. |
| A4 | The **Fullscreen API** works on a `<video>` element. | 2026.26 added full-screen video to the browser. | Render into `<video>` and call `requestFullscreen()` to use the whole panel, with no browser chrome. |
| A5 | `<video>` playback is **blocked while the car is in gear** and allowed in Park. | Long-standing Tesla policy. | ParkScreen is Park-only. We use a normal `<video>` element and do **not** use canvas tricks to get around the lockout. |
| A6 | `RTCPeerConnection` works on plain `http://` pages. Camera and mic, WebCodecs and some other APIs need HTTPS. | Chromium platform rules. | v1 can run from `http://<pc-ip>` on the LAN with no certificates. HTTPS becomes a Phase 3 polish item, not a blocker. |
| A7 | DRM (EME) is irrelevant. | We stream our own non-DRM video. | None. |

### 3.2 Target devices

| Tier | Cars | Default stream |
|---|---|---|
| **Primary** | AMD Ryzen infotainment (Model 3/Y since about 2021–22, Highland and Juniper, S/X 2021+, Cybertruck) on 2026.26 or later | Full-screen at native panel resolution, 60 fps, WebRTC H.264 |
| **Secondary** | Intel Atom infotainment | 1280×720 at 30 fps, WebRTC H.264. JPEG fallback if WebRTC fails |

### 3.3 Panel resolutions

| Vehicle | Panel | Full-screen `<video>` target |
|---|---|---|
| Model 3 / Y | 1920×1200 | 1920×1200 |
| Model S / X (2021+) | 2200×1300 | 2200×1300 |
| Cybertruck | 18.5" panel | Use the reported size |

The client still reports its real pixel size
(`screen.width × screen.height × devicePixelRatio` once full-screen), and the
driver adds that exact mode (§4.3), so frames are 1:1 with no scaling blur.

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

### 4.4 Signing and installation

You already have an **Azure Trusted Signing** account (now also called Azure
Artifact Signing). Here is where it fits:

| What | Signed with | Notes |
|---|---|---|
| Host agent `.exe`/`.dll`, Tauri app, installer (MSI/MSIX) | **Azure Trusted Signing** ✅ | The intended use: it builds SmartScreen reputation and avoids "unknown publisher" warnings. Run it in CI with `signtool` and the Trusted Signing dlib, or with the `azure/trusted-signing-action` GitHub Action. |
| Driver binary (`ParkScreenIdd.dll`) and catalog (`.cat`) | **Azure Trusted Signing, to be tested in Phase 0** ⚠️ | See below. |
| Microsoft attestation signing through Partner Center | ❌ Not possible with Trusted Signing | Partner Center needs an **EV** certificate on the account. Microsoft has confirmed Trusted Signing is not EV and is not supported for this. |

**Why the driver may still work with Trusted Signing:** ParkScreen's driver is
a **user-mode (UMDF) driver**. Windows only requires a *Microsoft* signature
for *kernel-mode* drivers. User-mode drivers only need a valid signature that
chains to a trusted root. This is how open-source IddCx drivers such as
*Virtual Display Driver* ship today: they are signed with a
**SignPath Foundation** certificate, not attestation-signed. Trusted Signing
certificates chain to Microsoft's trusted "Identity Verification" root and are
time-stamped, so:

1. **Phase 0 spike:** sign the `.dll` and `.cat` with Trusted Signing, then
   install with `pnputil /add-driver parkscreen.inf /install` on a clean
   Windows 11 24H2+ VM with Secure Boot, HVCI (memory integrity) and Smart App
   Control all on. Check that the device starts with no warnings, and test
   Windows 10 22H2 too.
2. **If that works:** use Trusted Signing for everything, at no extra cost.
3. **If PnP rejects it** (Microsoft states Trusted Signing "doesn't support
   driver signing", so this is a real possibility), in order of preference:
   - (a) Apply to **SignPath Foundation** (free if the driver is open source),
     like Virtual Display Driver does.
   - (b) Buy an **EV certificate** (about $300–500/year) and do Partner Center
     attestation signing. This is the most robust option.
   - (c) Until then, ship with the `ThirdPartyVdd` backend (§5) that drives an
     already-signed open-source IddCx driver.

**Development:** test-signing mode (`bcdedit /set testsigning on`) with a
self-signed certificate.

**Installer:** WiX MSI (signed with Trusted Signing). It installs the driver
package with `pnputil`, installs the host agent as a per-user startup app,
and adds a firewall rule (private networks only) for the agent's port.

## 5. Host agent (`host/`)

A Rust process with a tray icon (Tauri), split into modules:

| Module | Responsibility |
|---|---|
| `display` | `DisplayBackend` trait; `ParkScreenIdd` and `ThirdPartyVdd` implementations; plug/unplug; set modes |
| `capture` | WGC capture of the virtual monitor's `HMONITOR`; dirty-rect and "no change" detection; cursor shape and position |
| `encode` | **Media Foundation H.264 hardware MFT** (NVENC / Quick Sync / AMF are all exposed through MF), and a software fallback (openh264). Also a JPEG encoder for the fallback transport. |
| `transport` | HTTP + WS signalling server (axum) on `:8765`; WebRTC sender (`webrtc-rs`) with H.264 track and data channels; JPEG/WS fallback |
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

### 6.2 How the car reaches the PC

Because WebRTC works on plain HTTP pages (A6), v1 needs no certificates:

- **v1 (LAN, zero infrastructure):** the host agent serves the client at
  `http://<pc-lan-ip>:8765`. Signalling (SDP and ICE) runs over `ws://` on the
  same port, then media flows over WebRTC directly on the LAN. The tray app
  shows the URL, and the user bookmarks it once in the car (bookmarks sync
  across cars since 2026.26). We use a fixed port and the PC's hostname where
  mDNS resolves, so a changed DHCP address doesn't break the bookmark.
- **Phase 3 (nice URL plus HTTPS):** the car opens `https://parkscreen.app`, a
  static CDN page. Signalling goes through our small WSS backend, and media is
  still **P2P over the LAN** through WebRTC ICE. Opening WebRTC to a LAN peer
  from an HTTPS page is not mixed content. This gives one memorable URL with
  no per-PC certificates, and it also opens the way to the cloud relay.
- **No `*.parkscreen.direct` wildcard certificate service.** We dropped this
  from the plan, because WebRTC removes the need for it.

### 6.3 Cloud relay (v2)
- Signalling over WSS through our backend, with WebRTC media P2P when
  possible and TURN when not. This only applies if Phase 0 shows that WebRTC
  works in the Tesla browser.
- Costs money (TURN bandwidth), so it would probably be a paid tier.

### 6.4 Pairing and auth
- The tray app shows a **6-digit code**. In v1 the user opens the LAN URL on the car;
  in Phase 3 they open `parkscreen.app`. Either way they enter the code. (A QR code does not help
  because the car cannot scan it.)
- Pairing exchanges a long-lived device token, stored in the car browser's
  `localStorage` (and as a cookie), so later visits connect without a code.
- Every WS/WebRTC session is authenticated with that token. The host keeps an
  allow-list of paired cars, which it can revoke from the tray.
- Input injection is off until the user turns it on per paired device. This
  matters because the car can control the PC.

---

## 7. Web client (`web/`)

### 7.1 Transports

| Rank | Transport | Used on | Decode |
|---|---|---|---|
| 1 | **WebRTC**: H.264 Constrained Baseline video track, plus an unreliable data channel for pointer and cursor, and a reliable one for control | All cars (A2, A3) | `<video>`, hardware decode, full-screen |
| 2 | **JPEG dirty tiles over WebSocket** | Only if WebRTC fails to connect (old firmware, or a network blocking UDP) | `createImageBitmap` → `<canvas>` |

We have dropped MSE and WebCodecs from v1. WebRTC covers the target cars and
already gives us jitter buffering, congestion control (via the
Transport-CC/GCC algorithm in `webrtc-rs`), NACK and PLI keyframe requests for
free. WebCodecs over WSS stays a **Phase 4 experiment** for lower latency on
Ryzen cars (it needs HTTPS, so it depends on Phase 3).

Low-latency WebRTC tuning:
- Set `playoutDelayHint`/`jitterBufferTarget = 0` on the receiver.
- Send at most 1 frame in flight, and prefer temporal-layer drops over
  queuing.
- Use `contentHint = "text"` on the host side so the encoder favours sharpness
  over motion.
- The client tells the host to send at most `min(panel fps, 60)`.

Audio: a WebRTC audio track (WASAPI loopback → Opus) is cheap to add, so we
move it from non-goal to Phase 3.

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

### Phase 0: Validate assumptions and signing (1 week)
- [ ] `web/probe`: one page that checks A1–A6 on a real car (user agent,
      `RTCPeerConnection` with H.264 in `getCapabilities`, Fullscreen API on
      `<video>`, panel size and DPR). It plays a 60-second WebRTC loopback
      stream from a test host, logs decode fps and dropped frames, and
      uploads a report.
- [ ] Run it on at least one Ryzen car (and an Intel car if available), in
      Park.
- [ ] **Signing spike:** sign a build of Microsoft's `IddSampleDriver` with
      Azure Trusted Signing and install it on a Windows 11 VM with Secure Boot,
      HVCI and Smart App Control on (§4.4). Decide on Trusted Signing,
      SignPath or an EV certificate.

### Phase 1: MVP over LAN (4–5 weeks)
- [ ] Host: `ThirdPartyVdd` backend, WGC capture, MF H.264 hardware encode,
      `webrtc-rs` sender, HTTP and WS signalling server on `:8765`.
- [ ] Web: WebRTC player, full-screen button, stats overlay, JPEG fallback.
- [ ] Viewport mode negotiation.
- [ ] PIN pairing.
- [ ] Sign the host binaries and installer with Azure Trusted Signing in CI.
- **Exit criteria:** extend a desktop to a Ryzen Model 3/Y at
  1920×1200, 60 fps, under 100 ms latency, stable for 1 hour. On Intel,
  720p30 must be usable.

### Phase 2: Own driver plus input (4–6 weeks)
- [ ] ParkScreen IddCx driver (Stage A): dynamic modes, plug/unplug, IOCTLs,
      signed per the Phase 0 decision.
- [ ] Touch and mouse input over the data channel, and trackpad mode.
- [ ] Hardware cursor channel.

### Phase 3: Product polish (3–4 weeks)
- [ ] `parkscreen.app` page and WSS signalling backend (media stays P2P on the
      LAN).
- [ ] Audio track.
- [ ] Installer, auto-update, tray UI, quality presets, laptop-hotspot helper.

### Phase 4: Performance and v2
- [ ] Driver Stage B (shared-texture frames, dirty rectangles).
- [ ] TURN relay for when the car and PC are on different networks.
- [ ] WebCodecs-over-WSS experiment; HEVC/AV1 if the car's decoder supports
      them in WebRTC.
- [ ] Multiple monitors.

---

## 9. Risks and mitigations

| Risk | Impact | Mitigation |
|---|---|---|
| An assumption (A1–A6) is wrong on some cars | Lower quality or no WebRTC | Phase 0 probe; JPEG-over-WebSocket fallback |
| Firmware updates change browser behaviour | Breakage | Feature detection on every connect; telemetry on which transport is used |
| PnP rejects a Trusted Signing signature on the driver | Can't ship our own driver yet | Phase 0 spike; SignPath or EV as a fallback; ship with the third-party driver backend until then |
| Phone hotspot client isolation, or UDP blocked | WebRTC can't connect | ICE over TCP host candidates; JPEG/WS fallback; recommend laptop-hotspot mode |
| Weak decode on Intel Atom cars | Stutter | 720p30 default; adaptive bitrate through WebRTC congestion control |
| Remote input is a security surface | PC takeover | Paired-token auth, input off by default, LAN-only by default. WebRTC media and data channels are always DTLS-encrypted; v1 signalling is plain WS on the LAN (only SDP and the token), and it moves to WSS in Phase 3 |
| Use while driving | Safety / legal | Built for Park. We use a standard `<video>` element, so Tesla's driving lockout applies as designed. We do **not** use canvas tricks to get around it. The JPEG fallback is only used when WebRTC fails, and it stops while the page is hidden or video is blocked. Clear in-app warnings. |

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
├── backend/           # parkscreen.app signalling (Phase 3)
└── installer/         # WiX / MSIX
```

## 11. Open questions
1. Does full-screen `<video>` stay full-screen when the user touches it
   (needed for touch input), or does the car's UI come back? (Phase 0.)
2. Should we support "mirror" mode (duplicating an existing monitor) as well
   as "extend"? It is cheap to add in the host because it is just capturing a
   different `HMONITOR`.
3. Licensing and business model: open-source driver and client, with a paid
   relay?
