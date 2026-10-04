//! ParkScreen host agent. Platform-independent core; Windows-only backends
//! (IddCx display, WGC capture, Media Foundation encode, pointer injection)
//! plug in through the traits in `display`, `capture`, `encode` and `input`.

pub mod adaptive;
pub mod agent;
pub mod allowlist;
pub mod capture;
pub mod display;
pub mod encode;
pub mod identity;
pub mod input;
pub mod rtc_sender;
pub mod signalling;
pub mod status;
pub mod updater;
#[cfg(windows)]
pub mod tray;
pub mod convert;
pub mod cursor;
pub mod monitors;
pub mod idd;
pub mod settings;

#[cfg(windows)]
pub mod capture_dxgi;
#[cfg(windows)]
pub mod encode_mf;
#[cfg(windows)]
pub mod input_windows;
#[cfg(windows)]
pub mod windows_media;
