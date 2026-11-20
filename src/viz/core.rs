//! Pure-Rust port of cava's `cavacore.c` (MIT, © Karl Stavestrand).
//!
//! Same algorithm: a long "bass" FFT and a shorter treble FFT (Hann
//! windowed), log-spaced bar cut-offs, per-bar EQ, then cava's gravity
//! fall-off + integral smoothing and automatic sensitivity. Linear scaling
//! mode only (what cava uses by default).

use std::sync::Arc;

use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};

pub struct CavaCore {
    bars: usize,
    channels: usize,
    rate: u32,
    bass_size: usize,
    fft_size: usize,
    input_buffer: Vec<f64>,
    bass_cut_off_bar: usize,
    lower: Vec<i64>,
    upper: Vec<i64>,
    eq: Vec<f64>,
    #[cfg_attr(not(test), allow(dead_code))]
    cut_off_frequency: Vec<f32>,
    bass_window: Vec<f64>,
    window: Vec<f64>,
    fft_bass: Arc<dyn Fft<f64>>,
    fft: Arc<dyn Fft<f64>>,
    buf_bass: [Vec<Complex<f64>>; 2],
    buf: [Vec<Complex<f64>>; 2],
    scratch: Vec<Complex<f64>>,
    fall: Vec<f64>,
    mem: Vec<f64>,
    peak: Vec<f64>,
    prev_out: Vec<f64>,
    sens: f64,
    sens_init: bool,
    autosens: bool,
    framerate: f64,
    frame_skip: u32,
    noise_reduction: f64,
}

#[derive(Debug)]
pub enum InitError {
    Channels(usize),
    Rate,
    Bars(usize, usize),
    CutOff(&'static str),
}

impl std::error::Error for InitError {}

impl std::fmt::Display for InitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InitError::Channels(c) => write!(f, "illegal number of channels {c} (1 or 2 supported)"),
            InitError::Rate => write!(f, "illegal sample rate"),
            InitError::Bars(b, max) => write!(f, "illegal number of bars {b} (1..={max})"),
            InitError::CutOff(m) => write!(f, "{m}"),
        }
    }
}

impl CavaCore {
    /// `bars` is per channel. Output of [`execute`](Self::execute) has
    /// `bars * channels` values: left bars then right bars.
    pub fn new(
        bars: usize,
        rate: u32,
        channels: usize,
        autosens: bool,
        noise_reduction: f64,
        low_cut_off: u32,
        high_cut_off: u32,
    ) -> Result<CavaCore, InitError> {
        if !(1..=2).contains(&channels) {
            return Err(InitError::Channels(channels));
        }
        if rate < 1 {
            return Err(InitError::Rate);
        }
        let mut fft_buffer_size: usize = 512;
        fft_buffer_size *= match rate {
            0..=8125 => 1,
            8126..=16250 => 2,
            16251..=32500 => 4,
            32501..=75000 => 8,
            75001..=150000 => 16,
            150001..=300000 => 32,
            300001..=705600 => 64,
            _ => 128,
        };
        if bars < 1 || bars > fft_buffer_size / 2 + 1 {
            return Err(InitError::Bars(bars, fft_buffer_size / 2 + 1));
        }
        if low_cut_off < 1 || high_cut_off < 1 {
            return Err(InitError::CutOff("low_cut_off must be a positive value"));
        }
        if low_cut_off >= high_cut_off {
            return Err(InitError::CutOff("high_cut_off must be higher than low_cut_off"));
        }
        if high_cut_off > rate / 2 {
            return Err(InitError::CutOff("high_cut_off can't be higher than sample rate / 2"));
        }

        let bass_size = fft_buffer_size * 2;
        let fft_size = fft_buffer_size;
        let hann = |n: usize| -> Vec<f64> {
            (0..n)
                .map(|i| 0.5 * (1.0 - (2.0 * std::f64::consts::PI * i as f64 / (n - 1) as f64).cos()))
                .collect()
        };

        let mut planner = FftPlanner::new();
        let fft_bass = planner.plan_fft_forward(bass_size);
        let fft = planner.plan_fft_forward(fft_size);
        let scratch_len = fft_bass.get_inplace_scratch_len().max(fft.get_inplace_scratch_len());

        let total = bars * channels;
        let mut lower = vec![0i64; bars + 1];
        let mut upper = vec![0i64; bars + 1];
        let mut cut_off_frequency = vec![0f32; bars + 1];
        let mut relative_cut_off = vec![0f32; bars + 1];

        // --- bar cut-off computation, faithfully following cavacore.c ---
        let bass_cut_off = 100.0f32;
        let frequency_constant =
            ((low_cut_off as f32 / high_cut_off as f32) as f64).log10() / (1.0 / (bars as f32 + 1.0) - 1.0) as f64;
        let mut bass_cut_off_bar = 0usize;
        let mut first_bar;
        // Integer division in C.
        let min_bandwidth = (rate as usize / bass_size) as f32;
        let half_bass = (bass_size / 2) as i64;
        let half_fft = (fft_size / 2) as i64;

        for n in 0..=bars {
            let mut coeff = -frequency_constant;
            coeff += ((n as f32 + 1.0) / (bars as f32 + 1.0)) as f64 * frequency_constant;
            cut_off_frequency[n] = (high_cut_off as f64 * 10f64.powf(coeff)) as f32;
            if n > 0 && cut_off_frequency[n - 1] >= cut_off_frequency[n] {
                cut_off_frequency[n] = cut_off_frequency[n - 1] + min_bandwidth;
            }
            relative_cut_off[n] = cut_off_frequency[n] / (rate as f32 / 2.0);

            if cut_off_frequency[n] < bass_cut_off {
                lower[n] = (relative_cut_off[n] * half_bass as f32) as i64;
                bass_cut_off_bar += 1;
                first_bar = bass_cut_off_bar <= 1;
                if lower[n] > half_bass {
                    lower[n] = half_bass;
                }
            } else {
                lower[n] = (relative_cut_off[n] * half_fft as f32).ceil() as i64;
                if n == bass_cut_off_bar {
                    first_bar = true;
                    if n > 0 {
                        upper[n - 1] = (relative_cut_off[n] * half_bass as f32) as i64 - 1;
                    }
                } else {
                    first_bar = false;
                }
                if lower[n] > half_fft {
                    lower[n] = half_fft;
                }
            }

            if n > 0 {
                if !first_bar {
                    upper[n - 1] = lower[n] - 1;
                    if lower[n] <= lower[n - 1] {
                        let room_for_more = if n < bass_cut_off_bar {
                            lower[n - 1] + 1 < half_bass + 1
                        } else {
                            lower[n - 1] + 1 < half_fft + 1
                        };
                        if room_for_more {
                            lower[n] = lower[n - 1] + 1;
                            upper[n - 1] = lower[n] - 1;
                        }
                    }
                } else if upper[n - 1] < lower[n - 1] {
                    upper[n - 1] = lower[n - 1] + 1;
                }
            }

            relative_cut_off[n] = if n < bass_cut_off_bar {
                lower[n] as f32 / (bass_size as f32 / 2.0)
            } else {
                lower[n] as f32 / (fft_size as f32 / 2.0)
            };
            cut_off_frequency[n] = relative_cut_off[n] * (rate as f32 / 2.0);
        }

        let mut eq = vec![0f64; bars + 1];
        for n in 0..bars {
            eq[n] = 1.0 / 2f64.powi(28);
            eq[n] *= (cut_off_frequency[n + 1] as f64).powf(0.85);
            eq[n] /= if n < bass_cut_off_bar {
                (bass_size as f64).log2()
            } else {
                (fft_size as f64).log2()
            };
            eq[n] /= (upper[n] - lower[n] + 1) as f64;
        }

        Ok(CavaCore {
            bars,
            channels,
            rate,
            bass_size,
            fft_size,
            input_buffer: vec![0.0; bass_size * channels],
            bass_cut_off_bar,
            lower,
            upper,
            eq,
            cut_off_frequency,
            bass_window: hann(bass_size),
            window: hann(fft_size),
            fft_bass,
            fft,
            buf_bass: [vec![Complex::default(); bass_size], vec![Complex::default(); bass_size]],
            buf: [vec![Complex::default(); fft_size], vec![Complex::default(); fft_size]],
            scratch: vec![Complex::default(); scratch_len],
            fall: vec![0.0; total],
            mem: vec![0.0; total],
            peak: vec![0.0; total],
            prev_out: vec![0.0; total],
            sens: 1.0,
            sens_init: true,
            autosens,
            framerate: 75.0,
            frame_skip: 1,
            noise_reduction,
        })
    }

    #[cfg(test)]
    pub fn bars(&self) -> usize {
        self.bars
    }
    #[cfg(test)]
    pub fn channels(&self) -> usize {
        self.channels
    }
    #[cfg(test)]
    pub fn rate(&self) -> u32 {
        self.rate
    }
    /// Maximum samples consumed per call.
    pub fn input_buffer_size(&self) -> usize {
        self.input_buffer.len()
    }
    #[cfg(test)]
    pub fn cut_off_frequencies(&self) -> &[f32] {
        &self.cut_off_frequency
    }

    // Index loops mirror cavacore.c line by line on purpose.
    #[allow(clippy::needless_range_loop)]
    /// Feed interleaved samples (scaled to the 16-bit range, as cava does)
    /// and compute the next frame into `out` (`bars * channels` values).
    pub fn execute(&mut self, input: &[f64], out: &mut [f64]) {
        let ch = self.channels;
        let nbars = self.bars;
        let new_samples = input.len().min(self.input_buffer.len());
        let input = &input[..new_samples];
        let mut silence = true;

        if new_samples > 0 {
            self.framerate -= self.framerate / 64.0;
            self.framerate += (self.rate as f64 * self.frame_skip as f64) / (new_samples / ch).max(1) as f64 / 64.0;
            self.frame_skip = 1;
            // Shift history right, newest samples (reversed) at the front.
            let len = self.input_buffer.len();
            self.input_buffer.copy_within(0..len - new_samples, new_samples);
            for (n, &s) in input.iter().enumerate() {
                self.input_buffer[new_samples - n - 1] = s;
                if s != 0.0 {
                    silence = false;
                }
            }
        } else {
            self.frame_skip += 1;
        }

        // De-interleave + window. Channel 0 = "right", 1 = "left" (cava order).
        for i in 0..self.bass_size {
            let w = self.bass_window[i];
            if ch == 2 {
                self.buf_bass[1][i] = Complex::new(w * self.input_buffer[i * 2 + 1], 0.0);
                self.buf_bass[0][i] = Complex::new(w * self.input_buffer[i * 2], 0.0);
            } else {
                self.buf_bass[1][i] = Complex::new(w * self.input_buffer[i], 0.0);
            }
        }
        for i in 0..self.fft_size {
            let w = self.window[i];
            if ch == 2 {
                self.buf[1][i] = Complex::new(w * self.input_buffer[i * 2 + 1], 0.0);
                self.buf[0][i] = Complex::new(w * self.input_buffer[i * 2], 0.0);
            } else {
                self.buf[1][i] = Complex::new(w * self.input_buffer[i], 0.0);
            }
        }
        let used: &[usize] = if ch == 2 { &[1, 0] } else { &[1] };
        for &c in used {
            self.fft_bass
                .process_with_scratch(&mut self.buf_bass[c], &mut self.scratch);
            self.fft.process_with_scratch(&mut self.buf[c], &mut self.scratch);
        }

        for n in 0..nbars {
            let (lo, hi) = (self.lower[n], self.upper[n]);
            let mut temp = [0.0f64; 2];
            for (slot, &c) in used.iter().enumerate() {
                let spectrum = if n < self.bass_cut_off_bar {
                    &self.buf_bass[c]
                } else {
                    &self.buf[c]
                };
                let mut acc = 0.0;
                if lo >= 0 {
                    for i in lo..=hi {
                        if let Some(v) = spectrum.get(i as usize) {
                            acc += v.re.hypot(v.im);
                        }
                    }
                }
                temp[slot] = acc * self.eq[n];
            }
            out[n] = temp[0];
            if ch == 2 {
                out[n + nbars] = temp[1];
            }
        }

        let total = nbars * ch;
        if self.autosens {
            for v in out.iter_mut().take(total) {
                *v *= self.sens;
            }
        }

        let mut overshoot = false;
        let framerate_mod = 66.0 / self.framerate;
        let gravity_mod = framerate_mod.powf(2.5) * 2.0 / self.noise_reduction;
        let integral_mod = framerate_mod.powf(0.1);

        for n in 0..total {
            if out[n] < self.prev_out[n] && self.noise_reduction > 0.1 {
                out[n] = self.peak[n] * (1.0 - (self.fall[n] * self.fall[n] * gravity_mod));
                if out[n] < 0.0 {
                    out[n] = 0.0;
                }
                self.fall[n] += 0.028;
            } else {
                self.peak[n] = out[n];
                self.fall[n] = 0.0;
            }
            self.prev_out[n] = out[n];

            out[n] += self.mem[n] * self.noise_reduction / integral_mod;
            self.mem[n] = out[n];

            if self.autosens && out[n] > 1.0 {
                overshoot = true;
                out[n] = 1.0;
            }
        }

        if self.autosens {
            if overshoot {
                self.sens *= 1.0 - 0.02 * framerate_mod;
                self.sens_init = false;
            } else if !silence {
                self.sens *= 1.0 + 0.001 * framerate_mod;
                if self.sens_init {
                    self.sens *= 1.0 + 0.1 * framerate_mod;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine_frames(core: &mut CavaCore, freqs: &[f64], frames: usize) -> Vec<f64> {
        let rate = core.rate() as f64;
        let chunk = 512; // stereo frames per call
        let mut out = vec![0.0; core.bars() * core.channels()];
        let mut t = 0usize;
        for _ in 0..frames {
            let mut input = Vec::with_capacity(chunk * 2);
            for _ in 0..chunk {
                let s: f64 = freqs
                    .iter()
                    .map(|f| (2.0 * std::f64::consts::PI * f * t as f64 / rate).sin())
                    .sum::<f64>()
                    / freqs.len() as f64;
                let v = s * 0.5 * 65535.0;
                input.push(v);
                input.push(v);
                t += 1;
            }
            core.execute(&input, &mut out);
        }
        out
    }

    fn bar_for(core: &CavaCore, freq: f32) -> usize {
        let cut = core.cut_off_frequencies();
        (0..core.bars()).find(|&n| freq >= cut[n] && freq < cut[n + 1]).unwrap()
    }

    #[test]
    fn rejects_bad_params() {
        assert!(CavaCore::new(10, 44100, 3, true, 0.77, 50, 10000).is_err());
        assert!(CavaCore::new(0, 44100, 2, true, 0.77, 50, 10000).is_err());
        assert!(CavaCore::new(10, 44100, 2, true, 0.77, 10000, 50).is_err());
        assert!(CavaCore::new(10, 44100, 2, true, 0.77, 50, 30000).is_err());
    }

    #[test]
    fn cut_offs_are_increasing() {
        let core = CavaCore::new(40, 44100, 2, true, 0.77, 50, 10000).unwrap();
        let c = core.cut_off_frequencies();
        for w in c.windows(2) {
            assert!(w[1] > w[0], "{c:?}");
        }
        assert!(c[0] > 30.0 && c[0] < 70.0, "{}", c[0]);
    }

    /// Same check as cava's cavacore_test.c: a 200 Hz and a 2000 Hz tone
    /// light up the bars containing those frequencies the most.
    #[test]
    fn tones_land_in_expected_bars() {
        let mut core = CavaCore::new(10, 44100, 2, true, 0.77, 50, 10000).unwrap();
        let out = sine_frames(&mut core, &[200.0, 2000.0], 300);
        let left = &out[..10];
        let right = &out[10..];
        let b200 = bar_for(&core, 200.0);
        let b2000 = bar_for(&core, 2000.0);
        let mut ranked: Vec<usize> = (0..10).collect();
        ranked.sort_by(|a, b| left[*b].partial_cmp(&left[*a]).unwrap());
        assert!(ranked[..2].contains(&b200), "200Hz bar {b200} not top: {left:?}");
        assert!(ranked[..2].contains(&b2000), "2kHz bar {b2000} not top: {left:?}");
        for (l, r) in left.iter().zip(right) {
            assert!((l - r).abs() < 1e-9);
        }
        assert!(left.iter().all(|v| (0.0..=1.0).contains(v)));
    }

    #[test]
    fn silence_decays_to_zero() {
        let mut core = CavaCore::new(16, 44100, 2, true, 0.77, 50, 10000).unwrap();
        sine_frames(&mut core, &[440.0], 100);
        let mut out = vec![0.0; 32];
        let zeros = vec![0.0; 1024];
        for _ in 0..400 {
            core.execute(&zeros, &mut out);
        }
        assert!(out.iter().all(|v| *v < 1e-3), "{out:?}");
    }
}
