import type { IceServer, ServerToCar } from "./protocol";
import { Session } from "./session";
import { httpUrl } from "./config";
import { Signalling } from "./signalling";
import { attachInput } from "./input";

const ui = document.getElementById("ui")!;
const video = document.getElementById("v") as HTMLVideoElement;
const bar = document.getElementById("bar") as HTMLDivElement;
const stopBtn = document.getElementById("stop") as HTMLButtonElement;
const fullBtn = document.getElementById("full") as HTMLButtonElement;
stopBtn.onclick = () => disconnect();

// Full screen is the user's choice (a button), not something a first touch triggers. The whole page
// goes full screen, not just the video, so Disconnect stays reachable.
fullBtn.onclick = () => {
  if (document.fullscreenElement) void document.exitFullscreen?.().catch(() => {});
  else void document.documentElement.requestFullscreen?.().catch(() => {});
};
document.addEventListener("fullscreenchange", () => {
  fullBtn.textContent = document.fullscreenElement ? "Exit full screen" : "Full screen";
});

// This is a live screen, not a video to scrub: no browser or Windows media controls (pause, elapsed
// time, picture-in-picture, cast) for it. The attributes are also set in car/index.html.
video.disablePictureInPicture = true;
video.disableRemotePlayback = true;
video.controls = false;
if ("mediaSession" in navigator) {
  const ms = navigator.mediaSession;
  ms.metadata = null;
  // Any play/pause from system media keys or overlays keeps the stream playing.
  for (const action of ["play", "pause", "stop", "seekbackward", "seekforward", "seekto", "previoustrack", "nexttrack"] as const) {
    try { ms.setActionHandler(action, () => { void video.play().catch(() => {}); }); } catch { /* unsupported action */ }
  }
}
const KEY = "ps_token";
const store = {
  get: () => { try { return localStorage.getItem(KEY); } catch { return null; } },
  set: (t: string) => { try { localStorage.setItem(KEY, t); } catch { /* ignore */ } },
  clear: () => { try { localStorage.removeItem(KEY); } catch { /* ignore */ } },
};

const WARNING = `<p class="warn">Use only while parked. Tesla blocks video while the car is in gear, and ParkScreen does not work around that.</p>`;

function show(html: string) { ui.hidden = false; ui.innerHTML = html; }

/** Buttons shown on every waiting or error screen so the car is never stuck. */
const ACTIONS = `<p><button id="retry">Reconnect</button> <button id="reset" class="secondary">Reset pairing</button></p>`;

/** Show a status message with Reconnect and Reset pairing buttons. */
function status(message: string) {
  show(`<p>${message}</p>${ACTIONS}`);
  document.getElementById("retry")!.onclick = () => reconnect();
  document.getElementById("reset")!.onclick = () => resetPairing();
}

function teardown() {
  if (graceTimer) clearTimeout(graceTimer);
  if (retryTimer) clearTimeout(retryTimer);
  graceTimer = retryTimer = undefined;
  session?.close();
  session = undefined;
  stopReporting?.();
  detachInput?.();
  video.hidden = true;
  bar.hidden = true;
  if (document.fullscreenElement) void document.exitFullscreen?.().catch(() => {});
}

/** Close the stream from the car and stay disconnected until the user taps Reconnect. */
function disconnect() {
  teardown();
  sig?.close();
  sig = undefined;
  lastHost = undefined;
  status("Disconnected.");
}

/** Forget this car's pairing and go back to the code screen. */
function resetPairing() {
  teardown();
  sig?.close();
  sig = undefined;
  lastHost = undefined;
  store.clear();
  pairingScreen();
}

/** Throw the current connection away and start again from the signalling socket. */
function reconnect() {
  teardown();
  attempts = 0;
  connect();
}

function pairingScreen(error = "") {
  show(`<h1>ParkScreen</h1><p>Enter the 6-digit pairing code from ParkScreen on your PC.</p>
    <input id="code" inputmode="numeric" maxlength="6" autofocus><button id="go">Pair</button>
    <p class="warn">${error}</p>${WARNING}`);
  const go = async () => {
    const code = (document.getElementById("code") as HTMLInputElement).value.trim();
    const r = await fetch(httpUrl("/api/pair/claim"), { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ code }) });
    if (!r.ok) return pairingScreen("Invalid or expired code.");
    store.set((await r.json()).token);
    connect();
  };
  document.getElementById("go")!.onclick = go;
}

let sig: Signalling | undefined;
let session: Session | undefined;
/** Sent by the server before `hosts`; falls back to STUN only. */
let iceServers: IceServer[] = [{ urls: ["stun:stun.cloudflare.com:3478"] }];
let stopReporting: (() => void) | undefined;
let detachInput: (() => void) | undefined;
/** The PC we last streamed from, for automatic retries. */
let lastHost: string | undefined;
let attempts = 0;
let graceTimer: ReturnType<typeof setTimeout> | undefined;
let retryTimer: ReturnType<typeof setTimeout> | undefined;

function connect() {
  const token = store.get();
  if (!token) return pairingScreen();
  status("Connecting…");
  sig?.close();
  sig = new Signalling(token, onMsg, (up) => { if (!up) status("Reconnecting to ParkScreen…"); });
}

function startSession(hostId: string) {
  lastHost = hostId;
  session?.close();
  stopReporting?.();
  detachInput?.();
  if (graceTimer) clearTimeout(graceTimer);
  const me: Session = new Session(sig!, hostId, video, iceServers, (state) => {
    if (me !== session) return; // a replaced session
    if (state === "connected") {
      if (graceTimer) clearTimeout(graceTimer);
      attempts = 0;
      ui.hidden = true;
      video.hidden = false;
      bar.hidden = false;
      video.play().catch(() => {});
    } else if (state === "disconnected") {
      // Usually a blip that heals by itself: give it a few seconds before giving up.
      graceTimer = setTimeout(() => connectionLost(hostId), 4000);
    } else if (state === "failed") {
      connectionLost(hostId);
    }
  });
  session = me;
  session.start();
  stopReporting = session.startReporting();
  detachInput = attachInput(video, session.sendInput);
}

/** The peer connection died: retry on our own a few times, then wait for the user. */
function connectionLost(hostId: string) {
  session?.close();
  session = undefined;
  stopReporting?.();
  detachInput?.();
  video.hidden = true;
  bar.hidden = true;
  if (attempts < 3) {
    attempts++;
    status(`Connection lost. Retrying (${attempts} of 3)…`);
    retryTimer = setTimeout(() => { if (sig) startSession(hostId); }, 2000 * attempts);
  } else {
    status("Connection lost. Check that ParkScreen is running on your PC, then tap Reconnect.");
  }
}

function onMsg(m: ServerToCar) {
  switch (m.type) {
    case "error":
      if (m.message === "not paired") { store.clear(); pairingScreen("This car is no longer paired."); }
      break;
    case "ice_servers": iceServers = m.ice_servers; break;
    case "hosts": {
      const online = m.hosts.filter((h) => h.online);
      if (online.length === 0) status("Your PC is offline. Start ParkScreen on it.");
      else if (online.length === 1) startSession(online[0].host_id);
      else show(`<p>Choose a PC</p>` + online.map((h, i) => `<button data-h="${h.host_id}">PC ${i + 1}</button>`).join(""));
      ui.querySelectorAll<HTMLButtonElement>("button[data-h]").forEach((b) => (b.onclick = () => startSession(b.dataset.h!)));
      break;
    }
    case "host_online": startSession(m.host_id); break;
    case "host_offline": teardown(); status("Your PC went offline. It will reconnect on its own when ParkScreen is running again."); break;
    case "signal": session?.onSignal(m.payload); break;
  }
}

// Double-tap-with-two-fingers stats overlay is a Phase 2 item; for now expose stats in the console.
window.addEventListener("resize", () => session?.sendViewport());

connect();
