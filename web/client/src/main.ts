import type { ServerToCar } from "./protocol";
import { Session } from "./session";
import { Signalling } from "./signalling";

const ui = document.getElementById("ui")!;
const video = document.getElementById("v") as HTMLVideoElement;
const KEY = "ps_token";
const store = {
  get: () => { try { return localStorage.getItem(KEY); } catch { return null; } },
  set: (t: string) => { try { localStorage.setItem(KEY, t); } catch { /* ignore */ } },
  clear: () => { try { localStorage.removeItem(KEY); } catch { /* ignore */ } },
};

const WARNING = `<p class="warn">Use only while parked. Tesla blocks video while the car is in gear, and ParkScreen does not work around that.</p>`;

function show(html: string) { ui.hidden = false; ui.innerHTML = html; }

function pairingScreen(error = "") {
  show(`<h1>ParkScreen</h1><p>Enter the 6-digit code shown in the ParkScreen tray menu on your PC.</p>
    <input id="code" inputmode="numeric" maxlength="6" autofocus><button id="go">Pair</button>
    <p class="warn">${error}</p>${WARNING}`);
  const go = async () => {
    const code = (document.getElementById("code") as HTMLInputElement).value.trim();
    const r = await fetch("/api/pair/claim", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ code }) });
    if (!r.ok) return pairingScreen("Invalid or expired code.");
    store.set((await r.json()).token);
    connect();
  };
  document.getElementById("go")!.onclick = go;
}

let sig: Signalling | undefined;
let session: Session | undefined;
let stopReporting: (() => void) | undefined;

function connect() {
  const token = store.get();
  if (!token) return pairingScreen();
  show("<p>Connecting…</p>");
  sig?.close();
  sig = new Signalling(token, onMsg, (up) => { if (!up) show("<p>Reconnecting…</p>"); });
}

function startSession(hostId: string) {
  session?.close();
  stopReporting?.();
  session = new Session(sig!, hostId, video, (s) => {
    if (s === "connected") {
      ui.hidden = true;
      video.hidden = false;
      video.play().catch(() => {});
    } else if (s === "failed" || s === "disconnected") {
      video.hidden = true;
      show("<p>Connection lost. Waiting for the PC…</p>");
    }
  });
  session.start();
  stopReporting = session.startReporting();
}

function onMsg(m: ServerToCar) {
  switch (m.type) {
    case "error":
      if (m.message === "not paired") { store.clear(); pairingScreen("This car is no longer paired."); }
      break;
    case "hosts": {
      const online = m.hosts.filter((h) => h.online);
      if (online.length === 0) show(`<p>Your PC is offline. Start ParkScreen on it.</p>`);
      else if (online.length === 1) startSession(online[0].host_id);
      else show(`<p>Choose a PC</p>` + online.map((h, i) => `<button data-h="${h.host_id}">PC ${i + 1}</button>`).join(""));
      ui.querySelectorAll<HTMLButtonElement>("button[data-h]").forEach((b) => (b.onclick = () => startSession(b.dataset.h!)));
      break;
    }
    case "host_online": startSession(m.host_id); break;
    case "host_offline": stopReporting?.(); session?.close(); video.hidden = true; show("<p>Your PC went offline.</p>"); break;
    case "signal": session?.onSignal(m.payload); break;
  }
}

// Double-tap-with-two-fingers stats overlay is a Phase 2 item; for now expose stats in the console.
window.addEventListener("resize", () => session?.sendViewport());
video.addEventListener("click", () => { if (!document.fullscreenElement) video.requestFullscreen?.().catch(() => {}); });

connect();
