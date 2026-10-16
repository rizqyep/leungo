//! Headphone profiles: fetch measurements from AutoEq, fit the device stage,
//! store the result and pick the active one. The engine polls the active
//! profile file, so a change is heard without a restart.

use crate::autoeq::{self, Model};
use crate::paths;
use anyhow::{Context, Result, bail};
use leungo_eq::device::{Filter, consensus_db, parse_parametric};
use leungo_eq::fit::{FitOptions, fit};
use leungo_eq::fr;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::{Duration, SystemTime};

const RESULTS_URL: &str = "https://raw.githubusercontent.com/jaakkopasanen/AutoEq/master/results/";
const INDEX_MAX_AGE: Duration = Duration::from_secs(30 * 24 * 3600);
pub const HARMAN_NAME: &str = "Harman in-ear 2019";

/// Returns the body, or `None` when the file doesn't exist (404).
pub type Fetch<'a> = &'a dyn Fn(&str) -> Result<Option<String>>;

pub fn http_get(url: &str) -> Result<Option<String>> {
    match ureq::get(url).call() {
        Ok(mut r) => Ok(Some(
            r.body_mut()
                .read_to_string()
                .with_context(|| format!("read {url}"))?,
        )),
        Err(ureq::Error::StatusCode(404)) => Ok(None),
        Err(e) => Err(e).with_context(|| format!("download {url}")),
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    pub name: String,
    pub sources: Vec<String>,
    /// What the correction aims at. AutoEq's own target when only
    /// ParametricEQ files were available.
    pub target: String,
    pub filters: Vec<Filter>,
    pub rms_db: f64,
}

pub struct Fitted {
    pub filters: Vec<Filter>,
    pub rms_db: f64,
    /// Where the sources disagree most below 10 kHz: (Hz, dB).
    pub worst_spread: Option<(f64, f64)>,
}

/// Fits `frequency,raw` measurements (median of all) toward `target`.
pub fn fit_measurements(csvs: &[&str], target: &str, fs: f64) -> Result<Fitted> {
    let pts = fr::grid();
    let mut curves = Vec::new();
    for (i, c) in csvs.iter().enumerate() {
        let r = fr::parse_csv(c).map_err(|e| anyhow::anyhow!("measurement {}: {e}", i + 1))?;
        curves.push(fr::normalized_on(&r, &pts));
    }
    let t = fr::parse_csv(target).map_err(|e| anyhow::anyhow!("target: {e}"))?;
    let cons = fr::consensus(&curves, &pts);
    let corr = fr::correction(
        &pts,
        &fr::normalized_on(&t, &pts),
        &cons,
        &fr::CorrectionLimits::default(),
    );
    let worst = pts
        .iter()
        .zip(&cons.spread_db)
        .filter(|(f, _)| **f <= 10_000.0)
        .max_by(|a, b| a.1.total_cmp(b.1))
        .filter(|_| csvs.len() > 1)
        .map(|(f, s)| (*f, *s));
    Ok(fit_curve(&pts, &corr, fs, worst))
}

/// Refits AutoEq ParametricEQ files (averaged in dB) onto the free filters.
pub fn fit_parametric(files: &[&str], fs: f64) -> Result<Fitted> {
    let pts = fr::grid();
    let mut profiles = Vec::new();
    for (i, f) in files.iter().enumerate() {
        profiles.push(parse_parametric(f).map_err(|e| anyhow::anyhow!("file {}: {e}", i + 1))?);
    }
    Ok(fit_curve(
        &pts,
        &consensus_db(&profiles, &pts, fs),
        fs,
        None,
    ))
}

fn fit_curve(pts: &[f64], corr: &[f64], fs: f64, worst: Option<(f64, f64)>) -> Fitted {
    let opt = FitOptions {
        fs,
        ..FitOptions::default()
    };
    let result = fit(pts, corr, &vec![1.0; pts.len()], &opt);
    let round = |v: f64, per: f64| (v * per).round() / per;
    Fitted {
        filters: result
            .filters
            .iter()
            .map(|f| Filter {
                fc: round(f.fc, 1.0),
                gain_db: round(f.gain_db, 100.0),
                q: round(f.q, 100.0),
                ..*f
            })
            .collect(),
        rms_db: result.rms_db,
        worst_spread: worst,
    }
}

/// The cached index when it's fresh, else a new download. A stale cache
/// still beats nothing when offline.
pub fn load_index(fetch: Fetch, path: &Path) -> Result<Vec<Model>> {
    let age = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| SystemTime::now().duration_since(t).ok());
    if age.is_some_and(|a| a < INDEX_MAX_AGE)
        && let Ok(md) = std::fs::read_to_string(path)
    {
        return Ok(autoeq::parse_index(&md));
    }
    match fetch(&format!("{RESULTS_URL}INDEX.md")) {
        Ok(Some(md)) => {
            write(path, &md)?;
            Ok(autoeq::parse_index(&md))
        }
        other => match std::fs::read_to_string(path) {
            Ok(md) => Ok(autoeq::parse_index(&md)),
            Err(_) => match other {
                Err(e) => Err(e),
                _ => bail!("AutoEq index not found online"),
            },
        },
    }
}

/// Downloads every measurement of `model`, fits it toward Harman and stores
/// the inputs plus the result under `<store>/<slug>/`.
pub fn build(model: &Model, fetch: Fetch, fs: f64, store: &Path) -> Result<Profile> {
    let mut csvs = Vec::new();
    let mut parametric = Vec::new();
    for e in &model.entries {
        let file = e.path.rsplit('/').next().unwrap_or_default();
        if let Some(t) = fetch(&format!("{RESULTS_URL}{}/{file}.csv", e.path))? {
            csvs.push((e.source.clone(), t));
        } else if let Some(t) = fetch(&format!(
            "{RESULTS_URL}{}/{file}%20ParametricEQ.txt",
            e.path
        ))? {
            parametric.push((e.source.clone(), t));
        }
    }
    let dir = store.join(slug(&model.name));
    // measurements win: they let us pick the target ourselves
    let (fitted, used, target, ext) = if !csvs.is_empty() {
        let texts: Vec<&str> = csvs.iter().map(|(_, t)| t.as_str()).collect();
        let f = fit_measurements(&texts, fr::HARMAN_IE_2019, fs)?;
        (f, csvs, HARMAN_NAME.to_string(), "csv")
    } else if !parametric.is_empty() {
        let texts: Vec<&str> = parametric.iter().map(|(_, t)| t.as_str()).collect();
        let f = fit_parametric(&texts, fs)?;
        (f, parametric, "AutoEq's own".to_string(), "txt")
    } else {
        bail!("no downloadable measurement for {}", model.name);
    };
    for (source, text) in &used {
        write(&dir.join(format!("{}.{ext}", slug(source))), text)?;
    }
    let profile = Profile {
        name: model.name.clone(),
        sources: used.into_iter().map(|(s, _)| s).collect(),
        target,
        filters: fitted.filters,
        rms_db: fitted.rms_db,
    };
    write(
        &dir.join("profile.json"),
        &serde_json::to_string_pretty(&profile)?,
    )?;
    Ok(profile)
}

/// Makes `profile` the one the engine applies.
pub fn activate(profile: &Profile) -> Result<()> {
    write(
        &paths::active_headphone_path(),
        &serde_json::to_string_pretty(profile)?,
    )
}

/// Back to the config's own device filters (none for most users).
pub fn deactivate() -> Result<()> {
    match std::fs::remove_file(paths::active_headphone_path()) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}

pub fn read_profile(path: &Path) -> Option<Profile> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

/// Every profile in `store`, by name.
pub fn stored(store: &Path) -> Vec<Profile> {
    let Ok(dirs) = std::fs::read_dir(store) else {
        return Vec::new();
    };
    let mut out: Vec<Profile> = dirs
        .flatten()
        .filter_map(|d| read_profile(&d.path().join("profile.json")))
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

pub fn slug(name: &str) -> String {
    let mut s = String::new();
    for c in name.to_lowercase().chars() {
        if c.is_alphanumeric() {
            s.push(c);
        } else if !s.ends_with('-') && !s.is_empty() {
            s.push('-');
        }
    }
    s.trim_end_matches('-').to_string()
}

fn write(path: &Path, body: &str) -> Result<()> {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p).with_context(|| format!("create {}", p.display()))?;
    }
    std::fs::write(path, body).with_context(|| format!("write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::autoeq::Entry;

    /// Real download from GitHub: `cargo test -p leungo-core -- --ignored`.
    #[test]
    #[ignore]
    fn downloads_and_fits_a_real_iem() {
        let dir = std::env::temp_dir().join(format!("leungo-hp-net-{}", std::process::id()));
        let models = load_index(&http_get, &dir.join("index.md")).unwrap();
        assert!(models.len() > 3000, "{} models", models.len());
        let hit = autoeq::search(&models, "kz edx pro", 1)[0];
        assert_eq!(hit.name, "KZ EDX Pro");
        let p = build(hit, &http_get, 48_000.0, &dir).unwrap();
        println!("{p:#?}");
        assert_eq!(p.target, HARMAN_NAME);
        assert_eq!(p.sources.len(), 2, "crinacle has no CSV");
        assert!(p.rms_db < 0.6);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn slugs_are_file_safe() {
        assert_eq!(slug("KZ EDX Pro"), "kz-edx-pro");
        assert_eq!(slug("1MORE Aero (ANC On)"), "1more-aero-anc-on");
        assert_eq!(slug("Super Review"), "super-review");
    }

    #[test]
    fn fits_the_bundled_target_to_almost_nothing() {
        let f = fit_measurements(&[fr::HARMAN_IE_2019], fr::HARMAN_IE_2019, 48_000.0).unwrap();
        assert!(
            f.filters.iter().all(|x| x.gain_db.abs() < 0.5),
            "{:?}",
            f.filters
        );
    }

    #[test]
    fn build_prefers_csvs_and_falls_back_to_parametric() {
        let dir = std::env::temp_dir().join(format!("leungo-hp-{}", std::process::id()));
        let entry = |source: &str| Entry {
            source: source.into(),
            rig: "in-ear".into(),
            path: format!("{}/in-ear/Test%20IEM", source.replace(' ', "%20")),
        };
        let model = Model {
            name: "Test IEM".into(),
            entries: vec![entry("crinacle"), entry("Super Review")],
        };
        let urls = std::sync::Mutex::new(Vec::new());
        let fetch = |url: &str| -> Result<Option<String>> {
            urls.lock().unwrap().push(url.to_string());
            Ok(url
                .contains("Super%20Review")
                .then(|| fr::HARMAN_IE_2019.to_string())
                .filter(|_| url.ends_with(".csv")))
        };
        let p = build(&model, &fetch, 48_000.0, &dir).unwrap();
        assert_eq!(p.sources, ["Super Review"]);
        assert_eq!(p.target, HARMAN_NAME);
        assert!(dir.join("test-iem/super-review.csv").exists());
        assert_eq!(stored(&dir), std::slice::from_ref(&p));
        assert!(urls.lock().unwrap()[0].ends_with("crinacle/in-ear/Test%20IEM/Test%20IEM.csv"));

        let only_peq = |url: &str| -> Result<Option<String>> {
            Ok(url
                .ends_with("ParametricEQ.txt")
                .then(|| "Filter 1: ON PK Fc 1000 Hz Gain -3.0 dB Q 1.0\n".to_string()))
        };
        let p = build(&model, &only_peq, 48_000.0, &dir).unwrap();
        assert_eq!(p.sources, ["crinacle", "Super Review"]);
        assert_eq!(p.target, "AutoEq's own");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
