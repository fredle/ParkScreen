use crate::capture::Frame;

#[derive(Debug, Clone, Copy)]
pub struct EncoderConfig {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate_kbps: u32,
}

/// H.264 Constrained Baseline/Main, no B-frames, low-latency (plan §5.1).
/// Backends: Media Foundation hardware MFT, openh264 software fallback (both TODO).
pub trait Encoder: Send {
    fn encode(&mut self, frame: &Frame) -> Vec<u8>;
    /// Force an IDR (client joined, PLI, decoder reset).
    fn request_keyframe(&mut self);
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
        use openh264::encoder::{BitRate, Encoder, FrameRate, IntraFramePeriod, Profile, RateControlMode, UsageType};
        let c = openh264::encoder::EncoderConfig::new()
            .usage_type(UsageType::ScreenContentRealTime)
            .profile(Profile::Baseline)
            .rate_control_mode(RateControlMode::Bitrate)
            .bitrate(BitRate::from_bps(cfg.bitrate_kbps * 1000))
            .max_frame_rate(FrameRate::from_hz(cfg.fps as f32))
            .intra_frame_period(IntraFramePeriod::from_num_frames(cfg.fps * 2))
            .skip_frames(true);
        let enc = Encoder::with_api_config(openh264::OpenH264API::from_source(), c).map_err(|e| e.to_string())?;
        Ok(Self { enc, cfg, first: true })
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
    fn set_bitrate(&mut self, kbps: u32) {
        self.cfg.bitrate_kbps = kbps; // applied on next encoder rebuild (TODO: live update)
    }
}
