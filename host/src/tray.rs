//! System tray icon and menu (Windows). Runs on its own thread with a Win32 message pump.
//! Nothing here needs administrator rights: the icon, a user-local mutex and the HKCU Run key.
//!
//! Menu: connection status, "Pair a car…", "Open ParkScreen website", "Start with Windows", "Quit".

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
            DispatchMessageW, MessageBoxW, PeekMessageW, TranslateMessage, MB_ICONINFORMATION, MB_OK, MB_SETFOREGROUND, MSG,
            PM_REMOVE,
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
    if !crate::status::online() {
        return "Offline: reconnecting to the server…".into();
    }
    match crate::updater::live_sessions() {
        0 => "Online: ready for your car".into(),
        1 => "Streaming to 1 car".into(),
        n => format!("Streaming to {n} cars"),
    }
}

/// Starts the tray on its own thread. `request_pair` asks the server for a pairing code; the code
/// arrives through the agent's `on_pair_code` callback.
pub fn spawn(request_pair: impl Fn() + Send + 'static) {
    thread::spawn(move || {
        let menu = Menu::new();
        let status = MenuItem::new(status_text(), false, None);
        let pair = MenuItem::new("Pair a car…", true, None);
        let site = MenuItem::new("Open ParkScreen website", true, None);
        let autostart = CheckMenuItem::new("Start with Windows", true, autostart_enabled(), None);
        let quit = MenuItem::new("Quit ParkScreen", true, None);
        let _ = menu.append_items(&[&status, &PredefinedMenuItem::separator(), &pair, &site, &autostart, &PredefinedMenuItem::separator(), &quit]);
        let Ok(tray) = TrayIconBuilder::new().with_menu(Box::new(menu)).with_tooltip("ParkScreen").with_icon(icon()).build() else {
            tracing::warn!("could not create the tray icon");
            return;
        };
        let (pair_id, site_id, auto_id, quit_id) = (pair.id().clone(), site.id().clone(), autostart.id().clone(), quit.id().clone());
        let mut last = String::new();
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
                } else if ev.id == site_id {
                    open_site();
                } else if ev.id == auto_id {
                    set_autostart(autostart.is_checked());
                } else if ev.id == quit_id {
                    std::process::exit(0);
                }
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
