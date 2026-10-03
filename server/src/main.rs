use parkscreen_server::{app, db::Db, state::AppState};
use std::sync::Arc;
use tracing::info;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::from_default_env()).init();
    let db_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| "sqlite://parkscreen.db".into());
    let addr = std::env::var("LISTEN").unwrap_or_else(|_| "0.0.0.0:8080".into());
    let state = Arc::new(AppState::new(Db::open(&db_url).expect("open database")));
    let listener = tokio::net::TcpListener::bind(&addr).await.expect("bind");
    info!(%addr, "parkscreen-server listening");
    axum::serve(listener, app(state)).await.unwrap();
}

