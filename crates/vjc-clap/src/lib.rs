//! CLAP audio plugin host (docs/specs/audio-plugins.md). Plugins run in a helper process:
//! blocks cross through a shared file mapping, and a crash or a block over its time budget
//! turns the effect into a bypass (the audio comes back untouched) instead of stopping the show.
//! Not for the audio callback itself: `process` blocks up to the budget. Call it from a
//! worker that feeds the callback, and let the callback play dry audio when a block is late.
pub mod abi;
pub mod proto;

use memmap2::MmapMut;
use proto::{Request, Response};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub use proto::ParamInfo;

/// Most frames sent to the plugin in one block; longer buffers are split.
pub const MAX_FRAMES: usize = 4096;
/// Deadline for control calls (load, scan info, parameter get/set), not for audio blocks.
const CONTROL_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq)]
pub enum HostError {
    Spawn(String),
    /// The helper died (crash in plugin code) or its pipe broke.
    Died,
    /// The block took longer than the budget.
    Timeout,
    Plugin(String),
}

/// A running helper process. Dropping it kills the process.
pub struct Helper {
    child: Child,
    stdin: ChildStdin,
    rx: Receiver<Response>,
}

/// Helper binary next to the running executable.
pub fn helper_path() -> PathBuf {
    let exe = std::env::current_exe().unwrap_or_default();
    let name = if cfg!(windows) {
        "vjc-clap-helper.exe"
    } else {
        "vjc-clap-helper"
    };
    let beside = exe.with_file_name(name);
    if beside.exists() {
        return beside;
    }
    // test binaries live in target/<profile>/deps
    exe.parent()
        .and_then(|d| d.parent())
        .map(|d| d.join(name))
        .unwrap_or(beside)
}

impl Helper {
    pub fn spawn(bin: &Path) -> Result<Helper, HostError> {
        let mut child = Command::new(bin)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| HostError::Spawn(e.to_string()))?;
        let stdin = child.stdin.take().ok_or(HostError::Died)?;
        let out = child.stdout.take().ok_or(HostError::Died)?;
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(out).lines() {
                let Ok(line) = line else { break };
                // plugin code may print on stdout; anything that is not a response is ignored
                if let Ok(r) = serde_json::from_str::<Response>(&line)
                    && tx.send(r).is_err()
                {
                    break;
                }
            }
        });
        Ok(Helper { child, stdin, rx })
    }

    /// One request/response with a deadline. On timeout or death the helper is killed.
    pub fn request(&mut self, req: &Request, timeout: Duration) -> Result<Response, HostError> {
        let line = serde_json::to_string(req).unwrap();
        if writeln!(self.stdin, "{line}")
            .and_then(|_| self.stdin.flush())
            .is_err()
        {
            self.kill();
            return Err(HostError::Died);
        }
        match self.rx.recv_timeout(timeout) {
            Ok(Response::Err { msg }) => Err(HostError::Plugin(msg)),
            Ok(r) => Ok(r),
            Err(RecvTimeoutError::Timeout) => {
                self.kill();
                Err(HostError::Timeout)
            }
            Err(RecvTimeoutError::Disconnected) => {
                self.kill();
                Err(HostError::Died)
            }
        }
    }

    pub fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Helper {
    fn drop(&mut self) {
        self.kill();
    }
}

// ---- paths + scan -------------------------------------------------------------------

/// The file that is actually loaded. A macOS `.clap` is a bundle (a directory): the
/// executable is the only file in `Contents/MacOS/`.
pub fn resolve_binary(p: &Path) -> PathBuf {
    if !p.is_dir() {
        return p.to_path_buf();
    }
    let files: Vec<PathBuf> = std::fs::read_dir(p.join("Contents/MacOS"))
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| p.is_file())
                .collect()
        })
        .unwrap_or_default();
    if files.len() == 1 {
        return files.into_iter().next().unwrap();
    }
    p.to_path_buf()
}

/// Standard CLAP folders for this OS, plus `~/.clap` and any in the `CLAP_PATH` variable.
pub fn default_scan_dirs() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = Vec::new();
    let home =
        std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from);
    if cfg!(target_os = "macos") {
        v.push(PathBuf::from("/Library/Audio/Plug-Ins/CLAP"));
        if let Some(h) = &home {
            v.push(h.join("Library/Audio/Plug-Ins/CLAP"));
        }
    } else if cfg!(windows) {
        if let Some(c) = std::env::var_os("COMMONPROGRAMFILES") {
            v.push(PathBuf::from(c).join("CLAP"));
        }
        if let Some(l) = std::env::var_os("LOCALAPPDATA") {
            v.push(
                PathBuf::from(l)
                    .join("Programs")
                    .join("Common")
                    .join("CLAP"),
            );
        }
    } else {
        v.push(PathBuf::from("/usr/lib/clap"));
    }
    if let Some(h) = &home {
        v.push(h.join(".clap"));
    }
    if let Some(p) = std::env::var_os("CLAP_PATH") {
        v.extend(std::env::split_paths(&p).filter(|p| !p.as_os_str().is_empty()));
    }
    v
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub path: String,
    pub mtime: u64,
    pub id: String,
    pub name: String,
    pub params: Vec<ParamInfo>,
}

#[derive(Default, Serialize, Deserialize)]
struct Cache {
    entries: BTreeMap<String, Vec<Entry>>,
    /// Files that failed, crashed or hung, by path -> mtime; not retried until the file changes.
    #[serde(default)]
    failed: BTreeMap<String, u64>,
}

fn mtime(p: &Path) -> u64 {
    std::fs::metadata(resolve_binary(p))
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

/// Scan `dirs` (not recursive) for `.clap` files. Results are cached in `cache_file` by path +
/// mtime; a file that crashes or hangs while being read is skipped and not retried until it
/// changes. Meant for a background thread. Returns the entries and the number of files loaded.
pub fn scan(dirs: &[PathBuf], cache_file: &Path, helper_bin: &Path) -> (Vec<Entry>, usize) {
    let mut cache: Cache = std::fs::read(cache_file)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    let mut found = Vec::new();
    let mut loads = 0;
    let mut helper: Option<Helper> = None;
    let mut next = BTreeMap::new();
    let mut next_failed = BTreeMap::new();
    for d in dirs {
        let Ok(rd) = std::fs::read_dir(d) else {
            continue;
        };
        let mut files: Vec<PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("clap"))
            .collect();
        files.sort();
        for f in files {
            let key = f.to_string_lossy().to_string();
            let mt = mtime(&f);
            if let Some(es) = cache
                .entries
                .remove(&key)
                .filter(|es| es.iter().all(|e| e.mtime == mt))
            {
                found.extend(es.iter().cloned());
                next.insert(key, es);
                continue;
            }
            if cache.failed.get(&key) == Some(&mt) {
                next_failed.insert(key, mt);
                continue;
            }
            loads += 1;
            if helper.is_none() {
                helper = Helper::spawn(helper_bin).ok();
            }
            let Some(h) = helper.as_mut() else { continue };
            match h.request(&Request::Info { path: key.clone() }, CONTROL_TIMEOUT) {
                Ok(Response::Info { plugins }) => {
                    let es: Vec<Entry> = plugins
                        .into_iter()
                        .map(|p| Entry {
                            path: key.clone(),
                            mtime: mt,
                            id: p.id,
                            name: p.name,
                            params: p.params,
                        })
                        .collect();
                    found.extend(es.iter().cloned());
                    next.insert(key, es);
                }
                Err(HostError::Plugin(_)) => {
                    next_failed.insert(key, mt);
                }
                _ => {
                    // crashed or hung: drop it, the next file gets a fresh helper
                    helper = None;
                    next_failed.insert(key, mt);
                }
            }
        }
    }
    if let Some(dir) = cache_file.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(
        cache_file,
        serde_json::to_vec(&Cache {
            entries: next,
            failed: next_failed,
        })
        .unwrap(),
    );
    (found, loads)
}

/// Effect id the plugin gets in the audio effect list.
pub fn effect_id(e: &Entry) -> String {
    format!("clap:{}", e.id)
}

// ---- effect -------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    Running,
    /// Shown as "plugin bypassed"; the audio passes through untouched. `enable` restarts it.
    Stopped(HostError),
}

pub struct ClapEffect {
    path: String,
    id: String,
    helper_bin: PathBuf,
    budget: Duration,
    rate: u32,
    helper: Option<Helper>,
    pub status: Status,
    params: Vec<ParamInfo>,
    values: Vec<f64>,
    shm_path: PathBuf,
    map: Option<MmapMut>,
}

impl ClapEffect {
    /// Load plugin `id` from `path`. `budget` is the longest a block may take.
    pub fn new(path: &str, id: &str, helper_bin: &Path, budget: Duration, rate: u32) -> ClapEffect {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let t = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let mut e = ClapEffect {
            path: path.into(),
            id: id.into(),
            helper_bin: helper_bin.into(),
            budget,
            rate,
            helper: None,
            status: Status::Stopped(HostError::Died),
            params: Vec::new(),
            values: Vec::new(),
            shm_path: std::env::temp_dir()
                .join(format!("vjc-clap-{}-{n}-{t}.shm", std::process::id())),
            map: None,
        };
        e.enable();
        e
    }

    /// (Re)start the helper and load the plugin; restores the parameters. Call again after a bypass.
    pub fn enable(&mut self) {
        self.helper = None;
        let r = Helper::spawn(&self.helper_bin).and_then(|mut h| {
            let r = h.request(
                &Request::Load {
                    path: self.path.clone(),
                    id: self.id.clone(),
                    rate: self.rate,
                    max_frames: MAX_FRAMES as u32,
                },
                CONTROL_TIMEOUT,
            )?;
            Ok((h, r))
        });
        match r {
            Ok((mut h, Response::Loaded { params })) => {
                if self.values.len() != params.len() {
                    self.values = params.iter().map(|p| p.default).collect();
                }
                self.params = params;
                for (i, v) in self.values.iter().enumerate() {
                    let set = Request::Set {
                        index: i as u32,
                        value: *v,
                    };
                    if let Err(e) = h.request(&set, CONTROL_TIMEOUT) {
                        self.status = Status::Stopped(e);
                        return; // helper stays None: bypass
                    }
                }
                self.helper = Some(h);
                self.status = Status::Running;
            }
            Ok(_) => self.status = Status::Stopped(HostError::Plugin("bad reply".into())),
            Err(e) => self.status = Status::Stopped(e),
        }
    }

    /// Parameters as the plugin lists them (native ranges).
    pub fn params(&self) -> &[ParamInfo] {
        &self.params
    }

    /// Set a parameter by position in `params()`. The value is clamped to the plugin's range and
    /// kept locally, so it survives a restart; it reaches the plugin with the next block.
    /// Returns false and sends nothing if the index is out of range or the value is not finite.
    pub fn set_param(&mut self, index: usize, value: f64) -> bool {
        if index >= self.params.len() || !value.is_finite() {
            return false;
        }
        let p = &self.params[index];
        let value = value.clamp(p.min.min(p.max), p.max.max(p.min));
        if self.values.len() <= index {
            self.values.resize(index + 1, 0.0);
        }
        self.values[index] = value;
        self.call(
            Request::Set {
                index: index as u32,
                value,
            },
            CONTROL_TIMEOUT,
        );
        true
    }

    /// Read a parameter back from the plugin (the last value set while bypassed).
    pub fn get_param(&mut self, index: usize) -> Option<f64> {
        if index >= self.params.len() {
            return None;
        }
        if self.helper.is_none() {
            return self.values.get(index).copied();
        }
        match self.call(
            Request::Get {
                index: index as u32,
            },
            CONTROL_TIMEOUT,
        ) {
            Some(Response::Value { value }) => Some(value),
            _ => None,
        }
    }

    fn call(&mut self, req: Request, timeout: Duration) -> Option<Response> {
        let h = self.helper.as_mut()?;
        match h.request(&req, timeout) {
            Ok(r) => Some(r),
            Err(e) => {
                self.status = Status::Stopped(e);
                self.helper = None;
                None
            }
        }
    }

    /// Process interleaved stereo f32 in place. Returns true when the plugin processed the whole
    /// buffer. Never fails the audio: when the plugin is bypassed, crashes or goes over the
    /// budget, the buffer keeps its input and `status` says why.
    pub fn process(&mut self, buf: &mut [f32]) -> bool {
        if self.helper.is_none() || buf.is_empty() || !buf.len().is_multiple_of(2) {
            return false;
        }
        if self.map.is_none() {
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(true)
                .open(&self.shm_path);
            let map = file.and_then(|f| {
                f.set_len((MAX_FRAMES * 16) as u64)?;
                unsafe { MmapMut::map_mut(&f) }
            });
            match map {
                Ok(m) => self.map = Some(m),
                Err(_) => return false,
            }
        }
        let shm = self.shm_path.to_string_lossy().to_string();
        // process into a copy so a failure halfway leaves the whole buffer dry
        let mut wet = buf.to_vec();
        for chunk in wet.chunks_mut(MAX_FRAMES * 2) {
            let frames = chunk.len() / 2;
            let map = self.map.as_mut().unwrap();
            for (i, v) in chunk.iter().enumerate() {
                map[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
            }
            let req = Request::Process {
                shm: shm.clone(),
                frames: frames as u32,
            };
            if !matches!(self.call(req, self.budget), Some(Response::Done)) {
                return false;
            }
            let map = self.map.as_ref().unwrap();
            let base = frames * 8;
            for (i, v) in chunk.iter_mut().enumerate() {
                let o = base + i * 4;
                *v = f32::from_le_bytes(map[o..o + 4].try_into().unwrap());
            }
        }
        buf.copy_from_slice(&wet);
        true
    }
}

impl Drop for ClapEffect {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.shm_path);
    }
}
