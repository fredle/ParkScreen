//! Host agent against the real server, in-process.
use parkscreen_host::{
    agent::{Agent, ViewportOnly},
    allowlist::AllowList,
    display::NullDisplay,
    identity::Identity,
    signalling::{self, Event},
};
use parkscreen_server::{app, db::Db, state::AppState};
use protocol::ServerToHost;
use std::{sync::Arc, time::Duration};
use tokio::time::timeout;

async fn start_server() -> (Arc<AppState>, String) {
    let state = Arc::new(AppState::new(Db::open(":memory:").unwrap()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let st = state.clone();
    tokio::spawn(async move { axum::serve(listener, app(st)).await.unwrap() });
    (state, format!("ws://{addr}/ws/host"))
}

async fn next(rx: &mut tokio::sync::mpsc::UnboundedReceiver<Event>) -> Event {
    timeout(Duration::from_secs(5), rx.recv()).await.expect("timeout").expect("closed")
}

#[tokio::test]
async fn host_signs_in_pairs_and_gets_signals() {
    let (state, url) = start_server().await;
    let id = Identity::generate();
    let host_id = id.host_id();
    let (tx, mut rx) = signalling::spawn(url, id);

    assert!(matches!(next(&mut rx).await, Event::Connected));
    assert!(state.hosts.lock().unwrap().contains_key(&host_id));

    // Pairing code, then a car claims it (through the shared server state).
    tx.send(protocol::HostToServer::PairStart);
    let code = match next(&mut rx).await {
        Event::Msg(ServerToHost::PairCode { code, .. }) => code,
        e => panic!("{e:?}"),
    };
    assert_eq!(state.claim_pair_code(&code).as_deref(), Some(host_id.as_str()));

    // Agent handles a paired car's viewport signal.
    let mut agent = Agent {
        tx,
        allow: AllowList::in_memory(),
        handler: ViewportOnly { display: NullDisplay::default() },
        on_pair_code: Box::new(|_| {}),
    };
    agent.handle(Event::Msg(ServerToHost::Signal { car_id: "unknown".into(), payload: serde_json::json!({"kind":"viewport","w":1920,"h":1200,"fps":60}) })).await;
    assert!(agent.handler.display.current.is_none(), "unpaired car must be ignored");
    agent.handle(Event::Msg(ServerToHost::Paired { car_id: "car1".into() })).await;
    agent.handle(Event::Msg(ServerToHost::Signal { car_id: "car1".into(), payload: serde_json::json!({"kind":"viewport","w":1920,"h":1200,"fps":60}) })).await;
    assert_eq!(agent.handler.display.current.unwrap().width, 1920);
    agent.handle(Event::Msg(ServerToHost::CarOffline { car_id: "car1".into() })).await;
    assert!(agent.handler.display.current.is_none());
}

#[tokio::test]
async fn bad_signature_is_rejected() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::{connect_async, tungstenite::Message};
    let (_s, url) = start_server().await;
    let (mut ws, _) = connect_async(&url).await.unwrap();
    ws.next().await.unwrap().unwrap(); // challenge
    let id = Identity::generate();
    let bogus = id.sign_challenge("AAAA").unwrap(); // signs the wrong nonce
    let hello = serde_json::json!({"type":"hello","host_id":id.host_id(),"signature":bogus});
    ws.send(Message::Text(hello.to_string().into())).await.unwrap();
    let Message::Text(t) = ws.next().await.unwrap().unwrap() else { panic!() };
    assert!(t.contains("bad signature"));
}
