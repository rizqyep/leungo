//! macOS: polls Spotify and Music through `osascript`.
//!
//! The first time it runs, macOS asks whether the terminal (or the app that
//! launched leungo) may control Spotify or Music. That permission has to be
//! granted for track information to appear. Without it leungo still works,
//! it just falls back to silence-gap detection for track changes.

use crate::NowPlaying;

/// Separates the fields in the script output (ASCII unit separator).
pub const SEP: char = '\u{1f}';

/// Parses `source<SEP>id<SEP>title<SEP>artist<SEP>album` into a track
/// signature and display info. Returns `None` for empty or malformed output.
pub fn parse_output(out: &str) -> Option<(String, NowPlaying)> {
    let line = out.trim_end_matches(['\n', '\r']);
    if line.is_empty() {
        return None;
    }
    let mut it = line.split(SEP);
    let source = it.next()?;
    let id = it.next()?;
    let title = it.next()?;
    let artist = it.next().unwrap_or("");
    let album = it.next().unwrap_or("");
    if source.is_empty() || (id.is_empty() && title.is_empty()) {
        return None;
    }
    let sig = if id.is_empty() {
        format!("{source}:{title}|{album}")
    } else {
        format!("{source}:{id}")
    };
    Some((
        sig,
        NowPlaying {
            title: title.to_string(),
            artist: artist.to_string(),
            album: album.to_string(),
        },
    ))
}

/// The AppleScript for one app. `id_expr` is the AppleScript expression for
/// a stable track id of `t`.
pub fn script_for(app: &str, source: &str, id_expr: &str) -> String {
    format!(
        r##"set sep to (ASCII character 31)
tell application "{app}"
  if player state is playing or player state is paused then
    set t to current track
    return "{source}" & sep & ({id_expr}) & sep & (name of t) & sep & (artist of t) & sep & (album of t)
  end if
end tell
return ""
"##
    )
}

#[cfg(target_os = "macos")]
pub use imp::spawn_with_now_playing;

#[cfg(target_os = "macos")]
mod imp {
    use super::*;
    use crate::NowPlayingHandle;
    use anyhow::{Context, Result};
    use std::process::Command;
    use std::sync::mpsc::{Receiver, channel};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    struct Source {
        process: &'static str,
        script: String,
    }

    fn sources() -> Vec<Source> {
        vec![
            Source {
                process: "Spotify",
                script: script_for("Spotify", "spotify", "id of t"),
            },
            Source {
                process: "Music",
                script: script_for("Music", "music", "(database ID of t) as text"),
            },
        ]
    }

    /// Whether a process with exactly this name is running. Checked first so
    /// `osascript` never has to resolve an app that is not installed, which
    /// would pop up a "Where is ..." dialog.
    fn is_running(name: &str) -> bool {
        Command::new("pgrep")
            .args(["-x", name])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn query(s: &Source) -> Option<(String, NowPlaying)> {
        if !is_running(s.process) {
            return None;
        }
        let out = Command::new("osascript")
            .args(["-e", &s.script])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        parse_output(&String::from_utf8_lossy(&out.stdout))
    }

    pub fn spawn_with_now_playing() -> Result<(Receiver<String>, NowPlayingHandle)> {
        let (tx, rx) = channel::<String>();
        let handle: NowPlayingHandle = Arc::new(Mutex::new(None));
        let shared = handle.clone();
        std::thread::Builder::new()
            .name("leungo-applescript".into())
            .spawn(move || {
                let srcs = sources();
                let mut last: Option<String> = None;
                loop {
                    // first player that reports something wins
                    let found = srcs.iter().find_map(query);
                    if let Some((sig, info)) = found {
                        if last.as_deref() != Some(sig.as_str()) {
                            last = Some(sig.clone());
                            if let Ok(mut n) = shared.lock() {
                                *n = Some(info);
                            }
                            if tx.send(sig).is_err() {
                                break;
                            }
                        }
                    }
                    std::thread::sleep(Duration::from_millis(1000));
                }
            })
            .context("spawn applescript poller")?;
        Ok((rx, handle))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(parts: &[&str]) -> String {
        parts.join(&SEP.to_string())
    }

    #[test]
    fn parses_a_spotify_line() {
        let out = line(&[
            "spotify",
            "spotify:track:abc",
            "Not Like Us",
            "Kendrick Lamar",
            "GNX",
        ]) + "\n";
        let (sig, np) = parse_output(&out).unwrap();
        assert_eq!(sig, "spotify:spotify:track:abc");
        assert_eq!(np.title, "Not Like Us");
        assert_eq!(np.artist, "Kendrick Lamar");
        assert_eq!(np.album, "GNX");
    }

    #[test]
    fn missing_id_falls_back_to_title_and_album() {
        let out = line(&["music", "", "Song", "Artist", "Album"]);
        assert_eq!(parse_output(&out).unwrap().0, "music:Song|Album");
    }

    #[test]
    fn empty_and_malformed_output_is_none() {
        assert!(parse_output("").is_none());
        assert!(parse_output("\n").is_none());
        assert!(parse_output("garbage without separators").is_none());
        assert!(parse_output(&line(&["spotify", "", "", "", ""])).is_none());
    }

    #[test]
    fn script_names_the_app_and_never_launches_it() {
        let s = script_for("Spotify", "spotify", "id of t");
        assert!(s.contains("tell application \"Spotify\""));
        assert!(s.contains("player state is playing"));
        assert!(!s.contains('\\'), "no stray backslashes: {s}");
        assert!(
            s.trim_end().ends_with("return \"\""),
            "falls back to empty: {s}"
        );
        // balanced quotes, a cheap guard against a broken literal
        assert_eq!(s.matches('"').count() % 2, 0, "{s}");
    }
}
