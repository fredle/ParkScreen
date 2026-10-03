use crate::identity::Identity;
use futures_util::{SinkExt, StreamExt};
use protocol::{HostToServer, ServerToHost};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{info, warn};

/// What the agent gets from the connection.
#[derive(Debug)]
pub enum Event {
    Connected,
    Disconnected,
    Msg(ServerToHost),
}

/// Handle for sending to the server (queued while reconnecting is NOT done: messages are dropped).
#[derive(Clone)]
pub struct Sender(mpsc::UnboundedSender<HostToServer>);

impl Sender {
    pub fn send(&self, m: HostToServer) {
        let _ = self.0.send(m);
    }
}

/// Connect to `url` (e.g. `wss://parkscreen.leatham.net/ws/host`), authenticate, and
/// reconnect forever with backoff. Pings every 30 s (Cloudflare idles sockets at ~100 s).
pub fn spawn(url: String, identity: Identity) -> (Sender, mpsc::UnboundedReceiver<Event>) {
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<HostToServer>();
    let (ev_tx, ev_rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        let mut backoff = Duration::from_secs(1);
        loop {
            match run_once(&url, &identity, &mut out_rx, &ev_tx).await {
                Ok(true) => backoff = Duration::from_secs(1),
                Ok(false) => {}
                Err(e) => warn!("signalling: {e}"),
            }
            if ev_tx.send(Event::Disconnected).is_err() {
                return;
            }
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(Duration::from_secs(30));
        }
    });
    (Sender(out_tx), ev_rx)
}

/// Returns Ok(true) if the connection became ready (so backoff resets).
async fn run_once(
    url: &str,
    id: &Identity,
    out_rx: &mut mpsc::UnboundedReceiver<HostToServer>,
    ev_tx: &mpsc::UnboundedSender<Event>,
) -> Result<bool, Box<dyn std::error::Error>> {
    let (ws, _) = connect_async(url).await?;
    let (mut sink, mut stream) = ws.split();
    let mut ready = false;
    let mut ping = tokio::time::interval_at(tokio::time::Instant::now() + Duration::from_secs(30), Duration::from_secs(30));
    loop {
        tokio::select! {
            m = stream.next() => {
                let Some(m) = m else { return Ok(ready) };
                let Message::Text(t) = m? else { continue };
                match serde_json::from_str::<ServerToHost>(&t)? {
                    ServerToHost::Challenge { nonce } => {
                        let signature = id.sign_challenge(&nonce).ok_or("bad nonce")?;
                        let hello = HostToServer::Hello { host_id: id.host_id(), signature };
                        sink.send(Message::Text(serde_json::to_string(&hello)?.into())).await?;
                    }
                    ServerToHost::Ready => {
                        info!("signed in to server");
                        ready = true;
                        let _ = ev_tx.send(Event::Connected);
                    }
                    ServerToHost::Error { message } => return Err(message.into()),
                    other => { let _ = ev_tx.send(Event::Msg(other)); }
                }
            }
            Some(m) = out_rx.recv() => {
                sink.send(Message::Text(serde_json::to_string(&m)?.into())).await?;
            }
            _ = ping.tick() => {
                sink.send(Message::Text(serde_json::to_string(&HostToServer::Ping)?.into())).await?;
            }
        }
    }
}

/// `https://parkscreen-server-x.a.run.app` (or an already-`wss://` URL) → the host socket URL.
pub fn host_socket_url(server: &str) -> String {
    let s = server.trim().trim_end_matches('/');
    let s = s.strip_suffix("/ws/host").unwrap_or(s);
    let s = if let Some(rest) = s.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = s.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        s.to_string()
    };
    format!("{s}/ws/host")
}

#[cfg(test)]
mod tests {
    use super::host_socket_url;

    #[test]
    fn builds_socket_urls() {
        assert_eq!(host_socket_url("https://a.run.app"), "wss://a.run.app/ws/host");
        assert_eq!(host_socket_url("https://a.run.app/"), "wss://a.run.app/ws/host");
        assert_eq!(host_socket_url("http://127.0.0.1:8080"), "ws://127.0.0.1:8080/ws/host");
        assert_eq!(host_socket_url("wss://a.run.app/ws/host"), "wss://a.run.app/ws/host");
    }
}
