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
    fn next_frame(&mut self) -> Option<Frame> {
        self.tick = self.tick.wrapping_add(8);
        let px = [self.tick, 255 - self.tick, 128, 255];
        Some(Frame { width: self.width, height: self.height, bgra: px.repeat((self.width * self.height) as usize) })
    }
}
