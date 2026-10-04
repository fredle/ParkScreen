//! Windows touch injection: car touches become real multi-touch contacts on the chosen
//! monitor via `InjectTouchInput`; scroll events become mouse-wheel input.

use crate::{
    input::{InputInjector, PointerEvent},
    monitors::{self, Monitor, Selector},
};
use std::collections::BTreeMap;
use windows::Win32::{
    Foundation::{POINT, RECT},
    UI::{
        Input::{
            KeyboardAndMouse::{SendInput, INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_HWHEEL, MOUSEEVENTF_WHEEL, MOUSEINPUT},
            Pointer::{
                InitializeTouchInjection, InjectTouchInput, POINTER_TOUCH_INFO, TOUCH_FEEDBACK_DEFAULT,
                POINTER_FLAG_DOWN, POINTER_FLAG_INCONTACT, POINTER_FLAG_INRANGE, POINTER_FLAG_UP,
                POINTER_FLAG_UPDATE, POINTER_FLAGS,
            },
        },
        WindowsAndMessaging::{PT_TOUCH, SetCursorPos, TOUCH_MASK_CONTACTAREA, TOUCH_MASK_PRESSURE},
    },
};

const MAX_CONTACTS: u32 = 10;
/// Half-size of the contact patch in pixels.
const CONTACT_RADIUS: i32 = 2;

struct Contact {
    x: i32,
    y: i32,
    flags: POINTER_FLAGS,
}

pub struct WindowsTouch {
    selector: Selector,
    ready: bool,
    contacts: BTreeMap<u32, Contact>,
    last: (i32, i32),
}

impl WindowsTouch {
    pub fn new(selector: Selector) -> Self {
        Self { selector, ready: false, contacts: BTreeMap::new(), last: (0, 0) }
    }

    /// Current desktop rectangle of the shared monitor (looked up each time: it can move
    /// or change mode while the host runs).
    fn monitor(&self) -> Option<Monitor> {
        let all = monitors::enumerate().ok()?;
        monitors::select(&all, &self.selector).ok().cloned()
    }

    fn init(&mut self) -> bool {
        if !self.ready {
            self.ready = unsafe { InitializeTouchInjection(MAX_CONTACTS, TOUCH_FEEDBACK_DEFAULT) }.is_ok();
            if !self.ready {
                tracing::warn!("InitializeTouchInjection failed; touch input disabled");
            }
        }
        self.ready
    }

    fn to_pixels(m: &Monitor, x: f32, y: f32) -> (i32, i32) {
        let px = m.x + (x * (m.width.saturating_sub(1)) as f32).round() as i32;
        let py = m.y + (y * (m.height.saturating_sub(1)) as f32).round() as i32;
        (px, py)
    }

    /// Send the whole current frame: every active contact, with its pending flags.
    fn flush(&mut self) {
        if self.contacts.is_empty() {
            return;
        }
        let frame: Vec<POINTER_TOUCH_INFO> = self
            .contacts
            .iter()
            .map(|(id, c)| {
                let mut t = POINTER_TOUCH_INFO::default();
                t.pointerInfo.pointerType = PT_TOUCH;
                t.pointerInfo.pointerId = *id;
                t.pointerInfo.pointerFlags = c.flags;
                t.pointerInfo.ptPixelLocation = POINT { x: c.x, y: c.y };
                t.touchMask = TOUCH_MASK_CONTACTAREA | TOUCH_MASK_PRESSURE;
                t.rcContact = RECT {
                    left: c.x - CONTACT_RADIUS,
                    top: c.y - CONTACT_RADIUS,
                    right: c.x + CONTACT_RADIUS,
                    bottom: c.y + CONTACT_RADIUS,
                };
                t.pressure = 512;
                t
            })
            .collect();
        if let Err(e) = unsafe { InjectTouchInput(&frame) } {
            tracing::debug!("InjectTouchInput failed: {e}");
        }
        // Lifted contacts are gone; the rest continue as plain updates.
        self.contacts.retain(|_, c| !c.flags.contains(POINTER_FLAG_UP));
        for c in self.contacts.values_mut() {
            c.flags = POINTER_FLAG_UPDATE | POINTER_FLAG_INRANGE | POINTER_FLAG_INCONTACT;
        }
    }

    fn wheel(&self, dx: f32, dy: f32) {
        let _ = unsafe { SetCursorPos(self.last.0, self.last.1) };
        let mk = |flags, data: f32| INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT { mouseData: (data as i32) as u32, dwFlags: flags, ..Default::default() },
            },
        };
        let mut v = Vec::new();
        if dy != 0.0 {
            v.push(mk(MOUSEEVENTF_WHEEL, -dy)); // browser: positive = down; wheel: positive = up
        }
        if dx != 0.0 {
            v.push(mk(MOUSEEVENTF_HWHEEL, dx));
        }
        if !v.is_empty() {
            unsafe { SendInput(&v, std::mem::size_of::<INPUT>() as i32) };
        }
    }
}

impl InputInjector for WindowsTouch {
    fn inject(&mut self, ev: PointerEvent) {
        match ev {
            PointerEvent::Down { id, x, y } | PointerEvent::Move { id, x, y } => {
                let is_down = matches!(ev, PointerEvent::Down { .. });
                if !self.init() {
                    return;
                }
                let Some(m) = self.monitor() else { return };
                let (px, py) = Self::to_pixels(&m, x, y);
                self.last = (px, py);
                let active = POINTER_FLAG_INRANGE | POINTER_FLAG_INCONTACT;
                match self.contacts.get_mut(&id) {
                    Some(c) if !is_down => {
                        c.x = px;
                        c.y = py;
                        if !c.flags.contains(POINTER_FLAG_DOWN) {
                            c.flags = POINTER_FLAG_UPDATE | active;
                        }
                    }
                    // Moves for an unknown pointer (dropped Down) are ignored.
                    None if !is_down => return,
                    _ => {
                        self.contacts.insert(id, Contact { x: px, y: py, flags: POINTER_FLAG_DOWN | active });
                    }
                }
                self.flush();
            }
            PointerEvent::Up { id } => {
                if let Some(c) = self.contacts.get_mut(&id) {
                    c.flags = POINTER_FLAG_UP;
                    self.flush();
                }
            }
            PointerEvent::Scroll { dx, dy } => self.wheel(dx, dy),
        }
    }
}
