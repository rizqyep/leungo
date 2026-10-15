//! Shared domain types: config, engine traits, path helpers, and the
//! offline analyzer used by the CLI.

pub mod config;
pub mod engine;
pub mod offline;
pub mod paths;

pub use config::{
    AnalysisCfg, ApplierCfg, CaptureCfg, Config, EqCfg, GuiCfg, Preset, TargetsCfg, target_delta,
};
pub use engine::{Engine, Status};

use anyhow::Result;

/// Events produced by a capture source.
#[derive(Debug)]
pub enum SourceEvent {
    /// Interleaved samples at the configured rate/channels.
    Chunk(Vec<f32>),
    /// No data available right now (poll again).
    Idle,
    /// The stream is finished.
    Ended,
}

pub trait SampleSource: Send {
    fn next(&mut self) -> Result<SourceEvent>;
}

pub trait EqApplier: Send {
    fn apply_eq(&mut self, gains: &[f64]) -> Result<()>;
    fn set_bypass(&mut self, on: bool) -> Result<()>;
}
