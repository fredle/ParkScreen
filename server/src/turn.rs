//! ICE server list handed to the host and the car. STUN is always included; if Cloudflare
//! Realtime TURN is configured (`CF_TURN_KEY_ID` + `CF_TURN_API_TOKEN`) a TURN relay with
//! short-lived credentials is added, for networks where a direct path can't be punched.

use protocol::IceServer;
use serde_json::{json, Value};
use std::time::{Duration, Instant};

const STUN: &str = "stun:stun.cloudflare.com:3478";
/// How long issued credentials stay valid. Must outlast a session plus the time a socket
/// (at most an hour on Cloud Run) holds them before a reconnect delivers fresh ones.
const CREDENTIAL_TTL: Duration = Duration::from_secs(6 * 3600);
/// Reuse one credential for this long instead of calling Cloudflare per connection.
const REFRESH_AFTER: Duration = Duration::from_secs(3600);

struct Cloudflare {
    key_id: String,
    api_token: String,
    http: reqwest::Client,
}

pub struct IceProvider {
    cloudflare: Option<Cloudflare>,
    cache: tokio::sync::Mutex<Option<(Instant, Vec<IceServer>)>>,
}

impl IceProvider {
    /// STUN only.
    pub fn stun_only() -> Self {
        Self { cloudflare: None, cache: Default::default() }
    }

    pub fn cloudflare(key_id: String, api_token: String) -> Self {
        let http = reqwest::Client::builder().timeout(Duration::from_secs(5)).build().unwrap();
        Self { cloudflare: Some(Cloudflare { key_id, api_token, http }), cache: Default::default() }
    }

    pub fn from_env() -> Self {
        match (std::env::var("CF_TURN_KEY_ID"), std::env::var("CF_TURN_API_TOKEN")) {
            (Ok(id), Ok(token)) if !id.is_empty() && !token.is_empty() => Self::cloudflare(id, token),
            _ => Self::stun_only(),
        }
    }

    pub fn has_turn(&self) -> bool {
        self.cloudflare.is_some()
    }

    /// Servers for a new connection. Never fails: on a TURN error it serves the last good
    /// credentials, or STUN alone.
    pub async fn servers(&self) -> Vec<IceServer> {
        let Some(cf) = &self.cloudflare else { return stun_only() };
        let mut cache = self.cache.lock().await;
        if let Some((at, s)) = cache.as_ref() {
            if at.elapsed() < REFRESH_AFTER {
                return s.clone();
            }
        }
        match cf.generate().await {
            Ok(s) => {
                *cache = Some((Instant::now(), s.clone()));
                s
            }
            Err(e) => {
                tracing::warn!("TURN credentials: {e}");
                // Stale credentials are still valid for most of their TTL.
                cache.as_ref().filter(|(at, _)| at.elapsed() < CREDENTIAL_TTL - REFRESH_AFTER).map(|(_, s)| s.clone()).unwrap_or_else(stun_only)
            }
        }
    }
}

fn stun_only() -> Vec<IceServer> {
    vec![IceServer { urls: vec![STUN.into()], username: None, credential: None }]
}

impl Cloudflare {
    async fn generate(&self) -> Result<Vec<IceServer>, String> {
        let url = format!("https://rtc.live.cloudflare.com/v1/turn/keys/{}/credentials/generate-ice-servers", self.key_id);
        let resp = self
            .http
            .post(url)
            .bearer_auth(&self.api_token)
            .json(&json!({ "ttl": CREDENTIAL_TTL.as_secs() }))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !resp.status().is_success() {
            return Err(format!("Cloudflare returned {}", resp.status()));
        }
        parse_ice_servers(&resp.json::<Value>().await.map_err(|e| e.to_string())?)
    }
}

/// `{"iceServers":[{"urls":[..]},{"urls":[..],"username":..,"credential":..}]}`
fn parse_ice_servers(v: &Value) -> Result<Vec<IceServer>, String> {
    let servers: Vec<IceServer> = serde_json::from_value(v.get("iceServers").cloned().ok_or("no iceServers")?).map_err(|e| e.to_string())?;
    if !servers.iter().any(|s| s.credential.is_some()) {
        return Err("response has no TURN credentials".into());
    }
    Ok(servers)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn without_config_serves_stun_only() {
        let p = IceProvider::stun_only();
        assert!(!p.has_turn());
        assert_eq!(p.servers().await, stun_only());
    }

    #[test]
    fn parses_cloudflare_response() {
        let v = json!({"iceServers":[
            {"urls":["stun:stun.cloudflare.com:3478"]},
            {"urls":["turn:turn.cloudflare.com:3478?transport=udp"],"username":"u","credential":"c"}]});
        let s = parse_ice_servers(&v).unwrap();
        assert_eq!(s.len(), 2);
        assert_eq!(s[1].username.as_deref(), Some("u"));
        assert!(parse_ice_servers(&json!({"iceServers":[{"urls":["stun:x"]}]})).is_err());
        assert!(parse_ice_servers(&json!({})).is_err());
    }
}
