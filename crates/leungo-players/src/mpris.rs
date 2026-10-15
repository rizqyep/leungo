//! MPRIS track monitor: watches `org.mpris.MediaPlayer2` players on the
//! session bus and reports a new track signature whenever a player starts
//! playing something else. This gives the engine an authoritative track
//! change signal for gapless players (Spotify crossfade/buffering) where
//! neither silence gaps nor spectral spikes are reliable.
//!
//! At startup the current track of every running player is read and sent as
//! the first signature, which the engine only records. Every later change is
//! sent, so the first skip after launch is never lost.

use crate::{NowPlaying, NowPlayingHandle};
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use zbus::zvariant::Value;

/// Signature used to decide whether the playing track changed: track id if
/// the player provides one, else url, else title + album.
fn track_signature(metadata: &HashMap<String, Value>) -> Option<String> {
    for key in ["mpris:trackid", "xesam:url"] {
        if let Some(Value::Str(s)) = metadata.get(key) {
            return Some(s.as_str().to_string());
        }
        if let Some(Value::ObjectPath(p)) = metadata.get(key) {
            return Some(p.as_str().to_string());
        }
    }
    let title = metadata.get("xesam:title").and_then(as_string);
    let album = metadata.get("xesam:album").and_then(as_string);
    match (title, album) {
        (Some(t), Some(a)) => Some(format!("{t}|{a}")),
        (Some(t), None) => Some(t.clone()),
        _ => None,
    }
}

fn now_playing(metadata: &HashMap<String, Value>) -> NowPlaying {
    let get = |k: &str| metadata.get(k).and_then(as_string).unwrap_or_default();
    NowPlaying {
        title: get("xesam:title"),
        artist: get("xesam:artist"),
        album: get("xesam:album"),
    }
}

/// Extracts the track signature from the `changed` dictionary of a
/// `PropertiesChanged` signal, if it carries new metadata.
fn parse_changed(changed: &HashMap<String, Value>) -> Option<(String, NowPlaying)> {
    let Value::Dict(metadata) = unwrap_variant(changed.get("Metadata")?) else {
        return None;
    };
    let metadata: HashMap<String, Value> = metadata
        .iter()
        .filter_map(|(k, v)| {
            let Value::Str(k) = unwrap_variant(k) else {
                return None;
            };
            Some((k.as_str().to_string(), unwrap_variant(v).try_clone().ok()?))
        })
        .collect();
    Some((track_signature(&metadata)?, now_playing(&metadata)))
}

/// Values inside `a{sv}` dictionaries arrive wrapped in one variant layer
/// (`Value::Value`); the property readers above match on the inner value.
fn unwrap_variant<'a, 'b>(mut v: &'a Value<'b>) -> &'a Value<'b> {
    while let Value::Value(inner) = v {
        v = inner;
    }
    v
}

fn as_string(value: &Value) -> Option<String> {
    match unwrap_variant(value) {
        Value::Str(s) => Some(s.as_str().to_string()),
        Value::Array(list) => {
            // Spotify sometimes wraps titles in string arrays
            list.iter()
                .find_map(|v| as_string(v))
                .or_else(|| list.iter().next().and_then(as_string))
        }
        _ => None,
    }
}

/// Spawns the monitor thread. The thread lives for the process (the daemon
/// owns it), so no stop flag is needed; if the consumer disconnects, it exits.
/// Like `spawn`, and also returns a handle holding the latest title/artist.
pub fn spawn_with_now_playing() -> Result<(Receiver<String>, NowPlayingHandle)> {
    let (tx, rx) = channel::<String>();
    let handle: NowPlayingHandle = Arc::new(Mutex::new(None));
    let thread_handle = handle.clone();
    std::thread::Builder::new()
        .name("leungo-mpris".into())
        .spawn(move || {
            if let Err(e) = monitor(tx, thread_handle) {
                tracing::warn!("mpris monitor stopped: {e:#}");
            }
        })
        .context("spawn mpris thread")?;
    Ok((rx, handle))
}

fn monitor(tx: Sender<String>, now: NowPlayingHandle) -> Result<()> {
    let conn = zbus::blocking::Connection::session().context("open session bus")?;
    let rule = zbus::MatchRule::builder()
        .interface("org.freedesktop.DBus.Properties")
        .unwrap()
        .member("PropertiesChanged")
        .unwrap()
        .path_namespace("/org/mpris")
        .unwrap()
        .build();

    let stream = zbus::blocking::MessageIterator::for_match_rule(rule, &conn, None)
        .context("listen for mpris signals")?;
    // Last signature per player (keyed by its unique bus name), so two
    // players alternating never look like one track changing.
    let mut last: HashMap<String, String> = HashMap::new();
    if let Err(e) = seed_current_tracks(&conn, &mut last, &tx, &now) {
        tracing::warn!("could not read current mpris tracks: {e:#}");
    }

    for msg in stream {
        let Ok(msg) = msg else { continue };
        let header = msg.header();
        let sender = header
            .sender()
            .map(|s| s.as_str().to_string())
            .unwrap_or_default();
        let Some(path) = header.path() else { continue };
        if path.as_str() != "/org/mpris/MediaPlayer2" {
            continue;
        }
        let body = msg.body();
        let Ok((interface, changed, _)) =
            body.deserialize::<(String, HashMap<String, Value>, Vec<String>)>()
        else {
            continue;
        };
        if interface != "org.mpris.MediaPlayer2.Player" {
            continue;
        }
        let Some((sig, info)) = parse_changed(&changed) else {
            continue;
        };
        if last.get(&sender) == Some(&sig) {
            continue;
        }
        last.insert(sender, sig.clone());
        if let Ok(mut n) = now.lock() {
            *n = Some(info);
        }
        if tx.send(sig).is_err() {
            break;
        }
    }
    Ok(())
}

/// Reads the current track of each running player so the first change after
/// startup is a real change, and sends the first one as the engine's baseline.
fn seed_current_tracks(
    conn: &zbus::blocking::Connection,
    last: &mut HashMap<String, String>,
    tx: &Sender<String>,
    now: &NowPlayingHandle,
) -> Result<()> {
    let dbus = zbus::blocking::fdo::DBusProxy::new(conn)?;
    let mut sent = false;
    for name in dbus.list_names()? {
        if !name.as_str().starts_with("org.mpris.MediaPlayer2.") {
            continue;
        }
        let Ok(owner) = dbus.get_name_owner(name.inner().clone()) else {
            continue;
        };
        let proxy = zbus::blocking::Proxy::new(
            conn,
            name.as_str().to_string(),
            "/org/mpris/MediaPlayer2",
            "org.mpris.MediaPlayer2.Player",
        )?;
        let Ok(meta) =
            proxy.get_property::<HashMap<String, zbus::zvariant::OwnedValue>>("Metadata")
        else {
            continue;
        };
        let meta: HashMap<String, Value> =
            meta.into_iter().map(|(k, v)| (k, Value::from(v))).collect();
        if let Some(sig) = track_signature(&meta) {
            last.insert(owner.as_str().to_string(), sig.clone());
            if !sent {
                sent = true;
                if let Ok(mut n) = now.lock() {
                    *n = Some(now_playing(&meta));
                }
                let _ = tx.send(sig);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_track_id_then_url_then_title() {
        let mut m = HashMap::new();
        assert!(track_signature(&m).is_none());
        m.insert("xesam:title".into(), Value::Str("Song".into()));
        assert_eq!(track_signature(&m).as_deref(), Some("Song"));
        m.insert("xesam:album".into(), Value::Str("Album".into()));
        assert_eq!(track_signature(&m).as_deref(), Some("Song|Album"));
        m.insert("xesam:url".into(), Value::Str("file:///a.mp3".into()));
        assert_eq!(track_signature(&m).as_deref(), Some("file:///a.mp3"));
        m.insert("mpris:trackid".into(), Value::Str("spotify:track:x".into()));
        assert_eq!(track_signature(&m).as_deref(), Some("spotify:track:x"));
    }

    /// A body shaped like a real player's PropertiesChanged signal, sent
    /// through the D-Bus wire format and read back the way the monitor does.
    fn signature_over_the_wire(inner: HashMap<String, Value<'static>>) -> Option<String> {
        use zbus::zvariant::{Endian, serialized::Context};
        let mut changed: HashMap<String, Value> = HashMap::new();
        changed.insert("Metadata".into(), Value::new(inner));
        let body = (
            "org.mpris.MediaPlayer2.Player".to_string(),
            changed,
            Vec::<String>::new(),
        );
        let ctx = Context::new_dbus(Endian::Little, 0);
        let data = zbus::zvariant::to_bytes(ctx, &body).unwrap();
        let (_, changed, _): (String, HashMap<String, Value>, Vec<String>) =
            data.deserialize().unwrap().0;
        parse_changed(&changed).map(|(sig, _)| sig)
    }

    #[test]
    fn signal_body_with_object_path_trackid_is_recognised() {
        let mut inner: HashMap<String, Value> = HashMap::new();
        inner.insert(
            "mpris:trackid".into(),
            Value::ObjectPath(
                zbus::zvariant::ObjectPath::try_from("/com/spotify/track/abc").unwrap(),
            ),
        );
        inner.insert("xesam:title".into(), Value::Str("Song".into()));
        inner.insert(
            "xesam:artist".into(),
            Value::new(vec!["Artist One".to_string(), "Artist Two".to_string()]),
        );
        assert_eq!(
            signature_over_the_wire(inner).as_deref(),
            Some("/com/spotify/track/abc")
        );
    }

    #[test]
    fn title_and_first_artist_survive_the_wire() {
        use zbus::zvariant::{Endian, serialized::Context};
        let mut inner: HashMap<String, Value> = HashMap::new();
        inner.insert("mpris:trackid".into(), Value::Str("t1".into()));
        inner.insert("xesam:title".into(), Value::Str("Song".into()));
        inner.insert(
            "xesam:artist".into(),
            Value::new(vec!["Artist One".to_string(), "Artist Two".to_string()]),
        );
        inner.insert("xesam:album".into(), Value::Str("Album".into()));
        let mut changed: HashMap<String, Value> = HashMap::new();
        changed.insert("Metadata".into(), Value::new(inner));
        let body = ("p".to_string(), changed, Vec::<String>::new());
        let data = zbus::zvariant::to_bytes(Context::new_dbus(Endian::Little, 0), &body).unwrap();
        let (_, changed, _): (String, HashMap<String, Value>, Vec<String>) =
            data.deserialize().unwrap().0;
        let (sig, info) = parse_changed(&changed).unwrap();
        assert_eq!(sig, "t1");
        assert_eq!(info.title, "Song");
        assert_eq!(info.artist, "Artist One");
        assert_eq!(info.album, "Album");
    }
}
