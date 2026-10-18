//! Windows: polls the system media controls (the panel behind the volume
//! flyout), which Spotify, browsers, Media Player and most other players
//! report to.

use crate::NowPlaying;

/// The track signature and display info for what `app` reports. Returns
/// `None` when the session has no title yet.
pub fn track(app: &str, title: &str, artist: &str, album: &str) -> Option<(String, NowPlaying)> {
    if title.is_empty() {
        return None;
    }
    let app = app.strip_suffix(".exe").unwrap_or(app).to_lowercase();
    Some((
        format!("{app}:{title}|{artist}|{album}"),
        NowPlaying {
            title: title.to_string(),
            artist: artist.to_string(),
            album: album.to_string(),
        },
    ))
}

#[cfg(windows)]
pub use imp::spawn_with_now_playing;

#[cfg(windows)]
mod imp {
    use super::*;
    use crate::NowPlayingHandle;
    use anyhow::{Context, Result};
    use std::sync::mpsc::{Receiver, channel};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use windows::Media::Control::{
        GlobalSystemMediaTransportControlsSessionManager as Manager,
        GlobalSystemMediaTransportControlsSessionPlaybackStatus as PlaybackStatus,
    };

    fn query(manager: &Manager) -> Option<(String, NowPlaying)> {
        let session = manager.GetCurrentSession().ok()?;
        let playing = session
            .GetPlaybackInfo()
            .and_then(|i| i.PlaybackStatus())
            .is_ok_and(|s| s == PlaybackStatus::Playing || s == PlaybackStatus::Paused);
        if !playing {
            return None;
        }
        let props = session.TryGetMediaPropertiesAsync().ok()?.join().ok()?;
        let text = |v: windows::core::Result<windows::core::HSTRING>| {
            v.map(|s| s.to_string()).unwrap_or_default()
        };
        track(
            &text(session.SourceAppUserModelId()),
            &text(props.Title()),
            &text(props.Artist()),
            &text(props.AlbumTitle()),
        )
    }

    pub fn spawn_with_now_playing() -> Result<(Receiver<String>, NowPlayingHandle)> {
        let manager = Manager::RequestAsync()
            .and_then(|op| op.join())
            .context("open the system media controls")?;
        let (tx, rx) = channel::<String>();
        let handle: NowPlayingHandle = Arc::new(Mutex::new(None));
        let shared = handle.clone();
        std::thread::Builder::new()
            .name("leungo-smtc".into())
            .spawn(move || {
                let mut last: Option<String> = None;
                loop {
                    if let Some((sig, info)) = query(&manager)
                        && last.as_deref() != Some(sig.as_str())
                    {
                        last = Some(sig.clone());
                        if let Ok(mut n) = shared.lock() {
                            *n = Some(info);
                        }
                        if tx.send(sig).is_err() {
                            break;
                        }
                    }
                    std::thread::sleep(Duration::from_millis(1000));
                }
            })
            .context("spawn media controls poller")?;
        Ok((rx, handle))
    }
}

#[cfg(test)]
mod tests {
    use super::track;

    #[test]
    fn signs_a_track_by_app_title_artist_and_album() {
        let (sig, np) = track("Spotify.exe", "Not Like Us", "Kendrick Lamar", "GNX").unwrap();
        assert_eq!(sig, "spotify:Not Like Us|Kendrick Lamar|GNX");
        assert_eq!(np.artist, "Kendrick Lamar");
    }

    #[test]
    fn a_session_without_a_title_is_nothing() {
        assert!(track("chrome", "", "", "").is_none());
    }
}
