//! The processing path between the two devices: resample (rate conversion
//! plus drift control) and equalize. Pure and synchronous, so it is tested
//! without any audio hardware.

use crate::chain::EqChain;
use crate::resample::{AdaptiveResampler, drift_ratio};

pub struct Bridge {
    resampler: AdaptiveResampler,
    pub chain: EqChain,
    base_ratio: f64,
    target_frames: usize,
}

impl Bridge {
    /// `in_rate` and `out_rate` are the device sample rates; `target_frames`
    /// the output queue length (in frames at `out_rate`) to hold.
    pub fn new(freqs: &[f64], q: f64, in_rate: f64, out_rate: f64, target_frames: usize) -> Self {
        Self {
            resampler: AdaptiveResampler::new(),
            chain: EqChain::new(freqs, q, out_rate),
            base_ratio: in_rate / out_rate,
            target_frames,
        }
    }

    /// Converts one block of input frames (interleaved L/R at the input
    /// rate) and appends the equalized result at the output rate to `out`.
    /// `out_fill_frames` is how many frames are already waiting to be played.
    pub fn process(&mut self, input: &[f32], out_fill_frames: usize, out: &mut Vec<f32>) {
        let ratio = drift_ratio(self.base_ratio, out_fill_frames, self.target_frames);
        let start = out.len();
        self.resampler.process(input, ratio, out);
        self.chain.process(&mut out[start..]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FREQS: [f64; 8] = [63.0, 125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0];

    fn sine_block(freq: f64, rate: f64, start: usize, frames: usize) -> Vec<f32> {
        (0..frames)
            .flat_map(|i| {
                let t = (start + i) as f64 / rate;
                let v = 0.4 * (std::f64::consts::TAU * freq * t).sin() as f32;
                [v, v]
            })
            .collect()
    }

    fn zero_crossings(x: &[f32]) -> usize {
        x.iter()
            .step_by(2)
            .copied()
            .collect::<Vec<_>>()
            .windows(2)
            .filter(|w| w[0] <= 0.0 && w[1] > 0.0)
            .count()
    }

    #[test]
    fn rate_conversion_keeps_pitch_and_level() {
        // 1 kHz at 44.1 kHz in, 48 kHz out, flat EQ.
        let mut b = Bridge::new(&FREQS, 1.41, 44_100.0, 48_000.0, 2880);
        let mut out = Vec::new();
        let mut pos = 0;
        for _ in 0..300 {
            let block = sine_block(1000.0, 44_100.0, pos, 441);
            pos += 441;
            b.process(&block, 2880, &mut out);
        }
        let frames = out.len() / 2;
        let seconds = frames as f64 / 48_000.0;
        let hz = zero_crossings(&out) as f64 / seconds;
        assert!((hz - 1000.0).abs() < 5.0, "pitch {hz} Hz");
        let peak = out.iter().cloned().fold(0.0f32, |a, v| a.max(v.abs()));
        assert!((peak - 0.4).abs() < 0.02, "peak {peak}");
    }

    #[test]
    fn eq_is_applied_after_conversion() {
        let mut b = Bridge::new(&FREQS, 1.41, 48_000.0, 48_000.0, 2880);
        let mut g = [0.0; 8];
        g[4] = -6.0;
        b.chain.set_target(&g);
        let mut out = Vec::new();
        let mut pos = 0;
        for _ in 0..400 {
            let block = sine_block(1000.0, 48_000.0, pos, 256);
            pos += 256;
            b.process(&block, 2880, &mut out);
        }
        let tail = &out[out.len() - 2 * 2048..];
        let rms =
            (tail.iter().map(|&v| (v as f64).powi(2)).sum::<f64>() / tail.len() as f64).sqrt();
        let in_rms = 0.4 / std::f64::consts::SQRT_2;
        let db = 20.0 * (rms / in_rms).log10();
        assert!((db + 6.0).abs() < 0.4, "{db}");
    }

    #[test]
    fn a_full_queue_makes_it_produce_fewer_frames() {
        let run = |fill: usize| {
            let mut b = Bridge::new(&FREQS, 1.41, 48_000.0, 48_000.0, 2400);
            let mut out = Vec::new();
            for _ in 0..200 {
                b.process(&vec![0.0f32; 2 * 256], fill, &mut out);
            }
            out.len() / 2
        };
        assert!(run(6000) < run(2400));
        assert!(run(0) > run(2400));
    }
}
