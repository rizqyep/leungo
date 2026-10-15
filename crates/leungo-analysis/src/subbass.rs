//! Fine-resolution path for the bass bands. The main 2048-point FFT at
//! 48 kHz has 23 Hz bins, so everything below about 100 Hz lands in one or
//! two bins. Here the signal is lowpassed and decimated to about 6 kHz
//! first, and the same FFT size then gives about 2.9 Hz bins.

use num_complex::Complex;
use realfft::{RealFftPlanner, RealToComplex};
use std::sync::Arc;

/// Rate the decimated signal runs at, roughly.
const TARGET_FS: f64 = 6000.0;
/// Bands up to this center frequency are measured here.
pub const SUB_MAX_HZ: f64 = 250.0;
const FFT_SIZE: usize = 2048;
/// Decimated samples between FFTs (about 85 ms).
const HOP: usize = 512;
/// Lowpass before decimating. Only bands below 300 Hz are used, so the
/// filter has to stay flat there and stop what would alias back onto them.
const CUTOFF_HZ: f64 = 3000.0;
const TAPS_AT_48K: f64 = 97.0;

struct Channel {
    hist: Vec<f64>,
    out: Vec<f64>,
}

pub struct SubBass {
    decim: usize,
    fs: f64,
    taps: Vec<f64>,
    window: Vec<f64>,
    r2c: Arc<dyn RealToComplex<f64>>,
    input: Vec<f64>,
    spectrum: Vec<Complex<f64>>,
    ch: [Channel; 2],
    power: Option<Vec<f64>>,
}

impl SubBass {
    pub fn new(fs: f64) -> Self {
        let decim = (fs / TARGET_FS).round().max(1.0) as usize;
        let taps = lowpass(CUTOFF_HZ / fs, odd(TAPS_AT_48K * fs / 48_000.0));
        let window = (0..FFT_SIZE)
            .map(|n| 0.5 * (1.0 - (std::f64::consts::TAU * n as f64 / (FFT_SIZE - 1) as f64).cos()))
            .collect();
        let r2c = RealFftPlanner::<f64>::new().plan_fft_forward(FFT_SIZE);
        let (input, spectrum) = (r2c.make_input_vec(), r2c.make_output_vec());
        let ch = || Channel {
            hist: Vec::new(),
            out: Vec::new(),
        };
        Self {
            decim,
            fs: fs / decim as f64,
            taps,
            window,
            r2c,
            input,
            spectrum,
            ch: [ch(), ch()],
            power: None,
        }
    }

    pub fn fs(&self) -> f64 {
        self.fs
    }

    pub fn fft_size(&self) -> usize {
        FFT_SIZE
    }

    /// Latest L+R power spectrum, `None` until the first full window.
    pub fn power(&self) -> Option<&[f64]> {
        self.power.as_deref()
    }

    pub fn reset(&mut self) {
        for c in &mut self.ch {
            c.hist.clear();
            c.out.clear();
        }
        self.power = None;
    }

    /// Adds new samples of both channels (same length).
    pub fn push(&mut self, l: &[f64], r: &[f64]) {
        for (c, x) in self.ch.iter_mut().zip([l, r]) {
            c.hist.extend_from_slice(x);
            let mut pos = 0;
            while pos + self.taps.len() <= c.hist.len() {
                let y = c.hist[pos..pos + self.taps.len()]
                    .iter()
                    .zip(&self.taps)
                    .map(|(a, b)| a * b)
                    .sum();
                c.out.push(y);
                pos += self.decim;
            }
            c.hist.drain(..pos);
        }
        while self.ch[0].out.len() >= FFT_SIZE && self.ch[1].out.len() >= FFT_SIZE {
            let mut sum = vec![0.0; FFT_SIZE / 2 + 1];
            for k in 0..2 {
                self.channel_power(k, &mut sum);
                self.ch[k].out.drain(..HOP);
            }
            self.power = Some(sum);
        }
    }

    fn channel_power(&mut self, k: usize, acc: &mut [f64]) {
        for ((dst, w), s) in self.input.iter_mut().zip(&self.window).zip(&self.ch[k].out) {
            *dst = w * s;
        }
        self.r2c
            .process(&mut self.input, &mut self.spectrum)
            .expect("fft buffers sized by plan");
        for (a, c) in acc.iter_mut().zip(&self.spectrum) {
            *a += c.norm_sqr();
        }
    }
}

fn odd(n: f64) -> usize {
    (n.round() as usize) | 1
}

/// Blackman-windowed sinc, `fc` as a fraction of the sample rate, unity at DC.
fn lowpass(fc: f64, n: usize) -> Vec<f64> {
    let m = (n - 1) as f64;
    let mut h: Vec<f64> = (0..n)
        .map(|i| {
            let x = i as f64 - m / 2.0;
            let sinc = if x == 0.0 {
                2.0 * fc
            } else {
                (std::f64::consts::TAU * fc * x).sin() / (std::f64::consts::PI * x)
            };
            let t = std::f64::consts::TAU * i as f64 / m;
            sinc * (0.42 - 0.5 * t.cos() + 0.08 * (2.0 * t).cos())
        })
        .collect();
    let dc: f64 = h.iter().sum();
    h.iter_mut().for_each(|v| *v /= dc);
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gain_db(h: &[f64], f: f64) -> f64 {
        let w = std::f64::consts::TAU * f;
        let (re, im) = h.iter().enumerate().fold((0.0, 0.0), |(re, im), (i, &v)| {
            (re + v * (w * i as f64).cos(), im - v * (w * i as f64).sin())
        });
        10.0 * (re * re + im * im).log10()
    }

    #[test]
    fn lowpass_is_flat_in_the_bass_and_stops_what_would_alias() {
        let fs = 48_000.0;
        let h = lowpass(CUTOFF_HZ / fs, odd(TAPS_AT_48K));
        for f in [20.0, 100.0, 300.0] {
            assert!(gain_db(&h, f / fs).abs() < 0.01, "{f} Hz");
        }
        // anything that folds onto 0..300 Hz after decimating by 8
        for f in [5700.0, 6000.0, 6300.0, 12_000.0, 18_000.0] {
            assert!(
                gain_db(&h, f / fs) < -60.0,
                "{f} Hz: {}",
                gain_db(&h, f / fs)
            );
        }
    }

    #[test]
    fn a_tone_lands_in_the_right_fine_bin() {
        let fs = 48_000.0;
        let mut s = SubBass::new(fs);
        let w = std::f64::consts::TAU * 41.2 / fs;
        let x: Vec<f64> = (0..fs as usize).map(|i| (w * i as f64).sin()).collect();
        for c in x.chunks(1024) {
            s.push(c, c);
        }
        let p = s.power().unwrap();
        let peak = (0..p.len()).max_by(|&a, &b| p[a].total_cmp(&p[b])).unwrap();
        let f = peak as f64 * s.fs() / s.fft_size() as f64;
        assert!((f - 41.2).abs() < 1.5, "peak at {f}");
    }
}
