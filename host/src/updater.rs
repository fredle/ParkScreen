//! Automatic updates (Velopack). Checks the release feed shortly after start and every few hours,
//! downloads a newer version in the background and restarts into it once no car is streaming.
//! Does nothing when the app was not installed by Velopack (cargo run, tests).

use std::{
    sync::atomic::{AtomicUsize, Ordering},
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

/// Called by the WebRTC handler whenever the number of streaming cars changes.
pub fn set_live_sessions(n: usize) {
    LIVE_SESSIONS.store(n, Ordering::Relaxed);
}

/// Cars currently streaming.
pub fn live_sessions() -> usize {
    LIVE_SESSIONS.load(Ordering::Relaxed)
}

pub fn spawn() {
    let url = std::env::var("PARKSCREEN_UPDATE_URL").unwrap_or_else(|_| FEED_URL.to_string());
    let Ok(um) = UpdateManager::new(HttpSource::new(url), None, None) else {
        info!("not installed by Velopack: automatic updates off");
        return;
    };
    info!("version {}: automatic updates on", um.get_current_version_as_string());
    thread::spawn(move || {
        thread::sleep(FIRST_CHECK);
        loop {
            if let Err(e) = check_once(&um) {
                warn!("update check failed: {e}");
            }
            thread::sleep(CHECK_EVERY);
        }
    });
}

fn check_once(um: &UpdateManager) -> Result<(), velopack::Error> {
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
            _ => return Ok(()),
        },
    };
    while LIVE_SESSIONS.load(Ordering::Relaxed) > 0 {
        thread::sleep(IDLE_POLL);
    }
    info!("restarting into {}", asset.Version);
    um.apply_updates_and_restart(&asset)
}
