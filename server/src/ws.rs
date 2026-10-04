use crate::state::{send, AppState};
use axum::extract::ws::{Message, WebSocket};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64, Engine};
use ed25519_dalek::{Signature, VerifyingKey};
use futures_util::{SinkExt, StreamExt};
use protocol::*;
use sha2::{Digest, Sha256};
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{info, warn};

pub fn car_id_for_token(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

fn verify_host(host_id: &str, nonce: &[u8], signature: &str) -> bool {
    let (Ok(key), Ok(sig)) = (B64.decode(host_id), B64.decode(signature)) else { return false };
    let (Ok(key), Ok(sig)) = (<[u8; 32]>::try_from(key), <[u8; 64]>::try_from(sig)) else { return false };
    let Ok(key) = VerifyingKey::from_bytes(&key) else { return false };
    key.verify_strict(nonce, &Signature::from_bytes(&sig)).is_ok()
}

/// Pumps outbound strings to the socket and returns inbound text frames via a channel.
fn split(socket: WebSocket) -> (mpsc::UnboundedSender<String>, mpsc::UnboundedReceiver<String>) {
    let (mut sink, mut stream) = socket.split();
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();
    let (in_tx, in_rx) = mpsc::unbounded_channel::<String>();
    tokio::spawn(async move {
        while let Some(s) = out_rx.recv().await {
            if sink.send(Message::Text(s.into())).await.is_err() {
                break;
            }
        }
    });
    tokio::spawn(async move {
        while let Some(Ok(m)) = stream.next().await {
            match m {
                Message::Text(t) => {
                    if in_tx.send(t.to_string()).is_err() {
                        break;
                    }
                }
                Message::Close(_) => break,
                _ => {}
            }
        }
    });
    (out_tx, in_rx)
}

pub async fn host_socket(socket: WebSocket, state: Arc<AppState>) {
    let (tx, mut rx) = split(socket);

    let nonce: [u8; 32] = rand::random();
    send(&tx, &ServerToHost::Challenge { nonce: B64.encode(nonce) });

    let first = tokio::time::timeout(std::time::Duration::from_secs(10), rx.recv()).await;
    let Ok(Some(first)) = first else { return };
    let Ok(HostToServer::Hello { host_id, signature }) = serde_json::from_str(&first) else {
        send(&tx, &ServerToHost::Error { message: "expected hello".into() });
        return;
    };
    if !verify_host(&host_id, &nonce, &signature) {
        send(&tx, &ServerToHost::Error { message: "bad signature".into() });
        return;
    }
    if let Err(e) = state.db.register_host(&host_id).await {
        warn!("register_host: {e}");
        send(&tx, &ServerToHost::Error { message: "storage unavailable".into() });
        return;
    }
    state.hosts.lock().unwrap().insert(host_id.clone(), tx.clone());
    info!(%host_id, "host online");
    send(&tx, &ServerToHost::Ready);
    send(&tx, &ServerToHost::IceServers { ice_servers: state.ice.servers().await });
    // Tell paired cars that are already connected.
    for car_id in state.db.cars_for_host(&host_id) {
        if let Some(c) = state.cars.lock().unwrap().get(&car_id) {
            send(c, &ServerToCar::HostOnline { host_id: host_id.clone() });
        }
        if state.cars.lock().unwrap().contains_key(&car_id) {
            send(&tx, &ServerToHost::CarOnline { car_id });
        }
    }

    while let Some(text) = rx.recv().await {
        let Ok(msg) = serde_json::from_str::<HostToServer>(&text) else {
            send(&tx, &ServerToHost::Error { message: "bad message".into() });
            continue;
        };
        match msg {
            HostToServer::Hello { .. } => {}
            HostToServer::Ping => send(&tx, &ServerToHost::Pong),
            HostToServer::PairStart => {
                let code = state.new_pair_code(&host_id);
                send(&tx, &ServerToHost::PairCode { code, expires_in: crate::state::PAIR_CODE_TTL.as_secs() });
            }
            HostToServer::Revoke { car_id } => {
                let _ = state.db.revoke(&car_id, &host_id).await;
            }
            HostToServer::Signal { car_id, payload } => {
                if state.db.is_paired(&car_id, &host_id) {
                    if let Some(c) = state.cars.lock().unwrap().get(&car_id) {
                        send(c, &ServerToCar::Signal { host_id: host_id.clone(), payload });
                    }
                }
            }
        }
    }

    // Only remove our own registration (a reconnect may have replaced it).
    {
        let mut hosts = state.hosts.lock().unwrap();
        if hosts.get(&host_id).is_some_and(|t| t.same_channel(&tx)) {
            hosts.remove(&host_id);
        }
    }
    info!(%host_id, "host offline");
    for car_id in state.db.cars_for_host(&host_id) {
        if let Some(c) = state.cars.lock().unwrap().get(&car_id) {
            send(c, &ServerToCar::HostOffline { host_id: host_id.clone() });
        }
    }
}

pub async fn car_socket(socket: WebSocket, state: Arc<AppState>) {
    let (tx, mut rx) = split(socket);

    let first = tokio::time::timeout(std::time::Duration::from_secs(10), rx.recv()).await;
    let Ok(Some(first)) = first else { return };
    let Ok(CarToServer::Hello { token }) = serde_json::from_str(&first) else {
        send(&tx, &ServerToCar::Error { message: "expected hello".into() });
        return;
    };
    let car_id = car_id_for_token(&token);
    let paired = state.db.hosts_for_car(&car_id);
    if paired.is_empty() {
        send(&tx, &ServerToCar::Error { message: "not paired".into() });
        return;
    }
    state.cars.lock().unwrap().insert(car_id.clone(), tx.clone());
    info!(car = %&car_id[..8], "car online");

    let statuses = {
        let hosts = state.hosts.lock().unwrap();
        paired.iter().map(|h| HostStatus { host_id: h.clone(), online: hosts.contains_key(h) }).collect()
    };
    send(&tx, &ServerToCar::IceServers { ice_servers: state.ice.servers().await });
    send(&tx, &ServerToCar::Hosts { hosts: statuses });
    for h in &paired {
        if let Some(ht) = state.hosts.lock().unwrap().get(h) {
            send(ht, &ServerToHost::CarOnline { car_id: car_id.clone() });
        }
    }

    while let Some(text) = rx.recv().await {
        match serde_json::from_str::<CarToServer>(&text) {
            Ok(CarToServer::Ping) => send(&tx, &ServerToCar::Pong),
            Ok(CarToServer::Hello { .. }) => {}
            Ok(CarToServer::Signal { host_id, payload }) => {
                if state.db.is_paired(&car_id, &host_id) {
                    if let Some(h) = state.hosts.lock().unwrap().get(&host_id) {
                        send(h, &ServerToHost::Signal { car_id: car_id.clone(), payload });
                    }
                } else {
                    warn!("signal to unpaired host");
                }
            }
            Err(_) => send(&tx, &ServerToCar::Error { message: "bad message".into() }),
        }
    }

    {
        let mut cars = state.cars.lock().unwrap();
        if cars.get(&car_id).is_some_and(|t| t.same_channel(&tx)) {
            cars.remove(&car_id);
        }
    }
    for h in &paired {
        if let Some(ht) = state.hosts.lock().unwrap().get(h) {
            send(ht, &ServerToHost::CarOffline { car_id: car_id.clone() });
        }
    }
}
