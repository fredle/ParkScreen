use parkscreen_server::{
    app,
    db::{Db, NoPersist, Persist},
    persist_firestore::FirestorePersist,
    persist_sqlite::SqlitePersist,
    state::AppState,
};
use std::sync::Arc;
use tracing::info;

/// `STORE=firestore` (Cloud Run; needs `FIRESTORE_PROJECT` or `GOOGLE_CLOUD_PROJECT`),
/// `STORE=sqlite` (default; `DATABASE_URL`, default `sqlite://parkscreen.db`) or `STORE=memory`.
async fn persist_from_env() -> Box<dyn Persist> {
    match std::env::var("STORE").as_deref().unwrap_or("sqlite") {
        "firestore" => {
            let project = std::env::var("FIRESTORE_PROJECT")
                .or_else(|_| std::env::var("GOOGLE_CLOUD_PROJECT"))
                .expect("STORE=firestore needs FIRESTORE_PROJECT");
            Box::new(FirestorePersist::new(&project))
        }
        "memory" => Box::new(NoPersist),
        _ => {
            let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| "sqlite://parkscreen.db".into());
            Box::new(SqlitePersist::open(&url).expect("open sqlite"))
        }
    }
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::from_default_env()).init();
    // Cloud Run provides PORT.
    let addr = std::env::var("LISTEN").unwrap_or_else(|_| format!("0.0.0.0:{}", std::env::var("PORT").unwrap_or_else(|_| "8080".into())));
    let origins = std::env::var("ALLOWED_ORIGINS")
        .unwrap_or_else(|_| "https://parkscreen.web.app".into())
        .split(',')
        .map(|s| s.trim().trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let db = Db::open(persist_from_env().await).await.expect("open store");
    let state = Arc::new(AppState::new(db).with_origins(origins));
    let listener = tokio::net::TcpListener::bind(&addr).await.expect("bind");
    info!(%addr, "parkscreen-server listening");
    axum::serve(listener, app(state)).await.unwrap();
}
