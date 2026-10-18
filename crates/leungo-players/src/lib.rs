//! Track changes and now-playing information from media players.
//!
//! Each platform has its own source: MPRIS over D-Bus on Linux, AppleScript
//! polling of Spotify and Music on macOS, the system media controls on
//! Windows. All of them report a track
//! signature on a channel (used to decide when to re-analyze) and keep a
//! shared `NowPlaying` up to date for display.

use anyhow::Result;
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};

pub mod applescript;
#[cfg(target_os = "linux")]
mod mpris;
pub mod smtc;

/// What a player says it is playing, for display.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NowPlaying {
    pub title: String,
    pub artist: String,
    pub album: String,
}

/// Shared with the UI; the monitor keeps it current.
pub type NowPlayingHandle = Arc<Mutex<Option<NowPlaying>>>;

/// Starts the monitor for this platform. The receiver yields one signature
/// per track change (and one for the track playing at startup).
pub fn spawn_with_now_playing() -> Result<(Receiver<String>, NowPlayingHandle)> {
    #[cfg(target_os = "linux")]
    {
        mpris::spawn_with_now_playing()
    }
    #[cfg(target_os = "macos")]
    {
        applescript::spawn_with_now_playing()
    }
    #[cfg(windows)]
    {
        smtc::spawn_with_now_playing()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        anyhow::bail!("no media player integration for this platform")
    }
}

/// Like `spawn_with_now_playing` without the display handle.
pub fn spawn() -> Result<Receiver<String>> {
    spawn_with_now_playing().map(|(rx, _)| rx)
}
