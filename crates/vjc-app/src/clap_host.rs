//! CLAP audio plugins in the app (docs/specs/audio-plugins.md): the scanned catalog, the
//! effect kind stored in projects, and one shared state per CLAP effect of the project (its
//! parameter values and status) that the audio lanes and the UI both read.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use vjc_clap::{ClapShared, Entry};
use vjc_model::{Effect, EffectId, Value};

/// Kind string of a CLAP effect in a project: `clap:<plugin id>|<file path>`.
pub fn kind_of(e: &Entry) -> String {
    format!("clap:{}|{}", e.id, e.path)
}

/// Plugin id and path out of a kind string; None for anything that is not a CLAP effect.
pub fn parse_kind(kind: &str) -> Option<(&str, &str)> {
    let (id, path) = kind.strip_prefix("clap:")?.split_once('|')?;
    (!id.is_empty() && !path.is_empty()).then_some((id, path))
}

/// Platform folders plus the ones from the preferences, without repeats.
pub fn scan_dirs(extra: &[String]) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = vjc_clap::default_scan_dirs();
    v.extend(
        extra
            .iter()
            .filter(|d| !d.trim().is_empty())
            .map(PathBuf::from),
    );
    let mut seen = std::collections::HashSet::new();
    v.retain(|d| seen.insert(d.clone()));
    v
}

#[derive(Default)]
struct Catalog {
    entries: Vec<Entry>,
    scanning: bool,
}

#[derive(Default)]
pub struct ClapHost {
    catalog: Arc<Mutex<Catalog>>,
    rack: Mutex<HashMap<EffectId, Arc<ClapShared>>>,
}

impl ClapHost {
    pub fn entries(&self) -> Vec<Entry> {
        self.catalog
            .lock()
            .map(|c| c.entries.clone())
            .unwrap_or_default()
    }

    pub fn scanning(&self) -> bool {
        self.catalog.lock().is_ok_and(|c| c.scanning)
    }

    #[cfg(test)]
    pub fn set_entries(&self, entries: Vec<Entry>) {
        if let Ok(mut c) = self.catalog.lock() {
            c.entries = entries;
        }
    }

    /// Scans in a background thread (nothing happens while a scan runs). The cache file keeps
    /// the plugin info by path and mtime, so only new or changed files are loaded.
    /// `force` (the Rescan button) also retries the files remembered as failed. Returns whether
    /// a scan was started (false while another one runs).
    pub fn rescan(&self, extra_dirs: &[String], cache: Option<PathBuf>, force: bool) -> bool {
        {
            let Ok(mut c) = self.catalog.lock() else {
                return false;
            };
            if c.scanning {
                return false;
            }
            c.scanning = true;
        }
        let dirs = scan_dirs(extra_dirs);
        let cat = self.catalog.clone();
        let spawned = std::thread::Builder::new().name("clap-scan".into()).spawn({
            let cat = cat.clone();
            move || {
                let cache =
                    cache.unwrap_or_else(|| std::env::temp_dir().join("vjc-clap-cache.json"));
                let helper = vjc_clap::helper_path();
                let (found, _) = if force {
                    vjc_clap::scan_forced(&dirs, &cache, &helper)
                } else {
                    vjc_clap::scan(&dirs, &cache, &helper)
                };
                if let Ok(mut c) = cat.lock() {
                    c.entries = found;
                    c.scanning = false;
                }
            }
        });
        if spawned.is_err()
            && let Ok(mut c) = cat.lock()
        {
            c.scanning = false;
            return false;
        }
        true
    }

    /// The shared state of a CLAP effect (made on first use from the values in the project);
    /// None when the effect is not a CLAP one.
    pub fn shared(&self, e: &Effect) -> Option<Arc<ClapShared>> {
        let (id, path) = parse_kind(&e.kind)?;
        let mut rack = self.rack.lock().ok()?;
        Some(
            rack.entry(e.id)
                .or_insert_with(|| {
                    let vals: Vec<f64> = e
                        .params
                        .iter()
                        .map(|p| match p.value {
                            Value::Number(n) => n,
                            _ => 0.0,
                        })
                        .collect();
                    ClapShared::new(path, id, &vals)
                })
                .clone(),
        )
    }

    /// Forgets the effects that are not in `live` (removed from the project); their lanes
    /// stop when the chains drop them.
    pub fn retain(&self, live: &[EffectId]) {
        if let Ok(mut r) = self.rack.lock() {
            r.retain(|id, _| live.contains(id));
        }
    }
}

#[cfg(test)]
#[path = "clap_host_tests.rs"]
mod tests;
