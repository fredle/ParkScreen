import type { Signal } from "./protocol";
import type { Signalling } from "./signalling";

export type Stats = { fps: number; dropped: number; decodeMs: number; jitterMs: number };

/** Receive-only WebRTC session to a host. The car creates the offer. */
export class Session {
  pc: RTCPeerConnection;
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
  private last = { frames: 0, t: performance.now(), decode: 0, decoded: 0 };
  async stats(): Promise<Stats> {
    let fps = 0, dropped = 0, decodeMs = 0, jitterMs = 0;
    const now = performance.now();
    (await this.pc.getStats()).forEach((r) => {
      if (r.type === "inbound-rtp" && r.kind === "video") {
        const dt = (now - this.last.t) / 1000;
        fps = dt > 0 ? (r.framesDecoded - this.last.frames) / dt : 0;
        const dd = r.framesDecoded - this.last.decoded;
        decodeMs = dd > 0 ? ((r.totalDecodeTime - this.last.decode) / dd) * 1000 : 0;
        this.last = { frames: r.framesDecoded, t: now, decode: r.totalDecodeTime, decoded: r.framesDecoded };
        dropped = r.framesDropped ?? 0;
        jitterMs = (r.jitterBufferDelay / Math.max(1, r.jitterBufferEmittedCount)) * 1000;
      }
    });
    return { fps, dropped, decodeMs, jitterMs };
  }
  close() {
    this.pc.close();
  }
}
