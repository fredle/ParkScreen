//! Connection state shared with the tray menu.

use std::sync::atomic::{AtomicBool, Ordering};

static ONLINE: AtomicBool = AtomicBool::new(false);

pub fn set_online(v: bool) {
    ONLINE.store(v, Ordering::Relaxed);
}

pub fn online() -> bool {
    ONLINE.load(Ordering::Relaxed)
}
