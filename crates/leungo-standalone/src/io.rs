//! Device I/O through cpal: capture from the loopback device, play to the
//! real output. All processing is in `bridge`; this module only moves
//! samples between the devices and the bridge thread.
//!
//! cpal streams are not `Send`, so one dedicated thread builds both streams,
//! keeps them alive, and runs the bridge loop.

use crate::bridge::Bridge;
use anyhow::{Context, Result, bail};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use leungo_core::{EqApplier, SampleSource, SourceEvent};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Longest the queues may grow, in frames at 48 kHz-ish rates (about 1 s).
const MAX_QUEUE_FRAMES: usize = 48_000;

pub struct Settings {
    /// Part of the loopback device's name, for example "BlackHole".
    pub input_device: String,
    /// Part of the output device's name; empty means the system default.
    pub output_device: String,
    pub latency_ms: u32,
    pub freqs: Vec<f64>,
    pub q: f64,
}

pub struct DeviceEntry {
    pub name: String,
    pub input: bool,
    pub output: bool,
}

pub struct Running {
    pub source: StandaloneSource,
    pub applier: StandaloneApplier,
    pub input_rate: u32,
    pub output_rate: u32,
    pub input_name: String,
    pub output_name: String,
    pub stop: Arc<AtomicBool>,
}

struct Shared {
    input: Mutex<VecDeque<f32>>,
    output: Mutex<VecDeque<f32>>,
    primed: AtomicBool,
    bypass: AtomicBool,
    target: Mutex<Vec<f64>>,
    stop: Arc<AtomicBool>,
}

struct InitInfo {
    input_rate: u32,
    output_rate: u32,
    input_name: String,
    output_name: String,
}

pub struct StandaloneSource {
    rx: Receiver<Vec<f32>>,
}

impl SampleSource for StandaloneSource {
    fn next(&mut self) -> Result<SourceEvent> {
        match self.rx.recv_timeout(Duration::from_millis(200)) {
            Ok(chunk) => Ok(SourceEvent::Chunk(chunk)),
            Err(RecvTimeoutError::Timeout) => Ok(SourceEvent::Idle),
            Err(RecvTimeoutError::Disconnected) => Ok(SourceEvent::Ended),
        }
    }
}

pub struct StandaloneApplier {
    shared: Arc<Shared>,
}

impl EqApplier for StandaloneApplier {
    fn apply_eq(&mut self, gains: &[f64]) -> Result<()> {
        if let Ok(mut t) = self.shared.target.lock() {
            t.clear();
            t.extend_from_slice(gains);
        }
        Ok(())
    }

    fn set_bypass(&mut self, on: bool) -> Result<()> {
        self.shared.bypass.store(on, Ordering::Relaxed);
        Ok(())
    }
}

fn name_of(d: &cpal::Device) -> String {
    d.name().unwrap_or_else(|_| "(unnamed)".into())
}

fn contains_ci(hay: &str, needle: &str) -> bool {
    hay.to_lowercase().contains(&needle.to_lowercase())
}

pub fn list_devices() -> Result<Vec<DeviceEntry>> {
    let host = cpal::default_host();
    let mut out = Vec::new();
    for d in host.devices().context("list audio devices")? {
        let input = d
            .supported_input_configs()
            .map(|mut c| c.next().is_some())
            .unwrap_or(false);
        let output = d
            .supported_output_configs()
            .map(|mut c| c.next().is_some())
            .unwrap_or(false);
        out.push(DeviceEntry {
            name: name_of(&d),
            input,
            output,
        });
    }
    Ok(out)
}

fn device_help() -> String {
    match list_devices() {
        Ok(list) => list
            .iter()
            .map(|d| {
                let kind = match (d.input, d.output) {
                    (true, true) => "in/out",
                    (true, false) => "in",
                    (false, true) => "out",
                    _ => "-",
                };
                format!("  [{kind}] {}", d.name)
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Err(e) => format!("  (could not list devices: {e})"),
    }
}

fn pick_input(host: &cpal::Host, needle: &str) -> Result<cpal::Device> {
    for d in host.input_devices().context("list input devices")? {
        if contains_ci(&name_of(&d), needle) {
            return Ok(d);
        }
    }
    bail!(
        "no input device matching {needle:?}. Install BlackHole (brew install blackhole-2ch) or \
         set standalone.input_device. Devices:\n{}",
        device_help()
    )
}

fn pick_output(host: &cpal::Host, wanted: &str, loopback: &str) -> Result<cpal::Device> {
    if !wanted.is_empty() {
        for d in host.output_devices().context("list output devices")? {
            if contains_ci(&name_of(&d), wanted) {
                return Ok(d);
            }
        }
        bail!(
            "no output device matching {wanted:?}. Devices:\n{}",
            device_help()
        );
    }
    let d = host
        .default_output_device()
        .context("no default output device")?;
    if contains_ci(&name_of(&d), loopback) {
        bail!(
            "the system output is the loopback device ({}), so playing to it would feed back. \
             Set standalone.output_device to your speakers or headphones. Devices:\n{}",
            name_of(&d),
            device_help()
        );
    }
    Ok(d)
}

fn push_stereo(sh: &Shared, data: &[f32], channels: usize) {
    let Ok(mut q) = sh.input.lock() else { return };
    let channels = channels.max(1);
    for frame in data.chunks_exact(channels) {
        let l = frame[0];
        let r = if channels > 1 { frame[1] } else { l };
        q.push_back(l);
        q.push_back(r);
    }
    let excess = q.len().saturating_sub(MAX_QUEUE_FRAMES * 2);
    if excess > 0 {
        q.drain(..excess);
    }
}

fn fill_output(sh: &Shared, data: &mut [f32], channels: usize, target_frames: usize) {
    let channels = channels.max(1);
    let Ok(mut q) = sh.output.lock() else {
        data.fill(0.0);
        return;
    };
    if !sh.primed.load(Ordering::Relaxed) {
        if q.len() / 2 >= target_frames {
            sh.primed.store(true, Ordering::Relaxed);
        } else {
            data.fill(0.0);
            return;
        }
    }
    for frame in data.chunks_exact_mut(channels) {
        if q.len() >= 2 {
            let l = q.pop_front().unwrap_or(0.0);
            let r = q.pop_front().unwrap_or(0.0);
            if channels == 1 {
                frame[0] = 0.5 * (l + r);
            } else {
                frame[0] = l;
                frame[1] = r;
                for s in frame[2..].iter_mut() {
                    *s = 0.0;
                }
            }
        } else {
            frame.fill(0.0);
            // ran dry: refill to the target before playing again
            sh.primed.store(false, Ordering::Relaxed);
        }
    }
}

fn build_streams(
    s: &Settings,
    sh: &Arc<Shared>,
) -> Result<((cpal::Stream, cpal::Stream), InitInfo, usize)> {
    let host = cpal::default_host();
    let input = pick_input(&host, &s.input_device)?;
    let output = pick_output(&host, &s.output_device, &s.input_device)?;
    let in_cfg = input.default_input_config().context("input config")?;
    let out_cfg = output.default_output_config().context("output config")?;
    if in_cfg.sample_format() != cpal::SampleFormat::F32
        || out_cfg.sample_format() != cpal::SampleFormat::F32
    {
        bail!(
            "unsupported sample format (input {:?}, output {:?}); only 32-bit float devices are supported",
            in_cfg.sample_format(),
            out_cfg.sample_format()
        );
    }
    let (in_ch, out_ch) = (in_cfg.channels() as usize, out_cfg.channels() as usize);
    let (in_rate, out_rate) = (in_cfg.sample_rate().0, out_cfg.sample_rate().0);
    let target_frames = (out_rate as u64 * s.latency_ms as u64 / 1000) as usize;

    let sh_in = sh.clone();
    let in_stream = input
        .build_input_stream(
            &in_cfg.config(),
            move |data: &[f32], _: &cpal::InputCallbackInfo| push_stereo(&sh_in, data, in_ch),
            |e| tracing::error!("input stream: {e}"),
            None,
        )
        .context("open input stream")?;
    let sh_out = sh.clone();
    let out_stream = output
        .build_output_stream(
            &out_cfg.config(),
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                fill_output(&sh_out, data, out_ch, target_frames)
            },
            |e| tracing::error!("output stream: {e}"),
            None,
        )
        .context("open output stream")?;
    in_stream.play().context("start input")?;
    out_stream.play().context("start output")?;
    Ok((
        (in_stream, out_stream),
        InitInfo {
            input_rate: in_rate,
            output_rate: out_rate,
            input_name: name_of(&input),
            output_name: name_of(&output),
        },
        target_frames,
    ))
}

fn bridge_loop(sh: &Shared, mut bridge: Bridge, tx: Sender<Vec<f32>>) {
    let mut block: Vec<f32> = Vec::new();
    let mut produced: Vec<f32> = Vec::new();
    while !sh.stop.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(4));
        block.clear();
        if let Ok(mut q) = sh.input.lock() {
            block.extend(q.drain(..));
        }
        if block.is_empty() {
            continue;
        }
        // pre-EQ copy for the analysis
        if tx.send(block.clone()).is_err() {
            break;
        }
        let gains = if sh.bypass.load(Ordering::Relaxed) {
            vec![0.0; 8]
        } else {
            sh.target.lock().map(|t| t.clone()).unwrap_or_default()
        };
        bridge.chain.set_target(&gains);
        let fill = sh.output.lock().map(|q| q.len() / 2).unwrap_or(0);
        produced.clear();
        bridge.process(&block, fill, &mut produced);
        if let Ok(mut q) = sh.output.lock() {
            q.extend(produced.iter().copied());
            let excess = q.len().saturating_sub(MAX_QUEUE_FRAMES * 2);
            if excess > 0 {
                q.drain(..excess);
            }
        }
    }
}

/// Opens both devices and starts processing. Returns once audio is flowing
/// or the devices could not be opened.
pub fn start(settings: Settings, stop: Arc<AtomicBool>) -> Result<Running> {
    let shared = Arc::new(Shared {
        input: Mutex::new(VecDeque::new()),
        output: Mutex::new(VecDeque::new()),
        primed: AtomicBool::new(false),
        bypass: AtomicBool::new(false),
        target: Mutex::new(vec![0.0; settings.freqs.len()]),
        stop: stop.clone(),
    });
    let (init_tx, init_rx) = channel::<std::result::Result<InitInfo, String>>();
    let (chunk_tx, chunk_rx) = channel::<Vec<f32>>();
    let sh = shared.clone();
    std::thread::Builder::new()
        .name("leungo-audio".into())
        .spawn(move || match build_streams(&settings, &sh) {
            Ok((streams, info, target_frames)) => {
                let bridge = Bridge::new(
                    &settings.freqs,
                    settings.q,
                    info.input_rate as f64,
                    info.output_rate as f64,
                    target_frames,
                );
                let _ = init_tx.send(Ok(info));
                bridge_loop(&sh, bridge, chunk_tx);
                drop(streams);
            }
            Err(e) => {
                let _ = init_tx.send(Err(format!("{e:#}")));
            }
        })
        .context("spawn audio thread")?;

    let info = init_rx
        .recv_timeout(Duration::from_secs(10))
        .context("audio thread did not start")?
        .map_err(|e| anyhow::anyhow!(e))?;
    Ok(Running {
        source: StandaloneSource { rx: chunk_rx },
        applier: StandaloneApplier { shared },
        input_rate: info.input_rate,
        output_rate: info.output_rate,
        input_name: info.input_name,
        output_name: info.output_name,
        stop,
    })
}
