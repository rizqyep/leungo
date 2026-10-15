//! Spectral analysis, feature extraction, track-change detection, and the
//! rule-based archetype classifier.

pub mod classifier;
pub mod detector;
pub mod features;
pub mod fitted;
pub mod robust;
pub mod spectrum;

pub use classifier::{classify, classify_descriptors, descriptors, signature_centers};
pub use detector::{Detection, DetectorConfig, TrackDetector};
pub use features::{Features, extract, third_octave_levels};
pub use fitted::{FitReport, FittedClassifier, MIN_SAMPLES, fit};
pub use robust::BandTracker;
pub use spectrum::{FrameResult, SpectrumAnalyzer};
