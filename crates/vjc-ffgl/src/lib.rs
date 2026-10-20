//! FFGL plugin host, stage 1 (docs/specs/ffgl-host.md): plugins run in a helper process,
//! frames cross through a shared file mapping, and any crash or timeout turns the effect
//! into a pass-through instead of taking the show down.
pub mod abi;
pub mod glhost;
pub mod proto;

use memmap2::MmapMut;
use proto::*;
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, PartialEq)]
pub enum HostError {
    Spawn(String),
    /// The helper died (crash in plugin code) or its pipe broke.
    Died,
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
        "vjc-ffgl-helper.exe"
    } else {
        "vjc-ffgl-helper"
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

    /// One request/response with a deadline. On timeout the helper is killed.
    pub fn request(&mut self, req: &Request, timeout: Duration) -> Result<Response, HostError> {
        let line = serde_json::to_string(req).unwrap();
        if writeln!(self.stdin, "{line}")
            .and_then(|_| self.stdin.flush())
            .is_err()
        {
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

// ---- scan + cache -------------------------------------------------------------------

const EXTS: [&str; 4] = ["dylib", "bundle", "so", "dll"];

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Entry {
    pub path: String,
    pub mtime: u64,
    pub info: PluginInfo,
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct Cache {
    entries: BTreeMap<String, Entry>,
    /// Plugins that failed, crashed or hung, by path -> mtime; not retried until the file changes.
    #[serde(default)]
    failed: BTreeMap<String, u64>,
    /// Plugins whose info timed out once; retried with the long deadline.
    #[serde(default)]
    slow: BTreeMap<String, u64>,
}

/// The file that is actually loaded. A macOS `.bundle` is a directory: the executable lives in
/// `Contents/MacOS/` (named by `CFBundleExecutable` in Info.plist, else the only file there).
pub fn resolve_binary(p: &Path) -> PathBuf {
    if !p.is_dir() {
        return p.to_path_buf();
    }
    let macos = p.join("Contents/MacOS");
    if let Some(name) = std::fs::read_to_string(p.join("Contents/Info.plist"))
        .ok()
        .and_then(|t| {
            let rest = t.split("<key>CFBundleExecutable</key>").nth(1)?;
            let rest = rest.split("<string>").nth(1)?;
            Some(rest.split("</string>").next()?.trim().to_string())
        })
        .filter(|n| !n.is_empty() && !n.contains(['/', '\\']) && n != "..")
    {
        let f = macos.join(name);
        if f.is_file() {
            return f;
        }
    }
    let files: Vec<PathBuf> = std::fs::read_dir(&macos)
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

fn mtime(p: &Path) -> u64 {
    std::fs::metadata(resolve_binary(p))
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

/// Scan `dirs` for plugins. Results are cached in `cache_file` by path + mtime; a plugin
/// that crashes or hangs while being read is skipped (the helper is restarted). Returns the
/// entries and the number of plugins that had to be loaded (cache misses).
pub fn scan(dirs: &[PathBuf], cache_file: &Path, helper_bin: &Path) -> (Vec<Entry>, usize) {
    scan_with(dirs, cache_file, helper_bin, false, SCAN_QUICK, SCAN_LONG)
}

/// Like [`scan`], but forgets the plugins remembered as failed or slow first (the Rescan button).
pub fn scan_forced(dirs: &[PathBuf], cache_file: &Path, helper_bin: &Path) -> (Vec<Entry>, usize) {
    scan_with(dirs, cache_file, helper_bin, true, SCAN_QUICK, SCAN_LONG)
}

pub const SCAN_QUICK: Duration = Duration::from_secs(5);
pub const SCAN_LONG: Duration = Duration::from_secs(30);

/// [`scan`] with explicit deadlines: `quick` for a first try, `long` for a retry of a plugin that
/// timed out. `force` clears the failed and slow memory and gives every uncached plugin `long`.
pub fn scan_with(
    dirs: &[PathBuf],
    cache_file: &Path,
    helper_bin: &Path,
    force: bool,
    quick: Duration,
    long: Duration,
) -> (Vec<Entry>, usize) {
    let mut cache: Cache = std::fs::read(cache_file)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    if force {
        cache.failed.clear();
        cache.slow.clear();
    }
    let mut found = Vec::new();
    let mut loads = 0;
    let mut helper: Option<Helper> = None;
    let mut next = BTreeMap::new();
    let mut next_failed = BTreeMap::new();
    let mut next_slow = BTreeMap::new();
    for d in dirs {
        let Ok(rd) = std::fs::read_dir(d) else {
            continue;
        };
        let mut files: Vec<PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| EXTS.contains(&e))
            })
            .collect();
        files.sort();
        for f in files {
            let key = f.to_string_lossy().to_string();
            let mt = mtime(&f);
            if let Some(e) = cache.entries.remove(&key).filter(|e| e.mtime == mt) {
                next.insert(key, e.clone());
                found.push(e);
                continue;
            }
            if cache.failed.get(&key) == Some(&mt) {
                next_failed.insert(key, mt);
                continue;
            }
            let retry = force || cache.slow.get(&key) == Some(&mt);
            loads += 1;
            if helper.is_none() {
                helper = Helper::spawn(helper_bin).ok();
            }
            let Some(h) = helper.as_mut() else { continue };
            let deadline = if retry { long } else { quick };
            let r = h.request(&Request::Info { path: key.clone() }, deadline);
            match r {
                Ok(Response::Info { info }) => {
                    let e = Entry {
                        path: key.clone(),
                        mtime: mt,
                        info,
                    };
                    next.insert(key, e.clone());
                    found.push(e);
                }
                Err(HostError::Plugin(_)) => {
                    next_failed.insert(key, mt);
                }
                Err(HostError::Timeout) => {
                    helper = None; // killed by the timeout
                    if retry {
                        next_failed.insert(key, mt);
                    } else {
                        next_slow.insert(key, mt);
                    }
                }
                _ => {
                    // crashed: drop it, next plugin gets a fresh one
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
            slow: next_slow,
        })
        .unwrap(),
    );
    (found, loads)
}

/// Effect id the plugin gets in the effect list.
pub fn effect_id(e: &Entry) -> String {
    format!("ffgl:{}", e.info.id)
}

/// Parameters as ISF-style inputs, so dashboard, MIDI/OSC and automation treat them the same.
pub fn inputs(info: &PluginInfo) -> Vec<vjc_fx::Input> {
    info.params
        .iter()
        .map(|p| vjc_fx::Input {
            name: p.name.clone(),
            group: None,
            kind: match p.kind {
                ParamKind::Bool => vjc_fx::InputKind::Bool {
                    default: p.default >= 0.5,
                },
                ParamKind::Float => vjc_fx::InputKind::Float {
                    min: 0.0,
                    max: 1.0,
                    default: p.default as f64,
                },
            },
        })
        .collect()
}

// ---- effect -------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    Running,
    /// Shown as "plugin stopped"; the layer passes its input through.
    Stopped(HostError),
}

pub struct Frame {
    pub w: u32,
    pub h: u32,
    pub rgba: Vec<u8>,
}

pub struct FfglEffect {
    path: String,
    helper_bin: PathBuf,
    timeout: Duration,
    helper: Option<Helper>,
    pub status: Status,
    params: Vec<f32>,
    /// Changed since the last frame: sent together with the next one, latest value only.
    dirty: BTreeMap<usize, f32>,
    /// Remark from the host shown next to the status (e.g. the input was resized).
    pub note: Option<String>,
    shm_path: PathBuf,
    map: Option<(MmapMut, usize)>,
}

impl FfglEffect {
    pub fn new(path: &str, helper_bin: &Path, timeout: Duration) -> FfglEffect {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let t = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let shm_path =
            std::env::temp_dir().join(format!("vjc-ffgl-{}-{n}-{t}.shm", std::process::id()));
        let mut e = FfglEffect {
            path: path.into(),
            helper_bin: helper_bin.into(),
            timeout,
            helper: None,
            status: Status::Stopped(HostError::Died),
            params: Vec::new(),
            dirty: BTreeMap::new(),
            note: None,
            shm_path,
            map: None,
        };
        e.enable();
        e
    }

    /// (Re)start the helper and load the plugin; restores the parameters. Call again after "plugin stopped".
    pub fn enable(&mut self) {
        self.helper = None;
        self.dirty.clear();
        let r = Helper::spawn(&self.helper_bin).and_then(|mut h| {
            let r = h.request(
                &Request::Load {
                    path: self.path.clone(),
                },
                self.timeout,
            )?;
            Ok((h, r))
        });
        match r {
            Ok((mut h, Response::Info { info })) => {
                if self.params.len() != info.params.len() {
                    self.params = info.params.iter().map(|p| p.default).collect();
                }
                for (i, v) in self.params.iter().enumerate() {
                    let set = Request::Set {
                        index: i as u32,
                        value: *v,
                    };
                    match h.request(&set, self.timeout) {
                        Ok(_) => {}
                        // real plugins reject single values: log it and carry on
                        Err(HostError::Plugin(msg)) => {
                            eprintln!("ffgl: restoring parameter {i} rejected: {msg}");
                        }
                        Err(e) => {
                            self.status = Status::Stopped(e);
                            return; // helper stays None: pass-through
                        }
                    }
                }
                self.helper = Some(h);
                self.status = Status::Running;
            }
            Ok(_) => self.status = Status::Stopped(HostError::Plugin("bad reply".into())),
            Err(e) => self.status = Status::Stopped(e),
        }
    }

    /// Set a parameter (kept locally so it survives a restart). Returns false and sends nothing
    /// if the index is out of range or the value is not finite.
    pub fn set_param(&mut self, index: usize, value: f32) -> bool {
        if index >= self.params.len() || !value.is_finite() {
            return false;
        }
        if self.params[index] == value {
            return true; // already there: no round trip to the helper
        }
        self.params[index] = value;
        self.dirty.insert(index, value);
        true
    }

    /// Kills the helper process but leaves `status` alone, like a crash would: the next frame
    /// finds the helper gone, passes through and reports "plugin stopped".
    pub fn kill_helper(&mut self) {
        if let Some(h) = self.helper.as_mut() {
            h.kill();
        }
    }

    /// Read a parameter back from the plugin.
    pub fn get_param(&mut self, index: usize) -> Option<f32> {
        if index >= self.params.len() {
            return None;
        }
        self.flush();
        match self.call(Request::Get {
            index: index as u32,
        }) {
            Some(Response::Value { value }) => Some(value),
            _ => None,
        }
    }

    /// Sends the pending parameter changes on their own (only needed before a read).
    fn flush(&mut self) {
        for (index, value) in std::mem::take(&mut self.dirty) {
            self.call(Request::Set {
                index: index as u32,
                value,
            });
        }
    }

    fn call(&mut self, req: Request) -> Option<Response> {
        let h = self.helper.as_mut()?;
        match h.request(&req, self.timeout) {
            Ok(r) => Some(r),
            Err(e) => {
                self.status = Status::Stopped(e);
                self.helper = None;
                None
            }
        }
    }

    /// Process a frame. Never fails: when the plugin is stopped, crashes or times out the
    /// input comes back unchanged and `status` says why.
    pub fn process(&mut self, input: &Frame) -> Frame {
        let pass = || Frame {
            w: input.w,
            h: input.h,
            rgba: input.rgba.clone(),
        };
        if self.helper.is_none() {
            return pass();
        }
        let n = input.w as usize * input.h as usize * 4;
        if input.rgba.len() != n {
            return pass();
        }
        if self.map.as_ref().is_none_or(|(_, sz)| *sz != n) {
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(true)
                .open(&self.shm_path);
            let map = file.and_then(|f| {
                f.set_len(2 * n as u64)?;
                unsafe { MmapMut::map_mut(&f) }
            });
            match map {
                Ok(m) => self.map = Some((m, n)),
                Err(_) => return pass(),
            }
        }
        let (map, _) = self.map.as_mut().unwrap();
        map[..n].copy_from_slice(&input.rgba);
        let shm = self.shm_path.to_string_lossy().to_string();
        let params = std::mem::take(&mut self.dirty)
            .into_iter()
            .map(|(i, v)| (i as u32, v))
            .collect();
        match self.call(Request::Process {
            shm,
            w: input.w,
            h: input.h,
            params,
        }) {
            Some(Response::Done) => {
                let (map, _) = self.map.as_ref().unwrap();
                Frame {
                    w: input.w,
                    h: input.h,
                    rgba: map[n..2 * n].to_vec(),
                }
            }
            _ => pass(),
        }
    }
}

impl Drop for FfglEffect {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.shm_path);
    }
}
