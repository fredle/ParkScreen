//! H.264 encoding behind an [`Encoder`] trait so a hardware encoder can replace the
//! software one later (roadmap W2).

use crate::capture::Frame;
use crate::convert;
use anyhow::{Context, Result};
use openh264::encoder::{
    BitRate, Complexity, Encoder as H264, EncoderConfig, FrameRate, IntraFramePeriod, Profile,
    RateControlMode, UsageType, VuiConfig,
};
use openh264::formats::YUVSlices;
use openh264::OpenH264API;

#[derive(Debug, Clone, Copy)]
pub struct EncoderSettings {
    pub fps: u32,
    pub bitrate_bps: u32,
}

/// An H.264 encoder producing Annex-B access units.
pub trait Encoder {
    /// Encode one frame. Returns `None` if the encoder chose to skip it.
    /// The first frame after a size change, or after `force_keyframe`, is an IDR
    /// with SPS/PPS in front.
    fn encode(&mut self, frame: &Frame) -> Result<Option<Vec<u8>>>;
    fn force_keyframe(&mut self);
}

pub struct OpenH264Encoder {
    settings: EncoderSettings,
    inner: Option<H264>,
    size: (usize, usize),
    planes: (Vec<u8>, Vec<u8>, Vec<u8>),
    want_idr: bool,
}

impl OpenH264Encoder {
    pub fn new(settings: EncoderSettings) -> Self {
        Self { settings, inner: None, size: (0, 0), planes: Default::default(), want_idr: true }
    }

    fn build(&self) -> Result<H264> {
        let cfg = EncoderConfig::new()
            // ScreenContentRealTime costs ~25 ms per 1920x1200 frame in software (4x the camera
            // mode), which cannot hold 60 fps; camera mode at 12 Mbps is still sharp for desktops.
            .usage_type(UsageType::CameraVideoRealTime)
            .bitrate(BitRate::from_bps(self.settings.bitrate_bps))
            .max_frame_rate(FrameRate::from_hz(self.settings.fps as f32))
            .rate_control_mode(RateControlMode::Bitrate)
            .profile(Profile::Baseline)
            .complexity(Complexity::Low)
            .scene_change_detect(false)
            .background_detection(false)
            .adaptive_quantization(false)
            .long_term_reference(false)
            .vui(VuiConfig::bt709())
            .skip_frames(true)
            // A keyframe every ~10 s as insurance; PLI covers real losses.
            .intra_frame_period(IntraFramePeriod::from_num_frames(self.settings.fps * 10));
        H264::with_api_config(OpenH264API::from_source(), cfg).context("creating OpenH264 encoder")
    }
}

impl Encoder for OpenH264Encoder {
    fn encode(&mut self, frame: &Frame) -> Result<Option<Vec<u8>>> {
        // H.264 4:2:0 needs even dimensions; crop one pixel if odd.
        let w = frame.width as usize & !1;
        let h = frame.height as usize & !1;
        if self.inner.is_none() || self.size != (w, h) {
            self.inner = Some(self.build()?);
            self.planes = (vec![0; w * h], vec![0; w * h / 4], vec![0; w * h / 4]);
            self.size = (w, h);
            self.want_idr = true;
        }
        let enc = self.inner.as_mut().unwrap();
        let (y, u, v) = &mut self.planes;
        convert::bgra_to_i420(&frame.bgra, frame.width as usize * 4, w, h, y, u, v);
        let yuv = YUVSlices::new((y, u, v), (w, h), (w, w / 2, w / 2));

        if std::mem::take(&mut self.want_idr) {
            enc.force_intra_frame();
        }
        let bits = enc.encode(&yuv).context("OpenH264 encode")?;
        let data = bits.to_vec();
        Ok(if data.is_empty() { None } else { Some(data) })
    }

    fn force_keyframe(&mut self) {
        self.want_idr = true;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum EncoderKind {
    /// Hardware if available, otherwise software.
    Auto,
    Hardware,
    Software,
}

/// Uses `primary` until it fails, then switches to OpenH264 for good.
struct Fallback {
    primary: Option<Box<dyn Encoder>>,
    software: OpenH264Encoder,
}

impl Encoder for Fallback {
    fn encode(&mut self, frame: &Frame) -> Result<Option<Vec<u8>>> {
        if let Some(p) = self.primary.as_mut() {
            match p.encode(frame) {
                Ok(r) => return Ok(r),
                Err(e) => {
                    tracing::warn!("hardware encoder failed ({e:#}); switching to software");
                    self.primary = None;
                    self.software.force_keyframe();
                }
            }
        }
        self.software.encode(frame)
    }

    fn force_keyframe(&mut self) {
        if let Some(p) = self.primary.as_mut() {
            p.force_keyframe();
        }
        self.software.force_keyframe();
    }
}

/// Build the encoder for `kind`. `width`/`height` are the initial capture size, used to
/// check up front that a hardware encoder works.
pub fn create(kind: EncoderKind, settings: EncoderSettings, width: u32, height: u32) -> Result<Box<dyn Encoder>> {
    let software = OpenH264Encoder::new(settings);
    if kind == EncoderKind::Software {
        return Ok(Box::new(software));
    }
    #[cfg(windows)]
    match crate::encode_mf::MfEncoder::probe(settings, width, height) {
        Ok(hw) => return Ok(Box::new(Fallback { primary: Some(Box::new(hw)), software })),
        Err(e) if kind == EncoderKind::Hardware => return Err(e.context("hardware encoder requested")),
        Err(e) => tracing::info!("no usable hardware encoder ({e:#}); using OpenH264"),
    }
    let _ = (width, height);
    if kind == EncoderKind::Hardware {
        anyhow::bail!("hardware encoding is only supported on Windows");
    }
    Ok(Box::new(software))
}

/// NAL unit types in an Annex-B stream.
#[cfg(test)]
pub fn nal_types(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 3 < data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            out.push(data[i + 3] & 0x1f);
            i += 3;
        } else {
            i += 1;
        }
    }
    out
}

/// True if the access unit contains an IDR slice (NAL type 5).
#[cfg(test)]
pub fn contains_idr(data: &[u8]) -> bool {
    nal_types(data).contains(&5)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_nal_types_with_both_start_code_lengths() {
        // 4-byte start code SPS(7), 3-byte PPS(8), 4-byte IDR(5)
        let au = [0, 0, 0, 1, 0x67, 1, 2, 0, 0, 1, 0x68, 3, 0, 0, 0, 1, 0x65, 4, 5];
        assert_eq!(nal_types(&au), vec![7, 8, 5]);
        assert!(contains_idr(&au));
    }

    #[test]
    fn non_idr_is_not_a_keyframe() {
        let au = [0, 0, 0, 1, 0x41, 9, 9, 9];
        assert_eq!(nal_types(&au), vec![1]);
        assert!(!contains_idr(&au));
    }

    #[test]
    fn encodes_a_solid_frame_to_an_idr() {
        let mut enc = OpenH264Encoder::new(EncoderSettings { fps: 30, bitrate_bps: 2_000_000 });
        let frame = Frame { width: 64, height: 64, bgra: vec![128; 64 * 64 * 4] };
        let first = enc.encode(&frame).unwrap().expect("first frame encodes");
        assert!(contains_idr(&first), "first frame must be an IDR");
        assert!(nal_types(&first).contains(&7), "SPS present");
        let second = enc.encode(&frame).unwrap();
        if let Some(s) = second {
            assert!(!contains_idr(&s));
        }
        enc.force_keyframe();
        let third = enc.encode(&frame).unwrap().expect("forced frame encodes");
        assert!(contains_idr(&third));
    }

    #[test]
    fn odd_sizes_are_cropped() {
        let mut enc = OpenH264Encoder::new(EncoderSettings { fps: 30, bitrate_bps: 1_000_000 });
        let frame = Frame { width: 65, height: 63, bgra: vec![10; 65 * 63 * 4] };
        assert!(enc.encode(&frame).unwrap().is_some());
    }
}
