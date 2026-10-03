//! H.264 encoding behind an [`Encoder`] trait so a hardware encoder can replace the
//! software one later (roadmap W2).

use crate::capture::Frame;
use anyhow::{Context, Result};
use openh264::encoder::{
    BitRate, Complexity, Encoder as H264, EncoderConfig, FrameRate, IntraFramePeriod, Profile,
    RateControlMode, UsageType,
};
use openh264::formats::{BgraSliceU8, YUVBuffer};
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
    yuv: Option<YUVBuffer>,
    want_idr: bool,
}

impl OpenH264Encoder {
    pub fn new(settings: EncoderSettings) -> Self {
        Self { settings, inner: None, size: (0, 0), yuv: None, want_idr: true }
    }

    fn build(&self, w: usize, h: usize) -> Result<H264> {
        let _ = (w, h); // openh264 reads the size from the first frame
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
            self.inner = Some(self.build(w, h)?);
            self.yuv = Some(YUVBuffer::new(w, h));
            self.size = (w, h);
            self.want_idr = true;
        }
        let enc = self.inner.as_mut().unwrap();
        let yuv = self.yuv.as_mut().unwrap();

        let t_conv = std::time::Instant::now();
        if frame.width as usize == w && frame.height as usize == h {
            yuv.read_bgra8(BgraSliceU8::new(&frame.bgra, (w, h)));
        } else {
            let mut cropped = Vec::with_capacity(w * h * 4);
            for r in 0..h {
                let start = r * frame.width as usize * 4;
                cropped.extend_from_slice(&frame.bgra[start..start + w * 4]);
            }
            yuv.read_bgra8(BgraSliceU8::new(&cropped, (w, h)));
        }

        if std::mem::take(&mut self.want_idr) {
            enc.force_intra_frame();
        }
        let t_enc = std::time::Instant::now();
        let bits = enc.encode(yuv).context("OpenH264 encode")?;
        tracing::trace!("convert {:?} encode {:?}", t_enc - t_conv, t_enc.elapsed());
        let data = bits.to_vec();
        Ok(if data.is_empty() { None } else { Some(data) })
    }

    fn force_keyframe(&mut self) {
        self.want_idr = true;
    }
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

#[cfg(test)]
mod bench {
    use super::*;
    use std::time::Instant;

    #[test]
    #[ignore]
    fn bench_1920x1200() {
        let (w, h) = (1920usize, 1200usize);
        let mut bgra = vec![0u8; w * h * 4];
        for (i, b) in bgra.iter_mut().enumerate() { *b = (i * 31 % 251) as u8; }
        let mut yuv = YUVBuffer::new(w, h);
        let t = Instant::now();
        for _ in 0..10 { yuv.read_bgra8(BgraSliceU8::new(&bgra, (w, h))); }
        println!("convert: {:?}/frame", t.elapsed() / 10);
        let mut enc = OpenH264Encoder::new(EncoderSettings { fps: 60, bitrate_bps: 12_000_000 });
        let f = Frame { width: w as u32, height: h as u32, bgra };
        enc.encode(&f).unwrap();
        let mut f = f;
        let t = Instant::now();
        for n in 0..10usize { for (i, b) in f.bgra.iter_mut().enumerate().step_by(7) { *b = ((i + n * 13) * 31 % 251) as u8; } enc.encode(&f).unwrap(); }
        println!("encode incl convert: {:?}/frame", t.elapsed() / 10);
    }
}
