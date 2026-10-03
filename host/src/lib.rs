//! ParkScreen host agent. Platform-independent core; Windows-only backends
//! (IddCx display, WGC capture, Media Foundation encode, pointer injection)
//! plug in through the traits in `display`, `capture`, `encode` and `input`.

pub mod agent;
pub mod allowlist;
pub mod capture;
pub mod display;
pub mod encode;
pub mod identity;
pub mod input;
pub mod rtc_sender;
pub mod signalling;
