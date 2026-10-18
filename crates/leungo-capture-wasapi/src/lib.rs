//! Windows only (empty on other platforms).
//!
//! System-audio capture through WASAPI process loopback: every process
//! except leungo itself, mixed by Windows before the endpoint effects.
//! Equalizer APO runs as one of those effects, so this tap hears the music
//! without leungo's own correction, the way the EasyEffects sink monitor
//! does on Linux. Endpoint loopback would hear the corrected signal.

#![cfg(windows)]

use anyhow::{Context, Result, anyhow};
use leungo_core::{CaptureCfg, SampleSource, SourceEvent};
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::thread::JoinHandle;
use std::time::Duration;
use wasapi::{
    AudioClient, DeviceEnumerator, Direction, SampleType, StreamMode, WaveFormat, initialize_mta,
};

/// Frames gathered before a chunk is handed to the engine, so it is not
/// woken for every 10 ms packet.
const CHUNK_FRAMES: usize = 2048;

pub struct WasapiCapture {
    rx: Receiver<Vec<f32>>,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl WasapiCapture {
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
}

impl Drop for WasapiCapture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

impl SampleSource for WasapiCapture {
    fn next(&mut self) -> Result<SourceEvent> {
        match self.rx.recv_timeout(Duration::from_millis(200)) {
            Ok(samples) => Ok(SourceEvent::Chunk(samples)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Ok(SourceEvent::Idle),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Ok(SourceEvent::Ended),
        }
    }
}

struct Stream {
    client: AudioClient,
    capture: wasapi::AudioCaptureClient,
    event: wasapi::Handle,
}

fn open(cfg: &CaptureCfg) -> Result<Stream> {
    initialize_mta()
        .ok()
        .map_err(|e| anyhow!("initialize COM: {e}"))?;
    let format = WaveFormat::new(
        32,
        32,
        &SampleType::Float,
        cfg.rate as usize,
        cfg.channels,
        None,
    );
    let mut client = AudioClient::new_application_loopback_client(std::process::id(), false)
        .map_err(|e| anyhow!("open process loopback (needs Windows 10 2004 or later): {e}"))?;
    client
        .initialize_client(
            &format,
            &Direction::Capture,
            &StreamMode::EventsShared {
                autoconvert: true,
                buffer_duration_hns: 0,
            },
        )
        .map_err(|e| anyhow!("start loopback capture: {e}"))?;
    let event = client
        .set_get_eventhandle()
        .map_err(|e| anyhow!("loopback event: {e}"))?;
    let capture = client
        .get_audiocaptureclient()
        .map_err(|e| anyhow!("loopback capture client: {e}"))?;
    client
        .start_stream()
        .map_err(|e| anyhow!("start loopback stream: {e}"))?;
    Ok(Stream {
        client,
        capture,
        event,
    })
}

fn run(
    cfg: &CaptureCfg,
    tx: SyncSender<Vec<f32>>,
    init_tx: SyncSender<Result<()>>,
    stop: Arc<AtomicBool>,
) {
    let stream = match open(cfg) {
        Ok(s) => s,
        Err(e) => {
            let _ = init_tx.send(Err(e));
            return;
        }
    };
    let _ = init_tx.send(Ok(()));
    tracing::info!(
        "capturing every app except leungo (WASAPI process loopback, {} Hz)",
        cfg.rate
    );
    let chunk_bytes = CHUNK_FRAMES * cfg.channels * 4;
    let mut bytes: VecDeque<u8> = VecDeque::with_capacity(chunk_bytes * 2);
    while !stop.load(Ordering::Relaxed) {
        // nothing arrives while every app is silent, so wake up to check `stop`
        if stream.event.wait_for_event(200).is_err() {
            continue;
        }
        if let Err(e) = drain_packets(&stream.capture, &mut bytes) {
            tracing::error!("loopback capture failed: {e}");
            break;
        }
        while bytes.len() >= chunk_bytes {
            let samples = to_f32(bytes.drain(..chunk_bytes));
            if tx.send(samples).is_err() {
                return;
            }
        }
    }
    let _ = stream.client.stop_stream();
}

fn drain_packets(capture: &wasapi::AudioCaptureClient, bytes: &mut VecDeque<u8>) -> Result<()> {
    while capture
        .get_next_packet_size()
        .map_err(|e| anyhow!("{e}"))?
        .unwrap_or(0)
        > 0
    {
        let start = bytes.len();
        let info = capture
            .read_from_device_to_deque(bytes)
            .map_err(|e| anyhow!("{e}"))?;
        // a silent packet's buffer holds whatever was there before
        if info.flags.silent {
            bytes.range_mut(start..).for_each(|b| *b = 0);
        }
    }
    Ok(())
}

fn to_f32(bytes: impl Iterator<Item = u8>) -> Vec<f32> {
    let raw: Vec<u8> = bytes.collect();
    raw.as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b))
        .collect()
}

/// Name and endpoint id of the device Windows plays to by default.
pub fn default_output() -> Option<(String, String)> {
    initialize_mta().ok().ok()?;
    let device = DeviceEnumerator::new()
        .ok()?
        .get_default_device(&Direction::Render)
        .ok()?;
    Some((device.get_friendlyname().ok()?, device.get_id().ok()?))
}

#[cfg(test)]
mod tests {
    use super::to_f32;

    #[test]
    fn decodes_little_endian_floats() {
        let bytes: Vec<u8> = [0.5f32, -1.0]
            .iter()
            .flat_map(|f| f.to_le_bytes())
            .collect();
        assert_eq!(to_f32(bytes.into_iter()), vec![0.5, -1.0]);
    }
}
