//! Archetype prototypes fitted to the listener's own tracks. The hand-set
//! signatures are only a starting point: k-means moves each prototype to
//! where that listener's music really sits, and the confidence becomes a
//! Gaussian posterior over the fitted prototypes instead of a raw margin.

use crate::classifier::{classify_descriptors, signature_centers};
use ae_eq::Archetype;
use serde::{Deserialize, Serialize};

/// Fewest samples a fit is attempted with.
pub const MIN_SAMPLES: usize = 30;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FittedClassifier {
    pub archetypes: Vec<Archetype>,
    pub centers: Vec<[f64; 4]>,
    /// Within-cluster variance per dimension; sets how sharply the posterior
    /// falls off with distance.
    pub variance: f64,
    pub samples: usize,
}

impl FittedClassifier {
    /// Most probable archetype and its posterior probability.
    pub fn classify(&self, d: &[f64; 4]) -> (Archetype, f64) {
        let dist: Vec<f64> = self.centers.iter().map(|c| dist2(c, d)).collect();
        let min = dist.iter().cloned().fold(f64::INFINITY, f64::min);
        let w: Vec<f64> = dist
            .iter()
            .map(|x| (-(x - min) / (2.0 * self.variance)).exp())
            .collect();
        let total: f64 = w.iter().sum();
        let (best, wb) =
            w.iter().enumerate().fold(
                (0, f64::MIN),
                |acc, (i, &v)| if v > acc.1 { (i, v) } else { acc },
            );
        (self.archetypes[best], wb / total)
    }
}

pub struct FitReport {
    pub classifier: FittedClassifier,
    pub counts: Vec<(Archetype, usize)>,
    pub mean_confidence: f64,
    pub baseline_mean_confidence: f64,
}

fn dist2(a: &[f64; 4], b: &[f64; 4]) -> f64 {
    a.iter().zip(b.iter()).map(|(x, y)| (x - y).powi(2)).sum()
}

/// K-means seeded from the built-in prototypes, so each cluster keeps the
/// archetype name (and therefore its target delta).
pub fn fit(samples: &[[f64; 4]]) -> Option<FitReport> {
    if samples.len() < MIN_SAMPLES {
        return None;
    }
    let seeds = signature_centers();
    let archetypes: Vec<Archetype> = seeds.iter().map(|s| s.0).collect();
    let mut centers: Vec<[f64; 4]> = seeds.iter().map(|s| s.1).collect();
    let k = centers.len();

    let assign = |centers: &[[f64; 4]], x: &[f64; 4]| -> usize {
        (0..k)
            .min_by(|&a, &b| {
                dist2(&centers[a], x)
                    .partial_cmp(&dist2(&centers[b], x))
                    .unwrap()
            })
            .unwrap()
    };

    for _ in 0..100 {
        let mut sum = vec![[0.0f64; 4]; k];
        let mut n = vec![0usize; k];
        for x in samples {
            let c = assign(&centers, x);
            n[c] += 1;
            for d in 0..4 {
                sum[c][d] += x[d];
            }
        }
        let mut moved = 0.0;
        for c in 0..k {
            if n[c] == 0 {
                continue;
            }
            let new = [
                sum[c][0] / n[c] as f64,
                sum[c][1] / n[c] as f64,
                sum[c][2] / n[c] as f64,
                sum[c][3] / n[c] as f64,
            ];
            moved += dist2(&new, &centers[c]);
            centers[c] = new;
        }
        if moved < 1e-9 {
            break;
        }
    }

    // Prototypes that explain almost none of the data are dropped: their
    // distance would only make the remaining ones look more certain.
    let mut counts = vec![0usize; k];
    for x in samples {
        counts[assign(&centers, x)] += 1;
    }
    let min_pop = ((samples.len() as f64 * 0.03).ceil() as usize).max(3);
    let keep: Vec<usize> = (0..k).filter(|&c| counts[c] >= min_pop).collect();
    if keep.len() < 2 {
        return None;
    }
    let archetypes: Vec<Archetype> = keep.iter().map(|&c| archetypes[c]).collect();
    let centers: Vec<[f64; 4]> = keep.iter().map(|&c| centers[c]).collect();
    let k = centers.len();

    let mut counts = vec![0usize; k];
    let mut within = 0.0;
    for x in samples {
        let c = (0..k)
            .min_by(|&a, &b| {
                dist2(&centers[a], x)
                    .partial_cmp(&dist2(&centers[b], x))
                    .unwrap()
            })
            .unwrap();
        counts[c] += 1;
        within += dist2(&centers[c], x);
    }
    let variance = (within / (samples.len() as f64 * 4.0)).max(0.02);
    let classifier = FittedClassifier {
        archetypes: archetypes.clone(),
        centers,
        variance,
        samples: samples.len(),
    };
    let mean_confidence = samples
        .iter()
        .map(|x| classifier.classify(x).1)
        .sum::<f64>()
        / samples.len() as f64;
    let baseline_mean_confidence = samples
        .iter()
        .map(|x| classify_descriptors(x).1)
        .sum::<f64>()
        / samples.len() as f64;
    Some(FitReport {
        classifier,
        counts: archetypes.into_iter().zip(counts).collect(),
        mean_confidence,
        baseline_mean_confidence,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blob(center: [f64; 4], n: usize, seed: u64) -> Vec<[f64; 4]> {
        let mut s = seed;
        let mut r = move || {
            s = s
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((s >> 33) as f64 / (1u64 << 31) as f64) - 0.5
        };
        (0..n)
            .map(|_| {
                [
                    center[0] + 0.1 * r(),
                    center[1] + 0.1 * r(),
                    center[2] + 0.1 * r(),
                    center[3] + 0.1 * r(),
                ]
            })
            .collect()
    }

    #[test]
    fn too_few_samples_is_none() {
        assert!(fit(&[[0.0; 4]; 5]).is_none());
    }

    #[test]
    fn separated_clusters_give_high_posterior() {
        let mut data = Vec::new();
        for (i, (_, c)) in signature_centers().iter().enumerate() {
            // pull each cluster slightly off the built-in prototype
            let shifted = [c[0] * 0.8 + 0.1, c[1] * 0.8, c[2] * 0.8, c[3] * 0.8];
            data.extend(blob(shifted, 20, i as u64 + 1));
        }
        let r = fit(&data).unwrap();
        assert!(
            r.mean_confidence > r.baseline_mean_confidence,
            "fitted {} vs built-in {}",
            r.mean_confidence,
            r.baseline_mean_confidence
        );
        assert!(r.mean_confidence > 0.6, "got {}", r.mean_confidence);
    }

    #[test]
    fn single_blob_is_not_fitted() {
        // One blob only: there is nothing to separate, so no model and no
        // inflated confidence.
        let data = blob([0.0, 0.0, 0.0, 0.0], 100, 7);
        assert!(fit(&data).is_none());
    }
}
