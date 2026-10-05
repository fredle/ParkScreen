#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

#[cfg(not(windows))]
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
parkscreen-host [--pair] [--no-tray] [--with-input] [--bitrate 12M]
                [--display duplicate|extend] [--monitor auto|<index>|<name>] [--encoder auto|hardware|software] [--match-viewport]
parkscreen-host list
parkscreen-host set-mode --monitor <index>|<name> 1920x1200[@60]
parkscreen-host driver check|install   (display driver: look for it, or install/update it)

On Windows the agent streams the screen chosen in the tray menu (duplicate the main monitor, or
extend onto a ParkScreen virtual display); `--display duplicate|extend` sets that choice, and
`--monitor` streams a specific monitor instead. Elsewhere it streams a test pattern.
`--match-viewport` switches a duplicated monitor to the car's screen size when it connects. RUST_LOG=parkscreen_host=debug for more logging.";

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
        None => Selector::Follow,
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
        #[cfg(windows)]
        Some("driver") => {
            use parkscreen_host::driver_setup as ds;
            match args.get(1).map(String::as_str) {
                Some("check") => match ds::check() {
                    Err(e) => exit_with(e),
                    Ok(None) => println!("The display driver is up to date{}.", ds::installed_version().map(|v| format!(" ({v})")).unwrap_or_default()),
                    Ok(Some(m)) => println!("Available: display driver {} (installed: {}).", m.version, ds::installed_version().unwrap_or_else(|| "none".into())),
                },
                Some("install") => {
                    let m = ds::fetch_manifest().unwrap_or_else(|e| exit_with(e));
                    println!("Installing display driver {} (Windows will ask for administrator permission)...", m.version);
                    match ds::install(&m) {
                        Ok(msg) => println!("{msg}"),
                        Err(e) => exit_with(e),
                    }
                }
                _ => exit_with("usage: parkscreen-host driver check|install"),
            }
            true
        }
        _ => false,
    }
}

/// A windows-subsystem exe has no console. When started from a terminal, borrow the parent's so
/// `--help`, `--pair`, `list` and `set-mode` still print.
#[cfg(windows)]
fn attach_console() {
    use windows::{
        core::w,
        Win32::{
            Foundation::GENERIC_WRITE,
            Storage::FileSystem::{CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_WRITE, OPEN_EXISTING},
            System::Console::{AttachConsole, GetStdHandle, SetStdHandle, ATTACH_PARENT_PROCESS, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE},
        },
    };
    unsafe {
        // Output already redirected (a pipe or file): keep it.
        if matches!(GetStdHandle(STD_OUTPUT_HANDLE), Ok(h) if !h.is_invalid()) {
            return;
        }
        if AttachConsole(ATTACH_PARENT_PROCESS).is_err() {
            return;
        }
        if let Ok(h) = CreateFileW(w!("CONOUT$"), GENERIC_WRITE.0, FILE_SHARE_WRITE, None, OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL, None) {
            let _ = SetStdHandle(STD_OUTPUT_HANDLE, h);
            let _ = SetStdHandle(STD_ERROR_HANDLE, h);
        }
    }
}

#[tokio::main]
async fn main() {
    #[cfg(windows)]
    attach_console();
    // Velopack runs the app with --veloapp-install/-updated/-obsolete/-uninstall and waits 30 s
    // for it to exit. There is nothing to do in those hooks, so leave straight away.
    if std::env::args().any(|a| a.starts_with("--veloapp-")) {
        // Let Velopack handle the hook properly when installed; exit either way.
        velopack::VelopackApp::build().run();
        return;
    }
    velopack::VelopackApp::build().run();
    if flag("--help") || flag("-h") {
        println!("{USAGE}");
        return;
    }
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::from_default_env().add_directive("info".parse().unwrap())).init();
    init_dpi();
    if subcommand() {
        return;
    }
    // Windows: run as a tray app unless a terminal flag asks for console output.
    #[cfg(windows)]
    let tray_mode = !flag("--pair") && !flag("--no-tray");
    #[cfg(not(windows))]
    let tray_mode = false;
    #[cfg(windows)]
    if tray_mode && !parkscreen_host::tray::single_instance() {
        return;
    }
    parkscreen_host::updater::spawn();

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

    parkscreen_host::settings::init(&dir);
    if let Some(m) = opt("--display") {
        parkscreen_host::settings::set_display_mode(m.parse().unwrap_or_else(|e| exit_with(e)));
    }
    #[cfg(windows)]
    parkscreen_host::settings::set_driver_present(parkscreen_host::idd::driver_installed());

    let bitrate_kbps = opt("--bitrate").map(|b| parse_bitrate_kbps(&b).unwrap_or_else(|e| exit_with(e)));

    #[cfg(windows)]
    let (display, media) = {
        use parkscreen_host::windows_media::{EncoderKind, ExistingMonitorDisplay, SwitchableDisplay, WindowsMedia};
        let monitor = selector();
        let encoder = opt("--encoder").map_or(EncoderKind::Auto, |e| e.parse().unwrap_or_else(|e| exit_with(e)));
        (
            SwitchableDisplay {
                duplicate: ExistingMonitorDisplay { monitor: monitor.clone(), match_viewport: flag("--match-viewport") },
                extend: Default::default(),
            },
            Arc::new(WindowsMedia { monitor, encoder, idr_secs: 10 }),
        )
    };
    #[cfg(not(windows))]
    let (display, media) = (
        parkscreen_host::display::NullDisplay::default(),
        Arc::new(parkscreen_host::rtc_sender::SoftwareMedia::default()),
    );

    #[cfg(windows)]
    let injector: Box<dyn parkscreen_host::input::InputInjector> =
        Box::new(parkscreen_host::input_windows::WindowsTouch::new(selector()));
    #[cfg(not(windows))]
    let injector: Box<dyn parkscreen_host::input::InputInjector> = Box::new(NullInput);

    let (tx, events) = signalling::spawn(url, identity);
    let mut handler = {
        let gate = allow.clone();
        WebRtcHandler::new(display, media)
            .with_input(
                injector,
                // The tray's "Allow touch" switch covers every paired car; `--with-input` and an
                // `input` flag in cars.txt still enable single cars.
                Arc::new(move |car| {
                    let al = gate.lock().unwrap();
                    al.input_allowed(car) || (parkscreen_host::settings::touch_enabled() && al.allows(car))
                }),
            )
    };
    if let Some(k) = bitrate_kbps {
        handler.bitrate_kbps = k;
    }
    let mut agent = Agent {
        tx: tx.clone(),
        allow: allow.clone(),
        input_on_pair: flag("--with-input"),
        handler,
        on_pair_code: Box::new(move |code| {
            #[cfg(windows)]
            if tray_mode {
                parkscreen_host::tray::message(
                    "ParkScreen",
                    &format!("Pairing code: {code}\n\nIn your car's browser open parkscreen.web.app/car and enter this code. It is valid for 5 minutes."),
                );
                return;
            }
            println!("Pairing code (valid 5 min): {code}")
        }),
    };
    #[cfg(windows)]
    if tray_mode {
        let tx = tx.clone();
        parkscreen_host::tray::spawn(move || tx.send(protocol::HostToServer::PairStart));
    }
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
