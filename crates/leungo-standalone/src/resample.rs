//! Streaming cubic (Catmull-Rom) resampler for interleaved stereo with a
//! ratio that can change on every call, used to convert between device sample
//! rates.

pub struct AdaptiveResampler {
    tail: Vec<[f32; 2]>,
    pos: f64,
    primed: bool,
}

impl Default for AdaptiveResampler {
    fn default() -> Self {
        Self::new()
    }
}

fn catmull_rom(p0: f32, p1: f32, p2: f32, p3: f32, t: f32) -> f32 {
    let t2 = t * t;
    let t3 = t2 * t;
    0.5 * (2.0 * p1
        + (p2 - p0) * t
        + (2.0 * p0 - 5.0 * p1 + 4.0 * p2 - p3) * t2
        + (3.0 * p1 - p0 - 3.0 * p2 + p3) * t3)
}

impl AdaptiveResampler {
    pub fn new() -> Self {
        Self {
            tail: Vec::new(),
            pos: 0.0,
            primed: false,
        }
    }

    /// Resamples `input` (interleaved L/R) and appends to `out`. `ratio` is
    /// input frames consumed per output frame: 1.0 is a copy, 2.0 halves the
    /// rate. Two frames are held back as lookahead. Returns the number of
    /// output frames appended.
    pub fn process(&mut self, input: &[f32], ratio: f64, out: &mut Vec<f32>) -> usize {
        let frames = input.len() / 2;
        if frames == 0 {
            return 0;
        }
        if !self.primed {
            self.tail = vec![[input[0], input[1]]; 2];
            self.pos = 2.0;
            self.primed = true;
        }
        let ratio = ratio.clamp(0.25, 4.0);
        let tail = std::mem::take(&mut self.tail);
        let n = tail.len() + frames;
        let at = |i: usize, c: usize| -> f32 {
            if i < tail.len() {
                tail[i][c]
            } else {
                input[(i - tail.len()) * 2 + c]
            }
        };
        let mut produced = 0;
        while (self.pos.floor() as usize) + 2 < n {
            let i = self.pos.floor() as usize;
            let t = (self.pos - i as f64) as f32;
            for c in 0..2 {
                out.push(catmull_rom(
                    at(i - 1, c),
                    at(i, c),
                    at(i + 1, c),
                    at(i + 2, c),
                    t,
                ));
            }
            produced += 1;
            self.pos += ratio;
        }
        let keep = 3.min(n);
        self.tail = (n - keep..n).map(|i| [at(i, 0), at(i, 1)]).collect();
        self.pos -= (n - keep) as f64;
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
        // two frames are held back as lookahead
        assert_eq!(n, 98);
        assert_eq!(out, input[..196].to_vec());
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
