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
- Setup takes no more than one installer on the PC and one bookmark on the car:
  **`https://parkscreen.web.app`**.

**Non-goals (v1)**
- macOS/Linux hosts. The architecture allows them later.
- Audio in the MVP. It moves to Phase 3 (§7.1).
- Use while driving.
- Streaming when the car and PC are on different networks. This needs a TURN
  relay and is a Phase 4 feature (§6.3).

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

### 2.1 Network path

The web client is hosted on **Firebase Hosting** at `https://parkscreen.web.app`, and the
signalling server runs on **Cloud Run** (same pattern as Teeline). The server only brokers
the connection. **Video and touch go peer-to-peer over the LAN** through WebRTC, and never
through Cloud Run or Firebase.

```
        Firebase Hosting                       Cloud Run (1 instance) + Firestore
     parkscreen.web.app (static)         parkscreen-server  (WSS signalling, pairing)
              ▲                                   ▲                       ▲
              │ HTTPS (page)      HTTPS + WSS     │            outbound   │
              │             (pairing, signalling) │            WSS        │
┌─────────────┴──── Tesla ───────────┐            │   ┌──── Windows PC ───┴──────────┐
│ Tesla browser                      │────────────┘   │ ParkScreen IDD ⇄ Host Agent  │
│ full-screen <video>                │◀═ WebRTC ═════▶│ capture · H.264 · input      │
└────────────────────────────────────┘  video ▶       └──────────────────────────────┘
                                        ◀ touch (P2P, LAN, DTLS-SRTP)
```

Because the page (Firebase) and the server (Cloud Run) are on different origins, the page
and the host agent connect to the Cloud Run URL directly, and the server checks `Origin`
and answers CORS. See `deploy/README.md`.

This has several benefits:
- **Valid HTTPS with nothing to configure on the PC**: Google terminates TLS. The page is
  a secure context, so every browser API is available (including WebCodecs later).
- **The PC needs no inbound ports.** The host agent dials *out* over WSS.
- **One URL for every car and every PC.** Users bookmark `parkscreen.web.app` once, and
  Tesla syncs bookmarks across cars.
- **No VM to patch**, and the host agent's installer and update feed use the same
  Firebase Hosting + Velopack pattern as Teeline.

Limits: one server instance (state is in memory), so about 500 concurrent sessions, and
Cloud Run closes each WebSocket after at most 60 minutes (clients reconnect).

There are four deliverables:

| Component | Language / stack | Location in repo |
|---|---|---|
| Virtual display driver | C++ / UMDF 2 / IddCx | `driver/` |
| Host agent + tray app | Rust (`windows` crate), Tauri for the UI | `host/` |
| Web client | TypeScript, Vite, no framework (keeps it light for the car's CPU) | `web/` |
| Signalling server + deployment | Rust (axum) on Cloud Run, Firestore, Firebase Hosting | `server/`, `deploy/` |

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
| A6 | `RTCPeerConnection` works on plain `http://` pages. Camera and mic, WebCodecs and some other APIs need HTTPS. | Chromium platform rules. | The primary URL is HTTPS on Firebase Hosting, so every API is available. The offline `http://<pc-ip>` fallback still works for WebRTC. |
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
| `transport` | Outbound WSS client to `wss://<cloud-run-url>/ws/host` (with reconnect and backoff); WebRTC sender (`webrtc-rs`) with H.264 track and data channels; local HTTP/WS server on `:8765` for the offline and JPEG fallbacks (§6.2, §7.1) |
| `input` | Turns client touch and pointer events into `InjectSyntheticPointerInput` (real touch) or `SendInput` (mouse mode), mapped to the virtual monitor's desktop coordinates |
| `pairing` | Host identity key, pairing codes, the allow-list of paired cars (§6.4) |
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
4. **Different networks**: needs a TURN relay (Phase 4, §6.3).

In every case **the car needs internet access** to load the page and reach
the signalling server. Teslas have this through premium connectivity or
Wi-Fi. Only the media has to stay local.

### 6.2 How the car reaches the PC

1. The host agent starts and opens `wss://<cloud-run-url>/ws/host`,
   then authenticates with its host key (§6.4). The server marks the host
   online.
2. The car opens `https://parkscreen.web.app`. The page loads from
   Firebase Hosting (cached by a service worker after the first visit) and opens
   `wss://<cloud-run-url>/ws/car`. It then authenticates with its paired-car token, or
   enters a pairing code.
3. The server relays the SDP offer and answer and the ICE candidates between
   the two sockets. That is all the server does.
4. WebRTC ICE finds the direct LAN path, and media flows P2P. After that, the
   server could go away and the session would continue.

**ICE details**
- **STUN:** `stun:stun.cloudflare.com:3478` (free). It is only needed to give
  server-reflexive candidates for the hotspot and different-network cases.
- **mDNS candidates:** Chromium hides the car's LAN IP behind a random
  `*.local` name (it hasn't granted camera permission). The host agent
  publishes *real* host candidates and enables mDNS query mode in `webrtc-rs`.
  Either side's real address is enough for ICE to connect, because the other
  side is learned as a peer-reflexive candidate.
- We add ICE-TCP host candidates for networks that block UDP between clients.

**Offline fallback (no internet):** the host agent also serves the client at
`http://<pc-lan-ip>:8765` with a built-in local signalling endpoint. This
covers a laptop acting as a hotspot with no internet upstream. It works
because WebRTC doesn't need HTTPS (A6). This is a fallback only, and not
advertised in the UI by default.

### 6.3 Firebase + Cloud Run deployment

Hosting is set up like Teeline; the details, one-time setup commands and CI variables are
in `deploy/README.md`.

- **Client:** Firebase Hosting site `parkscreen` (`https://parkscreen.web.app`), built with
  `VITE_SERVER_URL` pointing at the server. `/assets/*` is cached immutably; HTML is not.
- **Server (`parkscreen-server`, Rust/axum):** Cloud Run, `--max-instances=1`,
  `--concurrency=1000`, `--timeout=3600`, unauthenticated at the IAM level (the host key
  and car token are the authentication). Pairings are held in memory and written through to
  Firestore (`hosts`, `pairings`); SQLite is the local-development store.
- **Origins:** the page and server are different origins, so the server answers CORS for
  `POST /api/pair/claim` and refuses WebSocket upgrades from other sites
  (`ALLOWED_ORIGINS`). The car token is kept in `localStorage`, not a cookie.
- **WebSockets:** Cloud Run closes each after at most 60 minutes. Clients and the host
  send a ping every 30 s and reconnect with backoff, and a running session does not depend
  on its signalling socket.
- **Rate limiting:** failed pairing attempts are capped (20 per minute, server-wide).
  There is no WAF in front, so consider Cloud Armor if this is ever public at scale.
- **Host agent releases:** `parkscreen-releases` Firebase site holding a Velopack feed,
  published by `.github/workflows/release.yml`, signed with Azure Trusted Signing.
- **CI/CD:** `.github/workflows/deploy.yml` deploys the server and the client on every push
  to `main`, authenticating with Workload Identity Federation (no stored keys).

**TURN for different networks (Phase 4):** neither Firebase nor Cloud Run can relay WebRTC
media. Options: **Cloudflare Realtime TURN** (managed, pay per GB, free monthly allowance)
or `coturn` on a small VM with UDP 3478 open. The server hands out short-lived TURN
credentials per session.

### 6.4 Pairing and auth
- **Host identity:** on first run the host agent generates an Ed25519 key
  pair and stores it with DPAPI. It registers the public key with the server,
  and authenticates each `/ws/host` connection by signing a server nonce.
- **Pairing a car:** the user clicks "Pair a car" in the tray. The host
  requests a **6-digit code**, valid for 5 minutes and single-use. The user
  opens `parkscreen.web.app` in the car and types the code. The server
  issues the car a random 256-bit **car token**, stored in a long-lived
  `HttpOnly; Secure; SameSite=Strict` cookie (and in `localStorage` as a
  backup), and links it to that host. (A QR code does not help because the
  car cannot scan it.)
- **Later visits:** the car's token automatically connects it to its paired
  host(s). If several hosts are online, the car shows a picker.
- **Defence in depth:** the server only brokers sessions. The host agent
  also checks the car token's fingerprint against its own allow-list before
  answering an offer, so a compromised server can't attach an unknown car. Hosts
  can revoke cars from the tray.
- **The server never sees screen content.** Media and input are end-to-end
  DTLS-encrypted between the PC and the car. The host agent checks the DTLS
  fingerprint it receives in the relayed SDP.
- Input injection is off until the user turns it on per paired car. This
  matters because the car can control the PC.

---

## 7. Web client (`web/`)

### 7.1 Transports

| Rank | Transport | Used on | Decode |
|---|---|---|---|
| 1 | **WebRTC**: H.264 Constrained Baseline video track, plus an unreliable data channel for pointer and cursor, and a reliable one for control | All cars (A2, A3) | `<video>`, hardware decode, full-screen |
| 2 | **JPEG dirty tiles over a plain WebSocket on the LAN** | Only if WebRTC fails to connect (old firmware, or a network blocking UDP). The hosted page can't open `ws://` to a LAN IP (mixed content), so it offers a button that switches to the PC's local page `http://<pc-ip>:8765` (the address is learned through signalling) | `createImageBitmap` → `<canvas>` |

We have dropped MSE and WebCodecs from v1. WebRTC covers the target cars and
already gives us jitter buffering, congestion control (via the
Transport-CC/GCC algorithm in `webrtc-rs`), NACK and PLI keyframe requests for
free. **WebCodecs fed from a WebRTC data channel** stays a **Phase 4
experiment** for lower latency on Ryzen cars. The hosted page is HTTPS, so
the API is available, and the stream stays P2P.

**Video never goes through Cloud Run or Firebase.** That keeps
the server cheap and keeps latency low.

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

### Phase 0: Validate assumptions, signing and hosting (1 week)
- [ ] Create the Firebase sites and the Cloud Run service (`deploy/README.md`), so
      `parkscreen.web.app` serves the page over HTTPS.
- [ ] `web/probe` deployed there. It checks A1–A6 on a real car (user agent,
      H.264 in `RTCRtpReceiver.getCapabilities`, Fullscreen API on `<video>`,
      panel size and DPR, WebSocket survival to Cloud Run over 10
      minutes). It plays a 60-second WebRTC test stream from a PC on the same
      LAN, logs decode fps and dropped frames, and posts a report.
- [ ] Run it on at least one Ryzen car (and an Intel car if available), in
      Park.
- [ ] **Signing spike:** sign Microsoft's `IddSampleDriver` with Azure Trusted
      Signing and install it on a Windows 11 VM with Secure Boot, HVCI and
      Smart App Control on (§4.4).

### Phase 1: MVP (4–5 weeks)
- [ ] `server/`: axum signalling and pairing, SQLite, embedded web client,
      Docker image, GitHub Action → Cloud Run deploy (`deploy.yml`).
- [ ] Host: `ThirdPartyVdd` backend, WGC capture, MF H.264 hardware encode,
      `webrtc-rs` sender, outbound WSS to the server.
- [ ] Web: WebRTC player, full-screen button, stats overlay, pairing screen,
      JPEG fallback.
- [ ] Viewport mode negotiation.
- [ ] Sign the host binaries and installer with Azure Trusted Signing in CI.
- **Exit criteria:** pair a Ryzen Model 3/Y through
  `parkscreen.web.app`, and extend a desktop at 1920×1200, 60 fps, under
  100 ms latency over the LAN, stable for 1 hour. On Intel, 720p30 must be
  usable.

### Phase 2: Own driver plus input (4–6 weeks)
- [ ] ParkScreen IddCx driver (Stage A): dynamic modes, plug/unplug, IOCTLs,
      signed per the Phase 0 decision.
- [ ] Touch and mouse input over the data channel, and trackpad mode.
- [ ] Hardware cursor channel.

### Phase 3: Product polish (3–4 weeks)
- [ ] Audio track.
- [ ] Offline local fallback (§6.2).
- [ ] Installer, auto-update, tray UI, quality presets, laptop-hotspot helper.
- [ ] Service worker so the page loads instantly; multi-host picker.

### Phase 4: Performance and v2
- [ ] Driver Stage B (shared-texture frames, dirty rectangles).
- [ ] TURN for different networks (Cloudflare Realtime TURN or coturn).
- [ ] WebCodecs-over-data-channel experiment; HEVC/AV1 if the car's decoder supports
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
| Cloud Run or Firebase outage | Can't start new sessions; running sessions continue (P2P) | Offline local fallback (§6.2); uptime check on `/status` |
| Cloud Run closes WebSockets after 60 min, and a single instance holds about 1000 sockets | Signalling drops hourly; capacity cap (~500 sessions) | 30 s pings and auto-reconnect; sessions don't depend on the socket once connected; move routing out of memory before scaling out |
| Pairing code brute force, or token theft | Unauthorised car attaches | Short-lived single-use codes; WAF rate limit; hashed tokens; host-side allow-list; DTLS fingerprint check |
| Remote input is a security surface | PC takeover | Paired-token auth, input off by default, Media stays on the LAN. WebRTC media and data channels are always DTLS-encrypted end to end; signalling is WSS to Cloud Run |
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
├── server/            # parkscreen-server: signalling, pairing, serves web client
├── protocol/          # shared message types (Rust → TS via ts-rs)
├── deploy/            # Firebase + Cloud Run setup notes
├── firebase.json      # Hosting targets: parkscreen, parkscreen-releases
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
