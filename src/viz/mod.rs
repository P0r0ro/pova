//! Visualizer pipeline:
//!
//! ```text
//! pw-record ──► capture thread ──(rtrb lock-free ring)──► DSP thread (cavacore port)
//!                                                              │ watch channel
//!                                                              ▼
//!                                                       UI renders bars
//! ```

pub mod capture;
pub mod core;
pub mod render;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use tokio::sync::watch;

use self::core::CavaCore;
use crate::config::VizConfig;

/// Latest visualizer output.
#[derive(Clone, Debug, Default)]
pub struct Frame {
    /// Per channel bar values in 0..=1: `left[..n]` then `right[..n]`.
    pub bars: Vec<f64>,
    /// Recent mono samples, normalised to roughly -1..=1.
    pub wave: Vec<f32>,
    /// True while audio is actually arriving.
    pub active: bool,
}

pub struct VizHandle {
    pub frames: watch::Receiver<Frame>,
    /// Bars per channel the UI wants; the DSP thread rebuilds when it changes.
    pub want_bars: Arc<AtomicUsize>,
    quit: Arc<AtomicBool>,
    threads: Vec<thread::JoinHandle<()>>,
}

impl VizHandle {
    pub fn set_bars(&self, n: usize) {
        self.want_bars.store(n.clamp(1, 512), Ordering::Relaxed);
    }

    pub fn shutdown(mut self) {
        self.quit.store(true, Ordering::Relaxed);
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

const WAVE_LEN: usize = 2048;

pub fn spawn(cfg: &VizConfig, client_name: String) -> VizHandle {
    let rate = cfg.sample_rate;
    // ~0.5 s of stereo audio.
    let (producer, consumer) = rtrb::RingBuffer::<f32>::new(rate as usize);
    let quit = Arc::new(AtomicBool::new(false));
    let want_bars = Arc::new(AtomicUsize::new(32));
    let (tx, rx) = watch::channel(Frame::default());

    let capture_cfg = capture::CaptureConfig {
        source: cfg.source,
        rate,
        client_name,
    };
    let q = quit.clone();
    let capture = thread::Builder::new()
        .name("viz-capture".into())
        .spawn(move || capture::run(capture_cfg, producer, q))
        .expect("spawn capture thread");

    let q = quit.clone();
    let wb = want_bars.clone();
    let dsp_cfg = cfg.clone();
    let dsp = thread::Builder::new()
        .name("viz-dsp".into())
        .spawn(move || dsp_loop(dsp_cfg, consumer, tx, wb, q))
        .expect("spawn dsp thread");

    VizHandle {
        frames: rx,
        want_bars,
        quit,
        threads: vec![capture, dsp],
    }
}

fn build_core(cfg: &VizConfig, bars: usize) -> Option<CavaCore> {
    let high = cfg.high_cut_off.min(cfg.sample_rate / 2);
    match CavaCore::new(
        bars,
        cfg.sample_rate,
        2,
        true,
        cfg.noise_reduction.clamp(0.0, 0.99),
        cfg.low_cut_off,
        high,
    ) {
        Ok(c) => Some(c),
        Err(e) => {
            tracing::error!("visualizer init: {e}");
            None
        }
    }
}

fn dsp_loop(
    cfg: VizConfig,
    mut consumer: rtrb::Consumer<f32>,
    tx: watch::Sender<Frame>,
    want_bars: Arc<AtomicUsize>,
    quit: Arc<AtomicBool>,
) {
    let frame_time = Duration::from_secs_f64(1.0 / cfg.framerate.clamp(10, 240) as f64);
    let mut bars = want_bars.load(Ordering::Relaxed);
    let mut core = build_core(&cfg, bars);
    let mut input: Vec<f64> = Vec::with_capacity(16384);
    let mut out: Vec<f64> = vec![0.0; bars * 2];
    let mut wave: std::collections::VecDeque<f32> = std::collections::VecDeque::with_capacity(WAVE_LEN);
    let mut wave_peak = 1e-4f32;
    let mut last_audio = Instant::now() - Duration::from_secs(60);
    let mut last_tick = Instant::now();
    let mut idle_frames = 0u32;

    while !quit.load(Ordering::Relaxed) {
        let started = Instant::now();
        let want = want_bars.load(Ordering::Relaxed);
        if want != bars || core.is_none() {
            bars = want;
            core = build_core(&cfg, bars);
            out = vec![0.0; bars * 2];
        }

        // Drain the ring buffer.
        input.clear();
        let available = consumer.slots();
        if available > 0
            && let Ok(chunk) = consumer.read_chunk(available & !1)
        {
            let (a, b) = chunk.as_slices();
            let mut samples = a.iter().chain(b).copied();
            while let (Some(l), Some(r)) = (samples.next(), samples.next()) {
                input.push(l as f64 * 65535.0);
                input.push(r as f64 * 65535.0);
                let m = (l + r) * 0.5;
                if wave.len() == WAVE_LEN {
                    wave.pop_front();
                }
                wave.push_back(m);
            }
            chunk.commit_all();
        }

        let now = Instant::now();
        // PipeWire keeps delivering zero-filled buffers while mpv is paused;
        // treat digital silence as "no audio" so we can idle.
        let active = input.iter().any(|v| *v != 0.0);
        if active {
            last_audio = now;
        } else if now.duration_since(last_audio) > Duration::from_millis(80) {
            // No audio (paused / idle): feed silence so bars fall like cava.
            let n = ((now.duration_since(last_tick).as_secs_f64() * cfg.sample_rate as f64) as usize).clamp(64, 4096);
            input.resize(n * 2, 0.0);
            for _ in 0..n {
                if wave.len() == WAVE_LEN {
                    wave.pop_front();
                }
                wave.push_back(0.0);
            }
        }
        last_tick = now;

        if let Some(core) = core.as_mut() {
            let max = core.input_buffer_size();
            let start = input.len().saturating_sub(max) & !1;
            core.execute(&input[start..], &mut out);
        }

        let peak = wave.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        wave_peak = if peak > wave_peak {
            peak
        } else {
            wave_peak * 0.995 + peak * 0.005
        }
        .max(1e-4);

        let all_zero = out.iter().all(|v| *v < 1e-4);
        idle_frames = if all_zero && !active {
            idle_frames.saturating_add(1)
        } else {
            0
        };
        // Publish unless we're idle and already published a silent frame.
        if idle_frames < 3 {
            tx.send_modify(|f| {
                f.bars.clear();
                f.bars.extend_from_slice(&out);
                f.wave.clear();
                f.wave.extend(wave.iter().map(|s| s / wave_peak));
                f.active = now.duration_since(last_audio) < Duration::from_millis(250);
            });
        }

        let budget = if idle_frames >= 3 {
            Duration::from_millis(100)
        } else {
            frame_time
        };
        if let Some(rest) = budget.checked_sub(started.elapsed()) {
            thread::sleep(rest);
        }
    }
}
