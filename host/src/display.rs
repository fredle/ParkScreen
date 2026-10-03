use async_trait::async_trait;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Mode {
    pub width: u32,
    pub height: u32,
    pub refresh_hz: u32,
}

/// Virtual monitor control. Implementations: `ParkScreenIdd` (our driver, Phase 2),
/// `ThirdPartyVdd` (Phase 1, drives an existing signed IddCx driver). Both Windows-only.
#[async_trait]
pub trait DisplayBackend: Send {
    /// Add the monitor if needed and switch it to `mode`.
    async fn plug(&mut self, mode: Mode) -> Result<(), String>;
    async fn unplug(&mut self) -> Result<(), String>;
}

/// Used on non-Windows hosts and in tests.
#[derive(Default)]
pub struct NullDisplay {
    pub current: Option<Mode>,
}

#[async_trait]
impl DisplayBackend for NullDisplay {
    async fn plug(&mut self, mode: Mode) -> Result<(), String> {
        self.current = Some(mode);
        Ok(())
    }
    async fn unplug(&mut self) -> Result<(), String> {
        self.current = None;
        Ok(())
    }
}
