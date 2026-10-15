//! Streaming linear-interpolation resampler for interleaved stereo with a
//! ratio that can change on every call, used both to convert between device
//! sample rates and to absorb clock drift between two devices.

pub struct AdaptiveResampler {
    /// Last input frame of the previous block (the interpolation anchor).
    prev: [f32; 2],
    /// Read position in input frames, relative to `prev` (0 is `prev`).
    pos: f64,
    primed: bool,
}

impl Default for AdaptiveResampler {
    fn default() -> Self {
        Self::new()
    }
}

impl AdaptiveResampler {
    pub fn new() -> Self {
        Self {
            prev: [0.0; 2],
            pos: 0.0,
            primed: false,
        }
    }

    /// Resamples `input` (interleaved L/R) and appends to `out`. `ratio` is
    /// input frames consumed per output frame: 1.0 is a copy, 2.0 halves the
    /// rate. Returns the number of output frames appended.
    pub fn process(&mut self, input: &[f32], ratio: f64, out: &mut Vec<f32>) -> usize {
        let frames = input.len() / 2;
        if frames == 0 {
            return 0;
        }
        if !self.primed {
            // Position 0 is the anchor, which on the very first block is not a
            // real frame. Start at position 1, the first real frame, so the
            // stream neither begins with a repeated sample nor a click.
            self.prev = [input[0], input[1]];
            self.pos = 1.0;
            self.primed = true;
        }
        let ratio = ratio.clamp(0.25, 4.0);
        let at = |i: usize, c: usize| -> f32 {
            if i == 0 {
                self.prev[c]
            } else {
                input[(i - 1) * 2 + c]
            }
        };
        let mut produced = 0;
        while self.pos < frames as f64 {
            let i = self.pos.floor() as usize;
            let frac = (self.pos - i as f64) as f32;
            for c in 0..2 {
                let a = at(i, c);
                let b = at(i + 1, c);
                out.push(a + (b - a) * frac);
            }
            produced += 1;
            self.pos += ratio;
        }
        self.pos -= frames as f64;
        self.prev = [input[(frames - 1) * 2], input[(frames - 1) * 2 + 1]];
        produced
    }
}

/// Input-frames-per-output-frame ratio that steers the output queue toward
/// `target` frames. A queue that is too full needs fewer output frames per
/// input frame, i.e. a larger ratio. The correction is clamped to +-0.5% so
/// it can never be heard as a pitch change.
pub fn drift_ratio(base: f64, fill_frames: usize, target_frames: usize) -> f64 {
    if target_frames == 0 {
        return base;
    }
    let err = (fill_frames as f64 - target_frames as f64) / target_frames as f64;
    base * (1.0 + (0.02 * err).clamp(-0.005, 0.005))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp(n: usize) -> Vec<f32> {
        (0..n).flat_map(|i| [i as f32, -(i as f32)]).collect()
    }

    #[test]
    fn ratio_one_is_a_copy() {
        let mut r = AdaptiveResampler::new();
        let input = ramp(100);
        let mut out = Vec::new();
        let n = r.process(&input, 1.0, &mut out);
        // the last frame of a block is held back as the next anchor
        assert_eq!(n, 99);
        assert_eq!(out, input[..198].to_vec());
    }

    #[test]
    fn ratio_two_halves_the_frame_count_and_keeps_values() {
        let mut r = AdaptiveResampler::new();
        let mut out = Vec::new();
        let n = r.process(&ramp(100), 2.0, &mut out);
        assert!((49..=51).contains(&n), "{n}");
        // a ramp stays a ramp: output frame k sits at input frame 2k
        for k in 1..n.min(40) {
            assert!(
                (out[k * 2] - (2 * k) as f32).abs() < 1e-3,
                "k={k} {}",
                out[k * 2]
            );
        }
    }

    #[test]
    fn blocks_join_without_a_seam() {
        let whole = ramp(400);
        let mut a = AdaptiveResampler::new();
        let mut one = Vec::new();
        a.process(&whole, 1.37, &mut one);

        let mut b = AdaptiveResampler::new();
        let mut split = Vec::new();
        for chunk in whole.chunks(2 * 37) {
            b.process(chunk, 1.37, &mut split);
        }
        let n = one.len().min(split.len());
        assert!(n > 200);
        for i in 0..n {
            assert!((one[i] - split[i]).abs() < 1e-3, "i={i}");
        }
    }

    #[test]
    fn total_output_follows_the_ratio_over_many_blocks() {
        let mut r = AdaptiveResampler::new();
        let mut out = Vec::new();
        let block = vec![0.0f32; 2 * 256];
        for _ in 0..400 {
            r.process(&block, 44_100.0 / 48_000.0, &mut out);
        }
        let frames_out = (out.len() / 2) as f64;
        let expected = 400.0 * 256.0 * 48_000.0 / 44_100.0;
        assert!(
            (frames_out - expected).abs() < 3.0,
            "{frames_out} vs {expected}"
        );
    }

    #[test]
    fn drift_correction_is_bounded_and_signed() {
        assert_eq!(drift_ratio(1.0, 1000, 1000), 1.0);
        assert!(drift_ratio(1.0, 4000, 1000) > 1.0);
        assert!(drift_ratio(1.0, 0, 1000) < 1.0);
        assert!(drift_ratio(1.0, 10_000_000, 1000) <= 1.005 + 1e-12);
        assert!(drift_ratio(1.0, 0, 1000) >= 0.995 - 1e-12);
    }

    #[test]
    fn controller_keeps_a_drifting_queue_bounded() {
        // Producer runs 0.04% fast against the consumer. Without control the
        // queue grows without bound; with it, it settles near the target.
        let target = 2400usize;
        let mut fill = target as f64;
        let mut worst = 0.0f64;
        let mut r = AdaptiveResampler::new();
        let block = vec![0.1f32; 2 * 256];
        let mut out = Vec::new();
        for _ in 0..20_000 {
            out.clear();
            let ratio = drift_ratio(1.0, fill.max(0.0) as usize, target);
            let produced = r.process(&block, ratio, &mut out) as f64;
            // consumer drains exactly 256 * (1 - 0.0004) frames per block
            fill += produced - 256.0 * (1.0 - 0.0004);
            worst = worst.max((fill - target as f64).abs());
        }
        assert!(worst < 600.0, "queue wandered {worst} frames from target");
    }
}
