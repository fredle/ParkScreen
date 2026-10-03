//! WebRTC sender: answers the car's offer and streams H.264 from a capture + encoder pair.
//!
//! Signalling is "non-trickle" from the host side: the answer is sent once local ICE
//! gathering finishes, so it already contains the host candidates. The car's trickled
//! candidates are still accepted.

use crate::{
    agent::SessionHandler,
    capture::Capture,
    display::{DisplayBackend, Mode},
    encode::{Encoder, EncoderConfig},
};
use async_trait::async_trait;
use rtc::{
    interceptor::Registry,
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
    rtp_transceiver::rtp_sender::{RTCRtpCodec, RTCRtpCodecParameters, RTCRtpCodingParameters, RTCRtpEncodingParameters, RtpCodecKind},
};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use tokio::sync::mpsc;
use tracing::{info, warn};
use webrtc::{
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
    fn make(&self, mode: Mode, bitrate_kbps: u32) -> Result<(Box<dyn Capture>, Box<dyn Encoder>), String>;
}

#[derive(Clone)]
struct Handler {
    gathered: mpsc::Sender<()>,
    state: mpsc::UnboundedSender<RTCPeerConnectionState>,
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
}

/// First non-loopback IPv4 address, found by asking the OS which interface routes outward.
pub fn local_ip() -> String {
    std::net::UdpSocket::bind("0.0.0.0:0")
        .and_then(|s| s.connect("8.8.8.8:80").map(|_| s))
        .and_then(|s| s.local_addr())
        .map(|a| a.ip().to_string())
        .unwrap_or_else(|_| "127.0.0.1".into())
}

struct Live {
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
    pub bitrate_kbps: u32,
    /// Overrides the address to bind UDP on (default: `local_ip()`).
    pub bind_ip: Option<String>,
}

impl<D: DisplayBackend> WebRtcHandler<D> {
    pub fn new(display: D, media: Arc<dyn MediaFactory>) -> Self {
        Self { display, media, viewport: HashMap::new(), live: HashMap::new(), bitrate_kbps: 12_000, bind_ip: None }
    }

    async fn answer_offer(&mut self, car_id: &str, sdp: String) -> Result<Value, String> {
        self.live.remove(car_id);
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
            ..Default::default()
        };
        let mut me = MediaEngine::default();
        me.register_codec(codec.clone(), RtpCodecKind::Video).map_err(|e| err(&e))?;
        let registry = register_default_interceptors(Registry::new(), &mut me).map_err(|e| err(&e))?;
        let config = RTCConfigurationBuilder::new()
            .with_ice_servers(vec![RTCIceServer { urls: vec!["stun:stun.cloudflare.com:3478".into()], ..Default::default() }])
            .build();

        let (gathered_tx, mut gathered_rx) = mpsc::channel(1);
        let (state_tx, mut state_rx) = mpsc::unbounded_channel();
        let ip = self.bind_ip.clone().unwrap_or_else(local_ip);
        let pc = PeerConnectionBuilder::new()
            .with_configuration(config)
            .with_media_engine(me)
            .with_interceptor_registry(registry)
            .with_handler(Arc::new(Handler { gathered: gathered_tx, state: state_tx }))
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
            if let Err(e) = stream(track, ssrc, pt, mode, bitrate, media, stop2).await {
                warn!("stream ended: {e}");
            }
        });

        self.live.insert(car_id.to_string(), Live { pc, stop });
        Ok(json!({ "kind": "answer", "sdp": local.sdp }))
    }
}

async fn stream(
    track: Arc<TrackLocalStaticSample>,
    ssrc: u32,
    pt: u8,
    mode: Mode,
    bitrate_kbps: u32,
    media: Arc<dyn MediaFactory>,
    stop: Arc<AtomicBool>,
) -> Result<(), String> {
    let (mut cap, mut enc) = media.make(mode, bitrate_kbps)?;
    let frame_dur = Duration::from_secs_f64(1.0 / mode.refresh_hz.max(1) as f64);
    let mut tick = tokio::time::interval(frame_dur);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut sent = 0u64;
    let mut last = Instant::now();
    while !stop.load(Ordering::Relaxed) {
        tick.tick().await;
        let Some(frame) = cap.next_frame() else { continue };
        let data = enc.encode(&frame);
        if data.is_empty() {
            continue;
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
        }
        self.viewport.remove(car_id);
        let _ = self.display.unplug().await;
    }
}

/// Default (cross-platform) media: test pattern + openh264.
pub struct SoftwareMedia;
impl MediaFactory for SoftwareMedia {
    fn make(&self, mode: Mode, bitrate_kbps: u32) -> Result<(Box<dyn Capture>, Box<dyn Encoder>), String> {
        let cfg = EncoderConfig { width: mode.width, height: mode.height, fps: mode.refresh_hz, bitrate_kbps };
        Ok((Box::new(crate::capture::TestPattern::new(mode.width, mode.height)), Box::new(crate::encode::OpenH264Encoder::new(cfg)?)))
    }
}

// Silence unused warning for Mutex import on some cfgs.
#[allow(dead_code)]
type _Unused = Mutex<()>;
