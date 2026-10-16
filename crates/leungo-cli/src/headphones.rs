//! Dashboard side of headphone profiles: search and fitting run on a worker
//! thread so the page stays responsive; the page polls `state()`.

use leungo_core::autoeq::{self, Model};
use leungo_core::headphones::{self as hp, Profile};
use leungo_core::paths;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

const MAX_HITS: usize = 8;

#[derive(Default)]
struct Inner {
    index: Option<Arc<Vec<Model>>>,
    /// What the worker is doing, for the page.
    busy: Option<String>,
    error: Option<String>,
    query: String,
    hits: Vec<Value>,
}

#[derive(Clone)]
pub struct Headphones {
    inner: Arc<Mutex<Inner>>,
    fs: f64,
}

impl Headphones {
    pub fn new(fs: f64) -> Self {
        Self {
            inner: Arc::default(),
            fs,
        }
    }

    pub fn state(&self) -> Value {
        let g = self.inner.lock().unwrap();
        let active = hp::read_profile(&paths::active_headphone_path()).map(|p| profile_json(&p));
        let saved: Vec<String> = hp::stored(&paths::headphones_dir())
            .into_iter()
            .map(|p| p.name)
            .collect();
        json!({
            "busy": g.busy,
            "error": g.error,
            "query": g.query,
            "hits": g.hits,
            "active": active,
            "saved": saved,
        })
    }

    /// Searches the index, downloading it first if needed.
    pub fn search(&self, query: &str) {
        let query = query.trim().to_string();
        let index = {
            let mut g = self.inner.lock().unwrap();
            if g.busy.is_some() {
                return;
            }
            g.query = query.clone();
            g.error = None;
            g.index.clone()
        };
        match index {
            Some(models) => self.set_hits(&models, &query),
            None => self.spawn("Loading the headphone list…", move |me| {
                let models = me.index()?;
                me.set_hits(&models, &query);
                Ok(())
            }),
        }
    }

    /// Uses a saved profile right away, or downloads and fits a new one.
    pub fn pick(&self, name: &str) {
        let saved = hp::stored(&paths::headphones_dir())
            .into_iter()
            .find(|p| p.name == name);
        if let Some(p) = saved {
            let p = hp::refit(&p, &paths::headphones_dir(), self.fs)
                .ok()
                .flatten()
                .unwrap_or(p);
            let mut g = self.inner.lock().unwrap();
            g.error = hp::activate(&p).err().map(|e| format!("{e:#}"));
            return;
        }
        let name = name.to_string();
        self.spawn(&format!("Tuning for {name}…"), move |me| {
            let models = me.index()?;
            let model = models
                .iter()
                .find(|m| m.name == name)
                .ok_or_else(|| anyhow::anyhow!("{name} is not in the list"))?;
            let profile = hp::build(model, &hp::http_get, me.fs, &paths::headphones_dir())?;
            hp::activate(&profile)?;
            me.inner.lock().unwrap().hits.clear();
            Ok(())
        });
    }

    pub fn clear(&self) {
        let mut g = self.inner.lock().unwrap();
        g.error = hp::deactivate().err().map(|e| format!("{e:#}"));
    }

    fn index(&self) -> anyhow::Result<Arc<Vec<Model>>> {
        if let Some(i) = self.inner.lock().unwrap().index.clone() {
            return Ok(i);
        }
        let models = Arc::new(hp::load_index(&hp::http_get, &paths::autoeq_index_path())?);
        self.inner.lock().unwrap().index = Some(models.clone());
        Ok(models)
    }

    fn set_hits(&self, models: &[Model], query: &str) {
        let hits = autoeq::search(models, query, MAX_HITS)
            .into_iter()
            .map(|m| {
                let sources: Vec<&str> = m.entries.iter().map(|e| e.source.as_str()).collect();
                json!({ "name": m.name, "sources": sources })
            })
            .collect();
        self.inner.lock().unwrap().hits = hits;
    }

    fn spawn(
        &self,
        what: &str,
        job: impl FnOnce(&Headphones) -> anyhow::Result<()> + Send + 'static,
    ) {
        {
            let mut g = self.inner.lock().unwrap();
            if g.busy.is_some() {
                return;
            }
            g.busy = Some(what.to_string());
            g.error = None;
        }
        let me = self.clone();
        let spawned = std::thread::Builder::new()
            .name("ae-headphones".into())
            .spawn(move || {
                let result = job(&me);
                let mut g = me.inner.lock().unwrap();
                g.busy = None;
                if let Err(e) = result {
                    tracing::warn!("headphones: {e:#}");
                    g.error = Some(friendly(&e));
                }
            });
        if spawned.is_err() {
            let mut g = self.inner.lock().unwrap();
            g.busy = None;
            g.error = Some("could not start the download".into());
        }
    }
}

/// Refits the active profile once when it was made for an older target.
/// Strength goes back to full: the new target is meant to be used whole.
pub fn upgrade_active(fs: f64) {
    let Some(p) = hp::read_profile(&paths::active_headphone_path()) else {
        return;
    };
    match hp::refit(&p, &paths::headphones_dir(), fs) {
        Ok(Some(new)) => {
            if let Err(e) = hp::activate(&new).and_then(|()| full_strength()) {
                tracing::warn!("headphone upgrade: {e:#}");
            } else {
                tracing::info!("{} refit for {}", new.name, new.target);
            }
        }
        Ok(None) => {}
        Err(e) => tracing::warn!("headphone refit: {e:#}"),
    }
}

fn full_strength() -> anyhow::Result<()> {
    let path = paths::signature_path();
    let mut v: serde_json::Map<String, Value> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    v.insert("device".into(), json!(1.0));
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    std::fs::write(&path, Value::Object(v).to_string())?;
    Ok(())
}

fn profile_json(p: &Profile) -> Value {
    json!({
        "name": p.name,
        "sources": p.sources,
        "target": p.target,
        "rms": p.rms_db,
    })
}

/// Network errors read badly; say what the user can do instead.
fn friendly(e: &anyhow::Error) -> String {
    let text = format!("{e:#}");
    if text.contains("download") {
        "Couldn't reach GitHub. Check your internet connection and try again.".into()
    } else {
        text
    }
}
