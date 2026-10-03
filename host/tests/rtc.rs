//! The sender against a webrtc-rs receiver: SDP exchange, ICE over loopback/LAN,
//! H.264 packets arriving, and the reassembled stream decoding at the requested size.
use parkscreen_host::{
    agent::SessionHandler,
    display::NullDisplay,
    rtc_sender::{local_ip, SoftwareMedia, WebRtcHandler},
};
use rtc::{
    interceptor::Registry,
    media::io::{h26x_writer::H26xWriter, Writer},
    peer_connection::{
        configuration::{
            interceptor_registry::register_default_interceptors,
            media_engine::{MediaEngine, MIME_TYPE_H264},
            RTCConfigurationBuilder,
        },
        sdp::RTCSessionDescription,
    },
    rtp_transceiver::{
        rtp_sender::{RTCRtpCodec, RTCRtpCodecParameters, RtpCodecKind},
        RTCRtpTransceiverDirection, RTCRtpTransceiverInit,
    },
};
use std::{
    io::Write,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::mpsc;
use webrtc::{
    media_stream::track_remote::{TrackRemote, TrackRemoteEvent},
    peer_connection::{PeerConnection, PeerConnectionBuilder, PeerConnectionEventHandler, RTCIceGatheringState},
    runtime::default_runtime,
};

#[derive(Clone, Default)]
struct Sink(Arc<Mutex<Vec<u8>>>);
impl Write for Sink {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct Car {
    gathered: mpsc::Sender<()>,
    sink: Sink,
    track: Arc<Mutex<Option<Arc<dyn TrackRemote>>>>,
}

#[async_trait::async_trait]
impl PeerConnectionEventHandler for Car {
    async fn on_ice_gathering_state_change(&self, s: RTCIceGatheringState) {
        if s == RTCIceGatheringState::Complete {
            let _ = self.gathered.try_send(());
        }
    }
    async fn on_track(&self, track: Arc<dyn TrackRemote>) {
        *self.track.lock().unwrap() = Some(track.clone());
        let sink = self.sink.clone();
        tokio::spawn(async move {
            let mut w = H26xWriter::new(sink, false);
            while let Some(evt) = track.poll().await {
                if let TrackRemoteEvent::OnRtpPacket(p) = evt {
                    let _ = w.write_rtp(&p);
                }
            }
        });
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn streams_decodable_h264() {
    // ---- car side (receiver) ----
    let mut me = MediaEngine::default();
    me.register_codec(
        RTCRtpCodecParameters {
            rtp_codec: RTCRtpCodec {
                mime_type: MIME_TYPE_H264.to_owned(),
                clock_rate: 90000,
                channels: 0,
                sdp_fmtp_line: "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f".into(),
                rtcp_feedback: vec![],
            },
            payload_type: 102,
        },
        RtpCodecKind::Video,
    )
    .unwrap();
    let registry = register_default_interceptors(Registry::new(), &mut me).unwrap();
    let (gtx, mut grx) = mpsc::channel(1);
    let sink = Sink::default();
    let remote: Arc<Mutex<Option<Arc<dyn TrackRemote>>>> = Default::default();
    let pc = PeerConnectionBuilder::new()
        .with_configuration(RTCConfigurationBuilder::new().build())
        .with_media_engine(me)
        .with_interceptor_registry(registry)
        .with_handler(Arc::new(Car { gathered: gtx, sink: sink.clone(), track: remote.clone() }))
        .with_runtime(default_runtime().unwrap())
        .with_udp_addrs(vec![format!("{}:0", local_ip())])
        .build()
        .await
        .unwrap();
    pc.add_transceiver_from_kind(
        RtpCodecKind::Video,
        Some(RTCRtpTransceiverInit { direction: RTCRtpTransceiverDirection::Recvonly, ..Default::default() }),
    )
    .await
    .unwrap();
    let offer = pc.create_offer(None).await.unwrap();
    pc.set_local_description(offer).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), grx.recv()).await.unwrap();
    let offer_sdp = pc.local_description().await.unwrap().sdp;

    // ---- host side (sender under test) ----
    let mut host = WebRtcHandler::new(NullDisplay::default(), Arc::new(SoftwareMedia { idr_secs: 30 }));
    host.on_signal("car", serde_json::json!({"kind":"viewport","w":640,"h":360,"fps":30})).await;
    let out = host.on_signal("car", serde_json::json!({"kind":"offer","sdp":offer_sdp})).await;
    let answer = out[0]["sdp"].as_str().expect("answer").to_string();
    assert!(answer.contains("H264"), "{answer}");
    pc.set_remote_description(RTCSessionDescription::answer(answer).unwrap()).await.unwrap();

    // ---- wait for video, then decode it ----
    let mut decoded = None;
    for _ in 0..100 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let data = sink.0.lock().unwrap().clone();
        if data.len() < 2000 {
            continue;
        }
        let mut dec = openh264::decoder::Decoder::new().unwrap();
        let mut dims = None;
        for nal in split_annexb(&data) {
            if let Ok(Some(y)) = dec.decode(nal) {
                use openh264::formats::YUVSource;
                dims = Some(y.dimensions());
                break;
            }
        }
        if dims.is_some() {
            decoded = dims;
            break;
        }
    }
    assert_eq!(decoded, Some((640, 360)), "no decodable frame received");

    // ---- PLI from the car must produce a fresh IDR well before the 2 s periodic one ----
    let track = remote.lock().unwrap().clone().expect("remote track");
    let ssrc = *track.ssrcs().await.first().unwrap();
    let idrs = |d: &[u8]| d.windows(4).filter(|w| w[..3] == [0, 0, 1] && w[3] & 0x1f == 5).count();
    // Let the initial IDR settle, then sample right after a PLI.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let before = idrs(&sink.0.lock().unwrap());
    let sent_at = std::time::Instant::now();
    track
        .write_rtcp(vec![Box::new(rtc::rtcp::payload_feedbacks::picture_loss_indication::PictureLossIndication { sender_ssrc: 0, media_ssrc: ssrc })])
        .await
        .unwrap();
    let mut got = false;
    while sent_at.elapsed() < Duration::from_millis(1500) {
        tokio::time::sleep(Duration::from_millis(50)).await;
        if idrs(&sink.0.lock().unwrap()) > before {
            got = true;
            break;
        }
    }
    assert!(got, "no IDR within 1.5 s of PLI");

    // ---- adaptive: lossy stats from the car cut the bitrate, stream keeps flowing ----
    let start = host.bitrate_of("car").unwrap();
    host.on_signal("car", serde_json::json!({"kind":"stats","fps":60.0,"dropped":0,"decode_ms":3.0,"jitter_ms":20.0,"loss_pct":8.0})).await;
    let after = host.bitrate_of("car").unwrap();
    assert!(after < start, "bitrate not reduced: {start} -> {after}");
    let len = sink.0.lock().unwrap().len();
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert!(sink.0.lock().unwrap().len() > len, "stream stalled after adaptive change");

    // ---- live bitrate change: stream keeps flowing and decodable ----
    host.on_signal("car", serde_json::json!({"kind":"bitrate","kbps":1500})).await;
    let len = sink.0.lock().unwrap().len();
    tokio::time::sleep(Duration::from_millis(1000)).await;
    assert!(sink.0.lock().unwrap().len() > len, "stream stalled after bitrate change");

    host.on_car_offline("car").await;
    assert!(host.display.current.is_none());
}

fn split_annexb(d: &[u8]) -> Vec<&[u8]> {
    // Return NAL units with their start codes, grouped so each slice is one NAL.
    let mut starts = vec![];
    let mut i = 0;
    while i + 3 < d.len() {
        if d[i..i + 3] == [0, 0, 1] {
            starts.push(if i > 0 && d[i - 1] == 0 { i - 1 } else { i });
            i += 3;
        } else {
            i += 1;
        }
    }
    starts.iter().enumerate().map(|(k, &s)| &d[s..*starts.get(k + 1).unwrap_or(&d.len())]).collect()
}
