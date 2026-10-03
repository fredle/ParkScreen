#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PointerEvent {
    Down { id: u32, x: f32, y: f32 },
    Move { id: u32, x: f32, y: f32 },
    Up { id: u32 },
    Scroll { dx: f32, dy: f32 },
}

/// Injects pointer input at normalised (0..1) coordinates on the virtual monitor.
/// Windows: `InjectSyntheticPointerInput` (touch) or `SendInput` (mouse mode). TODO.
pub trait InputInjector: Send {
    fn inject(&mut self, ev: PointerEvent);
}

pub struct NullInput;
impl InputInjector for NullInput {
    fn inject(&mut self, _ev: PointerEvent) {}
}
