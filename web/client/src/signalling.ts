import type { CarToServer, ServerToCar } from "./protocol";

/** WSS client with 30 s pings (Cloudflare drops idle sockets at ~100 s) and backoff reconnect. */
export class Signalling {
  private ws?: WebSocket;
  private ping?: number;
  private retry = 1000;
  private closed = false;
  constructor(private token: string, private onMsg: (m: ServerToCar) => void, private onState: (up: boolean) => void) {
    this.connect();
  }
  private connect() {
    const proto = location.protocol === "https:" ? "wss" : "ws";
    const ws = new WebSocket(`${proto}://${location.host}/ws/car`);
    this.ws = ws;
    ws.onopen = () => {
      this.retry = 1000;
      this.send({ type: "hello", token: this.token });
      this.ping = window.setInterval(() => this.send({ type: "ping" }), 30_000);
      this.onState(true);
    };
    ws.onmessage = (e) => this.onMsg(JSON.parse(e.data));
    ws.onclose = () => {
      clearInterval(this.ping);
      this.onState(false);
      if (!this.closed) setTimeout(() => this.connect(), (this.retry = Math.min(this.retry * 2, 15_000)));
    };
  }
  send(m: CarToServer) {
    if (this.ws?.readyState === WebSocket.OPEN) this.ws.send(JSON.stringify(m));
  }
  close() {
    this.closed = true;
    this.ws?.close();
  }
}
