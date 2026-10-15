//! Local web GUI: the engine runs as a thread and a minimal HTTP server
//! serves one static page plus a JSON state endpoint. The browser polls
//! every 200 ms; bypass is a POST that moves the engine's flag file.

use ae_applier_ee::EasyEffectsApplier;
use ae_capture_pipewire::PulseCapture;
use ae_core::{Config, Engine, Status, paths};
use ae_eq::BAND_FREQS;
use anyhow::{Context, Result};
use serde_json::json;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const PAGE: &str = include_str!("gui.html");

pub struct GuiShared {
    pub status: Arc<Mutex<Status>>,
    pub spectrum: Arc<Mutex<Vec<(f64, f64)>>>,
    pub bypass_flag: PathBuf,
    pub tuning_flag: PathBuf,
    pub now_playing: Option<ae_mpris::NowPlayingHandle>,
}

pub fn run_gui(cfg: Config, port: u16, open_browser: bool) -> Result<()> {
    cfg.validate()?;
    let status = Arc::new(Mutex::new(Status::default()));
    let spectrum = Arc::new(Mutex::new(Vec::new()));
    let stop = Arc::new(AtomicBool::new(false));

    {
        let stop2 = stop.clone();
        ctrlc::set_handler(move || stop2.store(true, Ordering::Relaxed))
            .context("install ctrl-c handler")?;
    }

    let capture = PulseCapture::new(&cfg.capture)?;
    let applier = EasyEffectsApplier::new(&cfg.applier);
    let use_mpris = cfg.analysis.use_mpris;
    let mut engine = Engine::new(
        cfg,
        Box::new(capture),
        Box::new(applier),
        status.clone(),
        stop.clone(),
    );
    engine.set_spectrum_out(spectrum.clone());
    engine.set_track_log(paths::track_log_path());
    if let Some(c) = paths::load_fitted_classifier() {
        engine.set_classifier(c);
    }
    engine.set_tuning_path(paths::tuning_path());
    let mut now_playing = None;
    if use_mpris {
        match ae_mpris::spawn_with_now_playing() {
            Ok((rx, handle)) => {
                engine.set_track_events(rx);
                now_playing = Some(handle);
            }
            Err(e) => tracing::warn!("mpris monitor unavailable: {e:#}"),
        }
    }

    let engine_handle = std::thread::Builder::new()
        .name("ae-engine".into())
        .spawn(move || engine.run())
        .context("spawn engine thread")?;

    // Keep adaptive-eq status working while the GUI runs.
    {
        let status2 = status.clone();
        let stop2 = stop.clone();
        std::thread::Builder::new()
            .name("ae-status".into())
            .spawn(move || {
                let path = paths::status_path();
                if let Some(p) = path.parent() {
                    let _ = std::fs::create_dir_all(p);
                }
                while !stop2.load(Ordering::Relaxed) {
                    if let Ok(s) = status2.lock()
                        && let Ok(json) = serde_json::to_string_pretty(&*s)
                    {
                        let _ = std::fs::write(&path, json);
                    }
                    std::thread::sleep(Duration::from_millis(500));
                }
            })
            .context("spawn status writer thread")?;
    }

    let bypass_flag = paths::bypass_flag_path();
    if let Some(parent) = bypass_flag.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let shared = GuiShared {
        status,
        spectrum,
        bypass_flag,
        tuning_flag: paths::tuning_path(),
        now_playing,
    };

    let listener = TcpListener::bind(("127.0.0.1", port))
        .with_context(|| format!("bind port {port} (in use? try --port)"))?;
    listener.set_nonblocking(true)?;
    let url = format!("http://127.0.0.1:{port}");
    println!("adaptive-eq GUI: {url}  (ctrl-c to stop)");

    if open_browser {
        let opener = if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        };
        let _ = std::process::Command::new(opener).arg(&url).spawn();
    }

    while !stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _)) => handle_conn(stream, &shared),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => {
                tracing::warn!("accept failed: {e}");
                std::thread::sleep(Duration::from_millis(500));
            }
        }
    }
    stop.store(true, Ordering::Relaxed);
    let _ = engine_handle.join();
    Ok(())
}

fn respond(stream: &mut TcpStream, code: u16, ctype: &str, body: &[u8]) {
    let reason = match code {
        200 => "OK",
        404 => "Not Found",
        _ => "Error",
    };
    let head = format!(
        "HTTP/1.1 {code} {reason}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body);
    let _ = stream.flush();
    let _ = stream.shutdown(std::net::Shutdown::Both);
}

fn handle_conn(mut stream: TcpStream, shared: &GuiShared) {
    let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
    let mut buf = [0u8; 4096];
    let mut n = 0usize;
    while n < buf.len() {
        match stream.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(read) => {
                n += read;
                let head = &buf[..n];
                if head.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    let head = String::from_utf8_lossy(&buf[..n]);
    let mut parts = head.split_whitespace();
    let method = parts.next().unwrap_or("").to_ascii_uppercase();
    let path = parts.next().unwrap_or("/").split('?').next().unwrap_or("/");

    match (method.as_str(), path) {
        ("GET", "/") => {
            respond(
                &mut stream,
                200,
                "text/html; charset=utf-8",
                PAGE.as_bytes(),
            );
        }
        ("GET", "/api/state") => {
            let body = serde_json::to_vec(&state_json(shared)).unwrap_or_default();
            respond(&mut stream, 200, "application/json", &body);
        }
        ("POST", "/api/bypass/on") => {
            let _ = std::fs::write(&shared.bypass_flag, "");
            respond(&mut stream, 200, "application/json", b"{\"ok\":true}");
        }
        ("POST", "/api/bypass/off") => {
            let _ = std::fs::remove_file(&shared.bypass_flag);
            respond(&mut stream, 200, "application/json", b"{\"ok\":true}");
        }
        ("POST", p) if p.starts_with("/api/tuning/") => {
            let name = &p["/api/tuning/".len()..];
            if name == "custom" || ae_core::Preset::parse(name).is_some() {
                let _ = std::fs::write(&shared.tuning_flag, name);
                respond(&mut stream, 200, "application/json", b"{\"ok\":true}");
            } else {
                respond(&mut stream, 400, "text/plain", b"unknown preset");
            }
        }
        _ => respond(&mut stream, 404, "text/plain", b"not found"),
    }
}

fn state_json(shared: &GuiShared) -> serde_json::Value {
    let status = shared.status.lock().unwrap().clone();
    let spectrum = shared.spectrum.lock().unwrap().clone();
    let spectrum: Vec<[f64; 2]> = spectrum.iter().map(|(f, db)| [*f, *db]).collect();
    let now_playing = shared
        .now_playing
        .as_ref()
        .and_then(|h| h.lock().ok().and_then(|n| n.clone()))
        .map(|n| json!({ "title": n.title, "artist": n.artist, "album": n.album }));
    json!({
        "nowPlaying": now_playing,
        "tuning": status.tuning,
        "reliability": status.reliability,
        "state": status.state,
        "bypassed": status.bypassed,
        "archetype": status.archetype,
        "confidence": status.confidence,
        "tracks": status.tracks_analyzed,
        "gains": status.gains,
        "message": status.message,
        "spectrum": spectrum,
        "bandFreqs": BAND_FREQS.to_vec(),
    })
}
