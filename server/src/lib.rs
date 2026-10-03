pub mod db;
pub mod persist_firestore;
pub mod persist_sqlite;
pub mod state;
pub mod ws;

use axum::{
    extract::{State, WebSocketUpgrade},
    http::{header, HeaderMap, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use protocol::{PairClaimRequest, PairClaimResponse, ServerToHost};
use rand::RngCore;
use rust_embed::RustEmbed;
use state::{send, AppState};
use std::sync::Arc;

#[derive(RustEmbed)]
#[folder = "../web/client/dist"]
#[allow_missing = true]
struct Assets;

pub type S = Arc<AppState>;

async fn healthz() -> &'static str {
    "ok"
}

fn origin_of(h: &HeaderMap) -> Option<&str> {
    h.get(header::ORIGIN).and_then(|v| v.to_str().ok())
}

/// CORS headers for an allowed browser origin (the client is hosted on a different origin).
fn cors(origin: Option<&str>) -> HeaderMap {
    use axum::http::HeaderValue;
    let mut h = HeaderMap::new();
    if let Some(o) = origin.and_then(|o| HeaderValue::from_str(o).ok()) {
        h.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, o);
        h.insert(header::ACCESS_CONTROL_ALLOW_METHODS, HeaderValue::from_static("POST, OPTIONS"));
        h.insert(header::ACCESS_CONTROL_ALLOW_HEADERS, HeaderValue::from_static("content-type"));
        h.insert(header::ACCESS_CONTROL_MAX_AGE, HeaderValue::from_static("86400"));
        h.insert(header::VARY, HeaderValue::from_static("Origin"));
    }
    h
}

async fn pair_preflight(State(s): State<S>, headers: HeaderMap) -> Response {
    let origin = origin_of(&headers);
    if !s.origin_allowed(origin) {
        return StatusCode::FORBIDDEN.into_response();
    }
    (StatusCode::NO_CONTENT, cors(origin)).into_response()
}

async fn pair_claim(State(s): State<S>, headers: HeaderMap, Json(req): Json<PairClaimRequest>) -> Response {
    let origin = origin_of(&headers);
    if !s.origin_allowed(origin) {
        return (StatusCode::FORBIDDEN, "origin not allowed").into_response();
    }
    let Some(host_id) = s.claim_pair_code(req.code.trim()) else {
        return (StatusCode::FORBIDDEN, cors(origin), "invalid or expired code").into_response();
    };
    let mut raw = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut raw);
    let token = hex::encode(raw);
    let car_id = ws::car_id_for_token(&token);
    if let Err(e) = s.db.add_pairing(&car_id, &host_id).await {
        tracing::error!("add_pairing: {e}");
        return (StatusCode::INTERNAL_SERVER_ERROR, cors(origin)).into_response();
    }
    if let Some(h) = s.hosts.lock().unwrap().get(&host_id) {
        send(h, &ServerToHost::Paired { car_id });
    }
    // The token lives in the client's localStorage (the page and this API are on different
    // origins, so a cookie would be third-party and unusable).
    (cors(origin), Json(PairClaimResponse { token, host_id })).into_response()
}

/// Browsers always send Origin on WebSocket upgrades; refuse ones from other sites.
fn ws_gate(s: &S, headers: &HeaderMap) -> Option<Response> {
    (!s.origin_allowed(origin_of(headers))).then(|| (StatusCode::FORBIDDEN, "origin not allowed").into_response())
}

async fn static_file(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() || path.ends_with('/') { format!("{path}index.html") } else { path.to_string() };
    let path = path.as_str();
    match Assets::get(path) {
        Some(f) => {
            let cache = if path.starts_with("assets/") { "public, max-age=31536000, immutable" } else { "no-cache" };
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            ([(header::CONTENT_TYPE, mime.to_string()), (header::CACHE_CONTROL, cache.to_string())], f.data).into_response()
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

pub fn app(state: S) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        // Cloud Run's front end swallows exactly "/healthz", so probes use /status there.
        .route("/status", get(healthz))
        .route("/api/pair/claim", post(pair_claim).options(pair_preflight))
        .route(
            "/ws/host",
            get(|State(s): State<S>, h: HeaderMap, u: Result<WebSocketUpgrade, axum::extract::ws::rejection::WebSocketUpgradeRejection>| async move {
                // Origin is checked first, so a foreign site gets 403 whatever it sends.
                if let Some(r) = ws_gate(&s, &h) {
                    return r;
                }
                match u {
                    Ok(u) => u.on_upgrade(move |sock| ws::host_socket(sock, s)),
                    Err(rej) => rej.into_response(),
                }
            }),
        )
        .route(
            "/ws/car",
            get(|State(s): State<S>, h: HeaderMap, u: Result<WebSocketUpgrade, axum::extract::ws::rejection::WebSocketUpgradeRejection>| async move {
                // Origin is checked first, so a foreign site gets 403 whatever it sends.
                if let Some(r) = ws_gate(&s, &h) {
                    return r;
                }
                match u {
                    Ok(u) => u.on_upgrade(move |sock| ws::car_socket(sock, s)),
                    Err(rej) => rej.into_response(),
                }
            }),
        )
        .fallback(static_file)
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    const ORIGIN: &str = "https://parkscreen.web.app";

    fn state() -> S {
        Arc::new(AppState::new(db::Db::in_memory()).with_origins(vec![ORIGIN.into()]))
    }

    fn claim(state: &S, code: &str, origin: Option<&str>) -> impl std::future::Future<Output = Response> {
        let mut req = Request::post("/api/pair/claim").header("content-type", "application/json");
        if let Some(o) = origin {
            req = req.header("origin", o);
        }
        let fut = app(state.clone()).oneshot(req.body(Body::from(format!(r#"{{"code":"{code}"}}"#))).unwrap());
        async move { fut.await.unwrap() }
    }

    #[tokio::test]
    async fn claim_rejects_bad_code_and_accepts_good_with_cors() {
        let s = state();
        let code = s.new_pair_code("host1");
        assert_eq!(claim(&s, "000000", Some(ORIGIN)).await.status(), StatusCode::FORBIDDEN);
        let ok = claim(&s, &code, Some(ORIGIN)).await;
        assert_eq!(ok.status(), StatusCode::OK);
        assert_eq!(ok.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN], ORIGIN);
        assert!(ok.headers().get(header::SET_COOKIE).is_none());
        assert_eq!(claim(&s, &code, Some(ORIGIN)).await.status(), StatusCode::FORBIDDEN, "single use");
    }

    #[tokio::test]
    async fn claim_refuses_foreign_origin_without_burning_the_code() {
        let s = state();
        let code = s.new_pair_code("host1");
        let r = claim(&s, &code, Some("https://evil.example")).await;
        assert_eq!(r.status(), StatusCode::FORBIDDEN);
        assert!(r.headers().get(header::ACCESS_CONTROL_ALLOW_ORIGIN).is_none());
        assert_eq!(claim(&s, &code, Some(ORIGIN)).await.status(), StatusCode::OK, "code still valid");
    }

    #[tokio::test]
    async fn preflight_and_ws_origin_checks() {
        let s = state();
        let pre = |o: &str| {
            Request::options("/api/pair/claim")
                .header("origin", o)
                .header("access-control-request-method", "POST")
                .body(Body::empty())
                .unwrap()
        };
        let r = app(s.clone()).oneshot(pre(ORIGIN)).await.unwrap();
        assert_eq!(r.status(), StatusCode::NO_CONTENT);
        assert_eq!(r.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN], ORIGIN);
        assert_eq!(app(s.clone()).oneshot(pre("https://evil.example")).await.unwrap().status(), StatusCode::FORBIDDEN);

        // WebSocket upgrade from a foreign origin is refused before the upgrade.
        let ws = Request::get("/ws/car")
            .header("origin", "https://evil.example")
            .header("connection", "upgrade")
            .header("upgrade", "websocket")
            .header("sec-websocket-version", "13")
            .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==")
            .body(Body::empty())
            .unwrap();
        assert_eq!(app(s).oneshot(ws).await.unwrap().status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn health_endpoints() {
        for p in ["/healthz", "/status"] {
            let r = app(state()).oneshot(Request::get(p).body(Body::empty()).unwrap()).await.unwrap();
            assert_eq!(r.status(), StatusCode::OK, "{p}");
        }
    }
}
