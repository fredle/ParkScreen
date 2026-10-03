mod db;
mod state;
mod ws;

use axum::{
    extract::{State, WebSocketUpgrade},
    http::{header, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use protocol::{PairClaimRequest, PairClaimResponse, ServerToHost};
use rand::RngCore;
use rust_embed::RustEmbed;
use state::{send, AppState};
use std::sync::Arc;
use tracing::info;

#[derive(RustEmbed)]
#[folder = "../web/client/dist"]
#[allow_missing = true]
struct Assets;

type S = Arc<AppState>;

async fn healthz() -> &'static str {
    "ok"
}

async fn pair_claim(State(s): State<S>, Json(req): Json<PairClaimRequest>) -> Response {
    let Some(host_id) = s.claim_pair_code(req.code.trim()) else {
        return (StatusCode::FORBIDDEN, "invalid or expired code").into_response();
    };
    let mut raw = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut raw);
    let token = hex::encode(raw);
    let car_id = ws::car_id_for_token(&token);
    if s.db.add_pairing(&car_id, &host_id).is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    if let Some(h) = s.hosts.lock().unwrap().get(&host_id) {
        send(h, &ServerToHost::Paired { car_id });
    }
    let cookie = format!("ps_token={token}; Max-Age=315360000; Path=/; HttpOnly; Secure; SameSite=Strict");
    ([(header::SET_COOKIE, cookie)], Json(PairClaimResponse { token, host_id })).into_response()
}

async fn static_file(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    match Assets::get(path) {
        Some(f) => {
            let cache = if path.starts_with("assets/") { "public, max-age=31536000, immutable" } else { "no-cache" };
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            ([(header::CONTENT_TYPE, mime.to_string()), (header::CACHE_CONTROL, cache.to_string())], f.data).into_response()
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

fn app(state: S) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/api/pair/claim", post(pair_claim))
        .route("/ws/host", get(|u: WebSocketUpgrade, State(s): State<S>| async move { u.on_upgrade(move |sock| ws::host_socket(sock, s)) }))
        .route("/ws/car", get(|u: WebSocketUpgrade, State(s): State<S>| async move { u.on_upgrade(move |sock| ws::car_socket(sock, s)) }))
        .fallback(static_file)
        .with_state(state)
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::from_default_env()).init();
    let db_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| "sqlite://parkscreen.db".into());
    let addr = std::env::var("LISTEN").unwrap_or_else(|_| "0.0.0.0:8080".into());
    let state = Arc::new(AppState::new(db::Db::open(&db_url).expect("open database")));
    let listener = tokio::net::TcpListener::bind(&addr).await.expect("bind");
    info!(%addr, "parkscreen-server listening");
    axum::serve(listener, app(state)).await.unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    #[tokio::test]
    async fn claim_rejects_bad_code_and_accepts_good() {
        let state = Arc::new(AppState::new(db::Db::open(":memory:").unwrap()));
        let code = state.new_pair_code("host1");
        let call = |code: String| {
            let req = Request::post("/api/pair/claim")
                .header("content-type", "application/json")
                .body(Body::from(format!(r#"{{"code":"{code}"}}"#)))
                .unwrap();
            app(state.clone()).oneshot(req)
        };
        assert_eq!(call("000000".into()).await.unwrap().status(), StatusCode::FORBIDDEN);
        let ok = call(code.clone()).await.unwrap();
        assert_eq!(ok.status(), StatusCode::OK);
        assert!(ok.headers().get(header::SET_COOKIE).unwrap().to_str().unwrap().contains("HttpOnly"));
        assert_eq!(call(code).await.unwrap().status(), StatusCode::FORBIDDEN);
    }
}
