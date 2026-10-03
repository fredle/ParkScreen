use parkscreen_host::{
    agent::Agent,
    allowlist::AllowList,
    display::NullDisplay,
    rtc_sender::{SoftwareMedia, WebRtcHandler},
    identity::Identity,
    signalling,
};
use parkscreen_host::input::NullInput;
use std::{path::PathBuf, sync::{Arc, Mutex}};

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
    // Release builds bake the server in (PARKSCREEN_SERVER_URL at compile time); PARKSCREEN_URL
    // overrides it at run time, e.g. for local development.
    let server = std::env::var("PARKSCREEN_URL")
        .ok()
        .or_else(|| option_env!("PARKSCREEN_SERVER_URL").map(String::from))
        .unwrap_or_else(|| "http://127.0.0.1:8080".into());
    let url = signalling::host_socket_url(&server);
    let dir = data_dir();
    let identity = Identity::load_or_create(&dir.join("host.key")).expect("identity");
    let allow = Arc::new(Mutex::new(AllowList::load(dir.join("cars.txt")).expect("allow-list")));
    println!("host id: {}", identity.host_id());

    let (tx, events) = signalling::spawn(url, identity);
    let mut agent = Agent {
        tx: tx.clone(),
        allow: allow.clone(),
        input_on_pair: std::env::args().any(|a| a == "--with-input"),
        handler: {
            let gate = allow.clone();
            WebRtcHandler::new(NullDisplay::default(), Arc::new(SoftwareMedia::default()))
                .with_input(Box::new(NullInput), Arc::new(move |car| gate.lock().unwrap().input_allowed(car)))
        },
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
