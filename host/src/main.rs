use parkscreen_host::{
    agent::Agent,
    allowlist::AllowList,
    display::NullDisplay,
    rtc_sender::{SoftwareMedia, WebRtcHandler},
    identity::Identity,
    signalling,
};
use std::path::PathBuf;

fn data_dir() -> PathBuf {
    if let Ok(d) = std::env::var("PARKSCREEN_DATA") {
        return d.into();
    }
    let base = std::env::var("LOCALAPPDATA").or_else(|_| std::env::var("XDG_DATA_HOME")).unwrap_or_else(|_| {
        format!("{}/.local/share", std::env::var("HOME").unwrap_or_else(|_| ".".into()))
    });
    PathBuf::from(base).join("ParkScreen")
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::from_default_env().add_directive("info".parse().unwrap())).init();
    let url = std::env::var("PARKSCREEN_URL").unwrap_or_else(|_| "wss://parkscreen.leatham.net/ws/host".into());
    let dir = data_dir();
    let identity = Identity::load_or_create(&dir.join("host.key")).expect("identity");
    let allow = AllowList::load(dir.join("cars.txt")).expect("allow-list");
    println!("host id: {}", identity.host_id());

    let (tx, events) = signalling::spawn(url, identity);
    let mut agent = Agent {
        tx: tx.clone(),
        allow,
        handler: WebRtcHandler::new(NullDisplay::default(), std::sync::Arc::new(SoftwareMedia::default())),
        on_pair_code: Box::new(|code| println!("Pairing code (valid 5 min): {code}")),
    };
    if std::env::args().any(|a| a == "--pair") {
        agent.request_pair_code();
    }
    tokio::select! {
        _ = agent.run(events) => {}
        _ = tokio::signal::ctrl_c() => {}
    }
}
