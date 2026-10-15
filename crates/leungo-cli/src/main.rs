//! leungo: adaptive per-track parametric EQ driven by spectral analysis.

use anyhow::{Context, Result, bail, ensure};
use clap::{Parser, Subcommand};
use leungo_core::{Engine, Status, paths};
use leungo_eq::BAND_FREQS;
mod gui;
mod platform;
mod routing;
mod setup;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Parser)]
#[command(
    name = "leungo",
    version,
    about = "Adaptive per-track parametric EQ driven by spectral analysis"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the daemon: capture system audio, analyze per track, retune EasyEffects.
    Run,
    /// Run the daemon with the browser GUI at http://127.0.0.1:<port>.
    Gui {
        #[arg(long)]
        port: Option<u16>,
        /// Do not open the browser window automatically.
        #[arg(long)]
        no_open: bool,
        /// Quit when the dashboard window is closed.
        #[arg(long)]
        quit_on_close: bool,
    },
    /// Print the daemon status.
    Status,
    /// Watch MPRIS players and print track-change signatures (diagnostic).
    MonitorMpris,
    /// Show or set the tuning preset (conservative | balanced | aggressive | custom).
    Tuning { name: Option<String> },
    /// Show or set the sound signature: bass, warmth, presence, air (dB) and
    /// device (0 to 1, how much of the headphone correction is applied).
    Sound {
        #[arg(long, allow_negative_numbers = true)]
        bass: Option<f64>,
        #[arg(long, allow_negative_numbers = true)]
        warmth: Option<f64>,
        #[arg(long, allow_negative_numbers = true)]
        presence: Option<f64>,
        #[arg(long, allow_negative_numbers = true)]
        air: Option<f64>,
        #[arg(long)]
        device: Option<f64>,
    },
    /// Bypass the adaptive EQ (on | off).
    Bypass { state: String },
    /// Import base_curve gains from an existing EasyEffects output preset.
    Calibrate { preset: String },
    /// Build eq.device_curve from AutoEq ParametricEQ.txt files of your
    /// headphones/IEMs (several sources are averaged).
    Device { files: Vec<PathBuf> },
    /// Analyze a WAV file and print the EQ it would produce.
    Analyze { file: PathBuf },
    /// Fit archetype prototypes to the tracks you have listened to.
    Fit,
    /// List audio devices (macOS standalone engine).
    Devices,
    /// Put the system output back after an unclean exit (macOS).
    Restore,
    /// Check the audio prerequisites and install the BlackHole driver (macOS).
    Setup,
    /// Print the config file path.
    ConfigPath,
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
    leungo_core::paths::migrate_legacy_dirs();
    if let Err(e) = dispatch(Cli::parse()) {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn dispatch(cli: Cli) -> Result<()> {
    match cli.cmd {
        Cmd::Run => run(),
        Cmd::Gui {
            port,
            no_open,
            quit_on_close,
        } => gui(port, no_open, quit_on_close),
        Cmd::Status => status(),
        Cmd::MonitorMpris => monitor_mpris(),
        Cmd::Devices => platform::print_devices(),
        Cmd::Setup => setup_cmd(),
        Cmd::Restore => restore_cmd(),
        Cmd::Tuning { name } => tuning(name.as_deref()),
        Cmd::Sound {
            bass,
            warmth,
            presence,
            air,
            device,
        } => sound([bass, warmth, presence, air, device]),
        Cmd::Bypass { state } => bypass(&state),
        Cmd::Calibrate { preset } => calibrate(&preset),
        Cmd::Device { files } => device(&files),
        Cmd::Analyze { file } => analyze(&file),
        Cmd::Fit => fit(),
        Cmd::ConfigPath => {
            println!("{}", paths::config_path().display());
            Ok(())
        }
    }
}

fn run() -> Result<()> {
    let mut cfg = paths::load_or_create_config()?;
    paths::apply_device_choice(&mut cfg);
    cfg.validate()?;
    let status = Arc::new(Mutex::new(Status::default()));
    let stop = Arc::new(AtomicBool::new(false));

    {
        let stop2 = stop.clone();
        ctrlc::set_handler(move || stop2.store(true, Ordering::Relaxed))
            .context("install ctrl-c handler")?;
    }

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
            })?;
    }

    let platform = platform::setup(&mut cfg, stop.clone())?;
    let mut engine = Engine::new(cfg, platform.source, platform.applier, status, stop.clone());
    engine.set_tuning_path(paths::tuning_path());
    engine.set_signature_path(paths::signature_path());
    if let Some(rx) = platform.track_events {
        engine.set_track_events(rx);
    }
    engine.set_track_log(paths::track_log_path());
    if let Some(c) = paths::load_fitted_classifier() {
        engine.set_classifier(c);
    }
    tracing::info!("running; ctrl-c to stop");
    engine.run()?;
    stop.store(true, Ordering::Relaxed);
    tracing::info!("stopped");
    Ok(())
}

fn monitor_mpris() -> Result<()> {
    let rx = leungo_players::spawn()?;
    println!("watching MPRIS players for track changes; ctrl-c to stop");
    for id in rx {
        println!("track changed: {id}");
    }
    Ok(())
}

fn restore_cmd() -> Result<()> {
    match routing::restore_saved()? {
        Some(name) => println!("system output restored to {name}"),
        None => println!("nothing to restore"),
    }
    Ok(())
}

fn setup_cmd() -> Result<()> {
    if !cfg!(target_os = "macos") {
        println!("nothing to set up: on Linux leungo captures the PipeWire monitor directly.");
        return Ok(());
    }
    let mut cfg = paths::load_or_create_config()?;
    paths::apply_device_choice(&mut cfg);
    let driver = cfg.standalone.input_device.clone();
    if setup::driver_present(&driver) {
        println!("{driver}: found.");
    } else if setup::is_blackhole(&driver) {
        setup::install_driver(&|_, detail| println!("{detail}"))?;
        println!("After the driver is active, run `leungo setup` again to confirm.");
    } else {
        bail!("no input device matching {driver:?}; set standalone.input_device or install it");
    }
    println!("Next: choose BlackHole 2ch as the system output in System Settings > Sound,");
    println!(
        "then set standalone.output_device in {} to your speakers.",
        paths::config_path().display()
    );
    Ok(())
}

fn gui(port_arg: Option<u16>, no_open: bool, quit_on_close: bool) -> Result<()> {
    let mut cfg = paths::load_or_create_config()?;
    paths::apply_device_choice(&mut cfg);
    cfg.validate()?;
    let port = port_arg.unwrap_or(cfg.gui.port);
    gui::run_gui(cfg, port, !no_open, quit_on_close)
}

fn status() -> Result<()> {
    let path = paths::status_path();
    if !path.exists() {
        println!("daemon not running (no status file at {})", path.display());
        return Ok(());
    }
    let raw = std::fs::read_to_string(&path)?;
    let s: Status = serde_json::from_str(&raw)?;
    if s.bypassed {
        println!("state: {} (bypassed)", s.state);
    } else {
        println!("state: {}", s.state);
    }
    match &s.archetype {
        Some(a) => println!("archetype: {a} (confidence {:.2})", s.confidence),
        None => println!("archetype: (none yet)"),
    }
    println!("tracks analyzed: {}", s.tracks_analyzed);
    if s.gains.len() == BAND_FREQS.len() {
        let parts: Vec<String> = BAND_FREQS
            .iter()
            .zip(&s.gains)
            .map(|(f, g)| format!("{f:.0}Hz {g:+.1}"))
            .collect();
        println!("eq: {}", parts.join("  "));
    }
    if let Some(m) = &s.message {
        println!("last error: {m}");
    }
    Ok(())
}

fn tuning(name: Option<&str>) -> Result<()> {
    let path = paths::tuning_path();
    let Some(name) = name else {
        let cfg = paths::load_or_create_config()?;
        let current = std::fs::read_to_string(&path)
            .ok()
            .map(|t| t.trim().to_string())
            .or_else(|| cfg.tuning.map(|p| p.as_str().to_string()))
            .unwrap_or_else(|| "custom".into());
        println!("tuning: {current}");
        println!("options: conservative, balanced, aggressive, custom");
        return Ok(());
    };
    ensure!(
        name == "custom" || leungo_core::Preset::parse(name).is_some(),
        "unknown preset {name:?}; use conservative, balanced, aggressive or custom"
    );
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, name)?;
    println!("tuning set to {name} (a running daemon picks it up within a second)");
    Ok(())
}

fn sound(values: [Option<f64>; 5]) -> Result<()> {
    let keys = ["bass", "warmth", "presence", "air", "device"];
    let path = paths::signature_path();
    let cfg = paths::load_or_create_config()?;
    // Start from what is already set: the runtime file, else the config.
    let mut cur: serde_json::Map<String, serde_json::Value> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_else(|| {
            let s = cfg.eq.signature;
            let mut m = serde_json::Map::new();
            m.insert("bass".into(), s.bass_db.into());
            m.insert("warmth".into(), s.warmth_db.into());
            m.insert("presence".into(), s.presence_db.into());
            m.insert("air".into(), s.air_db.into());
            m.insert("device".into(), cfg.eq.device_strength.into());
            m
        });
    let mut changed = false;
    for (k, v) in keys.iter().zip(values) {
        if let Some(v) = v {
            ensure!(v.is_finite(), "{k} must be a number");
            cur.insert((*k).into(), v.into());
            changed = true;
        }
    }
    if changed {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, serde_json::Value::Object(cur.clone()).to_string())?;
        println!("sound updated (a running daemon applies it within a second)");
    }
    for k in keys {
        let v = cur.get(k).and_then(|x| x.as_f64()).unwrap_or(0.0);
        match k {
            "device" => println!("  {k:<9} {:.0}%", v * 100.0),
            _ => println!("  {k:<9} {v:+.1} dB"),
        }
    }
    Ok(())
}

fn bypass(state: &str) -> Result<()> {
    let path = paths::bypass_flag_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    match state {
        "on" => {
            std::fs::write(&path, "")?;
            println!("bypass ON (run 'leungo bypass off' to restore)");
        }
        "off" => {
            let _ = std::fs::remove_file(&path);
            println!("bypass OFF");
        }
        _ => bail!("use 'on' or 'off'"),
    }
    Ok(())
}

fn calibrate(preset: &str) -> Result<()> {
    let path = if Path::new(preset).exists() {
        PathBuf::from(preset)
    } else {
        leungo_applier_ee::default_ee_output_dir().join(format!("{preset}.json"))
    };
    let raw = std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let v: serde_json::Value = serde_json::from_str(&raw).context("parse preset json")?;
    let left = v["output"]["equalizer"]["left"]
        .as_object()
        .context("preset has no output.equalizer.left")?;
    let mut bands: Vec<(f64, f64)> = left
        .values()
        .filter_map(|b| {
            let f = b["frequency"].as_f64()?;
            let g = b["gain"].as_f64()?;
            Some((f, g))
        })
        .collect();
    ensure!(!bands.is_empty(), "preset has no equalizer bands");
    bands.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());

    let half_oct = 0.5 * std::f64::consts::LN_2;
    let curve: Vec<f64> = BAND_FREQS
        .iter()
        .map(|&f| {
            let mut best = (f64::INFINITY, 0.0f64);
            for &(bf, bg) in &bands {
                let d = (bf.ln() - f.ln()).abs();
                if d < best.0 {
                    best = (d, bg);
                }
            }
            if best.0 <= half_oct { best.1 } else { 0.0 }
        })
        .collect();

    let mut cfg = paths::load_or_create_config()?;
    cfg.eq.base_curve = curve.clone();
    paths::save_config(&cfg)?;
    println!("base_curve imported from {}:", path.display());
    for (f, g) in BAND_FREQS.iter().zip(&curve) {
        println!("  {f:>6.0} Hz  {g:+.2} dB");
    }
    Ok(())
}

fn device(files: &[PathBuf]) -> Result<()> {
    ensure!(!files.is_empty(), "give at least one ParametricEQ.txt file");
    let mut cfg = paths::load_or_create_config()?;
    let fs = cfg.capture.rate as f64;
    let mut profiles = Vec::new();
    for f in files {
        let text = std::fs::read_to_string(f).with_context(|| format!("read {}", f.display()))?;
        let p = leungo_eq::device::parse_parametric(&text)
            .map_err(|e| anyhow::anyhow!("{}: {e}", f.display()))?;
        println!("{}: {} filters", f.display(), p.len());
        profiles.push(p);
    }
    let solver = leungo_eq::EqSolver::new(
        cfg.eq.frequencies.clone(),
        cfg.eq.q,
        cfg.eq.clamp_db,
        cfg.eq.ridge,
        fs,
        cfg.analysis.fft_size,
    );
    let target = leungo_eq::device::consensus_db(&profiles, &solver.points, fs);
    let zeros = vec![0.0; target.len()];
    let bands = solver.solve(&target, &zeros);
    // Headroom: AutoEq ships a negative preamp with every profile because the
    // corrections are net boosts. Keep the largest gain at 0 dB instead, a
    // fixed level offset that is the same for every track.
    let headroom = bands.iter().map(|b| b.gain_db).fold(0.0f64, f64::max);
    cfg.eq.device_curve = bands.iter().map(|b| b.gain_db - headroom).collect();

    // How well do 8 bands reproduce the consensus? (level offset removed)
    let model: Vec<f64> = solver
        .points
        .iter()
        .map(|&f| {
            bands
                .iter()
                .map(|b| leungo_dsp::peaking_magnitude_db(f, b.frequency, b.gain_db, b.q, fs))
                .sum::<f64>()
        })
        .collect();
    let n = target.len() as f64;
    let off = (target.iter().sum::<f64>() - model.iter().sum::<f64>()) / n;
    let rms = (target
        .iter()
        .zip(&model)
        .map(|(t, m)| (t - m - off).powi(2))
        .sum::<f64>()
        / n)
        .sqrt();
    paths::save_config(&cfg)?;
    println!(
        "device_curve written ({} sources averaged):",
        profiles.len()
    );
    for (b, g) in bands.iter().zip(&cfg.eq.device_curve) {
        println!("  {:>6.0} Hz  {g:+.2} dB", b.frequency);
    }
    println!("level offset for headroom: {:+.1} dB", -headroom);
    println!(
        "fit error vs consensus: {rms:.2} dB rms over {} points",
        target.len()
    );
    Ok(())
}

fn analyze(file: &Path) -> Result<()> {
    let cfg = paths::load_or_create_config()?;
    let mut reader =
        hound::WavReader::open(file).with_context(|| format!("open {}", file.display()))?;
    let spec = reader.spec();
    if spec.sample_rate != cfg.capture.rate {
        bail!(
            "file is {} Hz but capture rate is {} Hz; resample first",
            spec.sample_rate,
            cfg.capture.rate
        );
    }
    let samples: Vec<f64> = match spec.sample_format {
        hound::SampleFormat::Float => reader
            .samples::<f32>()
            .map_while(Result::ok)
            .map(|s| s as f64)
            .collect(),
        hound::SampleFormat::Int => {
            let bits = spec.bits_per_sample;
            match bits {
                16 | 24 | 32 => {
                    let scale = (1i64 << (bits - 1)) as f64;
                    reader
                        .samples::<i32>()
                        .map_while(Result::ok)
                        .map(|s| s as f64 / scale)
                        .collect()
                }
                _ => bail!("unsupported bits per sample: {bits}"),
            }
        }
    };
    ensure!(!samples.is_empty(), "no samples decoded");
    let channels = spec.channels as usize;
    let mono: Vec<f64> = if channels > 1 {
        samples
            .chunks(channels)
            .map(|c| c.iter().sum::<f64>() / channels as f64)
            .collect()
    } else {
        samples
    };
    let result = leungo_core::offline::analyze_samples(&mono, spec.sample_rate as f64, &cfg)?;
    println!(
        "archetype: {} (confidence {:.2})",
        result.archetype.as_str(),
        result.confidence
    );
    println!(
        "reliability {:.2}  transient fraction {:.2}",
        result.reliability, result.transient
    );
    println!("bands:");
    for b in &result.bands {
        println!(
            "  {:>6.0} Hz  q {:.2}  {:+.2} dB",
            b.frequency, b.q, b.gain_db
        );
    }
    Ok(())
}

fn fit() -> Result<()> {
    let log = paths::track_log_path();
    let raw = std::fs::read_to_string(&log).with_context(|| {
        format!(
            "read {} (play some tracks with the daemon first)",
            log.display()
        )
    })?;
    let mut samples: Vec<[f64; 4]> = Vec::new();
    for line in raw.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let Some(d) = v["d"].as_array() else { continue };
        if d.len() != 4 {
            continue;
        }
        let mut x = [0.0f64; 4];
        for (dst, src) in x.iter_mut().zip(d.iter()) {
            *dst = src.as_f64().unwrap_or(0.0);
        }
        samples.push(x);
    }
    println!("{} analyzed tracks in {}", samples.len(), log.display());
    let Some(r) = leungo_analysis::fit(&samples) else {
        bail!(
            "not enough usable data: need at least {} tracks that fall into two or more groups",
            leungo_analysis::MIN_SAMPLES
        );
    };
    for (a, n) in &r.counts {
        println!("  {:<16} {n}", a.as_str());
    }
    println!(
        "mean confidence: {:.0}% fitted, {:.0}% with the built-in prototypes",
        r.mean_confidence * 100.0,
        r.baseline_mean_confidence * 100.0
    );
    println!(
        "note: this measures how typical a track is of its group, not whether the group is right"
    );
    let path = paths::fitted_classifier_path();
    std::fs::write(&path, serde_json::to_string_pretty(&r.classifier)?)?;
    println!("wrote {} (restart the daemon to use it)", path.display());
    Ok(())
}
