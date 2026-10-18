//! CLAP audio plugin host (docs/specs/audio-plugins.md). Stub: API only, written before the code.
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq)]
pub enum HostError {
    Spawn(String),
    Died,
    Timeout,
    Plugin(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    Running,
    Stopped(HostError),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParamInfo {
    pub id: u32,
    pub name: String,
    pub min: f64,
    pub max: f64,
    pub default: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub path: String,
    pub mtime: u64,
    pub id: String,
    pub name: String,
    pub params: Vec<ParamInfo>,
}

pub struct ClapEffect {
    pub status: Status,
}

impl ClapEffect {
    pub fn new(_path: &str, _id: &str, _helper: &Path, _budget: Duration, _rate: u32) -> ClapEffect {
        ClapEffect {
            status: Status::Stopped(HostError::Died),
        }
    }
    pub fn enable(&mut self) {}
    pub fn params(&self) -> &[ParamInfo] {
        &[]
    }
    pub fn set_param(&mut self, _index: usize, _value: f64) -> bool {
        false
    }
    pub fn get_param(&mut self, _index: usize) -> Option<f64> {
        None
    }
    /// Interleaved stereo f32, in place. Returns true when the plugin processed the block.
    pub fn process(&mut self, _buf: &mut [f32]) -> bool {
        false
    }
}

pub fn helper_path() -> PathBuf {
    PathBuf::new()
}
pub fn scan(_dirs: &[PathBuf], _cache: &Path, _helper: &Path) -> (Vec<Entry>, usize) {
    (Vec::new(), 0)
}
pub fn default_scan_dirs() -> Vec<PathBuf> {
    Vec::new()
}
pub fn resolve_binary(p: &Path) -> PathBuf {
    p.to_path_buf()
}
