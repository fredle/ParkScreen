use crate::{
    allowlist::AllowList,
    display::{DisplayBackend, Mode},
    signalling::{Event, Sender},
};
use async_trait::async_trait;
use protocol::{HostToServer, ServerToHost};
use serde_json::Value;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc::UnboundedReceiver;
use tracing::{info, warn};

/// Called when a paired car sends a payload. The WebRTC sender (webrtc-rs) will implement this.
#[async_trait]
pub trait SessionHandler: Send {
    /// Return any payloads to send back to the car.
    async fn on_signal(&mut self, car_id: &str, payload: Value) -> Vec<Value>;
    async fn on_car_offline(&mut self, car_id: &str);
}

/// Placeholder until WebRTC lands: handles `viewport` (the only message we can act on without it).
pub struct ViewportOnly<D: DisplayBackend> {
    pub display: D,
}

#[async_trait]
impl<D: DisplayBackend> SessionHandler for ViewportOnly<D> {
    async fn on_signal(&mut self, _car: &str, payload: Value) -> Vec<Value> {
        if payload.get("kind").and_then(Value::as_str) == Some("viewport") {
            let g = |k| payload.get(k).and_then(Value::as_u64).unwrap_or(0) as u32;
            let (w, h, fps) = (g("w"), g("h"), g("fps"));
            if (320..=7680).contains(&w) && (240..=4320).contains(&h) {
                let mode = Mode { width: w, height: h, refresh_hz: fps.clamp(30, 60) };
                info!(?mode, "car viewport");
                let r = self.display.plug(mode).await;
                if let Err(e) = r {
                    warn!("plug failed: {e}");
                }
            }
        }
        Vec::new()
    }
    async fn on_car_offline(&mut self, _car: &str) {
        let _ = self.display.unplug().await;
    }
}

pub struct Agent<H: SessionHandler> {
    pub tx: Sender,
    pub allow: Arc<Mutex<AllowList>>,
    /// Turn input injection on for cars that pair while this is set (explicit opt-in).
    pub input_on_pair: bool,
    pub handler: H,
    /// Receives each pairing code so the tray UI / CLI can show it.
    pub on_pair_code: Box<dyn FnMut(String) + Send>,
}

impl<H: SessionHandler> Agent<H> {
    pub fn request_pair_code(&self) {
        self.tx.send(HostToServer::PairStart);
    }

    pub async fn run(&mut self, mut events: UnboundedReceiver<Event>) {
        while let Some(ev) = events.recv().await {
            self.handle(ev).await;
        }
    }

    pub async fn handle(&mut self, ev: Event) {
        let Event::Msg(m) = ev else { return };
        match m {
            ServerToHost::PairCode { code, .. } => (self.on_pair_code)(code),
            ServerToHost::Paired { car_id } => {
                info!(car = %&car_id[..8.min(car_id.len())], "car paired");
                let mut al = self.allow.lock().unwrap();
                al.add(&car_id);
                if self.input_on_pair {
                    al.set_input(&car_id, true);
                }
            }
            ServerToHost::Signal { car_id, payload } => {
                if !self.allow.lock().unwrap().allows(&car_id) {
                    warn!("ignoring signal from car not in allow-list");
                    return;
                }
                for p in self.handler.on_signal(&car_id, payload).await {
                    self.tx.send(HostToServer::Signal { car_id: car_id.clone(), payload: p });
                }
            }
            ServerToHost::CarOffline { car_id } => self.handler.on_car_offline(&car_id).await,
            _ => {}
        }
    }
}
