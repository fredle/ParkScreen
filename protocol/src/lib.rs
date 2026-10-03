//! Signalling messages shared by the server and the host agent.
//! The web client mirrors these in `web/client/src/protocol.ts`.
//!
//! The server only brokers: `Signal` payloads (SDP / ICE) are opaque to it.

use serde::{Deserialize, Serialize};

/// Messages sent by a host agent on `/ws/host`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HostToServer {
    /// Answer to `ServerToHost::Challenge`. `host_id` is the base64url
    /// Ed25519 public key; `signature` signs the raw nonce bytes.
    Hello { host_id: String, signature: String },
    /// Ask for a 6-digit pairing code.
    PairStart,
    /// Forget a paired car.
    Revoke { car_id: String },
    /// Relay a payload to one connected car.
    Signal { car_id: String, payload: serde_json::Value },
    Ping,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerToHost {
    /// base64url random nonce to sign.
    Challenge { nonce: String },
    Ready,
    PairCode { code: String, expires_in: u64 },
    /// A car finished pairing; `car_id` is the SHA-256 hex of its token.
    Paired { car_id: String },
    /// A paired car connected and wants a session.
    CarOnline { car_id: String },
    CarOffline { car_id: String },
    Signal { car_id: String, payload: serde_json::Value },
    Pong,
    Error { message: String },
}

/// Messages sent by a car (web client) on `/ws/car`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CarToServer {
    /// `token` is the paired-car token issued by `/api/pair/claim`.
    Hello { token: String },
    Signal { host_id: String, payload: serde_json::Value },
    Ping,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerToCar {
    /// Hosts this car is paired with and whether each is online.
    Hosts { hosts: Vec<HostStatus> },
    HostOnline { host_id: String },
    HostOffline { host_id: String },
    Signal { host_id: String, payload: serde_json::Value },
    Pong,
    Error { message: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HostStatus {
    pub host_id: String,
    pub online: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairClaimRequest {
    pub code: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairClaimResponse {
    pub token: String,
    pub host_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_format_is_snake_case_tagged() {
        let m = CarToServer::Signal { host_id: "h".into(), payload: serde_json::json!({"sdp": "x"}) };
        let s = serde_json::to_string(&m).unwrap();
        assert_eq!(s, r#"{"type":"signal","host_id":"h","payload":{"sdp":"x"}}"#);
        assert_eq!(serde_json::from_str::<CarToServer>(&s).unwrap(), m);
    }
}
