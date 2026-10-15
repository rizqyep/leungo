//! Linux only (empty on other platforms).
//!
//! System-audio capture through the PulseAudio compatibility API. Works on
//! both PulseAudio and PipeWire (via pipewire-pulse) by recording the
//! monitor of the default sink.
//!
//! When EasyEffects owns the signal path the default sink's monitor carries
//! audio *after* the EQ, so the analysis would measure its own correction.
//! `pick_source` therefore taps the EasyEffects sink monitor (the signal
//! before the EQ chain) whenever it exists.

#![cfg(target_os = "linux")]

use anyhow::{Context, Result, anyhow};
use leungo_core::{CaptureCfg, SampleSource, SourceEvent};
use libpulse_binding::context::{
    Context as PulseContext, FlagSet as CtxFlagSet, State as CtxState,
};
use libpulse_binding::mainloop::standard::Mainloop;
use libpulse_binding::sample::{Format, Spec};
use libpulse_binding::stream::{
    FlagSet as StreamFlagSet, PeekResult, State as StreamState, Stream,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::thread::JoinHandle;
use std::time::Duration;

pub struct PulseCapture {
    rx: Receiver<Vec<f32>>,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl PulseCapture {
    pub fn new(cfg: &CaptureCfg) -> Result<Self> {
        let (tx, rx) = sync_channel::<Vec<f32>>(64);
        let (init_tx, init_rx) = sync_channel::<Result<()>>(1);
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = stop.clone();
        let cfg = cfg.clone();
        let handle = std::thread::Builder::new()
            .name("ae-capture".into())
            .spawn(move || run(&cfg, tx, init_tx, stop_thread))
            .context("spawn capture thread")?;
        match init_rx.recv() {
            Ok(Ok(())) => Ok(Self {
                rx,
                stop,
                handle: Some(handle),
            }),
            Ok(Err(e)) => {
                let _ = handle.join();
                Err(e)
            }
            Err(_) => {
                let _ = handle.join();
                Err(anyhow!("capture thread exited during setup"))
            }
        }
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl Drop for PulseCapture {
    fn drop(&mut self) {
        self.stop();
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

impl SampleSource for PulseCapture {
    fn next(&mut self) -> Result<SourceEvent> {
        match self.rx.recv_timeout(Duration::from_millis(200)) {
            Ok(samples) => Ok(SourceEvent::Chunk(samples)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Ok(SourceEvent::Idle),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Ok(SourceEvent::Ended),
        }
    }
}

const EE_MONITOR: &str = "easyeffects_sink.monitor";

/// Chooses the capture source. `@DEFAULT_MONITOR@` is replaced by the
/// EasyEffects sink monitor when the listing (`pactl list short sources`)
/// contains it; an explicitly configured source is always respected.
pub fn pick_source(configured: &str, short_sources: &str) -> String {
    if configured == "@DEFAULT_MONITOR@"
        && short_sources
            .lines()
            .any(|l| l.split_whitespace().nth(1) == Some(EE_MONITOR))
    {
        return EE_MONITOR.to_string();
    }
    configured.to_string()
}

/// The source leungo records from for this configuration, for display.
pub fn source_name(configured: &str) -> String {
    resolve_source(configured)
}

fn resolve_source(configured: &str) -> String {
    let listing = std::process::Command::new("pactl")
        .args(["list", "short", "sources"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    let picked = pick_source(configured, &listing);
    if picked != configured {
        tracing::info!("capturing {picked} (before the EasyEffects chain) instead of {configured}");
    }
    picked
}

fn run(
    cfg: &CaptureCfg,
    tx: SyncSender<Vec<f32>>,
    init_tx: SyncSender<Result<()>>,
    stop: Arc<AtomicBool>,
) {
    let fail = |init_tx: &SyncSender<Result<()>>, e: anyhow::Error| {
        let _ = init_tx.send(Err(e));
    };

    let Some(mut mainloop) = Mainloop::new() else {
        fail(&init_tx, anyhow!("failed to create pulse mainloop"));
        return;
    };
    let Some(mut ctx) = PulseContext::new(&mainloop, "leungo") else {
        fail(&init_tx, anyhow!("failed to create pulse context"));
        return;
    };
    if let Err(e) = ctx.connect(None, CtxFlagSet::NOFLAGS, None) {
        fail(&init_tx, anyhow!("pulse connect failed: {e}"));
        return;
    }
    loop {
        match ctx.get_state() {
            CtxState::Ready => break,
            CtxState::Failed | CtxState::Terminated => {
                fail(&init_tx, anyhow!("pulse context failed"));
                return;
            }
            _ => {
                let _ = mainloop.iterate(false);
                std::thread::sleep(Duration::from_millis(2));
            }
        }
    }

    let spec = Spec {
        format: Format::FLOAT32NE,
        rate: cfg.rate,
        channels: cfg.channels as u8,
    };
    if !spec.is_valid() {
        fail(&init_tx, anyhow!("invalid sample spec"));
        return;
    }

    let Some(mut stream) = Stream::new(&mut ctx, "leungo-capture", &spec, None) else {
        fail(&init_tx, anyhow!("failed to create stream"));
        return;
    };
    let source = resolve_source(&cfg.source);
    if let Err(e) = stream.connect_record(Some(source.as_str()), None, StreamFlagSet::NOFLAGS) {
        fail(&init_tx, anyhow!("connect_record failed: {e}"));
        return;
    }
    loop {
        match stream.get_state() {
            StreamState::Ready => break,
            StreamState::Failed | StreamState::Terminated => {
                fail(&init_tx, anyhow!("stream failed to become ready"));
                return;
            }
            _ => {
                let _ = mainloop.iterate(false);
                std::thread::sleep(Duration::from_millis(2));
            }
        }
    }

    let _ = init_tx.send(Ok(()));

    // Polled transfer model: the mainloop thread peeks, converts, and
    // discards in its own loop, bounded by the sync channel for backpressure.
    while !stop.load(Ordering::Relaxed) {
        if mainloop.iterate(false).is_error() {
            tracing::warn!("pulse mainloop error");
            break;
        }
        loop {
            // Without a stop check here, the pollded drain loop never
            // returns and the channel blocks after the consumer stops.
            if stop.load(Ordering::Relaxed) {
                break;
            }
            match stream.peek() {
                Ok(PeekResult::Data(data)) => {
                    let samples: Vec<f32> = data
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .map(|c| f32::from_le_bytes(*c))
                        .collect();
                    if !samples.is_empty() && tx.send(samples).is_err() {
                        break;
                    }
                    let _ = stream.discard();
                }
                Ok(PeekResult::Hole(_)) => {
                    let _ = stream.discard();
                }
                Ok(PeekResult::Empty) => break,
                Err(e) => {
                    tracing::debug!("peek failed: {e}");
                    break;
                }
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let _ = stream.disconnect();
    ctx.disconnect();
}

#[cfg(test)]
mod tests {
    use super::*;

    const LISTING: &str = "166\talsa_output.pci-0000_05_00.6.analog-stereo.monitor\tPipeWire\ts32le 2ch 48000Hz\tSUSPENDED\n\
834\talsa_output.usb-Generic_AB17X.analog-stereo.monitor\tPipeWire\ts24le 2ch 48000Hz\tRUNNING\n\
2266\teasyeffects_sink.monitor\tPipeWire\tfloat32le 2ch 48000Hz\tRUNNING\n";

    #[test]
    fn default_monitor_becomes_easyeffects_monitor() {
        assert_eq!(
            pick_source("@DEFAULT_MONITOR@", LISTING),
            "easyeffects_sink.monitor"
        );
    }

    #[test]
    fn explicit_source_is_respected() {
        let s = "alsa_output.usb-Generic_AB17X.analog-stereo.monitor";
        assert_eq!(pick_source(s, LISTING), s);
    }

    #[test]
    fn without_easyeffects_default_monitor_stays() {
        let no_ee: String = LISTING.lines().take(2).collect::<Vec<_>>().join("\n");
        assert_eq!(
            pick_source("@DEFAULT_MONITOR@", &no_ee),
            "@DEFAULT_MONITOR@"
        );
    }
}
