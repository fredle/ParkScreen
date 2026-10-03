use crate::capture::Frame;

#[derive(Debug, Clone, Copy)]
pub struct EncoderConfig {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate_kbps: u32,
    /// Periodic IDR interval in seconds.
    pub idr_secs: u32,
}

/// H.264 Constrained Baseline/Main, no B-frames, low-latency (plan §5.1).
/// Backends: Media Foundation hardware MFT (Windows, TODO), openh264 software fallback.
pub trait Encoder: Send {
    /// May return an empty Vec when rate control skips the frame.
    fn encode(&mut self, frame: &Frame) -> Vec<u8>;
    /// Force an IDR (client joined, PLI/FIR, decoder reset).
    fn request_keyframe(&mut self);
    /// Change the target bitrate; takes effect from the next frame.
    fn set_bitrate(&mut self, kbps: u32);
}

/// Software H.264 (openh264, Constrained Baseline, no B-frames). Output is Annex-B,
/// which is what the WebRTC H.264 packetiser expects. Dimensions are rounded down to even.
pub struct OpenH264Encoder {
    enc: openh264::encoder::Encoder,
    cfg: EncoderConfig,
    first: bool,
}

impl OpenH264Encoder {
    pub fn new(cfg: EncoderConfig) -> Result<Self, String> {
        Ok(Self { enc: Self::build(&cfg)?, cfg, first: true })
    }

    fn build(cfg: &EncoderConfig) -> Result<openh264::encoder::Encoder, String> {
        use openh264::encoder::{BitRate, Encoder, FrameRate, IntraFramePeriod, Profile, RateControlMode, UsageType};
        let c = openh264::encoder::EncoderConfig::new()
            .usage_type(UsageType::ScreenContentRealTime)
            .profile(Profile::Baseline)
            .rate_control_mode(RateControlMode::Bitrate)
            .bitrate(BitRate::from_bps(cfg.bitrate_kbps * 1000))
            .max_frame_rate(FrameRate::from_hz(cfg.fps as f32))
            .intra_frame_period(IntraFramePeriod::from_num_frames(cfg.fps * cfg.idr_secs.max(1)))
            .skip_frames(true);
        Encoder::with_api_config(openh264::OpenH264API::from_source(), c).map_err(|e| e.to_string())
    }

    pub fn config(&self) -> EncoderConfig {
        self.cfg
    }
}

impl Encoder for OpenH264Encoder {
    fn encode(&mut self, frame: &Frame) -> Vec<u8> {
        use openh264::formats::{BgraSliceU8, YUVBuffer};
        let (w, h) = ((frame.width & !1) as usize, (frame.height & !1) as usize);
        if self.first {
            self.enc.force_intra_frame();
            self.first = false;
        }
        // Crop (rare: odd sizes) by copying rows; otherwise use the buffer directly.
        let cropped;
        let data: &[u8] = if w as u32 == frame.width && h as u32 == frame.height {
            &frame.bgra
        } else {
            let stride = frame.width as usize * 4;
            cropped = (0..h).flat_map(|y| frame.bgra[y * stride..y * stride + w * 4].iter().copied()).collect::<Vec<u8>>();
            &cropped
        };
        let yuv = YUVBuffer::from_bgra8_source(BgraSliceU8::new(data, (w, h)));
        match self.enc.encode(&yuv) {
            Ok(bs) => bs.to_vec(),
            Err(_) => Vec::new(),
        }
    }

    fn request_keyframe(&mut self) {
        self.enc.force_intra_frame();
    }

    /// openh264's rate control can't be retuned through the safe API, so rebuild the
    /// encoder (cheap) and start with an IDR so the decoder can pick up the new stream.
    fn set_bitrate(&mut self, kbps: u32) {
        let kbps = kbps.clamp(500, 50_000);
        if kbps == self.cfg.bitrate_kbps {
            return;
        }
        let cfg = EncoderConfig { bitrate_kbps: kbps, ..self.cfg };
        match Self::build(&cfg) {
            Ok(e) => {
                self.enc = e;
                self.cfg = cfg;
                self.first = true;
            }
            Err(e) => tracing::warn!("bitrate change failed: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::{Capture, TestPattern};

    /// First NAL type in an Annex-B buffer containing an IDR (5) anywhere?
    fn has_idr(d: &[u8]) -> bool {
        d.windows(4).any(|w| w[..3] == [0, 0, 1] && w[3] & 0x1f == 5)
    }

    fn enc() -> (OpenH264Encoder, TestPattern) {
        let cfg = EncoderConfig { width: 320, height: 240, fps: 30, bitrate_kbps: 2000, idr_secs: 2 };
        (OpenH264Encoder::new(cfg).unwrap(), TestPattern::new(320, 240))
    }

    #[test]
    fn keyframe_request_produces_idr() {
        let (mut e, mut c) = enc();
        assert!(has_idr(&e.encode(&c.next_frame().unwrap())));
        // Subsequent frames are not IDRs (period is 2 s)...
        let mut saw_idr = false;
        for _ in 0..5 {
            saw_idr |= has_idr(&e.encode(&c.next_frame().unwrap()));
        }
        assert!(!saw_idr);
        // ...until one is requested.
        e.request_keyframe();
        let mut got = false;
        for _ in 0..3 {
            got |= has_idr(&e.encode(&c.next_frame().unwrap()));
        }
        assert!(got, "no IDR after request_keyframe");
    }

    #[test]
    fn bitrate_change_applies_and_restarts_with_idr() {
        let (mut e, mut c) = enc();
        e.encode(&c.next_frame().unwrap());
        e.set_bitrate(800);
        assert_eq!(e.config().bitrate_kbps, 800);
        assert!(has_idr(&e.encode(&c.next_frame().unwrap())));
        e.set_bitrate(10); // clamped
        assert_eq!(e.config().bitrate_kbps, 500);
    }
}
