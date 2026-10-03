/// A captured frame (BGRA). Windows.Graphics.Capture backend is TODO.
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}

pub trait Capture: Send {
    /// Next frame, or `None` if nothing changed since the last call.
    fn next_frame(&mut self) -> Option<Frame>;
}

/// Solid-colour test source used until the WGC backend lands.
pub struct TestPattern {
    pub width: u32,
    pub height: u32,
    tick: u8,
}

impl TestPattern {
    pub fn new(width: u32, height: u32) -> Self {
        Self { width, height, tick: 0 }
    }
}

impl Capture for TestPattern {
    /// Static gradient with a small moving block, so the stream looks like a mostly
    /// static desktop (a full-frame colour flash would trigger scene-change IDRs).
    fn next_frame(&mut self) -> Option<Frame> {
        self.tick = self.tick.wrapping_add(1);
        let (w, h) = (self.width as usize, self.height as usize);
        let mut bgra = vec![255u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let o = (y * w + x) * 4;
                bgra[o] = (x * 255 / w.max(1)) as u8;
                bgra[o + 1] = (y * 255 / h.max(1)) as u8;
                bgra[o + 2] = 96;
            }
        }
        let bx = (self.tick as usize * 4) % w.saturating_sub(16).max(1);
        for y in 0..16.min(h) {
            for x in bx..(bx + 16).min(w) {
                let o = (y * w + x) * 4;
                bgra[o..o + 3].copy_from_slice(&[255, 255, 255]);
            }
        }
        Some(Frame { width: self.width, height: self.height, bgra })
    }
}
