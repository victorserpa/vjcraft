//! Effect library for the app: parses ISF headers on demand and compiles shaders on
//! worker threads, so the render thread never waits on naga.

use std::collections::HashMap;
use std::sync::mpsc::{Receiver, Sender, channel};
use vjc_fx::Isf;

/// A shader that finished translating, ready for `Compositor::add_effect`.
pub struct Compiled {
    pub kind: String,
    pub result: Result<String, String>,
}

#[derive(Clone, PartialEq)]
pub enum Status {
    Compiling,
    Ready,
    Failed(String),
}

pub struct FxLib {
    isfs: HashMap<String, Result<Isf, String>>,
    status: HashMap<String, Status>,
    tx: Sender<Compiled>,
    rx: Receiver<Compiled>,
}

impl FxLib {
    pub fn new() -> Self {
        let (tx, rx) = channel();
        FxLib {
            isfs: HashMap::new(),
            status: HashMap::new(),
            tx,
            rx,
        }
    }

    /// Parsed header of an effect (cached).
    pub fn isf(&mut self, kind: &str) -> Option<&Isf> {
        self.isfs
            .entry(kind.to_string())
            .or_insert_with(|| match vjc_fx::builtin_source(kind) {
                Some(src) => vjc_fx::parse(src).map_err(|e| e.to_string()),
                None => Err(format!("unknown effect {kind}")),
            })
            .as_ref()
            .ok()
    }

    /// Starts compiling `kind` on a worker thread the first time it is needed.
    pub fn request(&mut self, kind: &str) {
        if self.status.contains_key(kind) {
            return;
        }
        let Some(isf) = self.isf(kind).cloned() else {
            let why = match self.isfs.get(kind) {
                Some(Err(e)) => e.clone(),
                _ => "unknown effect".into(),
            };
            self.status.insert(kind.to_string(), Status::Failed(why));
            return;
        };
        self.status.insert(kind.to_string(), Status::Compiling);
        let (tx, kind) = (self.tx.clone(), kind.to_string());
        std::thread::spawn(move || {
            // A panic inside naga must not leave the effect stuck on Compiling.
            let result = std::panic::catch_unwind(|| vjc_fx::translate(&isf))
                .unwrap_or_else(|_| Err(vjc_fx::FxError("shader translation panicked".into())))
                .map_err(|e| e.to_string());
            let _ = tx.send(Compiled { kind, result });
        });
    }

    /// Finished compilations since the last call.
    pub fn poll(&mut self) -> Vec<Compiled> {
        self.rx.try_iter().collect()
    }

    /// Records the GPU-side outcome of a `Compiled` shader.
    pub fn set_status(&mut self, kind: &str, s: Status) {
        self.status.insert(kind.to_string(), s);
    }

    pub fn status(&self, kind: &str) -> Option<&Status> {
        self.status.get(kind)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiles_off_thread_and_reports_unknown() {
        let mut lib = FxLib::new();
        lib.request("invert");
        lib.request("nope");
        assert!(matches!(lib.status("nope"), Some(Status::Failed(_))));
        let mut got = vec![];
        for _ in 0..200 {
            got.extend(lib.poll());
            if !got.is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(got.len(), 1);
        assert!(got[0].result.is_ok());
    }
}
