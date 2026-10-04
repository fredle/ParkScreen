//! Windows backends for the host's traits: Desktop Duplication capture, a Media Foundation
//! hardware encoder that falls back to OpenH264, and mode changes on an existing monitor.

use crate::capture::Frame;
use crate::capture_dxgi::DxgiCapture;
use crate::display::{DisplayBackend, Mode};
use crate::encode::{Encoder, EncoderConfig, OpenH264Encoder};
use crate::encode_mf::{self, MfEncoder};
use crate::monitors::{self, MonitorMode, Selector};
use crate::rtc_sender::{MediaFactory, MediaPair};
use async_trait::async_trait;
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncoderKind {
    /// Hardware if available, otherwise software.
    Auto,
    /// Hardware; falls back to software (with a warning) if it cannot start.
    Hardware,
    Software,
}

impl FromStr for EncoderKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        match s.to_ascii_lowercase().as_str() {
            "auto" => Ok(Self::Auto),
            "hardware" | "hw" => Ok(Self::Hardware),
            "software" | "sw" => Ok(Self::Software),
            _ => Err(format!("unknown encoder '{s}' (auto, hardware or software)")),
        }
    }
}

/// Streams one monitor chosen by `monitor`, resolved each time a session starts.
pub struct WindowsMedia {
    pub monitor: Selector,
    pub encoder: EncoderKind,
    pub idr_secs: u32,
}

impl MediaFactory for WindowsMedia {
    fn make(&self, mode: Mode, bitrate_kbps: u32) -> Result<MediaPair, String> {
        let monitors = monitors::enumerate().map_err(|e| format!("{e:#}"))?;
        let m = monitors::select(&monitors, &self.monitor).map_err(|e| format!("{e:#}"))?;
        tracing::info!("capturing monitor {} {} \"{}\" ({}x{})", m.index, m.device_name, m.friendly_name, m.width, m.height);
        let cfg = EncoderConfig { width: mode.width, height: mode.height, fps: mode.refresh_hz, bitrate_kbps, idr_secs: self.idr_secs };
        Ok((Box::new(DxgiCapture::new(m.device_name.clone())), Box::new(HybridEncoder::new(self.encoder, cfg))))
    }
}

/// Media Foundation hardware encoder with OpenH264 as the fallback. Both are created on the
/// first frame, on the encode thread (Media Foundation objects belong to one thread).
pub struct HybridEncoder {
    kind: EncoderKind,
    cfg: EncoderConfig,
    started: bool,
    hw: Option<MfEncoder>,
    sw: Option<OpenH264Encoder>,
    sw_want_idr: bool,
}

// SAFETY: `hw` holds Media Foundation COM objects, which windows-rs does not mark `Send`. It
// is `None` until the first `encode`, which runs on the single thread that owns this value
// from then on, so the COM objects are never touched from two threads.
unsafe impl Send for HybridEncoder {}

impl HybridEncoder {
    pub fn new(kind: EncoderKind, cfg: EncoderConfig) -> Self {
        Self { kind, cfg, started: false, hw: None, sw: None, sw_want_idr: false }
    }

    fn start(&mut self, frame: &Frame) {
        self.started = true;
        if self.kind == EncoderKind::Software {
            return;
        }
        let settings = encode_mf::EncoderSettings { fps: self.cfg.fps, bitrate_bps: self.cfg.bitrate_kbps * 1000 };
        match MfEncoder::probe(settings, frame.width, frame.height) {
            Ok(hw) => self.hw = Some(hw),
            Err(e) if self.kind == EncoderKind::Hardware => {
                tracing::warn!("hardware encoder requested but unavailable ({e:#}); using OpenH264")
            }
            Err(e) => tracing::info!("no usable hardware encoder ({e:#}); using OpenH264"),
        }
    }

    fn software(&mut self) -> Option<&mut OpenH264Encoder> {
        if self.sw.is_none() {
            match OpenH264Encoder::new(self.cfg) {
                Ok(e) => self.sw = Some(e),
                Err(e) => tracing::error!("cannot create OpenH264 encoder: {e}"),
            }
        }
        self.sw.as_mut()
    }
}

impl Encoder for HybridEncoder {
    fn encode(&mut self, frame: &Frame) -> Vec<u8> {
        if !self.started {
            self.start(frame);
        }
        if let Some(hw) = self.hw.as_mut() {
            match hw.encode(frame) {
                Ok(data) => return data.unwrap_or_default(),
                Err(e) => {
                    tracing::warn!("hardware encoder failed ({e:#}); switching to software");
                    self.hw = None;
                    self.sw_want_idr = true;
                }
            }
        }
        let idr = std::mem::take(&mut self.sw_want_idr);
        match self.software() {
            Some(sw) => {
                if idr {
                    sw.request_keyframe();
                }
                sw.encode(frame)
            }
            None => Vec::new(),
        }
    }

    fn request_keyframe(&mut self) {
        match (self.hw.as_mut(), self.sw.as_mut()) {
            (Some(hw), _) => hw.force_keyframe(),
            (None, Some(sw)) => sw.request_keyframe(),
            (None, None) => self.sw_want_idr = true,
        }
    }

    fn set_bitrate(&mut self, kbps: u32) {
        self.cfg.bitrate_kbps = kbps;
        if let Some(hw) = self.hw.as_mut() {
            hw.set_bitrate(kbps * 1000);
        }
        if let Some(sw) = self.sw.as_mut() {
            sw.set_bitrate(kbps);
        }
    }
}

/// Changes the mode of an existing monitor to match the car's viewport. Does nothing unless
/// `match_viewport` is set, because resizing a real monitor is intrusive.
pub struct ExistingMonitorDisplay {
    pub monitor: Selector,
    pub match_viewport: bool,
}

#[async_trait]
impl DisplayBackend for ExistingMonitorDisplay {
    async fn plug(&mut self, mode: Mode) -> Result<(), String> {
        if !self.match_viewport {
            return Ok(());
        }
        let sel = self.monitor.clone();
        tokio::task::spawn_blocking(move || {
            let err = |e: anyhow::Error| format!("{e:#}");
            let monitors = monitors::enumerate().map_err(err)?;
            let m = monitors::select(&monitors, &sel).map_err(err)?;
            if (m.width, m.height, m.hz) == (mode.width, mode.height, mode.refresh_hz) {
                return Ok(());
            }
            let want = MonitorMode { width: mode.width, height: mode.height, hz: Some(mode.refresh_hz) };
            monitors::set_mode(&m.device_name, want).map_err(err)?;
            tracing::info!("monitor {} switched to {want}", m.device_name);
            Ok(())
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn unplug(&mut self) -> Result<(), String> {
        Ok(())
    }
}

/// Follows the tray's duplicate/extend choice each time a car connects: copy the main monitor
/// (`ExistingMonitorDisplay`) or add a ParkScreen monitor (`IddDisplay`).
pub struct SwitchableDisplay {
    pub duplicate: ExistingMonitorDisplay,
    pub extend: crate::idd::IddDisplay,
}

#[async_trait]
impl DisplayBackend for SwitchableDisplay {
    async fn plug(&mut self, mode: Mode) -> Result<(), String> {
        crate::settings::set_driver_present(crate::idd::driver_installed());
        match crate::settings::display_mode() {
            crate::settings::DisplayMode::Extend => self.extend.plug(mode).await,
            crate::settings::DisplayMode::Duplicate => {
                // Drop a monitor left over from an earlier extend session.
                let _ = self.extend.unplug().await;
                self.duplicate.plug(mode).await
            }
        }
    }

    async fn unplug(&mut self) -> Result<(), String> {
        let _ = self.duplicate.unplug().await;
        self.extend.unplug().await
    }
}
