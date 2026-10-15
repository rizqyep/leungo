//! Rule-based archetype classifier. Features are mapped to bounded
//! [-1, 1] descriptors and scored against fixed reference signatures with a
//! weighted squared distance. The guitar-band vs neutral boundary is
//! genuinely fuzzy on spectrum alone; the confidence value reflects that.

use crate::features::Features;
use leungo_eq::Archetype;

struct Signature {
    archetype: Archetype,
    bass: f64,
    treble: f64,
    crest: f64,
    centroid: f64,
}

const SIGNATURES: [Signature; 5] = [
    Signature {
        archetype: Archetype::BassHeavy,
        bass: 0.85,
        treble: -0.5,
        crest: -0.8,
        centroid: -0.6,
    },
    Signature {
        archetype: Archetype::VocalCentric,
        bass: 0.1,
        treble: -0.5,
        crest: 0.0,
        centroid: 0.2,
    },
    Signature {
        archetype: Archetype::GuitarBand,
        bass: 0.6,
        treble: -0.2,
        crest: -0.1,
        centroid: 0.35,
    },
    Signature {
        archetype: Archetype::BrightAcoustic,
        bass: -0.2,
        treble: 0.5,
        crest: 0.6,
        centroid: 0.6,
    },
    Signature {
        archetype: Archetype::Neutral,
        bass: 0.0,
        treble: 0.0,
        crest: 0.0,
        centroid: 0.0,
    },
];

/// The four bounded descriptors the classifier works in: bass vs mid,
/// treble vs mid, crest and centroid, each in [-1, 1].
pub fn descriptors(f: &Features) -> [f64; 4] {
    let bass_total = db_to_pow(f.sub_db) + db_to_pow(f.bass_db);
    let mid_total = db_to_pow(f.mid_db) + 1e-20;
    let high_total = db_to_pow(f.high_mid_db) + db_to_pow(f.treble_db);
    let bass_vs_mid = 10.0 * (bass_total / mid_total).log10();
    let treble_vs_mid = 10.0 * (high_total / mid_total).log10();

    let bass_norm = ((bass_vs_mid - 2.0) / 6.0).tanh();
    let treble_norm = ((treble_vs_mid + 1.0) / 4.0).tanh();
    let crest_norm = ((f.crest_db - 12.0) / 8.0).tanh();
    let centroid_norm = if f.centroid > 1.0 {
        ((f.centroid.log2() - 10.47) / 2.5).clamp(-1.0, 1.0)
    } else {
        -1.0
    };
    [bass_norm, treble_norm, crest_norm, centroid_norm]
}

/// Built-in archetype prototypes in descriptor space, used to seed fitting.
pub fn signature_centers() -> Vec<(Archetype, [f64; 4])> {
    SIGNATURES
        .iter()
        .map(|s| (s.archetype, [s.bass, s.treble, s.crest, s.centroid]))
        .collect()
}

/// Returns the best-matching archetype and a confidence in [0, 1] based on
/// the margin to the runner-up.
pub fn classify(f: &Features) -> (Archetype, f64) {
    classify_descriptors(&descriptors(f))
}

pub fn classify_descriptors(feats: &[f64; 4]) -> (Archetype, f64) {
    let mut scored: Vec<(f64, Archetype)> = SIGNATURES
        .iter()
        .map(|s| {
            let d = (feats[0] - s.bass).powi(2)
                + (feats[1] - s.treble).powi(2)
                + (feats[2] - s.crest).powi(2)
                + (feats[3] - s.centroid).powi(2);
            (d, s.archetype)
        })
        .collect();
    scored.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
    let (best_d, best) = scored[0];
    let conf = ((scored[1].0 - best_d) / 1.5).clamp(0.0, 1.0);
    (best, conf)
}

fn db_to_pow(db: f64) -> f64 {
    10f64.powf(db / 10.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spectrum::SpectrumAnalyzer;

    fn features_for(spec: &SpectrumAnalyzer, crest_db: f64) -> Features {
        crate::features::extract(spec, crest_db)
    }

    fn push_tones(spec: &mut SpectrumAnalyzer, parts: &[(f64, f64)], seconds: f64) {
        let fs = spec.fs();
        let len = (seconds * fs) as usize;
        let mut interleaved = Vec::with_capacity(len * 2);
        for i in 0..len {
            let mut m = 0.0;
            for &(f, a) in parts {
                m += a * (std::f64::consts::TAU * f * i as f64 / fs).sin();
            }
            interleaved.push(m as f32);
            interleaved.push(m as f32);
        }
        spec.push_interleaved(&interleaved, 2, -60.0);
    }

    fn new_spec() -> SpectrumAnalyzer {
        SpectrumAnalyzer::new(2048, 1024, 2.0, 48_000.0)
    }

    #[test]
    fn bass_stack_classifies_bass_heavy() {
        let mut spec = new_spec();
        push_tones(
            &mut spec,
            &[
                (55.0, 0.3),
                (80.0, 0.3),
                (110.0, 0.3),
                (160.0, 0.3),
                (1000.0, 0.003),
            ],
            6.0,
        );
        let (a, conf) = classify(&features_for(&spec, 11.0));
        assert_eq!(a, Archetype::BassHeavy);
        assert!(conf > 0.3, "confidence {conf}");
    }

    #[test]
    fn mid_stack_classifies_vocal() {
        let mut spec = new_spec();
        push_tones(
            &mut spec,
            &[
                (80.0, 0.003),
                (220.0, 0.3),
                (440.0, 0.25),
                (880.0, 0.2),
                (1320.0, 0.15),
                (2200.0, 0.12),
                (8000.0, 0.003),
            ],
            6.0,
        );
        let (a, _) = classify(&features_for(&spec, 10.0));
        assert_eq!(a, Archetype::VocalCentric);
    }

    #[test]
    fn bright_stack_classifies_bright_acoustic() {
        let mut spec = new_spec();
        push_tones(
            &mut spec,
            &[
                (100.0, 0.0025),
                (1000.0, 0.0025),
                (3000.0, 0.1),
                (5000.0, 0.08),
                (8000.0, 0.08),
                (12000.0, 0.06),
            ],
            5.0,
        );
        // sparse impulses for a dynamic crest
        let fs = spec.fs();
        let len = (1.0 * fs) as usize;
        let mut interleaved = Vec::with_capacity(len * 2);
        for i in 0..len {
            let v = if i % (fs as usize / 4) == 0 { 0.5 } else { 0.0 };
            interleaved.push(v as f32);
            interleaved.push(v as f32);
        }
        spec.push_interleaved(&interleaved, 2, -60.0);
        let (a, _) = classify(&features_for(&spec, 14.0));
        assert_eq!(a, Archetype::BrightAcoustic);
    }

    #[test]
    fn balanced_tones_classify_neutral() {
        let mut spec = new_spec();
        push_tones(
            &mut spec,
            &[(100.0, 0.2), (1000.0, 0.2), (4000.0, 0.2)],
            6.0,
        );
        let (a, _) = classify(&features_for(&spec, 8.0));
        assert_eq!(a, Archetype::Neutral);
    }

    #[test]
    fn silent_spectrum_classifies_something() {
        let spec = new_spec();
        let (_a, _c) = classify(&features_for(&spec, 0.0));
    }
}
