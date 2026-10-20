//! Gain that makes up for the loopback device's own volume attenuation.
//!
//! The loopback device applies its volume to samples that are already
//! queued, so a new volume shows up in the audio some time after the
//! makeup value changes. Cuts in makeup apply at once and rises wait
//! `rise_delay` frames, so a mismatch dips the level instead of clipping.

pub struct MakeupGain {
    current: f32,
    pending: f32,
    held_frames: usize,
    rise_delay: usize,
}

impl MakeupGain {
    pub fn new(rise_delay: usize) -> Self {
        Self {
            current: 1.0,
            pending: 1.0,
            held_frames: 0,
            rise_delay,
        }
    }

    pub fn current(&self) -> f32 {
        self.current
    }

    pub fn process(&mut self, target: f32, io: &mut [f32]) {
        let target = if target.is_finite() {
            target.max(0.0)
        } else {
            1.0
        };
        let frames = io.len() / 2;
        if target != self.pending {
            self.pending = target;
            self.held_frames = 0;
        }
        let next = if target <= self.current {
            target
        } else {
            self.held_frames += frames;
            if self.held_frames >= self.rise_delay {
                target
            } else {
                self.current
            }
        };
        let start = self.current;
        self.current = next;
        if start == 1.0 && next == 1.0 {
            return;
        }
        let ramp = frames.clamp(1, 128) as f32;
        for (i, frame) in io.as_chunks_mut::<2>().0.iter_mut().enumerate() {
            let t = (i as f32 / ramp).min(1.0);
            let g = start + (next - start) * t;
            frame[0] *= g;
            frame[1] *= g;
        }
    }
}

pub fn soft_limit(io: &mut [f32]) {
    const KNEE: f32 = 0.9;
    for x in io.iter_mut() {
        let a = x.abs();
        if a > KNEE {
            *x = x.signum() * (KNEE + (1.0 - KNEE) * ((a - KNEE) / (1.0 - KNEE)).tanh());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(v: f32, frames: usize) -> Vec<f32> {
        vec![v; frames * 2]
    }

    #[test]
    fn unity_is_untouched() {
        let mut m = MakeupGain::new(1000);
        let mut x = block(0.3, 256);
        m.process(1.0, &mut x);
        assert!(x.iter().all(|&v| v == 0.3));
    }

    #[test]
    fn a_rise_waits_for_the_delay_then_ramps_in() {
        let mut m = MakeupGain::new(1000);
        let mut x = block(0.01, 512);
        m.process(100.0, &mut x);
        assert_eq!(m.current(), 1.0);
        assert!(x.iter().all(|&v| v == 0.01));
        let mut x = block(0.01, 512);
        m.process(100.0, &mut x);
        assert_eq!(m.current(), 100.0);
        assert!(x[0] < 0.02, "{}", x[0]);
        assert!((x[x.len() - 1] - 1.0).abs() < 1e-4);
    }

    #[test]
    fn a_cut_applies_at_once() {
        let mut m = MakeupGain::new(0);
        m.process(100.0, &mut block(0.0, 256));
        let mut x = block(0.5, 256);
        m.process(2.0, &mut x);
        assert_eq!(m.current(), 2.0);
        assert!((x[x.len() - 1] - 1.0).abs() < 1e-4);
    }

    #[test]
    fn a_changing_target_restarts_the_wait() {
        let mut m = MakeupGain::new(600);
        m.process(10.0, &mut block(0.0, 400));
        m.process(20.0, &mut block(0.0, 400));
        assert_eq!(m.current(), 1.0);
        m.process(20.0, &mut block(0.0, 400));
        assert_eq!(m.current(), 20.0);
    }

    #[test]
    fn nonfinite_targets_fall_back_to_unity() {
        let mut m = MakeupGain::new(0);
        let mut x = block(0.5, 64);
        m.process(f32::NAN, &mut x);
        assert!(x.iter().all(|&v| v == 0.5));
    }

    #[test]
    fn limiter_keeps_quiet_samples_and_bounds_loud_ones() {
        let mut x = vec![0.5, -0.89, 0.95, -3.0, 40.0];
        soft_limit(&mut x);
        assert_eq!(x[0], 0.5);
        assert_eq!(x[1], -0.89);
        assert!(x[2] > 0.9 && x[2] < 0.95);
        assert!(x[3] >= -1.0 && x[3] < -0.99);
        assert!(x[4] <= 1.0 && x[4] > 0.99);
    }
}
