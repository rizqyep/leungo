//! Track-change detection by silence gap: audio resuming after a gap longer
//! than `gap_s` is a new track. Players that report track ids (MPRIS) are
//! authoritative; this is the fallback for sources without metadata.

use crate::spectrum::FrameResult;

#[derive(Clone, Copy, Debug)]
pub struct DetectorConfig {
    /// Silence longer than this, then audio, means a new track.
    pub gap_s: f64,
}

impl Default for DetectorConfig {
    fn default() -> Self {
        Self { gap_s: 1.2 }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Detection {
    None,
    /// Silence gap followed by audio: a different track.
    TrackStart,
}

pub struct TrackDetector {
    cfg: DetectorConfig,
    silence_run_s: f64,
    was_silent: bool,
}

impl TrackDetector {
    pub fn new(cfg: DetectorConfig) -> Self {
        Self {
            cfg,
            silence_run_s: 0.0,
            was_silent: false,
        }
    }

    /// Clears the silence run; called when a new analysis starts.
    pub fn mark_now(&mut self, _now_s: f64) {
        self.silence_run_s = 0.0;
        self.was_silent = false;
    }

    pub fn feed(&mut self, frame: &FrameResult, dt_s: f64, _now_s: f64) -> Detection {
        if frame.silent {
            self.silence_run_s += dt_s;
            self.was_silent = true;
            return Detection::None;
        }
        let gap_ended = self.was_silent && self.silence_run_s >= self.cfg.gap_s;
        self.silence_run_s = 0.0;
        self.was_silent = false;
        if gap_ended {
            Detection::TrackStart
        } else {
            Detection::None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(rms_db: f64, flux: f64) -> FrameResult {
        FrameResult {
            rms_db,
            peak: 0.0,
            sq_sum: 0.0,
            samples: 1024,
            flux,
            silent: rms_db < -60.0,
        }
    }

    #[test]
    fn silence_gap_triggers_track_start() {
        let mut d = TrackDetector::new(DetectorConfig::default());
        let dt = 1024.0 / 48_000.0;
        let mut now = 0.0;
        for _ in 0..300 {
            assert_eq!(d.feed(&frame(-20.0, 0.0), dt, now), Detection::None);
            now += dt;
        }
        for _ in 0..66 {
            assert_eq!(d.feed(&frame(-90.0, 0.0), dt, now), Detection::None);
            now += dt;
        }
        assert_eq!(d.feed(&frame(-20.0, 0.0), dt, now), Detection::TrackStart);
    }

    #[test]
    fn short_pause_does_not_trigger() {
        let mut d = TrackDetector::new(DetectorConfig::default());
        let dt = 1024.0 / 48_000.0;
        let mut now = 0.0;
        for _ in 0..100 {
            d.feed(&frame(-20.0, 0.0), dt, now);
            now += dt;
        }
        for _ in 0..20 {
            d.feed(&frame(-90.0, 0.0), dt, now);
            now += dt;
        }
        assert_eq!(d.feed(&frame(-20.0, 0.0), dt, now), Detection::None);
    }
}
