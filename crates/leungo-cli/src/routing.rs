//! Keeps the system output pointed at the loopback device while leungo runs,
//! plays to the user's real device, and puts the previous output back.

use anyhow::Result;
#[cfg(target_os = "macos")]
use leungo_core::paths;
use std::path::Path;

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn pick_real_output(
    outputs: &[String],
    default: Option<&str>,
    pinned: &str,
    stored_original: Option<&str>,
    loopback: &str,
) -> Option<String> {
    let is_loopback = |n: &str| n.to_lowercase().contains(&loopback.to_lowercase());
    let exists = |n: &str| outputs.iter().find(|o| o.as_str() == n).cloned();
    let matches = |frag: &str| {
        outputs
            .iter()
            .find(|o| o.to_lowercase().contains(&frag.to_lowercase()))
            .cloned()
    };
    if !pinned.is_empty()
        && let Some(found) = matches(pinned)
        && !is_loopback(&found)
    {
        return Some(found);
    }
    if let Some(d) = default
        && !is_loopback(d)
        && let Some(found) = exists(d)
    {
        return Some(found);
    }
    if let Some(o) = stored_original
        && !is_loopback(o)
        && let Some(found) = exists(o)
    {
        return Some(found);
    }
    outputs.iter().find(|o| !is_loopback(o)).cloned()
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn read_original(path: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    v["original"].as_str().map(str::to_string)
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn write_original(path: &Path, name: &str) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, serde_json::json!({ "original": name }).to_string());
}

#[cfg(target_os = "macos")]
pub struct Route {
    original: Option<String>,
    loopback_id: Option<u32>,
    loopback_volume: Vec<(u32, f32)>,
}

#[cfg(target_os = "macos")]
const BASELINE: f32 = 0.75;
#[cfg(target_os = "macos")]
const KEY_STEP: f32 = 1.0 / 16.0;

#[cfg(target_os = "macos")]
impl Route {
    /// Nothing is switched before `activate`: an idle built-in speaker exposes
    /// no streams, so the engine has to open it first.
    pub fn engage(loopback: &str, pinned: &str) -> Result<(Self, String)> {
        use leungo_standalone::route;
        let outs = route::outputs();
        let names: Vec<String> = outs.iter().map(|o| o.name.clone()).collect();
        let default = route::default_output().map(|o| o.name);
        let stored = read_original(&paths::route_path());
        let real = pick_real_output(
            &names,
            default.as_deref(),
            pinned,
            stored.as_deref(),
            loopback,
        )
        .ok_or_else(|| anyhow::anyhow!("no output device to play to"))?;

        let default_is_loopback = default
            .as_deref()
            .is_some_and(|d| d.to_lowercase().contains(&loopback.to_lowercase()));
        let original = if default_is_loopback {
            stored.unwrap_or_else(|| real.clone())
        } else {
            default.unwrap_or_else(|| real.clone())
        };
        let loopback_id = outs
            .iter()
            .find(|o| o.name.to_lowercase().contains(&loopback.to_lowercase()))
            .map(|o| o.id);
        Ok((
            Route {
                original: Some(original),
                loopback_id,
                loopback_volume: Vec::new(),
            },
            real,
        ))
    }

    pub fn activate(&mut self) -> Result<()> {
        use leungo_standalone::route;
        if let Some(original) = &self.original {
            write_original(&paths::route_path(), original);
        }
        if let Some(id) = self.loopback_id {
            route::set_default_output(id)?;
        }
        Ok(())
    }

    /// Keeps the loopback device at a fixed volume, makes up its attenuation
    /// digitally, and moves each volume-key press onto the real output.
    pub fn sync_volume(
        &mut self,
        real: &str,
        set_makeup_db: Box<dyn Fn(f32) + Send>,
        stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) {
        use leungo_standalone::route;
        use std::sync::atomic::Ordering;
        let Some(bh) = self.loopback_id else { return };
        self.loopback_volume = route::volumes(bh);
        if self.loopback_volume.is_empty() {
            return;
        }
        let real = real.to_string();
        let has_real_volume = route::find(&real).is_some_and(|o| !route::volumes(o.id).is_empty());
        let baseline = if has_real_volume { BASELINE } else { 1.0 };
        route::set_volume(bh, baseline);
        let db = route::volume_db(bh).unwrap_or(0.0);
        set_makeup_db(-db);
        if !has_real_volume {
            return;
        }
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                std::thread::sleep(std::time::Duration::from_millis(40));
                let Some(now) = route::volumes(bh).first().map(|v| v.1) else {
                    continue;
                };
                let steps = ((now - baseline) / KEY_STEP).round();
                if steps == 0.0 {
                    continue;
                }
                if let Some(dev) = route::find(&real) {
                    let vols = route::volumes(dev.id);
                    if !vols.is_empty() {
                        let avg = vols.iter().map(|v| v.1).sum::<f32>() / vols.len() as f32;
                        route::set_volume(dev.id, (avg + steps * KEY_STEP).clamp(0.0, 1.0));
                    }
                }
                route::set_volume(bh, baseline);
            }
        });
    }

    pub fn release(&mut self) {
        if let Some(bh) = self.loopback_id
            && let Some(&(_, v)) = self.loopback_volume.first()
        {
            leungo_standalone::route::set_volume(bh, v);
        }
        let Some(name) = self.original.take() else {
            return;
        };
        restore_to(&name);
        let _ = std::fs::remove_file(paths::route_path());
    }
}

#[cfg(target_os = "macos")]
impl Drop for Route {
    fn drop(&mut self) {
        self.release();
    }
}

#[cfg(target_os = "macos")]
fn restore_to(name: &str) {
    use leungo_standalone::route;
    if let Some(o) = route::find(name) {
        let _ = route::set_default_output(o.id);
    }
}

/// Puts back the output recorded by an earlier run that did not exit cleanly.
#[cfg(target_os = "macos")]
pub fn restore_saved() -> Result<Option<String>> {
    let path = paths::route_path();
    let Some(name) = read_original(&path) else {
        return Ok(None);
    };
    restore_to(&name);
    let _ = std::fs::remove_file(&path);
    Ok(Some(name))
}

#[cfg(not(target_os = "macos"))]
pub struct Route;

#[cfg(not(target_os = "macos"))]
impl Route {
    pub fn engage(_loopback: &str, _pinned: &str) -> Result<(Self, String)> {
        anyhow::bail!("output routing applies to macOS")
    }

    pub fn activate(&mut self) -> Result<()> {
        Ok(())
    }

    pub fn sync_volume(
        &mut self,
        _real: &str,
        _set_makeup_db: Box<dyn Fn(f32) + Send>,
        _stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) {
    }
}

#[cfg(not(target_os = "macos"))]
pub fn restore_saved() -> Result<Option<String>> {
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::pick_real_output;

    fn outs() -> Vec<String> {
        ["MacBook Air Speakers", "AB17X USB Audio", "BlackHole 2ch"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    #[test]
    fn follows_the_default_output_when_nothing_is_pinned() {
        let got = pick_real_output(&outs(), Some("AB17X USB Audio"), "", None, "BlackHole");
        assert_eq!(got.as_deref(), Some("AB17X USB Audio"));
    }

    #[test]
    fn a_pinned_device_wins_over_the_default() {
        let got = pick_real_output(
            &outs(),
            Some("AB17X USB Audio"),
            "speakers",
            None,
            "BlackHole",
        );
        assert_eq!(got.as_deref(), Some("MacBook Air Speakers"));
    }

    #[test]
    fn falls_back_to_the_stored_original_then_any_real_device() {
        let got = pick_real_output(
            &outs(),
            Some("BlackHole 2ch"),
            "",
            Some("AB17X USB Audio"),
            "BlackHole",
        );
        assert_eq!(got.as_deref(), Some("AB17X USB Audio"));
        let got = pick_real_output(
            &outs(),
            Some("BlackHole 2ch"),
            "",
            Some("Gone"),
            "BlackHole",
        );
        assert_eq!(got.as_deref(), Some("MacBook Air Speakers"));
    }

    #[test]
    fn a_missing_pinned_device_does_not_block_playback() {
        let got = pick_real_output(
            &outs(),
            Some("MacBook Air Speakers"),
            "Gone",
            None,
            "BlackHole",
        );
        assert_eq!(got.as_deref(), Some("MacBook Air Speakers"));
    }

    #[test]
    fn never_picks_the_loopback_device() {
        let only_loopback = vec!["BlackHole 2ch".to_string()];
        assert_eq!(
            pick_real_output(&only_loopback, Some("BlackHole 2ch"), "", None, "BlackHole"),
            None
        );
    }
}
