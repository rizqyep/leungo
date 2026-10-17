//! Integration tests: the full engine pipeline with a scripted source and a
//! recording applier.

use anyhow::Result;
use leungo_core::{Config, Engine, EqApplier, EqOutput, SampleSource, SourceEvent, Status};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

struct ScriptSource {
    events: std::vec::IntoIter<SourceEvent>,
}

impl SampleSource for ScriptSource {
    fn next(&mut self) -> Result<SourceEvent> {
        Ok(self.events.next().unwrap_or(SourceEvent::Ended))
    }
}

#[derive(Default)]
struct RecordingApplier {
    applied: Arc<Mutex<Vec<Vec<f64>>>>,
    bypasses: Arc<Mutex<u32>>,
}

impl EqApplier for RecordingApplier {
    fn apply_eq(&mut self, eq: &EqOutput) -> Result<()> {
        self.applied.lock().unwrap().push(eq.gains.clone());
        Ok(())
    }

    fn set_bypass(&mut self, on: bool) -> Result<()> {
        if on {
            *self.bypasses.lock().unwrap() += 1;
        }
        Ok(())
    }
}

/// Curves switch at once here, so each commit is one apply. The glide has
/// its own test.
fn cfg() -> Config {
    let mut c = Config::default();
    c.analysis.transition_s = 0.0;
    c
}

const FS: f64 = 48_000.0;
/// Seconds of audio per 4096-interleaved-sample chunk (2048 frames).
const SECS_PER_CHUNK: f64 = 2048.0 / FS;

const BASS_TRACK: &[(f64, f64)] = &[
    (55.0, 0.3),
    (80.0, 0.3),
    (110.0, 0.3),
    (160.0, 0.3),
    (1000.0, 0.003),
];

const VOCAL_TRACK: &[(f64, f64)] = &[
    (80.0, 0.003),
    (220.0, 0.3),
    (440.0, 0.25),
    (880.0, 0.2),
    (1320.0, 0.15),
    (2200.0, 0.12),
    (8000.0, 0.003),
];

fn chunk_after(seconds: f64) -> usize {
    (seconds / SECS_PER_CHUNK) as usize
}

/// Gapless splice of two tracks (no silence, like Spotify buffering), plus a
/// track hint sent right before the second track's first chunk.
struct HintedSource {
    chunks: std::vec::IntoIter<SourceEvent>,
    hint_at_chunk: usize,
    tx: std::sync::mpsc::Sender<String>,
    seed: Option<String>,
    idx: usize,
}

impl SampleSource for HintedSource {
    fn next(&mut self) -> Result<SourceEvent> {
        if let Some(seed) = self.seed.take() {
            let _ = self.tx.send(seed);
        }
        if self.idx == self.hint_at_chunk {
            let _ = self.tx.send("beta".into());
        }
        self.idx += 1;
        self.chunks
            .next()
            .ok_or_else(|| anyhow::anyhow!("exhausted test source"))
    }
}

/// Deterministic white noise generator (LCG).
struct WhiteGen {
    seed: u64,
}

impl WhiteGen {
    fn next(&mut self) -> f64 {
        self.seed = self
            .seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((self.seed >> 33) as f64 / (u64::MAX >> 33) as f64) * 2.0 - 1.0
    }
}

/// Paul Kellet's 7-stage pink noise filter: every band carries energy, like
/// a real mix.
struct PinkGen {
    b: [f64; 7],
}

impl PinkGen {
    fn next(&mut self, white: f64) -> f64 {
        self.b[0] = 0.99886 * self.b[0] + white * 0.0555179;
        self.b[1] = 0.99332 * self.b[1] + white * 0.0750759;
        self.b[2] = 0.96900 * self.b[2] + white * 0.1538520;
        self.b[3] = 0.86650 * self.b[3] + white * 0.3104856;
        self.b[4] = 0.55000 * self.b[4] + white * 0.5329522;
        self.b[5] = -0.7616 * self.b[5] - white * 0.0168980;
        let pink = self.b[0]
            + self.b[1]
            + self.b[2]
            + self.b[3]
            + self.b[4]
            + self.b[5]
            + self.b[6]
            + white * 0.5362;
        self.b[6] = white * 0.115926;
        pink * 0.11
    }
}

/// Pink noise floor plus band-emphasis tones, the way a real mix sits.
fn tone_chunks(tones: &[(f64, f64)], seconds: f64) -> Vec<SourceEvent> {
    let len = (seconds * FS) as usize;
    let mut events = Vec::new();
    let mut buf: Vec<f32> = Vec::new();
    let mut white = WhiteGen { seed: 0x5eed_0001 };
    let mut pink = PinkGen { b: [0.0; 7] };
    for i in 0..len {
        let mut m = pink.next(white.next());
        for &(f, a) in tones {
            m += a * (std::f64::consts::TAU * f * i as f64 / FS).sin();
        }
        buf.push(m as f32);
        buf.push(m as f32);
        if buf.len() >= 4096 {
            events.push(SourceEvent::Chunk(std::mem::take(&mut buf)));
        }
    }
    if !buf.is_empty() {
        events.push(SourceEvent::Chunk(buf));
    }
    events
}

fn silence_chunks(seconds: f64) -> Vec<SourceEvent> {
    let len = (seconds * FS) as usize * 2;
    (0..len)
        .step_by(4096)
        .map(|start| {
            let end = (start + 4096).min(len);
            SourceEvent::Chunk(vec![0.0f32; end - start])
        })
        .collect()
}

#[test]
fn two_tracks_produce_two_applies() {
    let mut events = tone_chunks(BASS_TRACK, 8.0);
    events.extend(silence_chunks(1.5));
    events.extend(tone_chunks(VOCAL_TRACK, 8.0));

    let applied = Arc::new(Mutex::new(Vec::new()));
    let status = Arc::new(Mutex::new(Status::default()));
    let stop = Arc::new(AtomicBool::new(false));
    let mut engine = Engine::new(
        cfg(),
        Box::new(ScriptSource {
            events: events.into_iter(),
        }),
        Box::new(RecordingApplier {
            applied: applied.clone(),
            bypasses: Arc::new(Mutex::new(0)),
        }),
        status.clone(),
        stop,
    );
    engine.run().unwrap();

    let applied = applied.lock().unwrap();
    assert_eq!(applied.len(), 2, "expected two applies");
    assert_ne!(applied[0], applied[1]);
    // bass track: excess bass measured, so the 63 Hz band sits below 4 kHz
    assert!(
        applied[0][0] < applied[0][6],
        "bass cut expected: 63Hz {:.2} vs 4kHz {:.2}",
        applied[0][0],
        applied[0][6]
    );
    // vocal track: mid emphasized instead of bass, so less bass cutting
    assert!(
        applied[1][0] > applied[0][0],
        "63Hz: vocal track {:.2} should cut less than bass track {:.2}",
        applied[1][0],
        applied[0][0]
    );

    let st = status.lock().unwrap();
    assert_eq!(st.tracks_analyzed, 2);
    assert_eq!(st.archetype.as_deref(), Some("vocal-centric"));
}

#[test]
fn bypass_flag_gates_apply() {
    let dir = std::env::temp_dir().join(format!("ae-bypass-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let flag = dir.join("bypass");
    std::fs::write(&flag, "").unwrap();

    let events = tone_chunks(&[(100.0, 0.2), (1000.0, 0.2), (4000.0, 0.2)][..], 7.0);
    let applied = Arc::new(Mutex::new(Vec::new()));
    let bypasses = Arc::new(Mutex::new(0));
    let status = Arc::new(Mutex::new(Status::default()));
    let stop = Arc::new(AtomicBool::new(false));
    let mut engine = Engine::new(
        cfg(),
        Box::new(ScriptSource {
            events: events.into_iter(),
        }),
        Box::new(RecordingApplier {
            applied: applied.clone(),
            bypasses: bypasses.clone(),
        }),
        status,
        stop,
    );
    engine.set_bypass_path(flag.clone());
    engine.run().unwrap();

    let applied = applied.lock().unwrap();
    assert!(applied.is_empty(), "bypassed engine must not apply");
    assert_eq!(*bypasses.lock().unwrap(), 1, "set_bypass(true) once");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn similar_track_is_still_analyzed_and_applied() {
    // Same tonal balance, barely different levels: the solved curve stays
    // under the reapply threshold, so nothing is applied the second time.
    let track_b: &[(f64, f64)] = &[
        (55.0, 0.285),
        (80.0, 0.285),
        (110.0, 0.285),
        (160.0, 0.285),
        (1000.0, 0.0029),
    ];
    let mut events = tone_chunks(BASS_TRACK, 7.0);
    events.extend(silence_chunks(1.5));
    events.extend(tone_chunks(track_b, 7.0));
    events.push(SourceEvent::Ended);

    let applied = Arc::new(Mutex::new(Vec::new()));
    let status = Arc::new(Mutex::new(Status::default()));
    let stop = Arc::new(AtomicBool::new(false));
    let mut engine = Engine::new(
        cfg(),
        Box::new(ScriptSource {
            events: events.into_iter(),
        }),
        Box::new(RecordingApplier {
            applied: applied.clone(),
            bypasses: Arc::new(Mutex::new(0)),
        }),
        status.clone(),
        stop,
    );
    engine.run().unwrap();

    let applied = applied.lock().unwrap();
    assert_eq!(applied.len(), 2, "every new track gets its own curve");
    for (a, b) in applied[0].iter().zip(applied[1].iter()) {
        assert!(
            (a - b).abs() < 1.0,
            "similar tracks, similar curves: {a:.2} vs {b:.2}"
        );
    }
    assert_eq!(status.lock().unwrap().tracks_analyzed, 2);
}

#[test]
fn mpris_hint_at_splice_reanalyzes() {
    // Same gapless splice, but the player monitor sends a hint at the exact
    // boundary: analysis must restart and the second track gets its own EQ.
    let mut events = tone_chunks(BASS_TRACK, 8.0);
    events.extend(tone_chunks(VOCAL_TRACK, 8.0));
    events.push(SourceEvent::Ended);

    let applied = Arc::new(Mutex::new(Vec::new()));
    let (tx, rx) = std::sync::mpsc::channel();
    let status = Arc::new(Mutex::new(Status::default()));
    let stop = Arc::new(AtomicBool::new(false));
    let mut engine = Engine::new(
        cfg(),
        Box::new(HintedSource {
            chunks: events.into_iter(),
            hint_at_chunk: chunk_after(8.0),
            tx,
            seed: Some("alpha".into()),
            idx: 0,
        }),
        Box::new(RecordingApplier {
            applied: applied.clone(),
            bypasses: Arc::new(Mutex::new(0)),
        }),
        status.clone(),
        stop,
    );
    engine.set_track_events(rx);
    engine.run().unwrap();

    let applied = applied.lock().unwrap();
    assert_eq!(applied.len(), 2, "the hint must trigger a second analysis");
    assert_ne!(applied[0], applied[1]);

    let st = status.lock().unwrap();
    assert_eq!(st.tracks_analyzed, 2);
    assert_eq!(st.archetype.as_deref(), Some("vocal-centric"));
}

fn run_and_collect(events: Vec<SourceEvent>) -> Vec<Vec<f64>> {
    let applied = Arc::new(Mutex::new(Vec::new()));
    let mut engine = Engine::new(
        cfg(),
        Box::new(ScriptSource {
            events: events.into_iter(),
        }),
        Box::new(RecordingApplier {
            applied: applied.clone(),
            bypasses: Arc::new(Mutex::new(0)),
        }),
        Arc::new(Mutex::new(Status::default())),
        Arc::new(AtomicBool::new(false)),
    );
    engine.run().unwrap();
    let out = applied.lock().unwrap();
    out.clone()
}

#[test]
fn same_track_gets_same_curve_regardless_of_previous_track() {
    let alone = run_and_collect(tone_chunks(VOCAL_TRACK, 8.0));

    // 8 s and 1.536 s are exact multiples of the 1024-sample hop, so the
    // second track starts on the same frame boundaries as the lone one. Any
    // difference is then history, not frame-alignment noise (which is up to
    // about 0.3 dB, see frame_alignment_noise_is_small).
    let mut events = tone_chunks(BASS_TRACK, 8.0);
    events.extend(silence_chunks(1.536));
    events.extend(tone_chunks(VOCAL_TRACK, 8.0));
    let after_bass = run_and_collect(events);

    assert_eq!(alone.len(), 1);
    assert_eq!(after_bass.len(), 2);
    for (a, b) in alone[0].iter().zip(after_bass[1].iter()) {
        assert!((a - b).abs() < 0.02, "history leaked: {a:.3} vs {b:.3}");
    }
}

#[test]
fn pause_and_resume_of_same_track_keeps_preset() {
    // Same player track id across a pause: the resumed audio (different
    // content here, to prove nothing is re-measured) must not re-analyze.
    let mut events = tone_chunks(BASS_TRACK, 8.0);
    events.extend(silence_chunks(3.0));
    events.extend(tone_chunks(VOCAL_TRACK, 8.0));

    let applied = Arc::new(Mutex::new(Vec::new()));
    let (tx, rx) = std::sync::mpsc::channel();
    tx.send("track-a".to_string()).unwrap();
    let mut engine = Engine::new(
        cfg(),
        Box::new(ScriptSource {
            events: events.into_iter(),
        }),
        Box::new(RecordingApplier {
            applied: applied.clone(),
            bypasses: Arc::new(Mutex::new(0)),
        }),
        Arc::new(Mutex::new(Status::default())),
        Arc::new(AtomicBool::new(false)),
    );
    engine.set_track_events(rx);
    engine.run().unwrap();

    assert_eq!(
        applied.lock().unwrap().len(),
        1,
        "resume must not re-analyze"
    );
}

fn gapless_splice_applies(defer_max_s: f64) -> usize {
    // Bass track spliced into a vocal track with no gap and no hint. Nothing
    // detects the change; only continuous refinement can follow it.
    let mut events = tone_chunks(BASS_TRACK, 8.0);
    events.extend(tone_chunks(VOCAL_TRACK, 40.0));
    let mut cfg = cfg();
    cfg.analysis.defer_max_s = defer_max_s;

    let applied = Arc::new(Mutex::new(Vec::new()));
    let mut engine = Engine::new(
        cfg,
        Box::new(ScriptSource {
            events: events.into_iter(),
        }),
        Box::new(RecordingApplier {
            applied: applied.clone(),
            bypasses: Arc::new(Mutex::new(0)),
        }),
        Arc::new(Mutex::new(Status::default())),
        Arc::new(AtomicBool::new(false)),
    );
    engine.run().unwrap();
    let n = applied.lock().unwrap();
    n.len()
}

#[test]
fn refinement_follows_a_gapless_splice() {
    assert!(
        gapless_splice_applies(1.0) >= 2,
        "slow refinement must adapt to the new section"
    );
}

#[test]
fn refinement_waits_for_a_quiet_moment() {
    assert_eq!(
        gapless_splice_applies(1000.0),
        1,
        "a steady tone has no quiet moment, so the loaded curve stays"
    );
}

#[test]
fn analyzed_tracks_are_logged_for_fitting() {
    let path = std::env::temp_dir().join(format!("ae-tracks-{}.jsonl", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let mut events = tone_chunks(BASS_TRACK, 8.0);
    events.extend(silence_chunks(1.5));
    events.extend(tone_chunks(VOCAL_TRACK, 8.0));
    let mut engine = Engine::new(
        cfg(),
        Box::new(ScriptSource {
            events: events.into_iter(),
        }),
        Box::new(RecordingApplier::default()),
        Arc::new(Mutex::new(Status::default())),
        Arc::new(AtomicBool::new(false)),
    );
    engine.set_track_log(path.clone());
    engine.run().unwrap();

    let raw = std::fs::read_to_string(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    let lines: Vec<serde_json::Value> = raw
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let (ends, starts): (Vec<_>, Vec<_>) = lines.iter().partition(|v| v["kind"] == "end");
    assert_eq!(starts.len(), 2, "one analysis line per track");
    assert_eq!(starts[0]["d"].as_array().unwrap().len(), 4);
    assert_eq!(ends.len(), 2, "one end line per track, the last on stop");
    let points = ends[0]["points"].as_array().unwrap().len();
    assert_eq!(ends[0]["shape"].as_array().unwrap().len(), points);
}

#[test]
fn tuning_file_switches_preset_live() {
    let dir = std::env::temp_dir().join(format!("ae-tuning-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("tuning");
    std::fs::write(&path, "aggressive").unwrap();

    let status = Arc::new(Mutex::new(Status::default()));
    let mut engine = Engine::new(
        cfg(),
        Box::new(ScriptSource {
            events: Vec::<SourceEvent>::new().into_iter(),
        }),
        Box::new(RecordingApplier {
            applied: Arc::new(Mutex::new(Vec::new())),
            bypasses: Arc::new(Mutex::new(0)),
        }),
        status.clone(),
        Arc::new(AtomicBool::new(false)),
    );
    assert_eq!(status.lock().unwrap().tuning, "custom");
    engine.set_tuning_path(path.clone());
    assert_eq!(status.lock().unwrap().tuning, "aggressive");

    std::fs::write(&path, "nonsense").unwrap();
    std::fs::write(&path, "conservative").unwrap();
    engine.set_tuning_path(path);
    assert_eq!(status.lock().unwrap().tuning, "conservative");
    let _ = std::fs::remove_dir_all(&dir);
}

fn final_archetype(events: Vec<SourceEvent>) -> Option<String> {
    let status = Arc::new(Mutex::new(Status::default()));
    let mut engine = Engine::new(
        cfg(),
        Box::new(ScriptSource {
            events: events.into_iter(),
        }),
        Box::new(RecordingApplier {
            applied: Arc::new(Mutex::new(Vec::new())),
            bypasses: Arc::new(Mutex::new(0)),
        }),
        status.clone(),
        Arc::new(AtomicBool::new(false)),
    );
    engine.run().unwrap();
    let archetype = status.lock().unwrap().archetype.clone();
    #[allow(clippy::let_and_return)]
    archetype
}

#[test]
fn label_is_decided_once_per_track_and_not_flipped_by_refinement() {
    // The first 8 s are a bass track. Everything after is vocal material
    // inside the same track (no gap, no hint). The label must stay the one
    // from the first window, while a standalone vocal track gets its own.
    let first = final_archetype(tone_chunks(BASS_TRACK, 8.0));
    let mut spliced = tone_chunks(BASS_TRACK, 8.0);
    spliced.extend(tone_chunks(VOCAL_TRACK, 40.0));
    let after = final_archetype(spliced);
    let vocal_alone = final_archetype(tone_chunks(VOCAL_TRACK, 8.0));
    assert_ne!(first, vocal_alone, "test needs two distinguishable tracks");
    assert_eq!(after, first, "label flipped mid-track");
}

#[test]
fn frame_alignment_noise_is_small() {
    // The same audio started at different sample offsets lands on different
    // frame boundaries. The curve may differ by measurement noise only.
    let mut curves = Vec::new();
    for offset in [0usize, 137, 411, 777] {
        let mut events = vec![SourceEvent::Chunk(vec![0.0f32; offset * 2])];
        events.extend(tone_chunks(VOCAL_TRACK, 8.0));
        let got = run_and_collect(events);
        assert_eq!(got.len(), 1);
        curves.push(got[0].clone());
    }
    let mut worst = 0.0f64;
    for c in &curves[1..] {
        for (a, b) in curves[0].iter().zip(c) {
            worst = worst.max((a - b).abs());
        }
    }
    eprintln!("alignment spread: {worst:.3} dB");
    assert!(worst < 0.3, "alignment spread {worst:.3} dB");
}

#[test]
fn playback_level_does_not_change_the_curve() {
    let quiet: Vec<SourceEvent> = tone_chunks(VOCAL_TRACK, 8.0)
        .into_iter()
        .map(|e| match e {
            SourceEvent::Chunk(v) => SourceEvent::Chunk(v.into_iter().map(|s| s * 0.2).collect()),
            other => other,
        })
        .collect();
    let a = run_and_collect(tone_chunks(VOCAL_TRACK, 8.0));
    let b = run_and_collect(quiet);
    assert_eq!((a.len(), b.len()), (1, 1));
    for (x, y) in a[0].iter().zip(&b[0]) {
        assert!(
            (x - y).abs() < 0.05,
            "level changed the curve: {x:.3} vs {y:.3}"
        );
    }
}

#[test]
fn sound_file_changes_the_curve_and_the_status() {
    let dir = std::env::temp_dir().join(format!("leungo-sound-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("signature.json");

    let run = |sound: Option<&str>| {
        if let Some(j) = sound {
            std::fs::write(&path, j).unwrap();
        } else {
            let _ = std::fs::remove_file(&path);
        }
        let applied = Arc::new(Mutex::new(Vec::new()));
        let status = Arc::new(Mutex::new(Status::default()));
        let mut engine = Engine::new(
            cfg(),
            Box::new(ScriptSource {
                events: tone_chunks(VOCAL_TRACK, 8.0).into_iter(),
            }),
            Box::new(RecordingApplier {
                applied: applied.clone(),
                bypasses: Arc::new(Mutex::new(0)),
            }),
            status.clone(),
            Arc::new(AtomicBool::new(false)),
        );
        engine.set_signature_path(path.clone());
        engine.run().unwrap();
        let last = applied.lock().unwrap().last().cloned().unwrap();
        let st = status.lock().unwrap().clone();
        (last, st)
    };

    let (flat, flat_status) = run(None);
    let (warm, warm_status) = run(Some(r#"{"bass": 6, "air": 4, "device": 0.6}"#));
    assert_eq!(flat_status.signature.bass_db, 0.0);
    assert_eq!(warm_status.signature.bass_db, 6.0);
    assert_eq!(warm_status.signature.air_db, 4.0);
    // bass and air bands sit higher relative to the mids than without it
    let rel = |g: &Vec<f64>, i: usize| g[i] - g[4];
    assert!(
        rel(&warm, 0) > rel(&flat, 0) + 2.0,
        "bass: {warm:?} vs {flat:?}"
    );
    assert!(
        rel(&warm, 7) > rel(&flat, 7) + 0.5,
        "air: {warm:?} vs {flat:?}"
    );
    // never a net boost from the signature itself
    assert!(
        warm.iter().cloned().fold(f64::MIN, f64::max)
            <= flat.iter().cloned().fold(f64::MIN, f64::max) + 0.5
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Writes the headphone profile partway through the stream, like a pick
/// in the dashboard while a song plays.
struct PickMidway {
    events: std::vec::IntoIter<SourceEvent>,
    at: usize,
    idx: usize,
    path: std::path::PathBuf,
    body: String,
}

impl SampleSource for PickMidway {
    fn next(&mut self) -> Result<SourceEvent> {
        if self.idx == self.at {
            std::fs::write(&self.path, &self.body).unwrap();
        }
        self.idx += 1;
        Ok(self.events.next().unwrap_or(SourceEvent::Ended))
    }
}

struct DeviceApplier(Arc<Mutex<Vec<Vec<leungo_eq::device::Filter>>>>);

impl EqApplier for DeviceApplier {
    fn apply_eq(&mut self, eq: &EqOutput) -> Result<()> {
        self.0.lock().unwrap().push(eq.device.clone());
        Ok(())
    }

    fn set_bypass(&mut self, _on: bool) -> Result<()> {
        Ok(())
    }
}

#[test]
fn picking_a_headphone_swaps_the_device_stage_mid_song() {
    use leungo_core::headphones::Profile;
    use leungo_eq::device::{Filter, Kind};

    let dir = std::env::temp_dir().join(format!("leungo-hp-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("headphone.json");
    let _ = std::fs::remove_file(&path);
    let profile = Profile {
        name: "Test IEM".into(),
        sources: vec!["Someone".into()],
        target: "Harman in-ear 2019".into(),
        filters: vec![Filter {
            kind: Kind::LowShelf,
            fc: 300.0,
            gain_db: -6.0,
            q: 0.7,
        }],
        rms_db: 0.3,
    };
    let applied = Arc::new(Mutex::new(Vec::new()));
    let status = Arc::new(Mutex::new(Status::default()));
    let mut engine = Engine::new(
        cfg(),
        Box::new(PickMidway {
            events: tone_chunks(VOCAL_TRACK, 14.0).into_iter(),
            at: chunk_after(10.0),
            idx: 0,
            path: path.clone(),
            body: serde_json::to_string(&profile).unwrap(),
        }),
        Box::new(DeviceApplier(applied.clone())),
        status.clone(),
        Arc::new(AtomicBool::new(false)),
    );
    engine.set_headphone_path(path.clone());
    engine.run().unwrap();

    let applied = applied.lock().unwrap();
    assert!(
        applied.first().unwrap().is_empty(),
        "no device stage before the pick"
    );
    let last = applied.last().unwrap();
    assert_eq!(last.len(), 1);
    assert_eq!(last[0].fc, 300.0);
    // scaled by device strength, never more than the profile itself
    assert!(last[0].gain_db < 0.0 && last[0].gain_db >= -6.0);
    assert_eq!(
        status.lock().unwrap().headphone.as_deref(),
        Some("Test IEM")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_new_curve_glides_in_instead_of_jumping() {
    let mut c = Config::default();
    c.analysis.transition_s = 3.0;
    c.analysis.transition_step_s = 0.5;
    let mut events = tone_chunks(BASS_TRACK, 8.0);
    events.extend(silence_chunks(1.5));
    events.extend(tone_chunks(VOCAL_TRACK, 12.0));

    let applied = Arc::new(Mutex::new(Vec::new()));
    let mut engine = Engine::new(
        c,
        Box::new(ScriptSource {
            events: events.into_iter(),
        }),
        Box::new(RecordingApplier {
            applied: applied.clone(),
            bypasses: Arc::new(Mutex::new(0)),
        }),
        Arc::new(Mutex::new(Status::default())),
        Arc::new(AtomicBool::new(false)),
    );
    engine.run().unwrap();

    let applied = applied.lock().unwrap();
    // 3 s at 0.5 s steps is about 6 steps per curve change
    assert!(applied.len() >= 8, "only {} applies", applied.len());
    let biggest = applied
        .windows(2)
        .flat_map(|w| w[0].iter().zip(&w[1]).map(|(a, b)| (a - b).abs()))
        .fold(0.0, f64::max);
    // widest swing any band makes over the run, flat start included
    let total = (0..applied[0].len())
        .map(|j| {
            let vals = std::iter::once(0.0).chain(applied.iter().map(|g| g[j]));
            let (lo, hi) = vals.fold((f64::MAX, f64::MIN), |(lo, hi), v| (lo.min(v), hi.max(v)));
            hi - lo
        })
        .fold(0.0, f64::max);
    assert!(total > 1.0, "curves too similar to test: {total:.2} dB");
    // raised cosine at its steepest moves ~26% of the change per 0.5 s step
    assert!(
        biggest < 0.35 * total,
        "one step jumped {biggest:.2} of {total:.2} dB"
    );
}
