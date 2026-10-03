//! The capture → encode thread and the hand-off to the async (WebRTC) side.
//!
//! D3D/DXGI objects are not `Send`, so capture and encoding live on one plain OS thread.
//! Encoded frames go to the current viewer over a bounded channel; if it is full the
//! frame is dropped and a keyframe is requested, because dropping a delta frame breaks
//! the reference chain.

use crate::capture::{Captured, Duplicator, Frame};
use crate::display::{self, Mode};
use crate::encode::{Encoder, EncoderSettings, OpenH264Encoder};
use bytes::Bytes;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Re-send the last frame at least this often, so the video never looks frozen to the
/// browser's decoder when the desktop is static.
const KEEPALIVE: Duration = Duration::from_millis(500);

pub enum Command {
    ForceKeyframe,
    Stop,
}

pub struct Encoded {
    pub data: Bytes,
}

pub struct Pipeline {
    device_name: String,
    fps: u32,
    cmd_tx: Mutex<Sender<Command>>,
    sink: Arc<Mutex<Option<tokio::sync::mpsc::Sender<Encoded>>>>,
    active: Arc<AtomicBool>,
}

impl Pipeline {
    /// Start the capture thread for `device_name` (e.g. `\\.\DISPLAY3`).
    pub fn start(device_name: String, settings: EncoderSettings) -> Arc<Self> {
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let sink = Arc::new(Mutex::new(None));
        let active = Arc::new(AtomicBool::new(false));
        let p = Arc::new(Pipeline {
            device_name: device_name.clone(),
            fps: settings.fps,
            cmd_tx: Mutex::new(cmd_tx),
            sink: sink.clone(),
            active: active.clone(),
        });
        std::thread::Builder::new()
            .name("capture-encode".into())
            .spawn(move || run(device_name, settings, cmd_rx, sink, active))
            .expect("spawn capture thread");
        p
    }

    pub fn send(&self, cmd: Command) {
        let _ = self.cmd_tx.lock().unwrap().send(cmd);
    }

    /// Route encoded frames to a new viewer (replacing any previous one) and start with a keyframe.
    pub fn attach_viewer(&self, tx: tokio::sync::mpsc::Sender<Encoded>) {
        *self.sink.lock().unwrap() = Some(tx);
        self.active.store(true, Ordering::SeqCst);
        self.send(Command::ForceKeyframe);
    }

    pub fn detach_viewer(&self) {
        *self.sink.lock().unwrap() = None;
        self.active.store(false, Ordering::SeqCst);
    }

    /// Change the monitor's mode to the viewer's reported size. Failures are logged, not fatal.
    pub async fn match_viewport(&self, width: u32, height: u32) {
        let device = self.device_name.clone();
        let mode = Mode { width, height, hz: Some(self.fps) };
        let res = tokio::task::spawn_blocking(move || display::set_mode(&device, mode)).await;
        match res {
            Ok(Ok(())) => tracing::info!("monitor switched to {mode}"),
            Ok(Err(e)) => tracing::warn!("could not match viewport {mode}: {e:#}"),
            Err(e) => tracing::warn!("mode change task failed: {e}"),
        }
    }
}

impl Drop for Pipeline {
    fn drop(&mut self) {
        self.send(Command::Stop);
    }
}

fn run(
    device_name: String,
    settings: EncoderSettings,
    cmd_rx: mpsc::Receiver<Command>,
    sink: Arc<Mutex<Option<tokio::sync::mpsc::Sender<Encoded>>>>,
    active: Arc<AtomicBool>,
) {
    let interval = Duration::from_micros(1_000_000 / settings.fps.max(1) as u64);
    let mut encoder = OpenH264Encoder::new(settings);
    let mut dup: Option<Duplicator> = None;
    let mut last: Option<Frame> = None;
    let mut pending = false;
    let mut last_encode = Instant::now() - interval;
    let mut last_sent = Instant::now();
    let mut stats = Stats::new();

    loop {
        // Commands (and the idle sleep when nobody is watching).
        let idle_wait = if active.load(Ordering::SeqCst) { Duration::ZERO } else { Duration::from_millis(100) };
        match cmd_rx.recv_timeout(idle_wait.max(Duration::from_micros(1))) {
            Ok(Command::ForceKeyframe) => {
                encoder.force_keyframe();
                pending = last.is_some();
            }
            Ok(Command::Stop) | Err(RecvTimeoutError::Disconnected) => return,
            Err(RecvTimeoutError::Timeout) => {}
        }
        while let Ok(cmd) = cmd_rx.try_recv() {
            match cmd {
                Command::ForceKeyframe => {
                    encoder.force_keyframe();
                    pending = last.is_some();
                }
                Command::Stop => return,
            }
        }
        if !active.load(Ordering::SeqCst) {
            // Release the duplication while idle so we do not block other capture apps.
            dup = None;
            continue;
        }

        if dup.is_none() {
            match Duplicator::new(&device_name) {
                Ok(d) => {
                    tracing::info!("capturing {device_name}");
                    dup = Some(d);
                    encoder.force_keyframe();
                }
                Err(e) => {
                    tracing::warn!("capture unavailable: {e:#}; retrying");
                    std::thread::sleep(Duration::from_secs(1));
                    continue;
                }
            }
        }

        // Wait for a new frame, but no longer than the next encode slot (or the keepalive).
        let since_encode = last_encode.elapsed();
        let timeout = if pending {
            interval.saturating_sub(since_encode)
        } else {
            KEEPALIVE.saturating_sub(last_sent.elapsed())
        };
        let timeout_ms = (timeout.as_millis() as u32).max(1);
        match dup.as_mut().unwrap().next_frame(timeout_ms) {
            Ok(Captured::Frame(f)) => {
                last = Some(f);
                pending = true;
            }
            Ok(Captured::Idle) => {}
            Ok(Captured::Lost) => {
                tracing::info!("desktop duplication lost; recreating");
                dup = None;
                continue;
            }
            Err(e) => {
                tracing::warn!("capture error: {e:#}; recreating");
                dup = None;
                std::thread::sleep(Duration::from_millis(200));
                continue;
            }
        }

        let keepalive_due = !pending && last.is_some() && last_sent.elapsed() >= KEEPALIVE;
        let slot_ready = last_encode.elapsed() >= interval;
        if !((pending && slot_ready) || keepalive_due) {
            continue;
        }
        let Some(frame) = last.as_ref() else { continue };

        let t = Instant::now();
        let data = match encoder.encode(frame) {
            Ok(Some(d)) => d,
            Ok(None) => {
                pending = false;
                continue;
            }
            Err(e) => {
                tracing::error!("encode failed: {e:#}");
                pending = false;
                continue;
            }
        };
        let encode_ms = t.elapsed().as_secs_f32() * 1000.0;
        last_encode = Instant::now();
        last_sent = last_encode;
        pending = false;

        let len = data.len();
        let tx = sink.lock().unwrap().clone();
        if let Some(tx) = tx {
            if tx.try_send(Encoded { data: Bytes::from(data) }).is_err() {
                // Viewer is behind or gone. Next frame must be a keyframe.
                encoder.force_keyframe();
                pending = true;
                stats.dropped += 1;
            }
        }
        stats.record(len, encode_ms);
    }
}

/// Logs throughput once every five seconds.
struct Stats {
    since: Instant,
    frames: u32,
    bytes: usize,
    encode_ms: f32,
    dropped: u32,
}

impl Stats {
    fn new() -> Self {
        Self { since: Instant::now(), frames: 0, bytes: 0, encode_ms: 0.0, dropped: 0 }
    }

    fn record(&mut self, bytes: usize, encode_ms: f32) {
        self.frames += 1;
        self.bytes += bytes;
        self.encode_ms += encode_ms;
        let secs = self.since.elapsed().as_secs_f32();
        if secs >= 5.0 {
            tracing::info!(
                "{:.1} fps, {:.1} Mbps, encode {:.1} ms avg, {} dropped",
                self.frames as f32 / secs,
                self.bytes as f32 * 8.0 / secs / 1e6,
                self.encode_ms / self.frames.max(1) as f32,
                self.dropped
            );
            *self = Stats::new();
        }
    }
}
