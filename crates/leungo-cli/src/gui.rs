//! Local web GUI: the engine runs as a thread and a minimal HTTP server
//! serves one static page plus a JSON state endpoint. The browser polls
//! every 200 ms; bypass is a POST that moves the engine's flag file.

use anyhow::{Context, Result};
use leungo_core::{Config, Engine, Status, paths};
use leungo_eq::BAND_FREQS;
use serde_json::json;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const PAGE: &str = include_str!("gui.html");

fn page_id() -> &'static str {
    static ID: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    ID.get_or_init(|| {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        PAGE.hash(&mut h);
        format!("{:x}", h.finish())
    })
}

const BYE_GRACE: Duration = Duration::from_secs(3);
const SILENCE_LIMIT: Duration = Duration::from_secs(120);

/// Browsers slow timers in hidden windows, so a window that merely stopped
/// polling gets a long grace; a closed window says goodbye.
pub struct Presence {
    inner: Mutex<(Option<std::time::Instant>, Option<std::time::Instant>)>,
}

impl Presence {
    fn new() -> Self {
        Presence {
            inner: Mutex::new((None, None)),
        }
    }

    fn touch(&self) {
        if let Ok(mut g) = self.inner.lock() {
            *g = (Some(std::time::Instant::now()), None);
        }
    }

    fn bye(&self) {
        if let Ok(mut g) = self.inner.lock() {
            g.1 = Some(std::time::Instant::now());
        }
    }

    fn gone(&self) -> bool {
        let Ok(g) = self.inner.lock() else {
            return false;
        };
        match (g.0, g.1) {
            (_, Some(bye)) => bye.elapsed() >= BYE_GRACE,
            (Some(seen), None) => seen.elapsed() >= SILENCE_LIMIT,
            _ => false,
        }
    }
}

pub struct GuiShared {
    pub status: Arc<Mutex<Status>>,
    pub spectrum: Arc<Mutex<Vec<(f64, f64)>>>,
    pub bypass_flag: PathBuf,
    pub tuning_flag: PathBuf,
    pub sound_file: PathBuf,
    pub now_playing: Option<leungo_players::NowPlayingHandle>,
    pub setup: Option<SetupShared>,
    pub devices: (String, String),
    pub active: String,
    pub presence: Arc<Presence>,
}

pub struct SetupShared {
    pub install: Arc<Mutex<crate::setup::Install>>,
    pub driver: String,
    pub blocker: crate::setup::Blocker,
}

fn bind(port: u16) -> Result<TcpListener> {
    let listener = TcpListener::bind(("127.0.0.1", port))
        .with_context(|| format!("bind port {port} (in use? try --port)"))?;
    listener.set_nonblocking(true)?;
    Ok(listener)
}

fn serve_once(listener: &TcpListener, shared: &GuiShared) {
    match listener.accept() {
        Ok((stream, _)) => handle_conn(stream, shared),
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
            std::thread::sleep(Duration::from_millis(100));
        }
        Err(e) => {
            tracing::warn!("accept failed: {e}");
            std::thread::sleep(Duration::from_millis(500));
        }
    }
}

fn wait_for_setup(
    listener: &TcpListener,
    devices: (&str, &str),
    blocker: crate::setup::Blocker,
    stop: &AtomicBool,
    presence: &Arc<Presence>,
    quit_on_close: bool,
) -> Result<()> {
    let driver = devices.0;
    let shared = GuiShared {
        status: Arc::new(Mutex::new(Status::default())),
        spectrum: Arc::new(Mutex::new(Vec::new())),
        bypass_flag: paths::bypass_flag_path(),
        tuning_flag: paths::tuning_path(),
        sound_file: paths::signature_path(),
        now_playing: None,
        setup: Some(SetupShared {
            install: Arc::new(Mutex::new(crate::setup::Install::default())),
            driver: driver.to_string(),
            blocker,
        }),
        devices: (devices.0.to_string(), devices.1.to_string()),
        active: devices.1.to_string(),
        presence: presence.clone(),
    };
    let mut last_check = std::time::Instant::now();
    while !stop.load(Ordering::Relaxed) {
        serve_once(listener, &shared);
        if quit_on_close && presence.gone() {
            stop.store(true, Ordering::Relaxed);
            break;
        }
        if last_check.elapsed() >= Duration::from_secs(2) {
            last_check = std::time::Instant::now();
            if crate::setup::blocker(devices.0, devices.1).is_none() {
                break;
            }
        }
    }
    Ok(())
}

pub fn run_gui(cfg: Config, port: u16, open_browser: bool, quit_on_close: bool) -> Result<()> {
    cfg.validate()?;
    let gui_open = cfg.gui.open.clone();
    let mut cfg = cfg;
    let status = Arc::new(Mutex::new(Status::default()));
    let spectrum = Arc::new(Mutex::new(Vec::new()));
    let stop = Arc::new(AtomicBool::new(false));

    {
        let stop2 = stop.clone();
        ctrlc::set_handler(move || stop2.store(true, Ordering::Relaxed))
            .context("install ctrl-c handler")?;
    }

    let devices = (
        cfg.standalone.input_device.clone(),
        cfg.standalone.output_device.clone(),
    );
    let listener = bind(port)?;
    let presence = Arc::new(Presence::new());
    let url = format!("http://127.0.0.1:{port}");
    let mut already_open = false;
    let wait = |blocker: crate::setup::Blocker, output: &str, opened: &mut bool| -> Result<()> {
        println!(
            "leungo GUI: {url}  (waiting for audio setup: {})",
            blocker.as_str()
        );
        if open_browser && !*opened {
            open_window(&url, &gui_open);
        }
        *opened = true;
        wait_for_setup(
            &listener,
            (&devices.0, output),
            blocker,
            &stop,
            &presence,
            quit_on_close,
        )
    };

    if !crate::setup::driver_present(&devices.0) {
        wait(crate::setup::Blocker::Driver, &devices.1, &mut already_open)?;
    }
    if stop.load(Ordering::Relaxed) {
        return Ok(());
    }

    let managed = cfg!(target_os = "macos") && cfg.standalone.manage_output;
    let mut active = devices.1.clone();
    let mut route = None;
    if managed {
        let (r, real) = crate::routing::Route::engage(&devices.0, &devices.1)?;
        active = real.clone();
        cfg.standalone.output_device = real;
        route = Some(r);
    } else if let Some(blocker) = crate::setup::blocker(&devices.0, &devices.1) {
        wait(blocker, &devices.1, &mut already_open)?;
    }
    if stop.load(Ordering::Relaxed) {
        return Ok(());
    }

    let mut platform = crate::platform::setup(&mut cfg, stop.clone())?;
    if let Some(r) = route.as_mut() {
        r.activate()?;
        if let Some(set) = platform.set_makeup_db.take() {
            r.sync_volume(&active, set, stop.clone());
        }
    }
    let mut engine = Engine::new(
        cfg,
        platform.source,
        platform.applier,
        status.clone(),
        stop.clone(),
    );
    engine.set_spectrum_out(spectrum.clone());
    engine.set_track_log(paths::track_log_path());
    if let Some(c) = paths::load_fitted_classifier() {
        engine.set_classifier(c);
    }
    engine.set_tuning_path(paths::tuning_path());
    engine.set_signature_path(paths::signature_path());
    if let Some(rx) = platform.track_events {
        engine.set_track_events(rx);
    }
    let now_playing = platform.now_playing;

    let engine_handle = std::thread::Builder::new()
        .name("ae-engine".into())
        .spawn(move || engine.run())
        .context("spawn engine thread")?;

    // Keep leungo status working while the GUI runs.
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
        sound_file: paths::signature_path(),
        now_playing,
        setup: None,
        devices,
        active,
        presence,
    };

    println!("leungo GUI: {url}  (ctrl-c to stop)");

    if open_browser && !already_open {
        open_window(&url, &gui_open);
    }

    let mut last_health = std::time::Instant::now();
    while !stop.load(Ordering::Relaxed) {
        serve_once(&listener, &shared);
        if quit_on_close && shared.presence.gone() {
            break;
        }
        if last_health.elapsed() >= Duration::from_secs(2) {
            last_health = std::time::Instant::now();
            let lost = crate::setup::device_lost(&shared.devices.0, &shared.active);
            let moved = managed
                && shared.devices.1.is_empty()
                && crate::setup::default_moved(&shared.devices.0, &shared.active);
            if lost || moved {
                restart_soon();
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

/// Chromium-family browsers that support `--app=URL`, in order of preference.
#[cfg_attr(target_os = "macos", allow(dead_code))]
const APP_BROWSERS: [&str; 6] = [
    "google-chrome-stable",
    "google-chrome",
    "chromium",
    "chromium-browser",
    "brave-browser",
    "microsoft-edge",
];

#[cfg_attr(target_os = "macos", allow(dead_code))]
fn pick_app_browser(exists: impl Fn(&str) -> bool) -> Option<&'static str> {
    APP_BROWSERS.iter().copied().find(|b| exists(b))
}

#[cfg_attr(target_os = "macos", allow(dead_code))]
fn on_path(name: &str) -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join(name).is_file()))
        .unwrap_or(false)
}

fn spawn_quiet(cmd: &mut std::process::Command) -> bool {
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .is_ok()
}

/// Shows the dashboard. `app` mode opens a chromeless window (no tabs, no
/// address bar) so it feels like its own program; anything that fails falls
/// back to the default browser, and finally to just printing the address.
fn open_window(url: &str, mode: &str) {
    if mode == "none" {
        return;
    }
    if mode == "app" {
        #[cfg(target_os = "macos")]
        {
            let chrome = std::path::Path::new("/Applications/Google Chrome.app");
            if chrome.exists()
                && spawn_quiet(
                    std::process::Command::new("open")
                        .args(["-na", "Google Chrome", "--args"])
                        .arg(format!("--app={url}")),
                )
            {
                return;
            }
        }
        #[cfg(not(target_os = "macos"))]
        if let Some(bin) = pick_app_browser(on_path)
            && spawn_quiet(std::process::Command::new(bin).arg(format!("--app={url}")))
        {
            return;
        }
    }
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    if !spawn_quiet(std::process::Command::new(opener).arg(url)) {
        println!("open {url} in a browser");
    }
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
    let target = parts.next().unwrap_or("/");
    let (path, query) = target.split_once('?').unwrap_or((target, ""));

    match (method.as_str(), path) {
        ("GET", "/") => {
            respond(
                &mut stream,
                200,
                "text/html; charset=utf-8",
                PAGE.as_bytes(),
            );
        }
        ("POST", "/api/bye") => {
            shared.presence.bye();
            respond(&mut stream, 200, "application/json", b"{\"ok\":true}");
        }
        ("GET", "/api/state") => {
            shared.presence.touch();
            let body = serde_json::to_vec(&state_json(shared)).unwrap_or_default();
            respond(&mut stream, 200, "application/json", &body);
        }
        ("GET", "/api/devices") => {
            let choices = crate::setup::choices(&shared.devices.0);
            let body = json!({
                "inputs": choices.inputs,
                "outputs": choices.outputs,
                "input": shared.devices.0,
                "output": shared.devices.1,
                "active": shared.active,
            });
            respond(
                &mut stream,
                200,
                "application/json",
                body.to_string().as_bytes(),
            );
        }
        ("POST", "/api/devices") => {
            let choices = crate::setup::choices(&shared.devices.0);
            let (mut input, mut output) = (None, None);
            for pair in query.split('&') {
                match pair.split_once('=') {
                    Some(("input", v)) => input = Some(percent_decode(v)),
                    Some(("output", v)) => output = Some(percent_decode(v)),
                    _ => {}
                }
            }
            let valid = match (&input, &output) {
                (Some(i), Some(o)) => {
                    crate::setup::is_known_device(&choices.inputs, i)
                        && (o.is_empty() || choices.outputs.contains(o))
                }
                _ => false,
            };
            if !valid {
                respond(&mut stream, 400, "text/plain", b"unknown device");
            } else if leungo_core::paths::save_device_choice(
                input.as_deref().unwrap_or_default(),
                output.as_deref().unwrap_or_default(),
            )
            .is_err()
            {
                respond(&mut stream, 500, "text/plain", b"could not save");
            } else {
                respond(&mut stream, 200, "application/json", b"{\"ok\":true}");
                restart_soon();
            }
        }
        ("POST", p) if p.starts_with("/api/mode/") => {
            let mode = &p["/api/mode/".len()..];
            if (mode == "simple" || mode == "advanced") && paths::save_ui_mode(mode).is_ok() {
                respond(&mut stream, 200, "application/json", b"{\"ok\":true}");
            } else {
                respond(&mut stream, 400, "text/plain", b"unknown mode");
            }
        }
        ("POST", "/api/setup/install") => match &shared.setup {
            Some(setup) => {
                start_install(setup);
                respond(&mut stream, 200, "application/json", b"{\"ok\":true}");
            }
            None => respond(&mut stream, 404, "text/plain", b"not found"),
        },
        ("POST", "/api/bypass/on") => {
            let _ = std::fs::write(&shared.bypass_flag, "");
            respond(&mut stream, 200, "application/json", b"{\"ok\":true}");
        }
        ("POST", "/api/bypass/off") => {
            let _ = std::fs::remove_file(&shared.bypass_flag);
            respond(&mut stream, 200, "application/json", b"{\"ok\":true}");
        }
        ("POST", "/api/sound") => match sound_json(query) {
            Some(body) => {
                let tmp = shared.sound_file.with_extension("tmp");
                let ok = std::fs::write(&tmp, body).is_ok()
                    && std::fs::rename(&tmp, &shared.sound_file).is_ok();
                if ok {
                    respond(&mut stream, 200, "application/json", b"{\"ok\":true}");
                } else {
                    respond(&mut stream, 500, "text/plain", b"could not write");
                }
            }
            None => respond(&mut stream, 400, "text/plain", b"bad values"),
        },
        ("POST", p) if p.starts_with("/api/tuning/") => {
            let name = &p["/api/tuning/".len()..];
            if name == "custom" || leungo_core::Preset::parse(name).is_some() {
                let _ = std::fs::write(&shared.tuning_flag, name);
                respond(&mut stream, 200, "application/json", b"{\"ok\":true}");
            } else {
                respond(&mut stream, 400, "text/plain", b"unknown preset");
            }
        }
        _ => respond(&mut stream, 404, "text/plain", b"not found"),
    }
}

/// Builds the sound file from `bass=..&warmth=..&presence=..&air=..&device=..`.
/// Unknown keys are ignored; a value that is not a finite number rejects the
/// whole request. The engine clamps ranges.
fn sound_json(query: &str) -> Option<String> {
    let mut out = serde_json::Map::new();
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (k, v) = pair.split_once('=')?;
        if !["bass", "warmth", "presence", "air", "device"].contains(&k) {
            continue;
        }
        let n: f64 = v.parse().ok()?;
        if !n.is_finite() {
            return None;
        }
        out.insert(k.to_string(), serde_json::json!(n));
    }
    Some(serde_json::Value::Object(out).to_string())
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
                match hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                    Some(b) => {
                        out.push(b);
                        i += 2;
                    }
                    None => out.push(b'%'),
                }
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Replaces this process with a fresh copy of itself so the engine reopens
/// the chosen devices; the browser window is left alone.
fn restart_soon() {
    std::thread::spawn(|| {
        std::thread::sleep(Duration::from_millis(300));
        let _ = crate::routing::restore_saved();
        let Ok(exe) = std::env::current_exe() else {
            return;
        };
        let mut args: Vec<String> = std::env::args().skip(1).collect();
        if !args.iter().any(|a| a == "--no-open") {
            args.push("--no-open".into());
        }
        use std::os::unix::process::CommandExt;
        let err = std::process::Command::new(exe).args(args).exec();
        tracing::warn!("restart failed: {err}");
    });
}

fn start_install(setup: &SetupShared) {
    use crate::setup::{Phase, install_driver};
    {
        let Ok(mut cur) = setup.install.lock() else {
            return;
        };
        if cur.phase == Phase::Downloading {
            return;
        }
        cur.phase = Phase::Downloading;
        cur.detail = "Starting".into();
    }
    let state = setup.install.clone();
    std::thread::spawn(move || {
        let report = |phase: Phase, detail: &str| {
            if let Ok(mut cur) = state.lock() {
                cur.phase = phase;
                cur.detail = detail.to_string();
            }
        };
        if let Err(e) = install_driver(&report) {
            report(Phase::Failed, &format!("{e:#}"));
        }
    });
}

fn setup_json(setup: &SetupShared) -> serde_json::Value {
    let cur = setup.install.lock().map(|c| c.clone()).unwrap_or_default();
    json!({
        "driver": setup.driver,
        "blocker": setup.blocker.as_str(),
        "canInstall": crate::setup::is_blackhole(&setup.driver),
        "phase": cur.phase.as_str(),
        "detail": cur.detail,
    })
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
        "sound": {
            "bass": status.signature.bass_db,
            "warmth": status.signature.warmth_db,
            "presence": status.signature.presence_db,
            "air": status.signature.air_db,
            "device": status.device_strength,
        },
        "state": status.state,
        "bypassed": status.bypassed,
        "archetype": status.archetype,
        "confidence": status.confidence,
        "tracks": status.tracks_analyzed,
        "gains": status.gains,
        "message": status.message,
        "spectrum": spectrum,
        "bandFreqs": BAND_FREQS.to_vec(),
        "setup": shared.setup.as_ref().map(setup_json),
        "page": page_id(),
        "playingTo": shared.active,
        "mode": paths::load_ui_mode(),
    })
}

#[cfg(test)]
mod tests {
    use super::{percent_decode, pick_app_browser, sound_json};

    #[test]
    fn decodes_device_names_from_a_query() {
        assert_eq!(percent_decode("AB17X%20USB+Audio"), "AB17X USB Audio");
        assert_eq!(
            percent_decode("MacBook%20Air%20Speakers"),
            "MacBook Air Speakers"
        );
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%zz"), "%zz");
    }

    #[test]
    fn builds_json_from_a_query() {
        let j = sound_json("bass=3.5&air=-1&device=0.6&junk=9").unwrap();
        let v: serde_json::Value = serde_json::from_str(&j).unwrap();
        assert_eq!(v["bass"], 3.5);
        assert_eq!(v["air"], -1.0);
        assert_eq!(v["device"], 0.6);
        assert!(v.get("junk").is_none());
    }

    #[test]
    fn rejects_non_numbers() {
        assert!(sound_json("bass=loud").is_none());
        assert!(sound_json("bass=NaN").is_none());
        assert!(sound_json("bass").is_none());
        assert_eq!(sound_json("").unwrap(), "{}");
    }

    #[test]
    fn picks_the_first_installed_chromium_browser() {
        assert_eq!(
            pick_app_browser(|b| b == "chromium" || b == "brave-browser"),
            Some("chromium")
        );
        assert_eq!(pick_app_browser(|_| false), None);
        assert_eq!(
            pick_app_browser(|_| true),
            Some("google-chrome-stable"),
            "preference order"
        );
    }
}
