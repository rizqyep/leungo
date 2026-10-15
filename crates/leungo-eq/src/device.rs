//! Device (headphone/IEM) correction from AutoEq-style `ParametricEQ.txt`
//! files: parsing, frequency response, and a fit onto the fixed bands.

use leungo_dsp::{Coeffs, magnitude_db, peaking_coeffs, shelf_coeffs};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Peak,
    LowShelf,
    HighShelf,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Filter {
    pub kind: Kind,
    pub fc: f64,
    pub gain_db: f64,
    pub q: f64,
}

/// Parses `Filter N: ON PK Fc 105 Hz Gain -2.3 dB Q 0.70` lines. The
/// `Preamp` line is ignored (level is not timbre) and `OFF` filters skipped.
pub fn parse_parametric(text: &str) -> Result<Vec<Filter>, String> {
    let mut out = Vec::new();
    for line in text.lines() {
        let t: Vec<&str> = line.split_whitespace().collect();
        if t.first() != Some(&"Filter") {
            continue;
        }
        if t.len() < 12 || t[2] != "ON" && t[2] != "OFF" {
            return Err(format!("unrecognized filter line: {line}"));
        }
        if t[2] == "OFF" {
            continue;
        }
        let kind = match t[3] {
            "PK" => Kind::Peak,
            "LSC" | "LS" => Kind::LowShelf,
            "HSC" | "HS" => Kind::HighShelf,
            other => return Err(format!("unsupported filter type {other}")),
        };
        let num = |key: &str| -> Result<f64, String> {
            let i = t
                .iter()
                .position(|w| *w == key)
                .ok_or_else(|| format!("missing {key} in: {line}"))?;
            t.get(i + 1)
                .and_then(|v| v.parse::<f64>().ok())
                .ok_or_else(|| format!("bad {key} value in: {line}"))
        };
        out.push(Filter {
            kind,
            fc: num("Fc")?,
            gain_db: num("Gain")?,
            q: num("Q")?,
        });
    }
    if out.is_empty() {
        return Err("no filters found".into());
    }
    Ok(out)
}

/// Biquad coefficients of one filter.
pub fn coeffs(x: &Filter, fs: f64) -> Coeffs {
    match x.kind {
        Kind::Peak => peaking_coeffs(x.fc, x.gain_db, x.q, fs),
        Kind::LowShelf => shelf_coeffs(x.fc, x.gain_db, x.q, fs, false),
        Kind::HighShelf => shelf_coeffs(x.fc, x.gain_db, x.q, fs, true),
    }
}

/// One filter's response in dB at each of `points`.
pub fn filter_curve(x: &Filter, points: &[f64], fs: f64) -> Vec<f64> {
    let c = coeffs(x, fs);
    points.iter().map(|&f| magnitude_db(&c, f, fs)).collect()
}

/// Combined magnitude response in dB of all filters at `f`.
pub fn response_db(filters: &[Filter], f: f64, fs: f64) -> f64 {
    filters
        .iter()
        .map(|x| magnitude_db(&coeffs(x, fs), f, fs))
        .sum()
}

/// Highest point of the combined response, 20 Hz to 20 kHz on a 1/24-octave
/// grid. The preamp has to take this much off to never clip.
pub fn peak_gain_db(filters: &[Filter], fs: f64) -> f64 {
    let top = 20_000f64.min(0.45 * fs);
    let c: Vec<Coeffs> = filters.iter().map(|x| coeffs(x, fs)).collect();
    (0..)
        .map(|k| 20.0 * 2f64.powf(k as f64 / 24.0))
        .take_while(|&f| f <= top)
        .map(|f| c.iter().map(|x| magnitude_db(x, f, fs)).sum::<f64>())
        .fold(f64::NEG_INFINITY, f64::max)
}

/// The fixed song bands as peaking filters.
pub fn peaks(freqs: &[f64], gains: &[f64], q: f64) -> Vec<Filter> {
    freqs
        .iter()
        .zip(gains)
        .map(|(&fc, &gain_db)| Filter {
            kind: Kind::Peak,
            fc,
            gain_db,
            q,
        })
        .collect()
}

/// Mean response in dB across several profiles of the same device, so one
/// unit or one coupler does not decide the correction.
pub fn consensus_db(profiles: &[Vec<Filter>], points: &[f64], fs: f64) -> Vec<f64> {
    points
        .iter()
        .map(|&f| {
            profiles.iter().map(|p| response_db(p, f, fs)).sum::<f64>() / profiles.len() as f64
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "Preamp: -5.1 dB\n\
Filter 1: ON LSC Fc 105 Hz Gain -2.3 dB Q 0.70\n\
Filter 2: ON PK Fc 169 Hz Gain -4.0 dB Q 0.82\n\
Filter 3: OFF PK Fc 9836 Hz Gain 2.9 dB Q 0.18\n\
Filter 4: ON HSC Fc 10000 Hz Gain -6.1 dB Q 0.70\n";

    #[test]
    fn parses_autoeq_format_and_skips_preamp_and_off() {
        let f = parse_parametric(SAMPLE).unwrap();
        assert_eq!(f.len(), 3);
        assert_eq!(f[0].kind, Kind::LowShelf);
        assert_eq!(f[1].fc, 169.0);
        assert_eq!(f[2].kind, Kind::HighShelf);
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_parametric("hello").is_err());
        assert!(parse_parametric("Filter 1: ON XX Fc 1 Hz Gain 1 dB Q 1").is_err());
    }

    #[test]
    fn peak_gain_finds_the_top_of_overlapping_boosts() {
        let f = peaks(&[1000.0, 1200.0], &[3.0, 3.0], 1.0);
        let p = peak_gain_db(&f, 48_000.0);
        assert!(p > 5.0 && p < 6.0, "{p}");
        assert!(peak_gain_db(&peaks(&[1000.0], &[-6.0], 1.0), 48_000.0) < 0.1);
    }

    #[test]
    fn filters_round_trip_through_toml_friendly_serde() {
        let f = parse_parametric(SAMPLE).unwrap();
        let json = serde_json::to_string(&f).unwrap();
        assert!(json.contains("\"low_shelf\""), "{json}");
        let back: Vec<Filter> = serde_json::from_str(&json).unwrap();
        assert_eq!(back, f);
    }

    #[test]
    fn response_matches_filter_gain_at_its_center() {
        let f = parse_parametric("Filter 1: ON PK Fc 1000 Hz Gain 4.0 dB Q 2.0").unwrap();
        assert!((response_db(&f, 1000.0, 48_000.0) - 4.0).abs() < 1e-6);
    }
}
