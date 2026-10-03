//! WebRTC sender plus the local HTTP signalling endpoint and player page.
//!
//! One viewer at a time: a new offer replaces the current connection.
//! Signalling is non-trickle: the browser POSTs one offer to `/api/offer` and gets the
//! answer back after ICE gathering has completed on our side.

use crate::pipeline::{Command, Encoded, Pipeline};
use anyhow::{anyhow, bail, Context, Result};
use axum::{
    extract::State,
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use rtc::interceptor::{Attribute, Interceptor, Packet, Slot, StreamInfo, TaggedPacket};
use rtc::media::Sample;
use rtc::media_stream::MediaStreamTrack;
use rtc::peer_connection::configuration::interceptor_registry::{
    register_default_interceptors, Registry,
};
use rtc::peer_connection::configuration::media_engine::{MediaEngine, MIME_TYPE_H264};
use rtc::peer_connection::configuration::RTCConfigurationBuilder;
use rtc::peer_connection::sdp::RTCSessionDescription;
use rtc::rtcp::payload_feedbacks::full_intra_request::FullIntraRequest;
use rtc::rtcp::payload_feedbacks::picture_loss_indication::PictureLossIndication;
use rtc::rtp_transceiver::rtp_sender::{
    RTCRtpCodec, RTCRtpCodecParameters, RTCRtpCodingParameters, RTCRtpEncodingParameters,
    RtpCodecKind,
};
use rtc::sansio::Protocol;
use rtc::shared::error::Error as RtcError;
use serde::Deserialize;
use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, Notify};
use webrtc::media_stream::track_local::static_sample::TrackLocalStaticSample;
use webrtc::media_stream::track_local::{TrackLocal, TrackLocalEvent};
use webrtc::media_stream::Track;
use webrtc::peer_connection::{
    PeerConnection, PeerConnectionBuilder, PeerConnectionEventHandler, RTCIceGatheringState,
    RTCPeerConnectionState,
};
use webrtc::runtime::{default_runtime, Sender};

const PLAYER_HTML: &str = include_str!("../assets/player.html");

/// H.264 Constrained Baseline, level 3.1, packetization-mode 1.
const H264_FMTP: &str =
    "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f";
const H264_PT: u8 = 102;

// ───────────────────────────── keyframe request forwarding ─────────────────────────────

/// Passes inbound PLI/FIR packets to the application (they are dropped otherwise) so we can
/// answer them with a keyframe. Adapted from webrtc-rs's `rtcp-processing` example.
#[derive(Default)]
struct KeyframeRequestInterceptor {
    read_queue: VecDeque<TaggedPacket>,
    write_queue: VecDeque<TaggedPacket>,
}

fn is_keyframe_request(packet: &dyn rtc::rtcp::Packet) -> bool {
    let any = packet.as_any();
    any.is::<PictureLossIndication>() || any.is::<FullIntraRequest>()
}

impl Protocol<TaggedPacket, TaggedPacket, ()> for KeyframeRequestInterceptor {
    type Rout = TaggedPacket;
    type Wout = TaggedPacket;
    type Eout = ();
    type Error = RtcError;
    type Time = Instant;

    fn handle_read(&mut self, mut msg: TaggedPacket) -> Result<(), Self::Error> {
        if let Packet::Rtcp(packets) = &msg.message.packet {
            let requests: Vec<Box<dyn rtc::rtcp::Packet>> = packets
                .iter()
                .filter(|p| is_keyframe_request(p.as_ref()))
                .cloned()
                .collect();
            if requests.is_empty() {
                return Ok(());
            }
            msg.message.packet = Packet::Rtcp(requests);
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

impl Interceptor for KeyframeRequestInterceptor {
    fn bind_local_stream(&mut self, _info: &StreamInfo) {}
    fn unbind_local_stream(&mut self, _info: &StreamInfo) {}
    fn bind_remote_stream(&mut self, _info: &StreamInfo) {}
    fn unbind_remote_stream(&mut self, _info: &StreamInfo) {}
}

// ───────────────────────────────── peer connection ─────────────────────────────────

#[derive(Clone)]
struct Handler {
    gather_done: Sender<()>,
    state: Sender<RTCPeerConnectionState>,
}

#[async_trait::async_trait]
impl PeerConnectionEventHandler for Handler {
    async fn on_ice_gathering_state_change(&self, s: RTCIceGatheringState) {
        if s == RTCIceGatheringState::Complete {
            let _ = self.gather_done.try_send(());
        }
    }
    async fn on_connection_state_change(&self, s: RTCPeerConnectionState) {
        tracing::info!("peer connection state: {s}");
        let _ = self.state.try_send(s);
    }
}

/// What the browser sends along with its offer.
#[derive(Debug, Deserialize)]
pub struct OfferRequest {
    #[serde(flatten)]
    pub offer: RTCSessionDescription,
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
}

pub struct ServerConfig {
    pub bind: SocketAddr,
    pub udp_port: u16,
    pub match_viewport: bool,
}

struct AppState {
    pipeline: Arc<Pipeline>,
    cfg: ServerConfig,
    /// Closes the current viewer when notified.
    current: Mutex<Option<Arc<Notify>>>,
}

pub async fn serve(pipeline: Arc<Pipeline>, cfg: ServerConfig) -> Result<()> {
    let bind = cfg.bind;
    let state = Arc::new(AppState { pipeline, cfg, current: Mutex::new(None) });
    let app = Router::new()
        .route("/", get(player))
        .route("/api/offer", post(offer))
        .route("/healthz", get(|| async { "ok" }))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| format!("binding {bind}"))?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn player() -> Response {
    ([(header::CONTENT_TYPE, "text/html; charset=utf-8"), (header::CACHE_CONTROL, "no-store")], PLAYER_HTML)
        .into_response()
}

async fn offer(State(st): State<Arc<AppState>>, Json(req): Json<OfferRequest>) -> Response {
    match handle_offer(st, req).await {
        Ok(answer) => Json(answer).into_response(),
        Err(e) => {
            tracing::error!("offer failed: {e:#}");
            (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")).into_response()
        }
    }
}

fn h264_codec() -> RTCRtpCodecParameters {
    RTCRtpCodecParameters {
        rtp_codec: RTCRtpCodec {
            mime_type: MIME_TYPE_H264.to_owned(),
            clock_rate: 90000,
            channels: 0,
            sdp_fmtp_line: H264_FMTP.to_owned(),
            rtcp_feedback: vec![],
        },
        payload_type: H264_PT,
    }
}

async fn handle_offer(st: Arc<AppState>, req: OfferRequest) -> Result<RTCSessionDescription> {
    tracing::info!("offer from viewer, viewport {:?}x{:?}", req.width, req.height);

    // Replace any current viewer, and give it a moment to release the UDP port.
    let close = Arc::new(Notify::new());
    let previous = st.current.lock().unwrap().replace(close.clone());
    if let Some(old) = previous {
        old.notify_one();
        tokio::time::sleep(Duration::from_millis(300)).await;
    }

    if st.cfg.match_viewport {
        if let (Some(w), Some(h)) = (req.width, req.height) {
            st.pipeline.match_viewport(w, h).await;
        }
    }

    let mut media_engine = MediaEngine::default();
    media_engine.register_codec(h264_codec(), RtpCodecKind::Video)?;
    let registry = register_default_interceptors(Registry::new(), &mut media_engine)?;
    let registry = registry.with(Slot::from(14_000), KeyframeRequestInterceptor::default());

    let (gather_tx, mut gather_rx) = webrtc::runtime::channel::<()>(1);
    let (state_tx, mut state_rx) = webrtc::runtime::channel::<RTCPeerConnectionState>(8);
    let handler = Arc::new(Handler { gather_done: gather_tx, state: state_tx });

    let pc = PeerConnectionBuilder::new()
        .with_configuration(RTCConfigurationBuilder::new().build())
        .with_media_engine(media_engine)
        .with_interceptor_registry(registry)
        .with_handler(handler)
        .with_runtime(default_runtime().ok_or_else(|| anyhow!("no async runtime"))?)
        .with_udp_addrs(vec![format!("0.0.0.0:{}", st.cfg.udp_port)])
        .build()
        .await
        .context("creating peer connection")?;
    let pc = Arc::new(pc);

    let ssrc: u32 = rand::random();
    let track = Arc::new(TrackLocalStaticSample::new(
        Instant::now(),
        MediaStreamTrack::new(
            "parkscreen".to_owned(),
            "screen".to_owned(),
            "ParkScreen".to_owned(),
            RtpCodecKind::Video,
            vec![RTCRtpEncodingParameters {
                rtp_coding_parameters: RTCRtpCodingParameters { ssrc: Some(ssrc), ..Default::default() },
                codec: h264_codec().rtp_codec,
                ..Default::default()
            }],
        ),
    )?);
    let sender = pc.add_track(track.clone() as Arc<dyn TrackLocal>).await?;

    pc.set_remote_description(req.offer).await.context("applying offer")?;
    let answer = pc.create_answer(None).await?;
    pc.set_local_description(answer).await?;
    tokio::time::timeout(Duration::from_secs(5), gather_rx.recv())
        .await
        .ok(); // proceed with whatever candidates were gathered
    let local = pc.local_description().await.ok_or_else(|| anyhow!("no local description"))?;

    // Everything after the answer runs in the background for the life of the connection.
    let pipeline = st.pipeline.clone();
    tokio::spawn(async move {
        let result = async {
            // Wait for the connection to come up.
            let connect = async {
                while let Some(s) = state_rx.recv().await {
                    match s {
                        RTCPeerConnectionState::Connected => return Ok(()),
                        RTCPeerConnectionState::Failed | RTCPeerConnectionState::Closed => {
                            bail!("connection {s}")
                        }
                        _ => {}
                    }
                }
                bail!("connection state channel closed")
            };
            tokio::time::timeout(Duration::from_secs(20), connect)
                .await
                .map_err(|_| anyhow!("timed out waiting for the connection (is UDP blocked? check the firewall)"))??;

            let pt = sender
                .get_parameters()
                .await?
                .rtp_parameters
                .codecs
                .first()
                .map(|c| c.payload_type)
                .ok_or_else(|| anyhow!("no negotiated codec"))?;
            let ssrc = *track.ssrcs().await.first().ok_or_else(|| anyhow!("track has no SSRC"))?;

            let (tx, rx) = mpsc::channel::<Encoded>(2);
            pipeline.attach_viewer(tx);

            // PLI/FIR → keyframe.
            let p2 = pipeline.clone();
            let t2 = track.clone();
            let rtcp_task = tokio::spawn(async move {
                while let Some(evt) = t2.poll().await {
                    if let TrackLocalEvent::OnRtcpPacket(_) = evt {
                        tracing::debug!("keyframe requested by viewer");
                        p2.send(Command::ForceKeyframe);
                    }
                }
            });

            let write = write_frames(track.clone(), ssrc, pt, rx);
            let state_watch = async {
                while let Some(s) = state_rx.recv().await {
                    if matches!(
                        s,
                        RTCPeerConnectionState::Disconnected
                            | RTCPeerConnectionState::Failed
                            | RTCPeerConnectionState::Closed
                    ) {
                        return;
                    }
                }
            };
            tokio::select! {
                r = write => r?,
                _ = state_watch => tracing::info!("viewer disconnected"),
                _ = close.notified() => tracing::info!("viewer replaced by a new connection"),
            }
            rtcp_task.abort();
            Ok::<_, anyhow::Error>(())
        }
        .await;
        if let Err(e) = result {
            tracing::warn!("viewer session ended: {e:#}");
        }
        pipeline.detach_viewer();
        let _ = pc.close().await;
    });

    Ok(local)
}

async fn write_frames(
    track: Arc<TrackLocalStaticSample>,
    ssrc: u32,
    pt: u8,
    mut rx: mpsc::Receiver<Encoded>,
) -> Result<()> {
    let mut last = Instant::now();
    while let Some(frame) = rx.recv().await {
        let now = Instant::now();
        let duration = now.duration_since(last).clamp(Duration::from_millis(1), Duration::from_secs(1));
        last = now;
        track
            .sample_writer(ssrc, pt)
            .write_sample(&Sample { data: frame.data, duration, ..Sample::new(now) })
            .await?;
    }
    Ok(())
}
