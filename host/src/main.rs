use parkscreen_host::input::NullInput;
use parkscreen_host::{
    agent::Agent,
    allowlist::AllowList,
    identity::Identity,
    monitors::{self, MonitorMode, Selector},
    rtc_sender::WebRtcHandler,
    signalling,
};
use std::{path::PathBuf, str::FromStr, sync::{Arc, Mutex}};

const USAGE: &str = "\
parkscreen-host [--pair] [--with-input] [--bitrate 12M]
                [--monitor auto|<index>|<name>] [--encoder auto|hardware|software] [--match-viewport]
parkscreen-host list
parkscreen-host set-mode --monitor <index>|<name> 1920x1200[@60]

On Windows the agent streams the chosen monitor (default: a virtual display if present);
elsewhere it streams a test pattern. `--match-viewport` switches the monitor to the car's
screen size when it connects. RUST_LOG=parkscreen_host=debug for more logging.";

fn data_dir() -> PathBuf {
    if let Ok(d) = std::env::var("PARKSCREEN_DATA") {
        return d.into();
    }
    let base = std::env::var("LOCALAPPDATA").or_else(|_| std::env::var("XDG_DATA_HOME")).unwrap_or_else(|_| {
        format!("{}/.local/share", std::env::var("HOME").unwrap_or_else(|_| ".".into()))
    });
    PathBuf::from(base).join("ParkScreen")
}

fn flag(name: &str) -> bool {
    std::env::args().any(|a| a == name)
}

/// Value of `--name value` or `--name=value`.
fn opt(name: &str) -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    let prefix = format!("{name}=");
    args.iter().enumerate().find_map(|(i, a)| {
        if a == name {
            args.get(i + 1).cloned()
        } else {
            a.strip_prefix(&prefix).map(String::from)
        }
    })
}

/// Parse `12M`, `8000k` or `5000000` (bits per second) into kilobits per second.
fn parse_bitrate_kbps(s: &str) -> Result<u32, String> {
    let s = s.trim().to_ascii_lowercase();
    let (num, mult) = match s.strip_suffix('m') {
        Some(n) => (n, 1_000_000.0),
        None => match s.strip_suffix('k') {
            Some(n) => (n, 1_000.0),
            None => (s.as_str(), 1.0),
        },
    };
    let v: f64 = num.parse().map_err(|_| format!("bad bitrate '{s}' (try 12M or 8000k)"))?;
    if v <= 0.0 {
        return Err("bitrate must be positive".into());
    }
    Ok((v * mult / 1000.0).round().max(1.0) as u32)
}

fn exit_with(msg: impl std::fmt::Display) -> ! {
    eprintln!("error: {msg}");
    std::process::exit(2);
}

#[cfg(windows)]
fn init_dpi() {
    use windows::Win32::UI::HiDpi::{SetProcessDpiAwareness, PROCESS_PER_MONITOR_DPI_AWARE};
    // Report real pixels, not scaled ones. Fails harmlessly if already set.
    let _ = unsafe { SetProcessDpiAwareness(PROCESS_PER_MONITOR_DPI_AWARE) };
}

#[cfg(not(windows))]
fn init_dpi() {}

fn selector() -> Selector {
    match opt("--monitor") {
        Some(s) => Selector::from_str(&s).unwrap_or_else(|e| exit_with(e)),
        None => Selector::Auto,
    }
}

/// Handles the `list` and `set-mode` subcommands. Returns false for anything else.
fn subcommand() -> bool {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("list") => {
            let monitors = monitors::enumerate().unwrap_or_else(|e| exit_with(format!("{e:#}")));
            if monitors.is_empty() {
                exit_with("no monitors found");
            }
            for m in &monitors {
                println!(
                    "{}: {}  \"{}\"  [{}]  {}x{}@{}Hz at ({},{}){}{}",
                    m.index,
                    m.device_name,
                    m.friendly_name,
                    m.adapter,
                    m.width,
                    m.height,
                    m.hz,
                    m.x,
                    m.y,
                    if m.primary { "  primary" } else { "" },
                    if m.is_virtual() { "  virtual?" } else { "" },
                );
            }
            true
        }
        Some("set-mode") => {
            let mode = args
                .iter()
                .skip(1)
                .rev()
                .find_map(|a| MonitorMode::from_str(a).ok())
                .unwrap_or_else(|| exit_with("set-mode needs a mode such as 1920x1200@60"));
            let all = monitors::enumerate().unwrap_or_else(|e| exit_with(format!("{e:#}")));
            let m = monitors::select(&all, &selector()).unwrap_or_else(|e| exit_with(format!("{e:#}")));
            monitors::set_mode(&m.device_name, mode).unwrap_or_else(|e| exit_with(format!("{e:#}")));
            println!("{} set to {mode}", m.device_name);
            true
        }
        _ => false,
    }
}

#[tokio::main]
async fn main() {
    if flag("--help") || flag("-h") {
        println!("{USAGE}");
        return;
    }
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::from_default_env().add_directive("info".parse().unwrap())).init();
    init_dpi();
    if subcommand() {
        return;
    }

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

    let bitrate_kbps = opt("--bitrate").map(|b| parse_bitrate_kbps(&b).unwrap_or_else(|e| exit_with(e)));

    #[cfg(windows)]
    let (display, media) = {
        use parkscreen_host::windows_media::{EncoderKind, ExistingMonitorDisplay, WindowsMedia};
        let monitor = selector();
        let encoder = opt("--encoder").map_or(EncoderKind::Auto, |e| e.parse().unwrap_or_else(|e| exit_with(e)));
        (
            ExistingMonitorDisplay { monitor: monitor.clone(), match_viewport: flag("--match-viewport") },
            Arc::new(WindowsMedia { monitor, encoder, idr_secs: 10 }),
        )
    };
    #[cfg(not(windows))]
    let (display, media) = (
        parkscreen_host::display::NullDisplay::default(),
        Arc::new(parkscreen_host::rtc_sender::SoftwareMedia::default()),
    );

    let (tx, events) = signalling::spawn(url, identity);
    let mut handler = {
        let gate = allow.clone();
        WebRtcHandler::new(display, media)
            .with_input(Box::new(NullInput), Arc::new(move |car| gate.lock().unwrap().input_allowed(car)))
    };
    if let Some(k) = bitrate_kbps {
        handler.bitrate_kbps = k;
    }
    let mut agent = Agent {
        tx: tx.clone(),
        allow: allow.clone(),
        input_on_pair: flag("--with-input"),
        handler,
        on_pair_code: Box::new(|code| println!("Pairing code (valid 5 min): {code}")),
    };
    if flag("--pair") {
        agent.request_pair_code();
    }
    tokio::select! {
        _ = agent.run(events) => {}
        _ = tokio::signal::ctrl_c() => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bitrates() {
        assert_eq!(parse_bitrate_kbps("12M").unwrap(), 12_000);
        assert_eq!(parse_bitrate_kbps("8000k").unwrap(), 8_000);
        assert_eq!(parse_bitrate_kbps("2.5m").unwrap(), 2_500);
        assert_eq!(parse_bitrate_kbps("5000000").unwrap(), 5_000);
        assert!(parse_bitrate_kbps("fast").is_err());
        assert!(parse_bitrate_kbps("0").is_err());
    }
}
