//! Connection state shared with the tray menu.

use std::sync::atomic::{AtomicBool, Ordering};

static ONLINE: AtomicBool = AtomicBool::new(false);
static PAUSED: AtomicBool = AtomicBool::new(false);
static CLOSE_SESSIONS: AtomicBool = AtomicBool::new(false);
static RECONNECT: AtomicBool = AtomicBool::new(false);

pub fn set_online(v: bool) {
    ONLINE.store(v, Ordering::Relaxed);
}

pub fn online() -> bool {
    ONLINE.load(Ordering::Relaxed)
}

/// While paused the agent drops every car's stream and ignores new offers.
pub fn set_paused(v: bool) {
    PAUSED.store(v, Ordering::Relaxed);
    if v {
        request_close_sessions();
    }
}

pub fn paused() -> bool {
    PAUSED.load(Ordering::Relaxed)
}

/// Ask the agent to close every live stream (the tray thread sets it, the agent loop takes it).
pub fn request_close_sessions() {
    CLOSE_SESSIONS.store(true, Ordering::Relaxed);
}

pub fn take_close_sessions() -> bool {
    CLOSE_SESSIONS.swap(false, Ordering::Relaxed)
}

/// Ask the signalling client to drop its socket and sign in again.
pub fn request_reconnect() {
    RECONNECT.store(true, Ordering::Relaxed);
}

pub fn take_reconnect() -> bool {
    RECONNECT.swap(false, Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_are_taken_once_and_pause_closes_sessions() {
        assert!(!take_close_sessions());
        set_paused(true);
        assert!(paused());
        assert!(take_close_sessions(), "pausing asks the agent to close every stream");
        assert!(!take_close_sessions());
        set_paused(false);
        assert!(!paused());
        request_reconnect();
        assert!(take_reconnect());
        assert!(!take_reconnect());
    }
}
