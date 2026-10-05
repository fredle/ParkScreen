//! User settings shared between the tray menu and the streaming code.

use std::{
    path::PathBuf,
    str::FromStr,
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
};

/// How the car's screen relates to the PC's desktop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayMode {
    /// The car shows a copy of the main monitor.
    Duplicate,
    /// The car is a separate screen added to the desktop (needs the ParkScreen display driver).
    Extend,
}

impl DisplayMode {
    pub fn as_str(self) -> &'static str {
        match self {
            DisplayMode::Duplicate => "duplicate",
            DisplayMode::Extend => "extend",
        }
    }
}

impl FromStr for DisplayMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "duplicate" | "mirror" => Ok(Self::Duplicate),
            "extend" | "extension" => Ok(Self::Extend),
            other => Err(format!("unknown display mode '{other}' (duplicate or extend)")),
        }
    }
}

static FILE: Mutex<Option<PathBuf>> = Mutex::new(None);
static MODE_EXTEND: AtomicBool = AtomicBool::new(false);
static DRIVER_PRESENT: AtomicBool = AtomicBool::new(false);
static TOUCH_FILE: Mutex<Option<PathBuf>> = Mutex::new(None);
static TOUCH_ON: AtomicBool = AtomicBool::new(false);

/// Load the saved choice from `dir/display-mode.txt`. Duplicate is the default until the user picks.
pub fn init(dir: &std::path::Path) {
    let file = dir.join("display-mode.txt");
    if let Ok(text) = std::fs::read_to_string(&file) {
        if let Ok(m) = text.parse::<DisplayMode>() {
            MODE_EXTEND.store(m == DisplayMode::Extend, Ordering::Relaxed);
        }
    }
    *FILE.lock().unwrap() = Some(file);

    let touch = dir.join("touch.txt");
    if let Ok(text) = std::fs::read_to_string(&touch) {
        TOUCH_ON.store(text.trim().eq_ignore_ascii_case("on"), Ordering::Relaxed);
    }
    *TOUCH_FILE.lock().unwrap() = Some(touch);
}

/// Whether paired cars may control this PC by touch (the tray's "Allow touch" switch). Off until
/// the user turns it on, and remembered between runs.
pub fn touch_enabled() -> bool {
    TOUCH_ON.load(Ordering::Relaxed)
}

pub fn set_touch_enabled(on: bool) {
    TOUCH_ON.store(on, Ordering::Relaxed);
    if let Some(file) = TOUCH_FILE.lock().unwrap().as_ref() {
        if let Some(parent) = file.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = std::fs::write(file, if on { "on" } else { "off" }) {
            tracing::warn!("could not save the touch setting: {e}");
        }
    }
}

/// The mode the user chose.
pub fn chosen_mode() -> DisplayMode {
    if MODE_EXTEND.load(Ordering::Relaxed) {
        DisplayMode::Extend
    } else {
        DisplayMode::Duplicate
    }
}

/// The mode actually used: a chosen `Extend` falls back to `Duplicate` while the driver is missing.
pub fn display_mode() -> DisplayMode {
    effective(chosen_mode(), driver_present())
}

pub fn effective(chosen: DisplayMode, driver: bool) -> DisplayMode {
    if chosen == DisplayMode::Extend && !driver {
        DisplayMode::Duplicate
    } else {
        chosen
    }
}

/// Save the user's choice.
pub fn set_display_mode(mode: DisplayMode) {
    MODE_EXTEND.store(mode == DisplayMode::Extend, Ordering::Relaxed);
    if let Some(file) = FILE.lock().unwrap().as_ref() {
        if let Some(parent) = file.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = std::fs::write(file, mode.as_str()) {
            tracing::warn!("could not save the display mode: {e}");
        }
    }
}

pub fn driver_present() -> bool {
    DRIVER_PRESENT.load(Ordering::Relaxed)
}

pub fn set_driver_present(v: bool) {
    DRIVER_PRESENT.store(v, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn touch_is_off_by_default_and_remembered() {
        let dir = std::env::temp_dir().join(format!("ps-touch-{}", rand::random::<u32>()));
        init(&dir);
        assert!(!touch_enabled());
        set_touch_enabled(true);
        assert!(std::fs::read_to_string(dir.join("touch.txt")).unwrap() == "on");
        TOUCH_ON.store(false, Ordering::Relaxed);
        init(&dir);
        assert!(touch_enabled());
        set_touch_enabled(false);
        assert!(!touch_enabled());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn parses_modes() {
        assert_eq!("Extend".parse::<DisplayMode>().unwrap(), DisplayMode::Extend);
        assert_eq!("mirror".parse::<DisplayMode>().unwrap(), DisplayMode::Duplicate);
        assert!("clone".parse::<DisplayMode>().is_err());
    }

    #[test]
    fn extend_needs_the_driver() {
        assert_eq!(effective(DisplayMode::Extend, false), DisplayMode::Duplicate);
        assert_eq!(effective(DisplayMode::Extend, true), DisplayMode::Extend);
        assert_eq!(effective(DisplayMode::Duplicate, true), DisplayMode::Duplicate);
    }
}
