//! Adaptive bitrate (plan §5.2). The car reports stats once a second; this decides the
//! target bitrate with AIMD: cut quickly on trouble, creep back up when clean.
//! Pure logic with an injectable clock so it is unit-testable.

use serde::Deserialize;
use std::time::{Duration, Instant};

/// One second of receiver-side stats from the car (`{"kind":"stats", ...}`).
#[derive(Debug, Clone, Copy, Deserialize, Default)]
pub struct ClientStats {
    /// Decoded frames per second.
    pub fps: f64,
    /// Frames dropped by the decoder/renderer during the last interval.
    #[serde(default)]
    pub dropped: f64,
    /// Mean decode time per frame, ms.
    #[serde(default)]
    pub decode_ms: f64,
    /// Mean jitter-buffer delay, ms.
    #[serde(default)]
    pub jitter_ms: f64,
    /// RTP packets lost as a percentage of expected during the last interval.
    #[serde(default)]
    pub loss_pct: f64,
}

#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub min_kbps: u32,
    pub max_kbps: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self { min_kbps: 1_000, max_kbps: 20_000 }
    }
}

pub struct Adaptive {
    pub limits: Limits,
    kbps: u32,
    target_fps: f64,
    last_change: Option<Instant>,
    clean_secs: u32,
}

const DECREASE: f64 = 0.75;
const INCREASE: f64 = 1.10;
const DECREASE_COOLDOWN: Duration = Duration::from_secs(2);
const INCREASE_COOLDOWN: Duration = Duration::from_secs(5);
/// Don't touch the encoder for changes smaller than this (each change restarts it with an IDR).
const MIN_STEP: f64 = 0.05;

impl Adaptive {
    pub fn new(start_kbps: u32, target_fps: u32, limits: Limits) -> Self {
        Self {
            limits,
            kbps: start_kbps.clamp(limits.min_kbps, limits.max_kbps),
            target_fps: target_fps as f64,
            last_change: None,
            clean_secs: 0,
        }
    }

    /// Override from outside (e.g. the car asked for a specific bitrate).
    pub fn set_kbps(&mut self, kbps: u32) {
        self.kbps = kbps.clamp(self.limits.min_kbps, self.limits.max_kbps);
        self.clean_secs = 0;
    }

    pub fn kbps(&self) -> u32 {
        self.kbps
    }

    /// Is this interval a sign the link or decoder can't keep up?
    fn congested(&self, s: &ClientStats) -> bool {
        let budget_ms = 1000.0 / self.target_fps.max(1.0);
        s.loss_pct > 2.0
            || s.dropped > 0.0
            || s.decode_ms > budget_ms * 0.8
            || s.jitter_ms > 150.0
            || (s.fps > 0.0 && s.fps < self.target_fps * 0.8)
    }

    /// Feed one stats sample. Returns a new target bitrate if it should change.
    pub fn update(&mut self, now: Instant, s: &ClientStats) -> Option<u32> {
        let since = self.last_change.map(|t| now.duration_since(t));
        if self.congested(s) {
            self.clean_secs = 0;
            if since.is_some_and(|d| d < DECREASE_COOLDOWN) {
                return None;
            }
            return self.set(now, (self.kbps as f64 * DECREASE) as u32);
        }
        self.clean_secs += 1;
        if self.clean_secs >= 5 && since.map_or(true, |d| d >= INCREASE_COOLDOWN) {
            return self.set(now, (self.kbps as f64 * INCREASE) as u32);
        }
        None
    }

    fn set(&mut self, now: Instant, want: u32) -> Option<u32> {
        let want = want.clamp(self.limits.min_kbps, self.limits.max_kbps);
        if (want as f64 - self.kbps as f64).abs() < self.kbps as f64 * MIN_STEP {
            return None;
        }
        self.kbps = want;
        self.last_change = Some(now);
        self.clean_secs = 0;
        Some(want)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn good() -> ClientStats {
        ClientStats { fps: 60.0, decode_ms: 3.0, jitter_ms: 20.0, ..Default::default() }
    }
    fn lossy() -> ClientStats {
        ClientStats { loss_pct: 5.0, ..good() }
    }

    #[test]
    fn cuts_on_loss_then_respects_cooldown_and_floor() {
        let t0 = Instant::now();
        let mut a = Adaptive::new(12_000, 60, Limits::default());
        assert_eq!(a.update(t0, &lossy()), Some(9_000));
        assert_eq!(a.update(t0 + Duration::from_secs(1), &lossy()), None, "cooldown");
        assert_eq!(a.update(t0 + Duration::from_secs(2), &lossy()), Some(6_750));
        let mut t = t0 + Duration::from_secs(2);
        for _ in 0..20 {
            t += Duration::from_secs(2);
            a.update(t, &lossy());
        }
        assert_eq!(a.kbps(), 1_000, "floor");
    }

    #[test]
    fn recovers_slowly_when_clean_up_to_ceiling() {
        let t0 = Instant::now();
        let mut a = Adaptive::new(4_000, 60, Limits::default());
        let mut changes = vec![];
        for s in 1..=60u64 {
            if let Some(k) = a.update(t0 + Duration::from_secs(s), &good()) {
                changes.push((s, k));
            }
        }
        assert_eq!(changes[0], (5, 4_400), "first raise after 5 clean seconds");
        assert!(changes.windows(2).all(|w| w[1].0 - w[0].0 >= 5));
        assert!(a.kbps() <= 20_000);
        // Run long enough to hit the ceiling.
        let mut t = t0 + Duration::from_secs(60);
        for _ in 0..400 {
            t += Duration::from_secs(1);
            a.update(t, &good());
        }
        assert_eq!(a.kbps(), 20_000);
    }

    #[test]
    fn slow_decode_and_low_fps_count_as_congestion() {
        let t0 = Instant::now();
        let mut a = Adaptive::new(10_000, 60, Limits::default());
        // 16.7 ms budget at 60 fps; 14 ms decode is > 80 %.
        assert!(a.update(t0, &ClientStats { decode_ms: 14.0, ..good() }).is_some());
        let mut b = Adaptive::new(10_000, 60, Limits::default());
        assert!(b.update(t0, &ClientStats { fps: 30.0, ..good() }).is_some());
    }

    #[test]
    fn tiny_changes_are_ignored() {
        let mut a = Adaptive::new(1_020, 60, Limits::default());
        // 75 % of 1020 clamps to the 1000 floor: a <5 % step, so no encoder restart.
        assert_eq!(a.update(Instant::now(), &lossy()), None);
    }
}
