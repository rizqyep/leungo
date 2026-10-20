//! Standalone engine: capture from a virtual loopback device, equalize in
//! process, play to the real output. Used on macOS, where there is no
//! EasyEffects to host the filters.
//!
//! Everything except device I/O is plain Rust and tested on any platform.

pub mod bridge;
pub mod chain;
pub mod makeup;
pub mod resample;

#[cfg(feature = "audio-io")]
pub mod io;

#[cfg(all(feature = "audio-io", target_os = "macos"))]
pub mod route;
