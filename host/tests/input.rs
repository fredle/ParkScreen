//! Input over the `input` data channel, end to end through two WebRTC peers.
use parkscreen_host::{
    agent::SessionHandler,
    display::NullDisplay,
    input::{PointerEvent, RecordingInput},
    rtc_sender::{local_ip, SoftwareMedia, WebRtcHandler},
};
use rtc::{
    interceptor::Registry,
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
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::sync::mpsc;
use webrtc::{
    data_channel::DataChannelEvent,
    peer_connection::{PeerConnection, PeerConnectionBuilder, PeerConnectionEventHandler, RTCIceGatheringState},
    runtime::default_runtime,
};

struct Car(mpsc::Sender<()>);

#[async_trait::async_trait]
impl PeerConnectionEventHandler for Car {
    async fn on_ice_gathering_state_change(&self, s: RTCIceGatheringState) {
        if s == RTCIceGatheringState::Complete {
            let _ = self.0.try_send(());
        }
    }
    // The sans-io driver backs up if incoming media isn't consumed.
    async fn on_track(&self, track: Arc<dyn webrtc::media_stream::track_remote::TrackRemote>) {
        tokio::spawn(async move { while track.poll().await.is_some() {} });
    }
}

async fn eventually(mut f: impl FnMut() -> bool) -> bool {
    for _ in 0..60 {
        if f() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn input_is_gated_validated_and_released() {
    let rec = RecordingInput::default();
    let allowed = Arc::new(AtomicBool::new(false));
    let gate = allowed.clone();
    let mut host = WebRtcHandler::new(NullDisplay::default(), Arc::new(SoftwareMedia::default()))
        .with_input(Box::new(rec.clone()), Arc::new(move |_| gate.load(Ordering::Relaxed)));

    // Car: recvonly video + an unreliable, unordered "input" channel (as the web client does).
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
            ..Default::default()
        },
        RtpCodecKind::Video,
    )
    .unwrap();
    let registry = register_default_interceptors(Registry::new(), &mut me).unwrap();
    let (gtx, mut grx) = mpsc::channel(1);
    let pc = PeerConnectionBuilder::new()
        .with_configuration(RTCConfigurationBuilder::new().build())
        .with_media_engine(me)
        .with_interceptor_registry(registry)
        .with_handler(Arc::new(Car(gtx)))
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
    let dc = pc
        .create_data_channel(
            "input",
            Some(rtc::data_channel::RTCDataChannelInit { ordered: false, max_retransmits: Some(0), ..Default::default() }),
        )
        .await
        .unwrap();
    let offer = pc.create_offer(None).await.unwrap();
    pc.set_local_description(offer).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), grx.recv()).await.unwrap();
    let offer_sdp = pc.local_description().await.unwrap().sdp;

    let out = host.on_signal("car", serde_json::json!({"kind":"offer","sdp":offer_sdp})).await;
    pc.set_remote_description(RTCSessionDescription::answer(out[0]["sdp"].as_str().unwrap().into()).unwrap()).await.unwrap();

    // Wait for the channel to open.
    loop {
        match tokio::time::timeout(Duration::from_secs(10), dc.poll()).await.expect("channel never opened") {
            Some(DataChannelEvent::OnOpen) => break,
            Some(_) => {}
            None => panic!("closed"),
        }
    }
    // Keep draining the car-side channel's events (the sans-io driver expects its events consumed).
    let drain = dc.clone();
    tokio::spawn(async move { while drain.poll().await.is_some() {} });
    let events = || rec.0.lock().unwrap().clone();

    // 1. Not permitted: everything is dropped.
    dc.send_text(r#"{"t":"down","id":0,"x":0.5,"y":0.5}"#).await.unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(events().is_empty(), "input injected without permission: {:?}", events());

    // 2. Permitted: valid events arrive (clamped), garbage doesn't.
    allowed.store(true, Ordering::Relaxed);
    dc.send_text("garbage").await.unwrap();
    dc.send_text(r#"{"t":"down","id":0,"x":0.5,"y":2.0}"#).await.unwrap();
    assert!(eventually(|| events().contains(&PointerEvent::Down { id: 0, x: 0.5, y: 1.0 })).await, "{:?}", events());
    assert_eq!(events().len(), 1);

    // 3. Permission revoked mid-touch: the pointer is lifted, later input is ignored.
    allowed.store(false, Ordering::Relaxed);
    dc.send_text(r#"{"t":"move","id":0,"x":0.1,"y":0.1}"#).await.unwrap();
    assert!(eventually(|| events().contains(&PointerEvent::Up { id: 0 })).await, "{:?}", events());
    assert!(!events().iter().any(|e| matches!(e, PointerEvent::Move { .. })));

    // 4. Link drops mid-touch: pointer released.
    allowed.store(true, Ordering::Relaxed);
    dc.send_text(r#"{"t":"down","id":3,"x":0.2,"y":0.2}"#).await.unwrap();
    assert!(eventually(|| events().contains(&PointerEvent::Down { id: 3, x: 0.2, y: 0.2 })).await);
    host.on_car_offline("car").await;
    assert!(eventually(|| events().iter().filter(|e| **e == PointerEvent::Up { id: 3 }).count() == 1).await, "{:?}", events());
}
