//! Input path: pointer/touch events from the car arrive on the `input` data channel as
//! small JSON messages with coordinates normalised to the video (0..1), and are validated
//! here before reaching an `InputInjector`.
//!
//! Wire format (one JSON object per message, ≤ 256 bytes):
//! `{"t":"down","id":0,"x":0.5,"y":0.25}`, `{"t":"move",...}`, `{"t":"up","id":0}`,
//! `{"t":"scroll","dx":0,"dy":-120}`.

use serde::Deserialize;
use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PointerEvent {
    Down { id: u32, x: f32, y: f32 },
    Move { id: u32, x: f32, y: f32 },
    Up { id: u32 },
    Scroll { dx: f32, dy: f32 },
}

/// Injects pointer input at normalised (0..1) coordinates on the virtual monitor.
/// Windows: `InjectSyntheticPointerInput` (touch) or `SendInput` (mouse mode).
pub trait InputInjector: Send {
    fn inject(&mut self, ev: PointerEvent);
}

pub struct NullInput;
impl InputInjector for NullInput {
    fn inject(&mut self, _ev: PointerEvent) {}
}

/// Records events; for tests.
#[derive(Clone, Default)]
pub struct RecordingInput(pub Arc<Mutex<Vec<PointerEvent>>>);
impl InputInjector for RecordingInput {
    fn inject(&mut self, ev: PointerEvent) {
        self.0.lock().unwrap().push(ev);
    }
}

/// One injector shared by all sessions.
pub type SharedInjector = Arc<Mutex<Box<dyn InputInjector>>>;

#[derive(Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
enum Wire {
    Down { id: u32, x: f64, y: f64 },
    Move { id: u32, x: f64, y: f64 },
    Up { id: u32 },
    Scroll { dx: f64, dy: f64 },
}

pub const MAX_MESSAGE_BYTES: usize = 256;
const MAX_POINTERS: usize = 10;
const MAX_PER_SECOND: u32 = 1000;
const MAX_SCROLL: f64 = 2000.0;

/// Per-car state: which pointers are down (so they can be released if the link drops
/// mid-touch) and a rate limit.
pub struct InputSession {
    pressed: HashSet<u32>,
    window_start: Instant,
    in_window: u32,
}

impl Default for InputSession {
    fn default() -> Self {
        Self { pressed: HashSet::new(), window_start: Instant::now(), in_window: 0 }
    }
}

impl InputSession {
    /// Validate and inject one message. Malformed, oversized, out-of-range-id or
    /// over-rate messages are dropped. Coordinates are clamped to 0..1.
    pub fn handle(&mut self, now: Instant, msg: &[u8], inj: &mut dyn InputInjector) {
        if msg.len() > MAX_MESSAGE_BYTES || !self.allow_rate(now) {
            return;
        }
        let Ok(w) = serde_json::from_slice::<Wire>(msg) else { return };
        let unit = |v: f64| v.is_finite().then(|| v.clamp(0.0, 1.0) as f32);
        match w {
            Wire::Down { id, x, y } => {
                let (Some(x), Some(y)) = (unit(x), unit(y)) else { return };
                if id as usize >= MAX_POINTERS || (self.pressed.len() >= MAX_POINTERS && !self.pressed.contains(&id)) {
                    return;
                }
                self.pressed.insert(id);
                inj.inject(PointerEvent::Down { id, x, y });
            }
            Wire::Move { id, x, y } => {
                let (Some(x), Some(y)) = (unit(x), unit(y)) else { return };
                if id as usize >= MAX_POINTERS {
                    return;
                }
                inj.inject(PointerEvent::Move { id, x, y });
            }
            Wire::Up { id } => {
                // Ignore an Up for a pointer that isn't down (the unreliable channel can
                // reorder or drop, and an unmatched Up could confuse the injector).
                if self.pressed.remove(&id) {
                    inj.inject(PointerEvent::Up { id });
                }
            }
            Wire::Scroll { dx, dy } => {
                if dx.is_finite() && dy.is_finite() {
                    inj.inject(PointerEvent::Scroll {
                        dx: dx.clamp(-MAX_SCROLL, MAX_SCROLL) as f32,
                        dy: dy.clamp(-MAX_SCROLL, MAX_SCROLL) as f32,
                    });
                }
            }
        }
    }

    /// Lift every pointer still down (channel closed, car left, input revoked).
    pub fn release_all(&mut self, inj: &mut dyn InputInjector) {
        let mut ids: Vec<_> = self.pressed.drain().collect();
        ids.sort_unstable();
        for id in ids {
            inj.inject(PointerEvent::Up { id });
        }
    }

    fn allow_rate(&mut self, now: Instant) -> bool {
        if now.duration_since(self.window_start) >= Duration::from_secs(1) {
            self.window_start = now;
            self.in_window = 0;
        }
        self.in_window += 1;
        self.in_window <= MAX_PER_SECOND
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(msgs: &[&str]) -> Vec<PointerEvent> {
        let mut s = InputSession::default();
        let mut r = RecordingInput::default();
        for m in msgs {
            s.handle(Instant::now(), m.as_bytes(), &mut r);
        }
        let v = r.0.lock().unwrap().clone();
        v
    }

    #[test]
    fn tap_and_drag() {
        let ev = run(&[
            r#"{"t":"down","id":0,"x":0.5,"y":0.25}"#,
            r#"{"t":"move","id":0,"x":0.6,"y":0.25}"#,
            r#"{"t":"up","id":0}"#,
        ]);
        assert_eq!(
            ev,
            vec![
                PointerEvent::Down { id: 0, x: 0.5, y: 0.25 },
                PointerEvent::Move { id: 0, x: 0.6, y: 0.25 },
                PointerEvent::Up { id: 0 },
            ]
        );
    }

    #[test]
    fn clamps_and_rejects_garbage() {
        let ev = run(&[
            r#"{"t":"down","id":1,"x":7,"y":-3}"#,
            r#"{"t":"down","id":99,"x":0.5,"y":0.5}"#, // id out of range
            r#"{"t":"move","id":0,"x":null,"y":0.5}"#,  // not a number
            r#"{"t":"explode"}"#,
            "not json",
            r#"{"t":"scroll","dx":0,"dy":1e9}"#,
        ]);
        assert_eq!(
            ev,
            vec![PointerEvent::Down { id: 1, x: 1.0, y: 0.0 }, PointerEvent::Scroll { dx: 0.0, dy: 2000.0 }]
        );
    }

    #[test]
    fn oversized_message_dropped() {
        let big = format!(r#"{{"t":"down","id":0,"x":0.5,"y":0.5,"pad":"{}"}}"#, "a".repeat(300));
        assert!(run(&[&big]).is_empty());
    }

    #[test]
    fn unmatched_up_ignored_and_release_all_lifts_pressed() {
        let mut s = InputSession::default();
        let mut r = RecordingInput::default();
        let t = Instant::now();
        s.handle(t, br#"{"t":"up","id":3}"#, &mut r);
        s.handle(t, br#"{"t":"down","id":2,"x":0.1,"y":0.1}"#, &mut r);
        s.handle(t, br#"{"t":"down","id":1,"x":0.2,"y":0.2}"#, &mut r);
        s.release_all(&mut r);
        let ev = r.0.lock().unwrap().clone();
        assert_eq!(&ev[2..], &[PointerEvent::Up { id: 1 }, PointerEvent::Up { id: 2 }]);
        assert_eq!(ev.len(), 4);
    }

    #[test]
    fn rate_limited() {
        let mut s = InputSession::default();
        let mut r = RecordingInput::default();
        let t = Instant::now();
        for _ in 0..(MAX_PER_SECOND + 500) {
            s.handle(t, br#"{"t":"move","id":0,"x":0.5,"y":0.5}"#, &mut r);
        }
        assert_eq!(r.0.lock().unwrap().len(), MAX_PER_SECOND as usize);
        s.handle(t + Duration::from_secs(1), br#"{"t":"move","id":0,"x":0.5,"y":0.5}"#, &mut r);
        assert_eq!(r.0.lock().unwrap().len(), MAX_PER_SECOND as usize + 1);
    }
}
