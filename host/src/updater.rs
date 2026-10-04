//! Automatic updates (Velopack). Checks the release feed shortly after start and every few hours,
//! downloads a newer version in the background and restarts into it once no car is streaming.
//! Does nothing when the app was not installed by Velopack (cargo run, tests).

use std::{
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    thread,
    time::Duration,
};
use tracing::{info, warn};
use velopack::{sources::HttpSource, UpdateCheck, UpdateManager};

/// Where `release.yml` publishes the feed.
const FEED_URL: &str = "https://parkscreen-releases.web.app/";
const FIRST_CHECK: Duration = Duration::from_secs(60);
const CHECK_EVERY: Duration = Duration::from_secs(4 * 3600);
const IDLE_POLL: Duration = Duration::from_secs(30);

static LIVE_SESSIONS: AtomicUsize = AtomicUsize::new(0);
/// Set while a check/download is running so the timer and the tray menu never overlap.
static CHECKING: AtomicBool = AtomicBool::new(false);

/// Called by the WebRTC handler whenever the number of streaming cars changes.
pub fn set_live_sessions(n: usize) {
    LIVE_SESSIONS.store(n, Ordering::Relaxed);
}

/// Cars currently streaming.
pub fn live_sessions() -> usize {
    LIVE_SESSIONS.load(Ordering::Relaxed)
}

fn manager() -> Result<UpdateManager, velopack::Error> {
    let url = std::env::var("PARKSCREEN_UPDATE_URL").unwrap_or_else(|_| FEED_URL.to_string());
    UpdateManager::new(HttpSource::new(url), None, None)
}

pub fn spawn() {
    let Ok(um) = manager() else {
        info!("not installed by Velopack: automatic updates off");
        return;
    };
    info!("version {}: automatic updates on", um.get_current_version_as_string());
    thread::spawn(move || {
        thread::sleep(FIRST_CHECK);
        loop {
            if !CHECKING.swap(true, Ordering::SeqCst) {
                if let Err(e) = check_once(&um, &|_| {}) {
                    warn!("update check failed: {e}");
                }
                CHECKING.store(false, Ordering::SeqCst);
            }
            thread::sleep(CHECK_EVERY);
        }
    });
}

/// Manual check from the tray menu. Runs in the background and reports the outcome through `notify`.
pub fn check_now(notify: impl Fn(&str) + Send + 'static) {
    thread::spawn(move || {
        if CHECKING.swap(true, Ordering::SeqCst) {
            notify("An update check is already running. Try again in a moment.");
            return;
        }
        match manager() {
            Err(_) => notify("Updates are only available in the installed version of ParkScreen."),
            Ok(um) => {
                if let Err(e) = check_once(&um, &notify) {
                    warn!("update check failed: {e}");
                    notify(&format!("Could not check for updates: {e}"));
                }
            }
        }
        CHECKING.store(false, Ordering::SeqCst);
    });
}

fn check_once(um: &UpdateManager, notify: &dyn Fn(&str)) -> Result<(), velopack::Error> {
    // A previous check may have downloaded an update that has not been applied yet.
    let pending = um.get_update_pending_restart();
    let asset = match pending {
        Some(a) => a,
        None => match um.check_for_updates()? {
            UpdateCheck::UpdateAvailable(info) => {
                info!("update {} available, downloading", info.TargetFullRelease.Version);
                um.download_updates(&info, None)?;
                info.TargetFullRelease.clone()
            }
            _ => {
                notify(&format!("ParkScreen {} is up to date.", um.get_current_version_as_string()));
                return Ok(());
            }
        },
    };
    if LIVE_SESSIONS.load(Ordering::Relaxed) > 0 {
        notify(&format!("ParkScreen {} is downloaded and will install when no car is streaming.", asset.Version));
    } else {
        notify(&format!("Installing ParkScreen {} and restarting…", asset.Version));
    }
    while LIVE_SESSIONS.load(Ordering::Relaxed) > 0 {
        thread::sleep(IDLE_POLL);
    }
    info!("restarting into {}", asset.Version);
    um.apply_updates_and_restart(&asset)
}
