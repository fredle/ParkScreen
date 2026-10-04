// Mirrors protocol/src/lib.rs (hand-written for now; ts-rs generation is planned).
export type HostStatus = { host_id: string; online: boolean };
export type IceServer = { urls: string[]; username?: string; credential?: string };
export type ServerToCar =
  | { type: "ice_servers"; ice_servers: IceServer[] }
  | { type: "hosts"; hosts: HostStatus[] }
  | { type: "host_online"; host_id: string }
  | { type: "host_offline"; host_id: string }
  | { type: "signal"; host_id: string; payload: Signal }
  | { type: "pong" }
  | { type: "error"; message: string };
export type CarToServer =
  | { type: "hello"; token: string }
  | { type: "signal"; host_id: string; payload: Signal }
  | { type: "ping" };
/** Opaque to the server; understood by host and client. */
export type Signal =
  | { kind: "offer"; sdp: string }
  | { kind: "answer"; sdp: string }
  | { kind: "ice"; candidate: RTCIceCandidateInit | null }
  | { kind: "viewport"; w: number; h: number; dpr: number; fps: number }
  | ({ kind: "stats" } & Stats)
  | { kind: "keyframe" };

/** One interval of receiver stats; mirrors host/src/adaptive.rs `ClientStats`. */
export type Stats = { fps: number; dropped: number; decode_ms: number; jitter_ms: number; loss_pct: number };
