import type { Signal, Stats } from "./protocol";
import type { Signalling } from "./signalling";

/** Receive-only WebRTC session to a host. The car creates the offer. */
export class Session {
  pc: RTCPeerConnection;
  /** Unreliable, unordered: a late pointer move is worthless, so never retransmit. */
  private input?: RTCDataChannel;
  private pending: RTCIceCandidateInit[] = [];
  private remoteSet = false;
  constructor(private sig: Signalling, private hostId: string, private video: HTMLVideoElement, onState: (s: RTCPeerConnectionState) => void) {
    this.pc = new RTCPeerConnection({ iceServers: [{ urls: "stun:stun.cloudflare.com:3478" }] });
    this.pc.onconnectionstatechange = () => onState(this.pc.connectionState);
    this.pc.onicecandidate = (e) => this.send({ kind: "ice", candidate: e.candidate ? e.candidate.toJSON() : null });
    this.pc.ontrack = (e) => {
      video.srcObject = e.streams[0] ?? new MediaStream([e.track]);
      // Lowest-latency playout.
      (e.receiver as any).playoutDelayHint = 0;
      (e.receiver as any).jitterBufferTarget = 0;
    };
  }
  private send(payload: Signal) {
    this.sig.send({ type: "signal", host_id: this.hostId, payload });
  }
  async start() {
    this.input = this.pc.createDataChannel("input", { ordered: false, maxRetransmits: 0 });
    this.pc.addTransceiver("video", { direction: "recvonly" });
    // Prefer H.264 (hardware decode); the host only offers Constrained Baseline.
    const caps = RTCRtpReceiver.getCapabilities("video");
    const tr = this.pc.getTransceivers()[0];
    if (caps && tr.setCodecPreferences) {
      const h264 = caps.codecs.filter((c) => /h264/i.test(c.mimeType));
      const rest = caps.codecs.filter((c) => !/h264/i.test(c.mimeType));
      if (h264.length) tr.setCodecPreferences([...h264, ...rest]);
    }
    const offer = await this.pc.createOffer();
    await this.pc.setLocalDescription(offer);
    this.send({ kind: "offer", sdp: offer.sdp! });
    this.sendViewport();
  }
  /** Send an input message if the channel is open (dropped otherwise). */
  sendInput = (msg: object) => {
    if (this.input?.readyState === "open") this.input.send(JSON.stringify(msg));
  };

  sendViewport() {
    const dpr = window.devicePixelRatio || 1;
    this.send({ kind: "viewport", w: Math.round(screen.width * dpr), h: Math.round(screen.height * dpr), dpr, fps: 60 });
  }
  async onSignal(p: Signal) {
    if (p.kind === "answer") {
      await this.pc.setRemoteDescription({ type: "answer", sdp: p.sdp });
      this.remoteSet = true;
      for (const c of this.pending.splice(0)) await this.pc.addIceCandidate(c);
    } else if (p.kind === "ice" && p.candidate) {
      if (this.remoteSet) await this.pc.addIceCandidate(p.candidate);
      else this.pending.push(p.candidate);
    }
  }
  private last = { frames: 0, t: performance.now(), decode: 0, dropped: 0, lost: 0, recv: 0 };

  /** Stats for the interval since the last call (deltas, not totals). */
  async stats(): Promise<Stats> {
    const out: Stats = { fps: 0, dropped: 0, decode_ms: 0, jitter_ms: 0, loss_pct: 0 };
    const now = performance.now();
    (await this.pc.getStats()).forEach((r) => {
      if (r.type !== "inbound-rtp" || r.kind !== "video") return;
      const l = this.last;
      const dt = (now - l.t) / 1000;
      const frames = r.framesDecoded - l.frames;
      out.fps = dt > 0 ? frames / dt : 0;
      out.decode_ms = frames > 0 ? ((r.totalDecodeTime - l.decode) / frames) * 1000 : 0;
      out.dropped = (r.framesDropped ?? 0) - l.dropped;
      out.jitter_ms = r.jitterBufferEmittedCount > 0 ? (r.jitterBufferDelay / r.jitterBufferEmittedCount) * 1000 : 0;
      const lost = (r.packetsLost ?? 0) - l.lost;
      const recv = r.packetsReceived - l.recv;
      out.loss_pct = lost + recv > 0 ? (Math.max(0, lost) / (lost + recv)) * 100 : 0;
      this.last = { frames: r.framesDecoded, t: now, decode: r.totalDecodeTime, dropped: r.framesDropped ?? 0, lost: r.packetsLost ?? 0, recv: r.packetsReceived };
    });
    return out;
  }

  /** Report stats to the host once a second while connected. Returns a stop function. */
  startReporting(): () => void {
    const id = window.setInterval(async () => {
      if (this.pc.connectionState !== "connected") return;
      this.send({ kind: "stats", ...(await this.stats()) });
    }, 1000);
    return () => clearInterval(id);
  }

  close() {
    this.pc.close();
  }
}
