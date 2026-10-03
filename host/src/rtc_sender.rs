//! WebRTC sender: answers the car's offer and streams H.264 from a capture + encoder pair.
//!
//! Signalling is "non-trickle" from the host side: the answer is sent once local ICE
//! gathering finishes, so it already contains the host candidates. The car's trickled
//! candidates are still accepted.

use crate::{
    adaptive::{Adaptive, ClientStats, Limits},
    agent::SessionHandler,
    capture::Capture,
    display::{DisplayBackend, Mode},
    input::{InputInjector, InputSession, NullInput, SharedInjector},
    encode::{Encoder, EncoderConfig},
};
use async_trait::async_trait;
use rtc::{
    interceptor::{Attribute, Interceptor, Packet, Registry, Slot, StreamInfo, TaggedPacket},
    media::Sample,
    media_stream::MediaStreamTrack,
    peer_connection::{
        configuration::{
            interceptor_registry::register_default_interceptors,
            media_engine::{MediaEngine, MIME_TYPE_H264},
            RTCConfigurationBuilder,
        },
        sdp::RTCSessionDescription,
        transport::{RTCIceCandidateInit, RTCIceServer},
    },
    rtcp::payload_feedbacks::{full_intra_request::FullIntraRequest, picture_loss_indication::PictureLossIndication},
    sansio::Protocol,
    shared::error::Error as RtcError,
    rtp_transceiver::rtp_sender::{RTCRtpCodec, RTCRtpCodecParameters, RTCRtpCodingParameters, RTCRtpEncodingParameters, RtpCodecKind},
};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, VecDeque},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use tokio::sync::mpsc;
use webrtc::media_stream::track_local::TrackLocalEvent;
use tracing::{info, warn};
use webrtc::{
    data_channel::{DataChannel, DataChannelEvent},
    media_stream::{
        track_local::{static_sample::TrackLocalStaticSample, TrackLocal},
        Track,
    },
    peer_connection::{PeerConnection, PeerConnectionBuilder, PeerConnectionEventHandler, RTCIceGatheringState, RTCPeerConnectionState},
    runtime::default_runtime,
};

const H264_FMTP: &str = "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f";
const H264_PT: u8 = 102;

/// Creates the capture and encoder for a given mode. Windows builds supply WGC + Media
/// Foundation here; elsewhere `TestPattern` + `OpenH264Encoder` are used.
pub trait MediaFactory: Send + Sync + 'static {
    fn make(&self, mode: Mode, bitrate_kbps: u32) -> Result<MediaPair, String>;
}

/// A capture and the encoder that consumes its frames.
pub type MediaPair = (Box<dyn Capture>, Box<dyn Encoder>);

fn release(inj: &SharedInjector, session: &Mutex<InputSession>) {
    session.lock().unwrap().release_all(inj.lock().unwrap().as_mut());
}

/// Decides, per message, whether a car may inject input (so revoking takes effect at once).
pub type InputGate = Arc<dyn Fn(&str) -> bool + Send + Sync>;

#[derive(Clone)]
struct Handler {
    gathered: mpsc::Sender<()>,
    state: mpsc::UnboundedSender<RTCPeerConnectionState>,
    car_id: String,
    injector: SharedInjector,
    gate: InputGate,
    input: Arc<Mutex<InputSession>>,
}

#[async_trait]
impl PeerConnectionEventHandler for Handler {
    async fn on_ice_gathering_state_change(&self, s: RTCIceGatheringState) {
        if s == RTCIceGatheringState::Complete {
            let _ = self.gathered.try_send(());
        }
    }
    async fn on_connection_state_change(&self, s: RTCPeerConnectionState) {
        let _ = self.state.send(s);
    }
    async fn on_data_channel(&self, dc: Arc<dyn DataChannel>) {
        if dc.label().await.ok().as_deref() != Some("input") {
            return;
        }
        let (car, inj, gate, session) = (self.car_id.clone(), self.injector.clone(), self.gate.clone(), self.input.clone());
        tokio::spawn(async move {
            let mut warned = false;
            while let Some(ev) = dc.poll().await {
                match ev {
                    DataChannelEvent::OnMessage(m) => {
                        let mut inj = inj.lock().unwrap();
                        let mut session = session.lock().unwrap();
                        if gate(&car) {
                            session.handle(Instant::now(), &m.data, inj.as_mut());
                        } else {
                            // Revoked or never enabled: make sure nothing is left pressed.
                            session.release_all(inj.as_mut());
                            if !warned {
                                warn!("input from a car without input permission is being ignored");
                                warned = true;
                            }
                        }
                    }
                    DataChannelEvent::OnClose => break,
                    _ => {}
                }
            }
            release(&inj, &session);
        });
    }
}

/// First non-loopback IPv4 address, found by asking the OS which interface routes outward.
pub fn local_ip() -> String {
    std::net::UdpSocket::bind("0.0.0.0:0")
        .and_then(|s| s.connect("8.8.8.8:80").map(|_| s))
        .and_then(|s| s.local_addr())
        .map(|a| a.ip().to_string())
        .unwrap_or_else(|_| "127.0.0.1".into())
}

/// Commands for the streaming loop.
#[derive(Debug)]
enum Ctl {
    Keyframe,
    Bitrate(u32),
}

/// Hands inbound keyframe requests (PLI/FIR) to the application; drops other inbound RTCP
/// (the default interceptors have already acted on it). Must sit last in the chain.
#[derive(Default)]
struct KeyframeForwarder {
    read_queue: VecDeque<TaggedPacket>,
    write_queue: VecDeque<TaggedPacket>,
}

impl Protocol<TaggedPacket, TaggedPacket, ()> for KeyframeForwarder {
    type Rout = TaggedPacket;
    type Wout = TaggedPacket;
    type Eout = ();
    type Error = RtcError;
    type Time = Instant;

    fn handle_read(&mut self, mut msg: TaggedPacket) -> Result<(), Self::Error> {
        if let Packet::Rtcp(packets) = &msg.message.packet {
            let reqs: Vec<_> = packets
                .iter()
                .filter(|p| p.as_any().is::<PictureLossIndication>() || p.as_any().is::<FullIntraRequest>())
                .cloned()
                .collect();
            if reqs.is_empty() {
                return Ok(());
            }
            msg.message.packet = Packet::Rtcp(reqs);
            msg.message.add(Attribute::DeliverToApplication);
        }
        self.read_queue.push_back(msg);
        Ok(())
    }
    fn poll_read(&mut self) -> Option<Self::Rout> {
        self.read_queue.pop_front()
    }
    fn handle_write(&mut self, msg: TaggedPacket) -> Result<(), Self::Error> {
        self.write_queue.push_back(msg);
        Ok(())
    }
    fn poll_write(&mut self) -> Option<Self::Wout> {
        self.write_queue.pop_front()
    }
}

impl Interceptor for KeyframeForwarder {
    fn bind_local_stream(&mut self, _: &StreamInfo) {}
    fn unbind_local_stream(&mut self, _: &StreamInfo) {}
    fn bind_remote_stream(&mut self, _: &StreamInfo) {}
    fn unbind_remote_stream(&mut self, _: &StreamInfo) {}
}

struct Live {
    input: Arc<Mutex<InputSession>>,
    adaptive: Adaptive,
    ctl: mpsc::UnboundedSender<Ctl>,
    pc: Arc<dyn PeerConnection>,
    stop: Arc<AtomicBool>,
}

impl Drop for Live {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Handles signalling for all cars. One live session per car; a new offer replaces it.
pub struct WebRtcHandler<D: DisplayBackend> {
    pub display: D,
    media: Arc<dyn MediaFactory>,
    viewport: HashMap<String, Mode>,
    live: HashMap<String, Live>,
    injector: SharedInjector,
    input_gate: InputGate,
    pub bitrate_kbps: u32,
    /// Bounds for adaptive bitrate; the starting bitrate is clamped into them.
    pub limits: Limits,
    /// Overrides the address to bind UDP on (default: `local_ip()`).
    pub bind_ip: Option<String>,
}

impl<D: DisplayBackend> WebRtcHandler<D> {
    pub fn new(display: D, media: Arc<dyn MediaFactory>) -> Self {
        Self { display, media, viewport: HashMap::new(), live: HashMap::new(), injector: Arc::new(Mutex::new(Box::new(NullInput))), input_gate: Arc::new(|_| false), bitrate_kbps: 12_000, limits: Limits::default(), bind_ip: None }
    }

    /// Enable input injection. `gate(car_id)` is consulted for every message; the default
    /// gate denies everything.
    pub fn with_input(mut self, injector: Box<dyn InputInjector>, gate: InputGate) -> Self {
        self.injector = Arc::new(Mutex::new(injector));
        self.input_gate = gate;
        self
    }

    /// Current adaptive target for a car's live session (kbps).
    pub fn bitrate_of(&self, car_id: &str) -> Option<u32> {
        self.live.get(car_id).map(|l| l.adaptive.kbps())
    }

    async fn answer_offer(&mut self, car_id: &str, sdp: String) -> Result<Value, String> {
        if let Some(old) = self.live.remove(car_id) {
            let _ = old.pc.close().await;
            release(&self.injector, &old.input);
        }
        let mode = self.viewport.get(car_id).copied().unwrap_or(Mode { width: 1280, height: 720, refresh_hz: 30 });
        let err = |e: &dyn std::fmt::Display| e.to_string();

        let codec = RTCRtpCodecParameters {
            rtp_codec: RTCRtpCodec {
                mime_type: MIME_TYPE_H264.to_owned(),
                clock_rate: 90000,
                channels: 0,
                sdp_fmtp_line: H264_FMTP.to_owned(),
                rtcp_feedback: vec![],
            },
            payload_type: H264_PT,
        };
        let mut me = MediaEngine::default();
        me.register_codec(codec.clone(), RtpCodecKind::Video).map_err(|e| err(&e))?;
        let registry = register_default_interceptors(Registry::new(), &mut me).map_err(|e| err(&e))?;
        let registry = registry.with(Slot::from(14_000), KeyframeForwarder::default());
        let config = RTCConfigurationBuilder::new()
            .with_ice_servers(vec![RTCIceServer { urls: vec!["stun:stun.cloudflare.com:3478".into()], ..Default::default() }])
            .build();

        let input_state = Arc::new(Mutex::new(InputSession::default()));
        let (gathered_tx, mut gathered_rx) = mpsc::channel(1);
        let (state_tx, mut state_rx) = mpsc::unbounded_channel();
        let ip = self.bind_ip.clone().unwrap_or_else(local_ip);
        let pc = PeerConnectionBuilder::new()
            .with_configuration(config)
            .with_media_engine(me)
            .with_interceptor_registry(registry)
            .with_handler(Arc::new(Handler { gathered: gathered_tx, state: state_tx, car_id: car_id.to_string(), injector: self.injector.clone(), gate: self.input_gate.clone(), input: input_state.clone() }))
            .with_runtime(default_runtime().ok_or("no runtime")?)
            .with_udp_addrs(vec![format!("{ip}:0")])
            .build()
            .await
            .map_err(|e| err(&e))?;
        let pc: Arc<dyn PeerConnection> = Arc::new(pc);

        let ssrc = rand::random::<u32>();
        let track = Arc::new(
            TrackLocalStaticSample::new(
                Instant::now(),
                MediaStreamTrack::new(
                    "parkscreen".into(),
                    "parkscreen-video".into(),
                    "ParkScreen".into(),
                    RtpCodecKind::Video,
                    vec![RTCRtpEncodingParameters {
                        rtp_coding_parameters: RTCRtpCodingParameters { ssrc: Some(ssrc), ..Default::default() },
                        codec: codec.rtp_codec.clone(),
                        ..Default::default()
                    }],
                ),
            )
            .map_err(|e| err(&e))?,
        );
        let sender = pc.add_track(track.clone() as Arc<dyn TrackLocal>).await.map_err(|e| err(&e))?;

        pc.set_remote_description(RTCSessionDescription::offer(sdp).map_err(|e| err(&e))?).await.map_err(|e| err(&e))?;
        let answer = pc.create_answer(None).await.map_err(|e| err(&e))?;
        pc.set_local_description(answer).await.map_err(|e| err(&e))?;
        let _ = tokio::time::timeout(Duration::from_secs(4), gathered_rx.recv()).await;
        let local = pc.local_description().await.ok_or("no local description")?;

        let pt = sender
            .get_parameters()
            .await
            .map_err(|e| err(&e))?
            .rtp_parameters
            .codecs
            .first()
            .map(|c| c.payload_type)
            .ok_or("no negotiated codec")?;

        // Stream once connected.
        let stop = Arc::new(AtomicBool::new(false));
        let media = self.media.clone();
        let bitrate = self.bitrate_kbps;
        let stop2 = stop.clone();
        let (ctl_tx, ctl_rx) = mpsc::unbounded_channel();
        let rtcp_ctl = ctl_tx.clone();
        let rtcp_track = track.clone();
        let rtcp_stop = stop.clone();
        tokio::spawn(async move {
            // Keyframe requests marked by KeyframeForwarder arrive as track-local events.
            while let Some(ev) = rtcp_track.poll().await {
                if rtcp_stop.load(Ordering::Relaxed) {
                    break;
                }
                let TrackLocalEvent::OnRtcpPacket(_) = ev else { continue };
                let _ = rtcp_ctl.send(Ctl::Keyframe);
            }
        });
        tokio::spawn(async move {
            while let Some(s) = state_rx.recv().await {
                info!("peer connection: {s}");
                if s == RTCPeerConnectionState::Connected {
                    break;
                }
                if matches!(s, RTCPeerConnectionState::Failed | RTCPeerConnectionState::Closed) {
                    return;
                }
            }
            let Some(&ssrc) = track.ssrcs().await.first() else { return };
            if let Err(e) = stream(track, ssrc, pt, mode, bitrate, media, stop2, ctl_rx).await {
                warn!("stream ended: {e}");
            }
        });

        self.live.insert(car_id.to_string(), Live { input: input_state, pc, stop, ctl: ctl_tx, adaptive: Adaptive::new(bitrate, mode.refresh_hz, self.limits) });
        Ok(json!({ "kind": "answer", "sdp": local.sdp }))
    }
}

/// Capture + encode run on their own thread (they are CPU-bound and must never starve the
/// async runtime that also drives ICE/DTLS/SCTP, i.e. input and RTCP). Encoded frames cross
/// to the async side over a tiny bounded channel; if the network can't keep up, frames are
/// dropped here rather than queued, which keeps latency low.
#[allow(clippy::too_many_arguments)]
async fn stream(
    track: Arc<TrackLocalStaticSample>,
    ssrc: u32,
    pt: u8,
    mode: Mode,
    bitrate_kbps: u32,
    media: Arc<dyn MediaFactory>,
    stop: Arc<AtomicBool>,
    mut ctl: mpsc::UnboundedReceiver<Ctl>,
) -> Result<(), String> {
    let (mut cap, mut enc) = media.make(mode, bitrate_kbps)?;
    let frame_dur = Duration::from_secs_f64(1.0 / mode.refresh_hz.max(1) as f64);
    let (tx, mut rx) = mpsc::channel::<Vec<u8>>(2);

    let thread_stop = stop.clone();
    std::thread::Builder::new()
        .name("parkscreen-encode".into())
        .spawn(move || {
            let mut next = Instant::now();
            while !thread_stop.load(Ordering::Relaxed) && !tx.is_closed() {
                while let Ok(c) = ctl.try_recv() {
                    match c {
                        Ctl::Keyframe => enc.request_keyframe(),
                        Ctl::Bitrate(k) => enc.set_bitrate(k),
                    }
                }
                if let Some(frame) = cap.next_frame() {
                    let data = enc.encode(&frame);
                    if !data.is_empty() && tx.try_send(data).is_err() && tx.is_closed() {
                        break;
                    }
                }
                // Fixed-rate pacing without drift; if we're behind, skip ahead instead of bursting.
                next += frame_dur;
                let now = Instant::now();
                if next > now {
                    std::thread::sleep(next - now);
                } else {
                    next = now;
                }
            }
        })
        .map_err(|e| e.to_string())?;

    let mut sent = 0u64;
    let mut last = Instant::now();
    while let Some(data) = rx.recv().await {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        track
            .sample_writer(ssrc, pt)
            .write_sample(&Sample { data: data.into(), duration: frame_dur, ..Sample::new(Instant::now()) })
            .await
            .map_err(|e| e.to_string())?;
        sent += 1;
        if last.elapsed() > Duration::from_secs(5) {
            info!(frames = sent, "streaming");
            last = Instant::now();
        }
    }
    stop.store(true, Ordering::Relaxed);
    Ok(())
}

#[async_trait]
impl<D: DisplayBackend> SessionHandler for WebRtcHandler<D> {
    async fn on_signal(&mut self, car_id: &str, payload: Value) -> Vec<Value> {
        match payload.get("kind").and_then(Value::as_str) {
            Some("offer") => {
                let Some(sdp) = payload.get("sdp").and_then(Value::as_str) else { return vec![] };
                match self.answer_offer(car_id, sdp.to_string()).await {
                    Ok(a) => return vec![a],
                    Err(e) => warn!("offer failed: {e}"),
                }
            }
            Some("ice") => {
                let cand = payload.get("candidate").filter(|c| !c.is_null());
                if let (Some(c), Some(live)) = (cand, self.live.get(car_id)) {
                    match serde_json::from_value::<RTCIceCandidateInit>(c.clone()) {
                        Ok(c) => {
                            if let Err(e) = live.pc.add_ice_candidate(c).await {
                                warn!("add_ice_candidate: {e}");
                            }
                        }
                        Err(e) => warn!("bad candidate: {e}"),
                    }
                }
            }
            Some("stats") => {
                if let (Ok(s), Some(live)) = (serde_json::from_value::<ClientStats>(payload.clone()), self.live.get_mut(car_id)) {
                    if let Some(k) = live.adaptive.update(Instant::now(), &s) {
                        info!(kbps = k, loss = s.loss_pct, decode_ms = s.decode_ms, fps = s.fps, "adaptive bitrate");
                        let _ = live.ctl.send(Ctl::Bitrate(k));
                    }
                }
            }
            Some("bitrate") => {
                if let (Some(k), Some(live)) = (payload.get("kbps").and_then(Value::as_u64), self.live.get_mut(car_id)) {
                    live.adaptive.set_kbps(k.min(u32::MAX as u64) as u32);
                    let k = live.adaptive.kbps();
                    let _ = live.ctl.send(Ctl::Bitrate(k));
                }
            }
            Some("keyframe") => {
                if let Some(live) = self.live.get(car_id) {
                    let _ = live.ctl.send(Ctl::Keyframe);
                }
            }
            Some("viewport") => {
                let g = |k| payload.get(k).and_then(Value::as_u64).unwrap_or(0) as u32;
                let (w, h, fps) = (g("w"), g("h"), g("fps"));
                if (320..=7680).contains(&w) && (240..=4320).contains(&h) {
                    let mode = Mode { width: w & !1, height: h & !1, refresh_hz: fps.clamp(30, 60) };
                    info!(?mode, "car viewport");
                    self.viewport.insert(car_id.to_string(), mode);
                    if let Err(e) = self.display.plug(mode).await {
                        warn!("plug failed: {e}");
                    }
                }
            }
            _ => {}
        }
        vec![]
    }

    async fn on_car_offline(&mut self, car_id: &str) {
        if let Some(l) = self.live.remove(car_id) {
            let _ = l.pc.close().await;
            release(&self.injector, &l.input);
        }
        self.viewport.remove(car_id);
        let _ = self.display.unplug().await;
    }
}

/// Default (cross-platform) media: test pattern + openh264.
pub struct SoftwareMedia {
    pub idr_secs: u32,
}
impl Default for SoftwareMedia {
    fn default() -> Self {
        Self { idr_secs: 2 }
    }
}
impl MediaFactory for SoftwareMedia {
    fn make(&self, mode: Mode, bitrate_kbps: u32) -> Result<MediaPair, String> {
        let cfg = EncoderConfig { width: mode.width, height: mode.height, fps: mode.refresh_hz, bitrate_kbps, idr_secs: self.idr_secs };
        Ok((Box::new(crate::capture::TestPattern::new(mode.width, mode.height)), Box::new(crate::encode::OpenH264Encoder::new(cfg)?)))
    }
}

