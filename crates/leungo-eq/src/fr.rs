//! Measured frequency responses (AutoEq CSVs), consensus across sources,
//! and the correction toward a target. Everything lives on one grid.

use crate::interpolate_curve;

pub const GRID_PER_OCTAVE: f64 = 12.0;

/// Harman in-ear 2019, from AutoEq `targets/` (MIT).
pub const HARMAN_IE_2019: &str = include_str!("../targets/harman-in-ear-2019.csv");

/// Spread at which a point's weight drops to 0.5. *Unverified*.
pub const SPREAD_HALF_WEIGHT_DB: f64 = 1.5;

/// 1/12-octave points, 20 Hz to 20 kHz.
pub fn grid() -> Vec<f64> {
    (0..)
        .map(|k| 20.0 * 2f64.powf(k as f64 / GRID_PER_OCTAVE))
        .take_while(|&f| f <= 20_000.0)
        .collect()
}

#[derive(Clone, Debug, PartialEq)]
pub struct Response {
    pub freqs: Vec<f64>,
    pub db: Vec<f64>,
}

/// Reads the `raw` column (any position), or the 2nd column if there's no
/// header. Rows with missing values are skipped.
pub fn parse_csv(text: &str) -> Result<Response, String> {
    let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty());
    let first = lines.next().ok_or("empty file")?;
    let cells: Vec<&str> = first.split(',').map(str::trim).collect();
    let has_header = cells.first().and_then(|c| c.parse::<f64>().ok()).is_none();
    let (fi, vi) = if has_header {
        let col = |name: &str| cells.iter().position(|c| c.eq_ignore_ascii_case(name));
        (
            col("frequency").ok_or("no 'frequency' column")?,
            col("raw").ok_or("no 'raw' column")?,
        )
    } else {
        (0, 1)
    };
    let mut out = Response {
        freqs: Vec::new(),
        db: Vec::new(),
    };
    let data_rows = (!has_header).then_some(first).into_iter().chain(lines);
    for line in data_rows {
        let c: Vec<&str> = line.split(',').map(str::trim).collect();
        let num = |i: usize| c.get(i).and_then(|x| x.parse::<f64>().ok());
        let (Some(f), Some(v)) = (num(fi), num(vi)) else {
            continue;
        };
        if f <= 0.0 || !f.is_finite() || !v.is_finite() {
            continue;
        }
        if out.freqs.last().is_some_and(|&last| f <= last) {
            return Err(format!("frequencies not increasing at {f} Hz"));
        }
        out.freqs.push(f);
        out.db.push(v);
    }
    if out.freqs.len() < 10 {
        return Err("fewer than 10 data rows".into());
    }
    Ok(out)
}

/// Resampled onto `points`, with 500 Hz to 1 kHz shifted to 0 dB.
pub fn normalized_on(r: &Response, points: &[f64]) -> Vec<f64> {
    let mut v = interpolate_curve(&r.freqs, &r.db, points);
    let mid: Vec<f64> = points
        .iter()
        .zip(&v)
        .filter(|(f, _)| (500.0..=1000.0).contains(*f))
        .map(|(_, d)| *d)
        .collect();
    if !mid.is_empty() {
        let m = mid.iter().sum::<f64>() / mid.len() as f64;
        v.iter_mut().for_each(|x| *x -= m);
    }
    v
}

/// Several measurements of one device, combined per point.
#[derive(Clone, Debug)]
pub struct Consensus {
    pub db: Vec<f64>,
    pub spread_db: Vec<f64>,
    /// 0..=1, low where the sources disagree.
    pub weight: Vec<f64>,
}

pub fn consensus(curves: &[Vec<f64>], points: &[f64]) -> Consensus {
    assert!(!curves.is_empty(), "no measurements");
    assert!(
        curves.iter().all(|c| c.len() == points.len()),
        "length mismatch"
    );
    let n = curves.len();
    let mut out = Consensus {
        db: Vec::with_capacity(points.len()),
        spread_db: Vec::with_capacity(points.len()),
        weight: Vec::with_capacity(points.len()),
    };
    for (k, &f) in points.iter().enumerate() {
        let mut col: Vec<f64> = curves.iter().map(|c| c[k]).collect();
        // median, so one bad unit or coupler can't drag it
        let med = median(&mut col);
        let spread = match n {
            1 => 0.0,
            2 => (curves[0][k] - curves[1][k]).abs() / 2f64.sqrt(),
            _ => {
                let mut dev: Vec<f64> = col.iter().map(|x| (x - med).abs()).collect();
                1.4826 * median(&mut dev)
            }
        };
        let w = if n == 1 {
            // nothing to compare against, so trust the treble less
            1.0 - 0.5 * log_ramp(f, 6_000.0, 10_000.0)
        } else {
            1.0 / (1.0 + (spread / SPREAD_HALF_WEIGHT_DB).powi(2))
        };
        out.db.push(med);
        out.spread_db.push(spread);
        out.weight.push(w);
    }
    out
}

#[derive(Clone, Copy, Debug)]
pub struct CorrectionLimits {
    pub max_boost_db: f64,
    pub max_cut_db: f64,
    /// Correction fades to 0 across this range (coupler and fit rule up there).
    pub treble_fade: (f64, f64),
    /// Max boost above the local median. *Unverified*.
    pub narrow_boost_db: f64,
}

impl Default for CorrectionLimits {
    fn default() -> Self {
        Self {
            max_boost_db: 6.0,
            max_cut_db: 12.0,
            treble_fade: (8_000.0, 12_000.0),
            narrow_boost_db: 3.0,
        }
    }
}

/// `weight * (target - measured)`, smoothed, narrow boosts limited, treble
/// faded, clamped. Inputs are on `points`.
pub fn correction(
    points: &[f64],
    target: &[f64],
    measured: &Consensus,
    lim: &CorrectionLimits,
) -> Vec<f64> {
    assert_eq!(points.len(), target.len(), "target length mismatch");
    assert_eq!(
        points.len(),
        measured.db.len(),
        "measurement length mismatch"
    );
    let raw: Vec<f64> = (0..points.len())
        .map(|k| measured.weight[k] * (target[k] - measured.db[k]))
        .collect();
    let smooth = smooth_points(&raw, 1);
    let span = (GRID_PER_OCTAVE / 3.0).round() as usize;
    (0..points.len())
        .map(|k| {
            let lo = k.saturating_sub(span);
            let hi = (k + span + 1).min(points.len());
            let local = median(&mut smooth[lo..hi].to_vec());
            // narrow dips are mostly canal resonances, EQ can't fill those
            let c = smooth[k].min(local + lim.narrow_boost_db)
                * (1.0 - log_ramp(points[k], lim.treble_fade.0, lim.treble_fade.1));
            c.clamp(-lim.max_cut_db, lim.max_boost_db)
        })
        .collect()
}

fn smooth_points(v: &[f64], half: usize) -> Vec<f64> {
    (0..v.len())
        .map(|k| {
            let lo = k.saturating_sub(half);
            let hi = (k + half + 1).min(v.len());
            v[lo..hi].iter().sum::<f64>() / (hi - lo) as f64
        })
        .collect()
}

/// 0 below `a`, 1 above `b`, linear in log-frequency between.
pub(crate) fn log_ramp(f: f64, a: f64, b: f64) -> f64 {
    ((f.ln() - a.ln()) / (b.ln() - a.ln())).clamp(0.0, 1.0)
}

fn median(v: &mut [f64]) -> f64 {
    v.sort_by(f64::total_cmp);
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        0.5 * (v[n / 2 - 1] + v[n / 2])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat_consensus(db: Vec<f64>) -> Consensus {
        let n = db.len();
        Consensus {
            db,
            spread_db: vec![0.0; n],
            weight: vec![1.0; n],
        }
    }

    #[test]
    fn grid_is_twelfth_octave_from_20_hz() {
        let g = grid();
        assert_eq!(g[0], 20.0);
        assert!((g[12] - 40.0).abs() < 1e-9);
        assert!(*g.last().unwrap() <= 20_000.0);
        assert_eq!(g.len(), 120);
    }

    #[test]
    fn parses_raw_column_in_any_position() {
        let mut text = "frequency,smoothed,raw,target\n".to_string();
        for k in 0..12 {
            text.push_str(&format!("{},1,{},3\n", 20 + k * 10, 2 + k));
        }
        let r = parse_csv(&text).unwrap();
        assert_eq!(r.db[0], 2.0);
        assert_eq!(r.db[3], 5.0);
    }

    #[test]
    fn parses_headerless_and_skips_bad_rows() {
        let mut text = String::new();
        for k in 0..12 {
            text.push_str(&format!("{},{}\n", 100 + k, k));
        }
        text.push_str("200,\n300,abc\n");
        assert_eq!(parse_csv(&text).unwrap().freqs.len(), 12);
    }

    #[test]
    fn rejects_missing_raw_column_and_unsorted_rows() {
        assert!(parse_csv("frequency,smoothed\n20,1\n").is_err());
        let mut text = "frequency,raw\n".to_string();
        for f in [20, 30, 25, 40, 50, 60, 70, 80, 90, 100, 110] {
            text.push_str(&format!("{f},0\n"));
        }
        assert!(parse_csv(&text).is_err());
    }

    #[test]
    fn bundled_target_is_zero_near_1k() {
        let t = parse_csv(HARMAN_IE_2019).unwrap();
        let v = interpolate_curve(&t.freqs, &t.db, &[1000.0]);
        assert!(v[0].abs() < 0.1, "{}", v[0]);
    }

    #[test]
    fn normalization_removes_level() {
        let r = Response {
            freqs: vec![20.0, 1000.0, 20_000.0],
            db: vec![90.0; 3],
        };
        assert!(normalized_on(&r, &grid()).iter().all(|x| x.abs() < 1e-9));
    }

    #[test]
    fn median_ignores_one_outlier_and_its_weight_drops() {
        let pts = grid();
        let a = vec![0.0; pts.len()];
        let mut b = a.clone();
        b[50] = 0.4;
        let mut c = a.clone();
        c[50] = 9.0;
        let cons = consensus(&[a, b, c], &pts);
        assert!((cons.db[50] - 0.4).abs() < 1e-9);
        assert_eq!(cons.weight[10], 1.0);
        assert!(cons.weight[50] < cons.weight[10]);
    }

    #[test]
    fn two_sources_that_disagree_are_trusted_less() {
        let pts = grid();
        let a = vec![0.0; pts.len()];
        let mut b = a.clone();
        b[40] = 3.0;
        let cons = consensus(&[a, b], &pts);
        assert!((cons.db[40] - 1.5).abs() < 1e-9);
        assert!(cons.weight[40] < 0.5);
    }

    #[test]
    fn correction_fades_treble_and_respects_limits() {
        let pts = grid();
        let target = vec![0.0; pts.len()];
        let cons = flat_consensus(vec![-20.0; pts.len()]);
        let c = correction(&pts, &target, &cons, &CorrectionLimits::default());
        for (f, v) in pts.iter().zip(&c) {
            assert!(*v <= 6.0 + 1e-9);
            if *f >= 12_000.0 {
                assert!(v.abs() < 1e-9, "{f} Hz: {v}");
            }
        }
    }

    #[test]
    fn a_narrow_dip_is_not_filled() {
        let pts = grid();
        let target = vec![0.0; pts.len()];
        let mut db = vec![0.0; pts.len()];
        db[60] = -15.0;
        let c = correction(
            &pts,
            &target,
            &flat_consensus(db),
            &CorrectionLimits::default(),
        );
        assert!(c[60] <= 3.0 + 1e-9, "{}", c[60]);
    }
}
