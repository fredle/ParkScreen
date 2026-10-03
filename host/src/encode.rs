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
