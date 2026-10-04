//! System tray icon and menu (Windows). Runs on its own thread with a Win32 message pump.
//! Nothing here needs administrator rights: the icon, a user-local mutex and the HKCU Run key.
//!
//! Menu: connection status, "Pair a car…", the screen mode (duplicate or extend), "Open ParkScreen website",
//! "Check for updates", "Start with Windows", "Quit".

use std::{os::windows::process::CommandExt, process::Command, thread, time::Duration};
use tray_icon::{
    menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem},
    Icon, TrayIconBuilder,
};
use windows::{
    core::HSTRING,
    Win32::{
        Foundation::{GetLastError, ERROR_ALREADY_EXISTS},
        System::Threading::CreateMutexW,
        UI::WindowsAndMessaging::{
            DispatchMessageW, MessageBoxW, PeekMessageW, TranslateMessage, IDYES, MB_ICONINFORMATION, MB_ICONQUESTION, MB_OK,
            MB_SETFOREGROUND, MB_YESNO, MSG, PM_REMOVE,
        },
    },
};

const SITE: &str = "https://parkscreen.web.app/";
const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// True when this is the only running agent. The mutex lives for the life of the process.
pub fn single_instance() -> bool {
    unsafe {
        let _ = CreateMutexW(None, false, &HSTRING::from("Local\\ParkScreenHostAgent"));
        GetLastError() != ERROR_ALREADY_EXISTS
    }
}

/// Show a message box on its own thread so neither the async runtime nor the tray blocks.
pub fn message(title: &str, text: &str) {
    let (title, text) = (HSTRING::from(title), HSTRING::from(text));
    thread::spawn(move || unsafe {
        MessageBoxW(None, &text, &title, MB_OK | MB_ICONINFORMATION | MB_SETFOREGROUND);
    });
}

/// Yes/No question. Blocks, so call it from a worker thread, never the tray loop.
pub fn confirm(title: &str, text: &str) -> bool {
    unsafe { MessageBoxW(None, &HSTRING::from(text), &HSTRING::from(title), MB_YESNO | MB_ICONQUESTION | MB_SETFOREGROUND) == IDYES }
}

/// Install or update the display driver on a worker thread (download, one UAC prompt).
fn driver_action() {
    use std::sync::atomic::{AtomicBool, Ordering};
    static BUSY: AtomicBool = AtomicBool::new(false);
    if BUSY.swap(true, Ordering::SeqCst) {
        return;
    }
    thread::spawn(|| {
        match crate::driver_setup::check() {
            Err(e) => message("ParkScreen display driver", &format!("Could not look for the display driver: {e}")),
            Ok(None) => message("ParkScreen display driver", "Your display driver is up to date."),
            Ok(Some(m)) => {
                let verb = if crate::settings::driver_present() { "Update" } else { "Install" };
                let signed = if m.test_signed {
                    "\n\nThis is a test-signed build: Windows only loads it when test-signing mode is on."
                } else {
                    ""
                };
                let ask = format!("{verb} the ParkScreen display driver (version {})?\n\nWindows will ask for administrator permission once.{signed}", m.version);
                if confirm("ParkScreen display driver", &ask) {
                    match crate::driver_setup::install(&m) {
                        Ok(msg) => message("ParkScreen display driver", &msg),
                        Err(e) => message("ParkScreen display driver", &e),
                    }
                }
            }
        }
        BUSY.store(false, Ordering::SeqCst);
    });
}

/// 32x32 icon drawn in code: a blue rounded square with a white monitor.
fn icon() -> Icon {
    const N: u32 = 32;
    let mut px = vec![0u8; (N * N * 4) as usize];
    let mut put = |x: u32, y: u32, c: [u8; 4]| {
        let i = ((y * N + x) * 4) as usize;
        px[i..i + 4].copy_from_slice(&c);
    };
    for y in 0..N {
        for x in 0..N {
            let (dx, dy) = (x.min(N - 1 - x) as i32, y.min(N - 1 - y) as i32);
            let outside_corner = dx < 5 && dy < 5 && (5 - dx) * (5 - dx) + (5 - dy) * (5 - dy) > 25;
            if !outside_corner {
                put(x, y, [0x2a, 0x6d, 0xf4, 255]);
            }
        }
    }
    let white = [255, 255, 255, 255];
    for y in 8..21 {
        for x in 6..26 {
            if !(8..24).contains(&x) || !(10..19).contains(&y) {
                put(x, y, white);
            }
        }
    }
    for x in 12..20 {
        put(x, 23, white);
        put(x, 24, white);
    }
    for y in 21..23 {
        put(15, y, white);
        put(16, y, white);
    }
    Icon::from_rgba(px, N, N).expect("icon")
}

fn reg(args: &[&str]) -> bool {
    Command::new("reg").args(args).creation_flags(CREATE_NO_WINDOW).output().map(|o| o.status.success()).unwrap_or(false)
}

fn autostart_enabled() -> bool {
    reg(&["query", RUN_KEY, "/v", "ParkScreen"])
}

fn set_autostart(on: bool) {
    if on {
        if let Ok(exe) = std::env::current_exe() {
            reg(&["add", RUN_KEY, "/v", "ParkScreen", "/t", "REG_SZ", "/d", &format!("\"{}\"", exe.display()), "/f"]);
        }
    } else {
        reg(&["delete", RUN_KEY, "/v", "ParkScreen", "/f"]);
    }
}

fn open_site() {
    let _ = Command::new("explorer").arg(SITE).creation_flags(CREATE_NO_WINDOW).spawn();
}

fn status_text() -> String {
    if crate::status::paused() {
        return "Paused: cars are disconnected".into();
    }
    if !crate::status::online() {
        return "Offline: reconnecting to the server…".into();
    }
    match crate::updater::live_sessions() {
        0 => "Online: ready for your car".into(),
        1 => "Streaming to 1 car".into(),
        n => format!("Streaming to {n} cars"),
    }
}

fn driver_label() -> &'static str {
    if !crate::settings::driver_present() {
        "Install display driver (needs admin)…"
    } else if crate::driver_setup::update_available() {
        "Update display driver…"
    } else {
        "Check for display driver update…"
    }
}

const EXTEND_LABEL: &str = "Extend: car is a second screen";
const EXTEND_MISSING: &str = "Extend: car is a second screen (needs the display driver)";

/// Starts the tray on its own thread. `request_pair` asks the server for a pairing code; the code
/// arrives through the agent's `on_pair_code` callback.
pub fn spawn(request_pair: impl Fn() + Send + 'static) {
    crate::driver_setup::spawn_update_checker();
    thread::spawn(move || {
        let menu = Menu::new();
        let status = MenuItem::new(status_text(), false, None);
        let pair = MenuItem::new("Pair a car…", true, None);
        let dup = CheckMenuItem::new("Duplicate: car shows your main screen", true, true, None);
        let ext = CheckMenuItem::new(EXTEND_LABEL, true, false, None);
        let pause = CheckMenuItem::new("Pause streaming (disconnect cars)", true, false, None);
        let reset = MenuItem::new("Reset connection", true, None);
        let driver_item = MenuItem::new(driver_label(), true, None);
        let site = MenuItem::new("Open ParkScreen website", true, None);
        let update = MenuItem::new("Check for updates", true, None);
        let autostart = CheckMenuItem::new("Start with Windows", true, autostart_enabled(), None);
        let quit = MenuItem::new("Quit ParkScreen", true, None);
        let _ = menu.append_items(&[&status, &PredefinedMenuItem::separator(), &pair, &pause, &reset, &PredefinedMenuItem::separator(), &dup, &ext, &driver_item, &PredefinedMenuItem::separator(), &site, &update, &autostart, &PredefinedMenuItem::separator(), &quit]);
        let Ok(tray) = TrayIconBuilder::new().with_menu(Box::new(menu)).with_tooltip("ParkScreen").with_icon(Icon::from_resource(1, Some((32, 32))).unwrap_or_else(|_| icon())).build() else {
            tracing::warn!("could not create the tray icon");
            return;
        };
        let (pair_id, site_id, update_id, auto_id, quit_id) =
            (pair.id().clone(), site.id().clone(), update.id().clone(), autostart.id().clone(), quit.id().clone());
        let (pause_id, reset_id, driver_id) = (pause.id().clone(), reset.id().clone(), driver_item.id().clone());
        let (dup_id, ext_id) = (dup.id().clone(), ext.id().clone());
        let sync_mode = |driver: bool| {
            let extend = crate::settings::chosen_mode() == crate::settings::DisplayMode::Extend && driver;
            dup.set_checked(!extend);
            ext.set_checked(extend);
            ext.set_enabled(driver);
            ext.set_text(if driver { EXTEND_LABEL } else { EXTEND_MISSING });
        };
        let mut driver = crate::settings::driver_present();
        sync_mode(driver);
        let mut last = String::new();
        let mut last_driver_label = driver_label();
        let mut last_check = std::time::Instant::now();
        loop {
            unsafe {
                let mut msg = MSG::default();
                while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
            while let Ok(ev) = MenuEvent::receiver().try_recv() {
                if ev.id == pair_id {
                    request_pair();
                } else if ev.id == dup_id || ev.id == ext_id {
                    let mode = if ev.id == ext_id { crate::settings::DisplayMode::Extend } else { crate::settings::DisplayMode::Duplicate };
                    crate::settings::set_display_mode(mode);
                    sync_mode(driver);
                    if crate::updater::live_sessions() > 0 {
                        message("ParkScreen", "The new screen mode applies the next time a car connects.");
                    }
                } else if ev.id == pause_id {
                    crate::status::set_paused(pause.is_checked());
                } else if ev.id == reset_id {
                    // Drop every stream and sign in to the server again; cars reconnect by themselves.
                    pause.set_checked(false);
                    crate::status::set_paused(false);
                    crate::status::request_close_sessions();
                    crate::status::request_reconnect();
                } else if ev.id == driver_id {
                    driver_action();
                } else if ev.id == site_id {
                    open_site();
                } else if ev.id == update_id {
                    crate::updater::check_now(|text| message("ParkScreen updates", text));
                } else if ev.id == auto_id {
                    set_autostart(autostart.is_checked());
                } else if ev.id == quit_id {
                    std::process::exit(0);
                }
            }
            if last_check.elapsed() > Duration::from_secs(5) {
                last_check = std::time::Instant::now();
                let now = crate::idd::driver_installed();
                crate::settings::set_driver_present(now);
                if now != driver {
                    driver = now;
                    sync_mode(driver);
                }
            }
            let label = driver_label();
            if label != last_driver_label {
                driver_item.set_text(label);
                last_driver_label = label;
            }
            let text = status_text();
            if text != last {
                status.set_text(&text);
                let _ = tray.set_tooltip(Some(format!("ParkScreen: {text}")));
                last = text;
            }
            thread::sleep(Duration::from_millis(100));
        }
    });
}
