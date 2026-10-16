//! Shared domain types: config, engine traits, path helpers, and the
//! offline analyzer used by the CLI.

pub mod autoeq;
pub mod config;
pub mod engine;
pub mod headphones;
pub mod offline;
pub mod paths;

pub use config::{
    AnalysisCfg, ApplierCfg, CaptureCfg, Config, EqCfg, GuiCfg, Preset, StandaloneCfg, TargetsCfg,
    target_delta,
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

/// Everything the output stage runs.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EqOutput {
    /// Song bands at `eq.frequencies`, Q `eq.q`.
    pub gains: Vec<f64>,
    /// Device correction, already scaled by `device_strength`.
    pub device: Vec<leungo_eq::device::Filter>,
    /// Never positive.
    pub preamp_db: f64,
}

pub trait EqApplier: Send {
    fn apply_eq(&mut self, eq: &EqOutput) -> Result<()>;
    fn set_bypass(&mut self, on: bool) -> Result<()>;
}
