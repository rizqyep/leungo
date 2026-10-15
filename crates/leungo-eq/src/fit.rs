//! Fits shelves + peaking filters (free fc, Q and gain) to a correction
//! curve: greedy placement, then Levenberg-Marquardt over everything.

use crate::device::{Filter, Kind, filter_curve};
use crate::solve_linear;

const GAIN_LIMIT_DB: f64 = 12.0;
const PEAK_FC: (f64, f64) = (20.0, 12_000.0);
const PEAK_Q: (f64, f64) = (0.4, 6.0);
/// Max Q falls to this by 10 kHz: treble peaks move with insertion depth,
/// so keep cuts there wide. *Unverified*.
const TREBLE_MAX_Q: f64 = 2.0;
const SHELF_Q: (f64, f64) = (0.4, 0.9);
const LOW_SHELF_FC: (f64, f64) = (30.0, 300.0);
const HIGH_SHELF_FC: (f64, f64) = (2_000.0, 16_000.0);
const MAX_ITERS: usize = 200;
const STEP: f64 = 1e-4;
/// Cost per dB² of filter gain, so it won't stack big filters that cancel.
const GAIN_RIDGE: f64 = 0.05;

#[derive(Clone, Copy, Debug)]
pub struct FitOptions {
    pub max_peaks: usize,
    /// Stop adding filters once the weighted rms error is below this.
    pub target_rms_db: f64,
    /// A new filter has to cut the rms error by at least this much.
    pub min_improvement_db: f64,
    pub fs: f64,
}

impl Default for FitOptions {
    fn default() -> Self {
        Self {
            max_peaks: 8,
            target_rms_db: 0.5,
            min_improvement_db: 0.1,
            fs: 48_000.0,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Fit {
    pub filters: Vec<Filter>,
    /// Level offset of the fit. Level is the preamp's job, not the filters'.
    pub offset_db: f64,
    /// Weighted rms of `residual_db`.
    pub rms_db: f64,
    /// `target - model`, what the filters couldn't do.
    pub residual_db: Vec<f64>,
}

/// Fits `target` (dB on `points`) with `weight` per point.
pub fn fit(points: &[f64], target: &[f64], weight: &[f64], opt: &FitOptions) -> Fit {
    assert_eq!(points.len(), target.len(), "target length mismatch");
    assert_eq!(points.len(), weight.len(), "weight length mismatch");
    let p = Problem {
        points,
        target,
        weight,
        fs: opt.fs,
    };
    let mut state = State {
        filters: vec![
            Filter {
                kind: Kind::LowShelf,
                fc: 105.0,
                gain_db: 0.0,
                q: 0.7,
            },
            Filter {
                kind: Kind::HighShelf,
                fc: 10_000.0,
                gain_db: 0.0,
                q: 0.7,
            },
        ],
        offset_db: 0.0,
    };
    p.refine(&mut state);
    let mut rms = p.rms(&state);
    for _ in 0..opt.max_peaks {
        if rms < opt.target_rms_db {
            break;
        }
        let Some(peak) = p.next_peak(&state) else {
            break;
        };
        let mut next = state.clone();
        next.filters.push(peak);
        p.refine(&mut next);
        let next_rms = p.rms(&next);
        if rms - next_rms < opt.min_improvement_db {
            break;
        }
        state = next;
        rms = next_rms;
    }
    state.filters.retain(|f| f.gain_db.abs() >= 0.1);
    let model = p.model(&state);
    Fit {
        rms_db: p.rms(&state),
        residual_db: target.iter().zip(&model).map(|(t, m)| t - m).collect(),
        filters: state.filters,
        offset_db: state.offset_db,
    }
}

#[derive(Clone, Debug)]
struct State {
    filters: Vec<Filter>,
    offset_db: f64,
}

struct Problem<'a> {
    points: &'a [f64],
    target: &'a [f64],
    weight: &'a [f64],
    fs: f64,
}

impl Problem<'_> {
    fn model(&self, s: &State) -> Vec<f64> {
        let mut m = vec![s.offset_db; self.points.len()];
        for f in &s.filters {
            for (v, d) in m.iter_mut().zip(filter_curve(f, self.points, self.fs)) {
                *v += d;
            }
        }
        m
    }

    fn rms(&self, s: &State) -> f64 {
        let m = self.model(s);
        let (num, den) = (0..m.len()).fold((0.0, 0.0), |(n, d), k| {
            let e = self.target[k] - m[k];
            (n + self.weight[k] * e * e, d + self.weight[k])
        });
        if den > 0.0 { (num / den).sqrt() } else { 0.0 }
    }

    fn cost(&self, s: &State) -> f64 {
        let m = self.model(s);
        let fit: f64 = (0..m.len())
            .map(|k| self.weight[k] * (m[k] - self.target[k]).powi(2))
            .sum();
        fit + GAIN_RIDGE * s.filters.iter().map(|f| f.gain_db.powi(2)).sum::<f64>()
    }

    /// A peak at the worst weighted error, Q from its width at half height.
    fn next_peak(&self, s: &State) -> Option<Filter> {
        let m = self.model(s);
        let e: Vec<f64> = (0..m.len()).map(|k| self.target[k] - m[k]).collect();
        let k = (0..e.len())
            .filter(|&k| (PEAK_FC.0..=PEAK_FC.1).contains(&self.points[k]))
            .max_by(|&a, &b| {
                (self.weight[a] * e[a].abs()).total_cmp(&(self.weight[b] * e[b].abs()))
            })?;
        let half = e[k].abs() / 2.0;
        let same = |i: usize| e[i].signum() == e[k].signum() && e[i].abs() >= half;
        let (mut lo, mut hi) = (k, k);
        while lo > 0 && same(lo - 1) {
            lo -= 1;
        }
        while hi + 1 < e.len() && same(hi + 1) {
            hi += 1;
        }
        let bw = (self.points[hi] / self.points[lo]).log2().max(0.25);
        let r = 2f64.powf(bw);
        Some(Filter {
            kind: Kind::Peak,
            fc: self.points[k],
            gain_db: e[k].clamp(-GAIN_LIMIT_DB, GAIN_LIMIT_DB),
            q: (r.sqrt() / (r - 1.0)).clamp(PEAK_Q.0, peak_max_q(self.points[k])),
        })
    }

    fn refine(&self, s: &mut State) {
        let mut x = pack(s);
        let mut cost = self.cost(s);
        let mut lambda = 1e-2;
        let n = x.len();
        let m = self.points.len();
        let sw: Vec<f64> = self.weight.iter().map(|w| w.sqrt()).collect();
        for _ in 0..MAX_ITERS {
            let base = self.model(&unpack(&x, s));
            let r: Vec<f64> = (0..m).map(|k| sw[k] * (base[k] - self.target[k])).collect();
            let jac = self.jacobian(&x, s, &sw);
            let mut a = vec![0.0; n * n];
            let mut g = vec![0.0; n];
            for k in 0..m {
                let row = &jac[k * n..(k + 1) * n];
                for i in 0..n {
                    g[i] += row[i] * r[k];
                    for j in 0..n {
                        a[i * n + j] += row[i] * row[j];
                    }
                }
            }
            for i in 0..n / 3 {
                let gi = 3 * i + 2;
                g[gi] += GAIN_RIDGE * x[gi];
                a[gi * n + gi] += GAIN_RIDGE;
            }
            let mut improved = None;
            while lambda < 1e8 {
                let mut damped = a.clone();
                for i in 0..n {
                    damped[i * n + i] += lambda * (a[i * n + i] + 1e-6);
                }
                let mut rhs: Vec<f64> = g.iter().map(|v| -v).collect();
                let step = solve_linear(&mut damped, &mut rhs, n);
                let mut cand: Vec<f64> = x.iter().zip(&step).map(|(a, b)| a + b).collect();
                clamp_params(&mut cand, s);
                let c = self.cost(&unpack(&cand, s));
                if c < cost {
                    improved = Some(cost - c);
                    x = cand;
                    cost = c;
                    lambda = (lambda * 0.3).max(1e-9);
                    break;
                }
                lambda *= 5.0;
            }
            match improved {
                Some(d) if d > 1e-10 * cost.max(1e-12) => {}
                _ => break,
            }
        }
        *s = unpack(&x, s);
    }

    /// Rows are points, columns are parameters, scaled by sqrt(weight).
    fn jacobian(&self, x: &[f64], s: &State, sw: &[f64]) -> Vec<f64> {
        let n = x.len();
        let m = self.points.len();
        let mut jac = vec![0.0; m * n];
        let cur = unpack(x, s);
        for (i, f) in cur.filters.iter().enumerate() {
            for p in 0..3 {
                let col = 3 * i + p;
                let shifted = |d: f64| {
                    let mut y = x[3 * i..3 * i + 3].to_vec();
                    y[p] += d;
                    filter_curve(&to_filter(f.kind, &y), self.points, self.fs)
                };
                let (up, down) = (shifted(STEP), shifted(-STEP));
                for k in 0..m {
                    jac[k * n + col] = sw[k] * (up[k] - down[k]) / (2.0 * STEP);
                }
            }
        }
        for k in 0..m {
            jac[k * n + n - 1] = sw[k];
        }
        jac
    }
}

// params per filter: ln fc, ln q, gain; the level offset goes last
fn pack(s: &State) -> Vec<f64> {
    let mut x: Vec<f64> = s
        .filters
        .iter()
        .flat_map(|f| [f.fc.ln(), f.q.ln(), f.gain_db])
        .collect();
    x.push(s.offset_db);
    x
}

fn unpack(x: &[f64], like: &State) -> State {
    State {
        filters: like
            .filters
            .iter()
            .enumerate()
            .map(|(i, f)| to_filter(f.kind, &x[3 * i..3 * i + 3]))
            .collect(),
        offset_db: x[x.len() - 1],
    }
}

fn to_filter(kind: Kind, p: &[f64]) -> Filter {
    Filter {
        kind,
        fc: p[0].exp(),
        q: p[1].exp(),
        gain_db: p[2],
    }
}

fn clamp_params(x: &mut [f64], like: &State) {
    for (i, f) in like.filters.iter().enumerate() {
        let (fc, q) = match f.kind {
            Kind::Peak => (PEAK_FC, PEAK_Q),
            Kind::LowShelf => (LOW_SHELF_FC, SHELF_Q),
            Kind::HighShelf => (HIGH_SHELF_FC, SHELF_Q),
        };
        x[3 * i] = x[3 * i].clamp(fc.0.ln(), fc.1.ln());
        let q_max = match f.kind {
            Kind::Peak => peak_max_q(x[3 * i].exp()),
            _ => q.1,
        };
        x[3 * i + 1] = x[3 * i + 1].clamp(q.0.ln(), q_max.ln());
        x[3 * i + 2] = x[3 * i + 2].clamp(-GAIN_LIMIT_DB, GAIN_LIMIT_DB);
    }
}

fn peak_max_q(fc: f64) -> f64 {
    let t = crate::fr::log_ramp(fc, 5_000.0, 10_000.0);
    PEAK_Q.1 + t * (TREBLE_MAX_Q - PEAK_Q.1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::response_db;
    use crate::fr::grid;

    fn curve_of(filters: &[Filter], pts: &[f64]) -> Vec<f64> {
        pts.iter()
            .map(|&f| response_db(filters, f, 48_000.0))
            .collect()
    }

    fn pk(fc: f64, gain_db: f64, q: f64) -> Filter {
        Filter {
            kind: Kind::Peak,
            fc,
            gain_db,
            q,
        }
    }

    #[test]
    fn recovers_a_known_filter_set() {
        let pts = grid();
        let known = [
            Filter {
                kind: Kind::LowShelf,
                fc: 90.0,
                gain_db: -4.0,
                q: 0.7,
            },
            pk(300.0, 3.0, 1.0),
            pk(3_000.0, -5.0, 2.0),
            pk(6_500.0, 4.0, 3.0),
        ];
        let target = curve_of(&known, &pts);
        let opt = FitOptions {
            target_rms_db: 0.05,
            min_improvement_db: 0.01,
            ..FitOptions::default()
        };
        let f = fit(&pts, &target, &vec![1.0; pts.len()], &opt);
        assert!(f.rms_db < 0.3, "rms {:.3}, {:?}", f.rms_db, f.filters);
    }

    #[test]
    fn flat_target_gives_no_filters() {
        let pts = grid();
        let flat = vec![0.0; pts.len()];
        let f = fit(&pts, &flat, &vec![1.0; pts.len()], &FitOptions::default());
        assert!(f.filters.is_empty(), "{:?}", f.filters);
        assert!(f.rms_db < 1e-6);
    }

    #[test]
    fn stays_inside_bounds_on_an_impossible_target() {
        let pts = grid();
        let target: Vec<f64> = pts
            .iter()
            .enumerate()
            .map(|(k, _)| if k % 2 == 0 { 30.0 } else { -30.0 })
            .collect();
        let f = fit(&pts, &target, &vec![1.0; pts.len()], &FitOptions::default());
        assert!(f.filters.len() <= 10);
        for x in &f.filters {
            assert!(x.gain_db.abs() <= GAIN_LIMIT_DB + 1e-9, "{x:?}");
            if x.kind == Kind::Peak {
                assert!(x.q <= peak_max_q(x.fc) + 1e-9, "{x:?}");
            }
            assert!(x.q >= PEAK_Q.0 - 1e-9 && x.q <= PEAK_Q.1 + 1e-9, "{x:?}");
        }
    }

    #[test]
    fn level_goes_to_the_offset_not_the_filters() {
        let pts = grid();
        let target = vec![5.0; pts.len()];
        let f = fit(&pts, &target, &vec![1.0; pts.len()], &FitOptions::default());
        assert!((f.offset_db - 5.0).abs() < 0.1, "{}", f.offset_db);
        assert!(f.filters.is_empty(), "{:?}", f.filters);
    }
}
