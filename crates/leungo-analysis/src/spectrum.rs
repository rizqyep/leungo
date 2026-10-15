//! Overlapping FFT frames with an exponentially averaged power spectrum.
//! The average is per track: the engine resets it on every track change.

use crate::hpss::Hpss;
use crate::robust::{BandTracker, Grid};
use crate::subbass::SubBass;
use num_complex::Complex;
use realfft::{RealFftPlanner, RealToComplex};
use std::sync::Arc;

/// One processed hop of audio.
#[derive(Clone, Copy, Debug)]
pub struct FrameResult {
    /// RMS of the mono mix for this frame, dBFS.
    pub rms_db: f64,
    /// Peak absolute sample of the mono mix.
    pub peak: f64,
    /// Sum of squares of the mono mix (for windowed crest factor).
    pub sq_sum: f64,
    /// Samples in the frame (for crest factor normalization).
    pub samples: usize,
    /// Mean positive spectral flux in dB per bin.
    pub flux: f64,
    /// True when the frame is below the silence threshold.
    pub silent: bool,
}

/// Audio needed before the codec edge is trusted.
const EDGE_MIN_S: f64 = 5.0;

pub struct SpectrumAnalyzer {
    fft_size: usize,
    hop: usize,
    fs: f64,
    window: Vec<f64>,
    r2c: Arc<dyn RealToComplex<f64>>,
    input: Vec<f64>,
    output: Vec<Complex<f64>>,
    chan_power: Vec<f64>,
    buf_l: Vec<f64>,
    buf_r: Vec<f64>,
    avg_power: Vec<f64>,
    prev_db: Option<Vec<f64>>,
    ema_alpha: f64,
    tracker: Option<BandTracker>,
    sub: Option<SubBass>,
    hpss: Option<Hpss>,
    /// Plain mean power over the track's audible frames, for the codec edge.
    track_power: Vec<f64>,
    track_frames: usize,
}

impl SpectrumAnalyzer {
    pub fn new(fft_size: usize, hop: usize, tau_s: f64, fs: f64) -> Self {
        assert!(hop > 0 && hop <= fft_size, "invalid hop");
        let mut planner = RealFftPlanner::<f64>::new();
        let r2c = planner.plan_fft_forward(fft_size);
        let window: Vec<f64> = (0..fft_size)
            .map(|n| 0.5 * (1.0 - (std::f64::consts::TAU * n as f64 / (fft_size - 1) as f64).cos()))
            .collect();
        let nbins = fft_size / 2 + 1;
        let ema_alpha = (hop as f64 / fs / tau_s).clamp(0.0, 1.0);
        let input = r2c.make_input_vec();
        let output = r2c.make_output_vec();
        Self {
            fft_size,
            hop,
            fs,
            window,
            r2c,
            input,
            output,
            chan_power: vec![0.0; nbins],
            buf_l: Vec::new(),
            buf_r: Vec::new(),
            avg_power: vec![0.0; nbins],
            prev_db: None,
            ema_alpha,
            tracker: None,
            sub: None,
            hpss: None,
            track_power: vec![0.0; nbins],
            track_frames: 0,
        }
    }

    pub fn fft_size(&self) -> usize {
        self.fft_size
    }

    pub fn fs(&self) -> f64 {
        self.fs
    }

    pub fn avg_power(&self) -> &[f64] {
        &self.avg_power
    }

    pub fn bin_freq(&self, i: usize) -> f64 {
        i as f64 * self.fs / self.fft_size as f64
    }

    /// Zeroes the long-term average; called at every track start.
    pub fn reset_spectrum(&mut self) {
        self.avg_power.fill(0.0);
        self.track_power.fill(0.0);
        self.track_frames = 0;
        if let Some(t) = self.tracker.as_mut() {
            t.clear();
        }
        if let Some(s) = self.sub.as_mut() {
            s.reset();
        }
        if let Some(h) = self.hpss.as_mut() {
            h.reset();
        }
    }

    /// Starts keeping per-frame levels at these 1/3-octave centers, up to
    /// `cap_frames` frames, for the robust shape and reliability estimates.
    /// Bass bands come from the fine path (`subbass`).
    pub fn enable_band_tracker(&mut self, centers: &[f64], cap_frames: usize) {
        let sub = SubBass::new(self.fs);
        let main = Grid {
            fs: self.fs,
            fft_size: self.fft_size,
        };
        let sub_grid = Grid {
            fs: sub.fs(),
            fft_size: sub.fft_size(),
        };
        let tracker = BandTracker::new(centers, main, Some(sub_grid), cap_frames);
        self.sub = tracker.uses_sub().then_some(sub);
        self.tracker = Some(tracker);
        let bands = centers
            .iter()
            .map(|&c| crate::robust::bins(c, main))
            .collect();
        self.hpss = Some(Hpss::new(bands));
    }

    /// Per tracked band, how much of the power is drum hits rather than
    /// tones (see `hpss`).
    pub fn percussive_share(&self) -> Option<Vec<f64>> {
        self.hpss.as_ref()?.percussive_share()
    }

    /// Upper edge of the track's content (codec lowpass), once a few
    /// seconds of audio have been seen.
    pub fn bandwidth_edge(&self) -> Option<f64> {
        if self.track_frames < self.frames_for(EDGE_MIN_S) {
            return None;
        }
        crate::edge::bandwidth_edge(&self.track_power, self.fs / self.fft_size as f64)
    }

    pub fn tracker(&self) -> Option<&BandTracker> {
        self.tracker.as_ref()
    }

    /// Frames corresponding to `seconds` of audio.
    pub fn frames_for(&self, seconds: f64) -> usize {
        ((seconds * self.fs / self.hop as f64).round() as usize).max(1)
    }

    /// Averaged power in dB summed over the band `lo..hi` Hz.
    pub fn band_level_db(&self, lo: f64, hi: f64) -> f64 {
        let mut sum = 0.0;
        for (i, &p) in self.avg_power.iter().enumerate() {
            let f = self.bin_freq(i);
            if f >= lo && f < hi {
                sum += p;
            }
        }
        10.0 * (sum + 1e-20).log10()
    }

    /// Deinterleaves a capture chunk and processes every full hop.
    pub fn push_interleaved(
        &mut self,
        chunk: &[f32],
        channels: usize,
        silence_threshold_db: f64,
    ) -> Vec<FrameResult> {
        for (i, &s) in chunk.iter().enumerate() {
            let s = s as f64;
            if channels == 1 {
                self.buf_l.push(s);
                self.buf_r.push(s);
            } else if i % 2 == 0 {
                self.buf_l.push(s);
            } else {
                self.buf_r.push(s);
            }
        }
        if let Some(sub) = self.sub.as_mut() {
            let fresh = chunk.len() / channels.max(1);
            let start = self.buf_l.len() - fresh;
            sub.push(&self.buf_l[start..], &self.buf_r[start..]);
        }
        let mut results = Vec::new();
        while self.buf_l.len() >= self.fft_size {
            let frame_l = self.buf_l[..self.fft_size].to_vec();
            let frame_r = self.buf_r[..self.fft_size].to_vec();
            self.buf_l.drain(..self.hop);
            self.buf_r.drain(..self.hop);
            results.push(self.process_frame(&frame_l, &frame_r, silence_threshold_db));
        }
        results
    }

    fn process_frame(&mut self, l: &[f64], r: &[f64], threshold_db: f64) -> FrameResult {
        let n = self.fft_size;
        let mut sq = 0.0;
        let mut peak = 0.0f64;
        for i in 0..n {
            let m = 0.5 * (l[i] + r[i]);
            sq += m * m;
            let a = m.abs();
            if a > peak {
                peak = a;
            }
        }
        let rms = (sq / n as f64).sqrt();
        let rms_db = 20.0 * rms.max(1e-10).log10();
        let silent = rms_db < threshold_db;

        self.channel_power(l);
        let power_l: Vec<f64> = self.chan_power.clone();
        self.channel_power(r);
        let nbins = n / 2 + 1;

        if let Some(h) = self.hpss.as_mut() {
            let mono: Vec<f64> = power_l
                .iter()
                .zip(&self.chan_power)
                .map(|(a, b)| a + b)
                .collect();
            h.push(&mono, silent);
        }

        let mut mono_db = Vec::with_capacity(nbins);
        for (pl, pr) in power_l.iter().zip(self.chan_power.iter()) {
            mono_db.push(10.0 * (pl + pr + 1e-20).log10());
        }

        if !silent {
            let mono: Vec<f64> = power_l
                .iter()
                .zip(self.chan_power.iter())
                .map(|(a, b)| a + b)
                .collect();
            if let Some(t) = self.tracker.as_mut() {
                match self.sub.as_ref().map(SubBass::power) {
                    // fine path still filling its first window
                    Some(None) => {}
                    Some(sub) => t.push(&mono, sub),
                    None => t.push(&mono, None),
                }
            }
            for (acc, p) in self.track_power.iter_mut().zip(&mono) {
                *acc += (p - *acc) / (self.track_frames + 1) as f64;
            }
            self.track_frames += 1;
            let alpha = self.ema_alpha;
            for (avg, (pl, pr)) in self
                .avg_power
                .iter_mut()
                .zip(power_l.iter().zip(self.chan_power.iter()))
            {
                let p = pl + pr;
                *avg = (1.0 - alpha) * *avg + alpha * p;
            }
        }

        let flux = match &self.prev_db {
            Some(prev) => {
                let sum: f64 = mono_db
                    .iter()
                    .zip(prev.iter())
                    .map(|(a, b)| (a - b).max(0.0))
                    .sum();
                sum / nbins as f64
            }
            None => 0.0,
        };
        self.prev_db = Some(mono_db);

        FrameResult {
            rms_db,
            peak,
            sq_sum: sq,
            samples: n,
            flux,
            silent,
        }
    }

    fn channel_power(&mut self, x: &[f64]) {
        let n = self.fft_size;
        for (dst, (w, s)) in self.input.iter_mut().zip(self.window.iter().zip(x.iter())) {
            *dst = w * s;
        }
        self.r2c
            .process(&mut self.input, &mut self.output)
            .expect("fft buffers sized by plan");
        let nbins = n / 2 + 1;
        for (dst, c) in self
            .chan_power
            .iter_mut()
            .zip(self.output.iter().take(nbins))
        {
            *dst = c.re * c.re + c.im * c.im;
        }
    }

    /// Log-spaced (frequency, dB) snapshot of the long-term average. Segments
    /// report the loudest bin they contain, or the nearest bin when the
    /// segment is narrower than one bin. Used by the GUI.
    pub fn snapshot_log_spectrum(&self, num_points: usize, lo: f64, hi: f64) -> Vec<(f64, f64)> {
        let bin_w = self.fs / self.fft_size as f64;
        let nbins = self.fft_size / 2 + 1;
        let mut out = Vec::with_capacity(num_points);
        let (llo, lhi) = (lo.max(1.0).ln(), hi.max(1.0).ln());
        for i in 0..num_points {
            let f0 = (llo + (lhi - llo) * i as f64 / num_points as f64).exp();
            let f1 = (llo + (lhi - llo) * (i + 1) as f64 / num_points as f64).exp();
            let mut b0 = (f0 / bin_w).floor() as usize;
            let mut b1 = ((f1 / bin_w).ceil() as usize).min(nbins);
            if b1 <= b0 {
                // segment narrower than a bin: use the nearest bin instead
                let center = 0.5 * (f0 + f1);
                let b = ((center / bin_w).round() as usize).min(nbins - 1);
                b0 = b;
                b1 = b + 1;
            }
            let mut max = 0.0f64;
            for &p in &self.avg_power[b0..b1] {
                if p > max {
                    max = p;
                }
            }
            let db = 10.0 * (max + 1e-20).log10();
            let center = (f0 * f1).sqrt();
            out.push((center, db));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn analyzer() -> SpectrumAnalyzer {
        SpectrumAnalyzer::new(2048, 1024, 2.0, 48_000.0)
    }

    fn push_tone(spec: &mut SpectrumAnalyzer, freq: f64, amp: f64, seconds: f64) {
        let w = std::f64::consts::TAU * freq / spec.fs();
        let samples: Vec<f32> = (0..(seconds * spec.fs()) as usize)
            .map(|i| (amp * (w * i as f64).sin()) as f32)
            .collect();
        // mono: duplicate into both channels
        let mut interleaved = Vec::with_capacity(samples.len() * 2);
        for s in &samples {
            interleaved.push(*s);
            interleaved.push(*s);
        }
        spec.push_interleaved(&interleaved, 2, -60.0);
    }

    #[test]
    fn tone_peaks_in_its_band() {
        let mut spec = analyzer();
        push_tone(&mut spec, 1000.0, 0.5, 4.0);
        let mid = spec.band_level_db(500.0, 2000.0);
        let bass = spec.band_level_db(60.0, 250.0);
        let treble = spec.band_level_db(6000.0, 16000.0);
        assert!(mid > bass + 20.0, "mid {mid} vs bass {bass}");
        assert!(mid > treble + 20.0, "mid {mid} vs treble {treble}");
    }

    #[test]
    fn centroid_matches_tone_frequency() {
        let mut spec = analyzer();
        push_tone(&mut spec, 1000.0, 0.5, 4.0);
        let mut total = 0.0;
        let mut weighted = 0.0;
        for (i, &p) in spec.avg_power().iter().enumerate() {
            let f = spec.bin_freq(i);
            if (40.0..=16_000.0).contains(&f) {
                total += p;
                weighted += f * p;
            }
        }
        let centroid = weighted / total;
        assert!((centroid - 1000.0).abs() < 25.0, "centroid {centroid}");
    }

    /// Pink noise (Paul Kellet's filter on a fixed-seed white source).
    fn pink(n: usize, seed: u64) -> Vec<f32> {
        let mut s = seed;
        let mut b = [0.0f64; 7];
        (0..n)
            .map(|_| {
                s = s
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                let w = (s >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0;
                b[0] = 0.99886 * b[0] + w * 0.0555179;
                b[1] = 0.99332 * b[1] + w * 0.0750759;
                b[2] = 0.96900 * b[2] + w * 0.1538520;
                b[3] = 0.86650 * b[3] + w * 0.3104856;
                b[4] = 0.55000 * b[4] + w * 0.5329522;
                b[5] = -0.7616 * b[5] - w * 0.0168980;
                let p = b.iter().sum::<f64>() + w * 0.5362;
                b[6] = w * 0.115926;
                (p * 0.05) as f32
            })
            .collect()
    }

    #[test]
    fn pink_noise_reads_flat_across_both_paths() {
        let mut spec = analyzer();
        let centers = leungo_eq::third_octave_centers(25.0, 16_000.0);
        spec.enable_band_tracker(&centers, 100_000);
        let (l, r) = (pink(48_000 * 60, 1), pink(48_000 * 60, 2));
        let mut x = Vec::with_capacity(2 * l.len());
        for (a, b) in l.iter().zip(&r) {
            x.push(*a);
            x.push(*b);
        }
        for c in x.chunks(2048) {
            spec.push_interleaved(c, 2, -90.0);
        }
        let t = spec.tracker().unwrap();
        let shape = t.shape(t.len()).unwrap();
        let mean = shape.iter().sum::<f64>() / shape.len() as f64;
        // 25 Hz is only two fine bins wide and reads about +0.5 dB; it sits
        // below the correction floor, so it is held to a looser bound.
        for (f, v) in centers.iter().zip(&shape) {
            let tol = if *f < 30.0 { 0.75 } else { 0.5 };
            assert!(
                (v - mean).abs() < tol,
                "{f} Hz: {:+.2} dB  {shape:.2?}",
                v - mean
            );
        }
        assert_eq!(spec.bandwidth_edge(), None);
    }

    #[test]
    fn silence_flags_frames() {
        let mut spec = analyzer();
        let silent_chunk = vec![0.0f32; 4096];
        let frames = spec.push_interleaved(&silent_chunk, 2, -60.0);
        assert!(!frames.is_empty());
        assert!(frames.iter().all(|f| f.silent));
    }
}
