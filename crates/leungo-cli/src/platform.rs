//! Per-platform wiring: where audio comes from, how the EQ is applied, and
//! where track information comes from.
//!
//! Linux captures the default sink's monitor and drives EasyEffects. macOS
//! captures a loopback device (BlackHole), equalizes in process and plays to
//! the real output.

use anyhow::Result;
use leungo_core::{Config, EqApplier, SampleSource};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::Receiver;

pub struct Platform {
    pub source: Box<dyn SampleSource>,
    pub applier: Box<dyn EqApplier>,
    pub track_events: Option<Receiver<String>>,
    pub now_playing: Option<leungo_players::NowPlayingHandle>,
}

fn players(
    cfg: &Config,
) -> (
    Option<Receiver<String>>,
    Option<leungo_players::NowPlayingHandle>,
) {
    if !cfg.analysis.use_mpris {
        return (None, None);
    }
    match leungo_players::spawn_with_now_playing() {
        Ok((rx, handle)) => (Some(rx), Some(handle)),
        Err(e) => {
            tracing::warn!("player integration unavailable: {e:#}");
            (None, None)
        }
    }
}

#[cfg(target_os = "linux")]
pub fn setup(cfg: &mut Config, _stop: Arc<AtomicBool>) -> Result<Platform> {
    let capture = leungo_capture_pipewire::PulseCapture::new(&cfg.capture)?;
    let applier = leungo_applier_ee::EasyEffectsApplier::new(&cfg.applier);
    let (track_events, now_playing) = players(cfg);
    Ok(Platform {
        source: Box::new(capture),
        applier: Box::new(applier),
        track_events,
        now_playing,
    })
}

#[cfg(target_os = "macos")]
pub fn setup(cfg: &mut Config, stop: Arc<AtomicBool>) -> Result<Platform> {
    use leungo_standalone::io;
    let running = io::start(
        io::Settings {
            input_device: cfg.standalone.input_device.clone(),
            output_device: cfg.standalone.output_device.clone(),
            latency_ms: cfg.standalone.latency_ms,
            freqs: cfg.eq.frequencies.clone(),
            q: cfg.eq.q,
        },
        stop,
    )?;
    tracing::info!(
        "capturing {} at {} Hz, playing to {} at {} Hz",
        running.input_name,
        running.input_rate,
        running.output_name,
        running.output_rate
    );
    // The analysis must know the real capture format.
    cfg.capture.rate = running.input_rate;
    cfg.capture.channels = 2;
    let (track_events, now_playing) = players(cfg);
    Ok(Platform {
        source: Box::new(running.source),
        applier: Box::new(running.applier),
        track_events,
        now_playing,
    })
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn setup(_cfg: &mut Config, _stop: Arc<AtomicBool>) -> Result<Platform> {
    anyhow::bail!("this platform is not supported yet (Linux and macOS are)")
}

/// Lists audio devices (macOS only, where the engine opens devices itself).
#[cfg(target_os = "macos")]
pub fn print_devices() -> Result<()> {
    for d in leungo_standalone::io::list_devices()? {
        let kind = match (d.input, d.output) {
            (true, true) => "in/out",
            (true, false) => "in    ",
            (false, true) => "out   ",
            _ => "      ",
        };
        println!("[{kind}] {}", d.name);
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
pub fn print_devices() -> Result<()> {
    println!("device listing applies to the standalone engine (macOS).");
    println!("On Linux, leungo captures the default sink monitor and uses EasyEffects.");
    Ok(())
}
