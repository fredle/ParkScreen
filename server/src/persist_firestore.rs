//! Firestore persistence over the REST API (no gRPC dependency).
//!
//! Layout: `hosts/{host_id}` and `pairings/{car_id}_{host_id}` (fields `car_id`, `host_id`).
//! Auth: the Cloud Run service account's access token from the metadata server; or, when
//! `FIRESTORE_EMULATOR_HOST` is set, plain HTTP with the emulator's `Bearer owner`.

use crate::db::Persist;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::{
    sync::Mutex,
    time::{Duration, Instant},
};

pub struct FirestorePersist {
    http: reqwest::Client,
    base: String,
    emulator: bool,
    token: Mutex<Option<(String, Instant)>>,
    metadata_url: String,
}

impl FirestorePersist {
    /// `project` is the GCP project id. Honours `FIRESTORE_EMULATOR_HOST`.
    pub fn new(project: &str) -> Self {
        let emulator = std::env::var("FIRESTORE_EMULATOR_HOST").ok();
        let root = match &emulator {
            Some(h) => format!("http://{h}"),
            None => "https://firestore.googleapis.com".to_string(),
        };
        Self::with_endpoint(project, &root, emulator.is_some(), "http://metadata.google.internal")
    }

    pub fn with_endpoint(project: &str, root: &str, emulator: bool, metadata_root: &str) -> Self {
        Self {
            http: reqwest::Client::builder().timeout(Duration::from_secs(15)).build().unwrap(),
            base: format!("{root}/v1/projects/{project}/databases/(default)/documents"),
            emulator,
            token: Mutex::new(None),
            metadata_url: format!("{metadata_root}/computeMetadata/v1/instance/service-accounts/default/token"),
        }
    }

    async fn auth(&self) -> Result<String, String> {
        if self.emulator {
            return Ok("owner".into());
        }
        if let Some((t, exp)) = &*self.token.lock().unwrap() {
            if *exp > Instant::now() {
                return Ok(t.clone());
            }
        }
        let v: Value = self
            .http
            .get(&self.metadata_url)
            .header("Metadata-Flavor", "Google")
            .send()
            .await
            .and_then(|r| r.error_for_status())
            .map_err(|e| format!("metadata token: {e}"))?
            .json()
            .await
            .map_err(|e| e.to_string())?;
        let tok = v["access_token"].as_str().ok_or("no access_token")?.to_string();
        let ttl = v["expires_in"].as_u64().unwrap_or(300).saturating_sub(60);
        *self.token.lock().unwrap() = Some((tok.clone(), Instant::now() + Duration::from_secs(ttl)));
        Ok(tok)
    }

    async fn req(&self, m: reqwest::Method, url: String, body: Option<Value>) -> Result<Value, String> {
        let mut r = self.http.request(m, url).bearer_auth(self.auth().await?);
        if let Some(b) = body {
            r = r.json(&b);
        }
        let resp = r.send().await.map_err(|e| e.to_string())?;
        let status = resp.status();
        let text = resp.text().await.map_err(|e| e.to_string())?;
        if !status.is_success() {
            return Err(format!("firestore {status}: {}", text.chars().take(300).collect::<String>()));
        }
        Ok(serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    async fn list(&self, collection: &str) -> Result<Vec<Value>, String> {
        let mut out = vec![];
        let mut token: Option<String> = None;
        loop {
            let mut url = format!("{}/{collection}?pageSize=300", self.base);
            if let Some(t) = &token {
                url.push_str(&format!("&pageToken={t}"));
            }
            let v = self.req(reqwest::Method::GET, url, None).await?;
            if let Some(docs) = v["documents"].as_array() {
                out.extend(docs.iter().cloned());
            }
            match v["nextPageToken"].as_str() {
                Some(t) if !t.is_empty() => token = Some(t.to_string()),
                _ => return Ok(out),
            }
        }
    }
}

fn s(v: &str) -> Value {
    json!({ "stringValue": v })
}

fn pairing_id(car: &str, host: &str) -> String {
    format!("{car}_{host}")
}

#[async_trait]
impl Persist for FirestorePersist {
    async fn load(&self) -> Result<(Vec<String>, Vec<(String, String)>), String> {
        let hosts = self
            .list("hosts")
            .await?
            .iter()
            .filter_map(|d| d["name"].as_str()?.rsplit('/').next().map(String::from))
            .collect();
        let pairs = self
            .list("pairings")
            .await?
            .iter()
            .filter_map(|d| Some((d["fields"]["car_id"]["stringValue"].as_str()?.to_string(), d["fields"]["host_id"]["stringValue"].as_str()?.to_string())))
            .collect();
        Ok((hosts, pairs))
    }

    async fn put_host(&self, id: &str) -> Result<(), String> {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let body = json!({ "fields": { "created": { "integerValue": now.to_string() } } });
        self.req(reqwest::Method::PATCH, format!("{}/hosts/{id}", self.base), Some(body)).await.map(|_| ())
    }

    async fn put_pairing(&self, car: &str, host: &str) -> Result<(), String> {
        let body = json!({ "fields": { "car_id": s(car), "host_id": s(host) } });
        self.req(reqwest::Method::PATCH, format!("{}/pairings/{}", self.base, pairing_id(car, host)), Some(body)).await.map(|_| ())
    }

    async fn delete_pairing(&self, car: &str, host: &str) -> Result<(), String> {
        self.req(reqwest::Method::DELETE, format!("{}/pairings/{}", self.base, pairing_id(car, host)), None).await.map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use axum::{
        extract::{Path, Query, State},
        http::{HeaderMap, StatusCode},
        routing::{get, patch},
        Json, Router,
    };
    use std::{collections::BTreeMap, sync::Arc};

    /// A minimal stand-in for the Firestore REST surface we use. It checks the wire shape
    /// we send (paths, bearer auth, field encoding), not Firestore's actual behaviour.
    type Docs = Arc<Mutex<BTreeMap<String, Value>>>;

    async fn fake() -> (String, Docs) {
        let docs: Docs = Default::default();
        async fn put(State(d): State<Docs>, h: HeaderMap, Path((_p, col, id)): Path<(String, String, String)>, Json(b): Json<Value>) -> StatusCode {
            if h.get("authorization").and_then(|v| v.to_str().ok()) != Some("Bearer owner") {
                return StatusCode::UNAUTHORIZED;
            }
            let mut doc = b;
            doc["name"] = json!(format!("projects/p/databases/(default)/documents/{col}/{id}"));
            d.lock().unwrap().insert(format!("{col}/{id}"), doc);
            StatusCode::OK
        }
        async fn del(State(d): State<Docs>, Path((_p, col, id)): Path<(String, String, String)>) -> Json<Value> {
            d.lock().unwrap().remove(&format!("{col}/{id}"));
            Json(json!({}))
        }
        async fn list(State(d): State<Docs>, Path((_p, col)): Path<(String, String)>, Query(q): Query<BTreeMap<String, String>>) -> Json<Value> {
            // Two documents per page, to exercise paging.
            let all: Vec<Value> = d.lock().unwrap().iter().filter(|(k, _)| k.starts_with(&format!("{col}/"))).map(|(_, v)| v.clone()).collect();
            let start: usize = q.get("pageToken").and_then(|t| t.parse().ok()).unwrap_or(0);
            let page: Vec<Value> = all.iter().skip(start).take(2).cloned().collect();
            let mut out = json!({ "documents": page });
            if start + 2 < all.len() {
                out["nextPageToken"] = json!((start + 2).to_string());
            }
            Json(out)
        }
        let app = Router::new()
            .route("/v1/projects/{p}/databases/(default)/documents/{col}/{id}", patch(put).delete(del))
            .route("/v1/projects/{p}/databases/(default)/documents/{col}", get(list))
            .with_state(docs.clone());
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        (format!("http://{addr}"), docs)
    }

    #[tokio::test]
    async fn roundtrip_through_rest() {
        let (root, docs) = fake().await;
        let mk = || FirestorePersist::with_endpoint("p", &root, true, "http://unused");
        {
            let db = Db::open(Box::new(mk())).await.unwrap();
            db.register_host("hostA").await.unwrap();
            for i in 0..5 {
                db.add_pairing(&format!("car{i}"), "hostA").await.unwrap();
            }
            db.revoke("car2", "hostA").await.unwrap();
        }
        assert!(docs.lock().unwrap().contains_key("hosts/hostA"));
        assert!(docs.lock().unwrap().contains_key("pairings/car0_hostA"));
        assert!(!docs.lock().unwrap().contains_key("pairings/car2_hostA"));
        // A fresh server instance sees the same pairings (paged load).
        let db = Db::open(Box::new(mk())).await.unwrap();
        assert_eq!(db.cars_for_host("hostA"), vec!["car0", "car1", "car3", "car4"]);
        assert!(!db.is_paired("car2", "hostA"));
    }

    #[tokio::test]
    async fn http_errors_surface_and_roll_back() {
        let (root, _docs) = fake().await;
        // Non-emulator mode with an unreachable metadata server: auth fails, so the write fails.
        let p = FirestorePersist::with_endpoint("p", &root, false, "http://127.0.0.1:9");
        let db = Db::open(Box::new(NoLoad(p))).await.unwrap();
        assert!(db.add_pairing("c", "h").await.is_err());
        assert!(!db.is_paired("c", "h"));
    }

    /// Skips the initial load so the failing-auth case reaches the write path.
    struct NoLoad(FirestorePersist);
    #[async_trait]
    impl Persist for NoLoad {
        async fn load(&self) -> Result<(Vec<String>, Vec<(String, String)>), String> {
            Ok((vec![], vec![]))
        }
        async fn put_host(&self, h: &str) -> Result<(), String> {
            self.0.put_host(h).await
        }
        async fn put_pairing(&self, c: &str, h: &str) -> Result<(), String> {
            self.0.put_pairing(c, h).await
        }
        async fn delete_pairing(&self, c: &str, h: &str) -> Result<(), String> {
            self.0.delete_pairing(c, h).await
        }
    }
}
