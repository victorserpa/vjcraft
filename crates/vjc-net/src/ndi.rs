//! NDI through libndi loaded at runtime with `libloading`. The few structs and functions
//! used are declared here from NDI's public C header description; the SDK is neither
//! linked nor bundled. Without the runtime every entry point returns an error string.

use crate::convert::{opaque, pack_rows};
use crate::{NetSource, Protocol, RETRY, resolve};
use libloading::Library;
use std::ffi::{CStr, CString, c_char, c_void};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

const fn fourcc(a: u8, b: u8, c: u8, d: u8) -> i32 {
    (a as i32) | ((b as i32) << 8) | ((c as i32) << 16) | ((d as i32) << 24)
}
const FOURCC_RGBA: i32 = fourcc(b'R', b'G', b'B', b'A');
const FRAME_PROGRESSIVE: i32 = 1;
const TIMECODE_SYNTHESIZE: i64 = i64::MAX;
const RECV_COLOR_RGBX_RGBA: i32 = 2;
const RECV_BANDWIDTH_HIGHEST: i32 = 100;
const FRAME_TYPE_VIDEO: i32 = 1;

#[repr(C)]
struct SourceT {
    name: *const c_char,
    url: *const c_char,
}

#[repr(C)]
struct FindCreate {
    show_local_sources: bool,
    groups: *const c_char,
    extra_ips: *const c_char,
}

#[repr(C)]
struct SendCreate {
    name: *const c_char,
    groups: *const c_char,
    clock_video: bool,
    clock_audio: bool,
}

#[repr(C)]
struct RecvCreateV3 {
    source: SourceT,
    color_format: i32,
    bandwidth: i32,
    allow_video_fields: bool,
    recv_name: *const c_char,
}

#[repr(C)]
struct VideoFrameV2 {
    xres: i32,
    yres: i32,
    fourcc: i32,
    frame_rate_n: i32,
    frame_rate_d: i32,
    picture_aspect_ratio: f32,
    frame_format_type: i32,
    timecode: i64,
    data: *mut u8,
    line_stride: i32,
    metadata: *const c_char,
    timestamp: i64,
}

impl VideoFrameV2 {
    fn empty() -> Self {
        VideoFrameV2 {
            xres: 0,
            yres: 0,
            fourcc: 0,
            frame_rate_n: 0,
            frame_rate_d: 0,
            picture_aspect_ratio: 0.0,
            frame_format_type: 0,
            timecode: 0,
            data: std::ptr::null_mut(),
            line_stride: 0,
            metadata: std::ptr::null(),
            timestamp: 0,
        }
    }
}

/// Loaded libndi and its entry points. Dropping the last `Arc` unloads the library.
pub struct Lib {
    initialize: unsafe extern "C" fn() -> bool,
    destroy: unsafe extern "C" fn(),
    find_create: unsafe extern "C" fn(*const FindCreate) -> *mut c_void,
    find_destroy: unsafe extern "C" fn(*mut c_void),
    find_wait: unsafe extern "C" fn(*mut c_void, u32) -> bool,
    find_sources: unsafe extern "C" fn(*mut c_void, *mut u32) -> *const SourceT,
    send_create: unsafe extern "C" fn(*const SendCreate) -> *mut c_void,
    send_destroy: unsafe extern "C" fn(*mut c_void),
    send_video: unsafe extern "C" fn(*mut c_void, *const VideoFrameV2),
    /// Optional: absent in old runtimes.
    send_connections: Option<unsafe extern "C" fn(*mut c_void, u32) -> i32>,
    recv_create: unsafe extern "C" fn(*const RecvCreateV3) -> *mut c_void,
    recv_destroy: unsafe extern "C" fn(*mut c_void),
    recv_capture:
        unsafe extern "C" fn(*mut c_void, *mut VideoFrameV2, *mut c_void, *mut c_void, u32) -> i32,
    recv_free_video: unsafe extern "C" fn(*mut c_void, *const VideoFrameV2),
    _lib: Library,
}

/// Where libndi may live: `NDI_RUNTIME_DIR_V6/V5` first, then the standard installs.
pub fn candidates(env: &dyn Fn(&str) -> Option<String>) -> Vec<String> {
    let mut v = vec![];
    for var in ["NDI_RUNTIME_DIR_V6", "NDI_RUNTIME_DIR_V5"] {
        if let Some(d) = env(var).filter(|d| !d.is_empty()) {
            v.push(format!("{}/libndi.dylib", d.trim_end_matches('/')));
        }
    }
    v.extend(
        [
            "/usr/local/lib/libndi.dylib",
            "/opt/homebrew/lib/libndi.dylib",
            "/Library/NDI SDK for Apple/lib/macOS/libndi_advanced.dylib",
            "/Library/NDI SDK for Apple/lib/macOS/libndi.dylib",
            "libndi.dylib",
        ]
        .map(String::from),
    );
    v
}

macro_rules! sym {
    ($lib:expr, $name:literal) => {
        *unsafe { $lib.get($name) }.map_err(|e| format!("libndi is missing a function: {e}"))?
    };
}

impl Lib {
    /// Tries each path in turn; the error says where it looked.
    pub fn load_from(paths: &[String]) -> Result<Lib, String> {
        let mut last = String::new();
        for p in paths {
            match unsafe { Library::new(p) } {
                Ok(lib) => return Lib::bind(lib),
                Err(e) => last = e.to_string(),
            }
        }
        Err(format!(
            "NDI runtime (libndi) not found. Install NDI Tools or set NDI_RUNTIME_DIR_V6. Last error: {last}"
        ))
    }

    fn bind(lib: Library) -> Result<Lib, String> {
        let l = Lib {
            initialize: sym!(lib, b"NDIlib_initialize\0"),
            destroy: sym!(lib, b"NDIlib_destroy\0"),
            find_create: sym!(lib, b"NDIlib_find_create_v2\0"),
            find_destroy: sym!(lib, b"NDIlib_find_destroy\0"),
            find_wait: sym!(lib, b"NDIlib_find_wait_for_sources\0"),
            find_sources: sym!(lib, b"NDIlib_find_get_current_sources\0"),
            send_create: sym!(lib, b"NDIlib_send_create\0"),
            send_destroy: sym!(lib, b"NDIlib_send_destroy\0"),
            send_video: sym!(lib, b"NDIlib_send_send_video_v2\0"),
            send_connections: unsafe {
                lib.get::<unsafe extern "C" fn(*mut c_void, u32) -> i32>(
                    b"NDIlib_send_get_no_connections\0",
                )
            }
            .ok()
            .map(|f| *f),
            recv_create: sym!(lib, b"NDIlib_recv_create_v3\0"),
            recv_destroy: sym!(lib, b"NDIlib_recv_destroy\0"),
            recv_capture: sym!(lib, b"NDIlib_recv_capture_v2\0"),
            recv_free_video: sym!(lib, b"NDIlib_recv_free_video_v2\0"),
            _lib: lib,
        };
        if !unsafe { (l.initialize)() } {
            return Err("NDI runtime refused to initialize (unsupported CPU?)".into());
        }
        Ok(l)
    }
}

impl Drop for Lib {
    fn drop(&mut self) {
        unsafe { (self.destroy)() }
    }
}

// The NDI runtime is thread-safe; instances are used from one thread at a time by us.
unsafe impl Send for Lib {}
unsafe impl Sync for Lib {}

static LIB: OnceLock<Result<Arc<Lib>, String>> = OnceLock::new();

/// The process-wide runtime, loaded on first use. Never panics.
pub fn lib() -> Result<Arc<Lib>, String> {
    LIB.get_or_init(|| {
        let paths = candidates(&|k| std::env::var(k).ok());
        Lib::load_from(&paths).map(Arc::new)
    })
    .clone()
}

unsafe fn cstr(p: *const c_char) -> String {
    if p.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
    }
}

/// `MACHINE (Source)` names become label "MACHINE (Source)" (NDI already formats them so).
fn to_source(name: &str) -> NetSource {
    let app = name
        .split_once(" (")
        .map(|(m, _)| m)
        .unwrap_or("")
        .to_string();
    NetSource {
        protocol: Protocol::Ndi,
        label: name.to_string(),
        app,
        name: name
            .split_once(" (")
            .map_or(name, |(_, r)| r.trim_end_matches(')'))
            .to_string(),
        id: name.to_string(),
    }
}

const FIND_TOTAL_MS: u32 = 1500;
const FIND_SLICE_MS: u32 = 100;

/// Waits up to `total_ms` for announcements in short slices, checking `alive` between
/// them. `finder(ms)` blocks for at most `ms`. Returns false if `alive` said stop.
fn wait_sliced(
    total_ms: u32,
    slice_ms: u32,
    finder: &mut dyn FnMut(u32),
    alive: &mut dyn FnMut() -> bool,
) -> bool {
    let mut left = total_ms;
    while left > 0 {
        if !alive() {
            return false;
        }
        let ms = left.min(slice_ms);
        finder(ms);
        left -= ms;
    }
    alive()
}

/// Finds sources on the network, waiting up to about 1.5 s for announcements.
pub fn list() -> Result<Vec<NetSource>, String> {
    list_while(&mut || true).map(|o| o.unwrap_or_default())
}

/// Like `list`, but gives up early (None) as soon as `alive` returns false.
fn list_while(alive: &mut dyn FnMut() -> bool) -> Result<Option<Vec<NetSource>>, String> {
    let l = lib()?;
    let cfg = FindCreate {
        show_local_sources: true,
        groups: std::ptr::null(),
        extra_ips: std::ptr::null(),
    };
    let f = unsafe { (l.find_create)(&cfg) };
    if f.is_null() {
        return Err("NDI finder could not be created".into());
    }
    let finder = &mut |ms: u32| {
        unsafe { (l.find_wait)(f, ms) };
    };
    if !wait_sliced(FIND_TOTAL_MS, FIND_SLICE_MS, finder, alive) {
        unsafe { (l.find_destroy)(f) };
        return Ok(None);
    }
    let mut n = 0u32;
    let p = unsafe { (l.find_sources)(f, &mut n) };
    let mut out = vec![];
    if !p.is_null() {
        for i in 0..n as usize {
            let name = unsafe { cstr((*p.add(i)).name) };
            if !name.is_empty() {
                out.push(to_source(&name));
            }
        }
    }
    unsafe { (l.find_destroy)(f) };
    Ok(Some(out))
}

/// Frame rate as a fraction; NTSC rates get their exact 1001 denominator.
pub fn fps_ratio(fps: f32) -> (i32, i32) {
    let fps = if fps.is_finite() {
        fps.clamp(1.0, 240.0)
    } else {
        60.0
    };
    let ntsc = fps.fract() != 0.0 && ((fps * 1.001).round() - fps * 1.001).abs() < 0.005;
    if ntsc {
        ((fps * 1.001).round() as i32 * 1000, 1001)
    } else {
        ((fps * 1000.0).round() as i32, 1000)
    }
}

pub struct Sender {
    lib: Arc<Lib>,
    inst: *mut c_void,
}

unsafe impl Send for Sender {}

impl Sender {
    pub fn new(name: &str) -> Result<Sender, String> {
        let lib = lib()?;
        let cname = CString::new(name).map_err(|_| "Invalid NDI name".to_string())?;
        let cfg = SendCreate {
            name: cname.as_ptr(),
            groups: std::ptr::null(),
            clock_video: false,
            clock_audio: false,
        };
        let inst = unsafe { (lib.send_create)(&cfg) };
        if inst.is_null() {
            return Err("NDI sender could not be created".into());
        }
        Ok(Sender { lib, inst })
    }

    /// False when the runtime reports zero receivers; true if it cannot tell.
    pub fn wants_frames(&self) -> bool {
        match self.lib.send_connections {
            Some(f) => unsafe { f(self.inst, 0) != 0 },
            None => true,
        }
    }

    pub fn send(
        &mut self,
        w: u32,
        h: u32,
        stride: u32,
        rgba: &[u8],
        fps: f32,
    ) -> Result<(), String> {
        if stride < w.saturating_mul(4)
            || (rgba.len() as u64)
                < u64::from(stride) * u64::from(h.saturating_sub(1)) + u64::from(w) * 4
        {
            return Err("Frame buffer smaller than its geometry".into());
        }
        let (fr_n, fr_d) = fps_ratio(fps);
        let frame = VideoFrameV2 {
            xres: w as i32,
            yres: h as i32,
            fourcc: FOURCC_RGBA,
            frame_rate_n: fr_n,
            frame_rate_d: fr_d,
            picture_aspect_ratio: w as f32 / h.max(1) as f32,
            frame_format_type: FRAME_PROGRESSIVE,
            timecode: TIMECODE_SYNTHESIZE,
            data: rgba.as_ptr().cast_mut(),
            line_stride: stride as i32,
            metadata: std::ptr::null(),
            timestamp: 0,
        };
        // Synchronous variant: NDI is done with the buffer when this returns.
        unsafe { (self.lib.send_video)(self.inst, &frame) };
        Ok(())
    }
}

impl Drop for Sender {
    fn drop(&mut self) {
        unsafe { (self.lib.send_destroy)(self.inst) }
    }
}

struct Recv {
    lib: Arc<Lib>,
    inst: *mut c_void,
}

impl Recv {
    fn open(lib: &Arc<Lib>, src: &NetSource) -> Option<Recv> {
        let name = CString::new(src.id.as_str()).ok()?;
        let rname = CString::new("VJCraft").ok()?;
        let cfg = RecvCreateV3 {
            source: SourceT {
                name: name.as_ptr(),
                url: std::ptr::null(),
            },
            color_format: RECV_COLOR_RGBX_RGBA,
            bandwidth: RECV_BANDWIDTH_HIGHEST,
            allow_video_fields: false,
            recv_name: rname.as_ptr(),
        };
        let inst = unsafe { (lib.recv_create)(&cfg) };
        (!inst.is_null()).then(|| Recv {
            lib: lib.clone(),
            inst,
        })
    }

    /// Waits up to `ms` for a video frame.
    fn capture(&mut self, ms: u32) -> Option<(u32, u32, Vec<u8>)> {
        let mut f = VideoFrameV2::empty();
        let t = unsafe {
            (self.lib.recv_capture)(
                self.inst,
                &mut f,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                ms,
            )
        };
        if t != FRAME_TYPE_VIDEO {
            return None;
        }
        let out = if f.data.is_null() || f.xres <= 0 || f.yres <= 0 || f.line_stride <= 0 {
            None
        } else {
            let len = f.line_stride as usize * f.yres as usize;
            let src = unsafe { std::slice::from_raw_parts(f.data, len) };
            pack_rows(src, f.xres as u32, f.yres as u32, f.line_stride as u32).map(|mut px| {
                opaque(&mut px);
                (f.xres as u32, f.yres as u32, px)
            })
        };
        unsafe { (self.lib.recv_free_video)(self.inst, &f) };
        out
    }
}

impl Drop for Recv {
    fn drop(&mut self) {
        unsafe { (self.lib.recv_destroy)(self.inst) }
    }
}

pub fn receive(name: &str, sink: &mut dyn FnMut(u32, u32, Vec<u8>) -> bool) -> Result<(), String> {
    let lib = lib()?;
    let mut recv: Option<Recv> = None;
    let mut next_try = Instant::now();
    loop {
        if recv.is_none() {
            if Instant::now() >= next_try {
                next_try = Instant::now() + RETRY * 2;
                // Connect by the stored name even if discovery has not seen it yet.
                let mut alive = || sink(0, 0, Vec::new());
                let found = match list_while(&mut alive) {
                    Ok(Some(l)) => Some(l),
                    Ok(None) => return Ok(()),
                    Err(_) => None,
                };
                let src = found
                    .and_then(|l| resolve(&l, name).cloned())
                    .unwrap_or_else(|| to_source(name));
                recv = Recv::open(&lib, &src);
            } else {
                std::thread::sleep(Duration::from_millis(50));
                if !sink(0, 0, Vec::new()) {
                    return Ok(());
                }
                continue;
            }
        }
        if let Some((w, h, px)) = recv.as_mut().and_then(|r| r.capture(100)) {
            if !sink(w, h, px) {
                return Ok(());
            }
        } else if !sink(0, 0, Vec::new()) {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn fps_ratio_values() {
        use super::fps_ratio;
        assert_eq!(fps_ratio(30.0), (30000, 1000));
        assert_eq!(fps_ratio(29.97), (30000, 1001));
        assert_eq!(fps_ratio(23.976), (24000, 1001));
        assert_eq!(fps_ratio(f32::NAN), (60000, 1000));
    }

    use super::*;

    #[test]
    fn struct_layouts_match_the_c_abi() {
        assert_eq!(std::mem::size_of::<VideoFrameV2>(), 72);
        assert_eq!(std::mem::size_of::<SourceT>(), 16);
        assert_eq!(std::mem::size_of::<SendCreate>(), 24);
        assert_eq!(std::mem::size_of::<FindCreate>(), 24);
        assert_eq!(std::mem::size_of::<RecvCreateV3>(), 40);
        assert_eq!(FOURCC_RGBA, 0x4142_4752);
    }

    #[test]
    fn find_wait_stops_within_a_slice() {
        let t0 = Instant::now();
        let mut finder = |ms: u32| std::thread::sleep(Duration::from_millis(ms as u64));
        let mut alive = || t0.elapsed() < Duration::from_millis(50);
        let done = wait_sliced(FIND_TOTAL_MS, FIND_SLICE_MS, &mut finder, &mut alive);
        assert!(!done);
        assert!(
            t0.elapsed() < Duration::from_millis(300),
            "{:?}",
            t0.elapsed()
        );
    }

    #[test]
    fn env_dirs_come_first() {
        let env = |k: &str| (k == "NDI_RUNTIME_DIR_V6").then(|| "/opt/ndi/".to_string());
        let c = candidates(&env);
        assert_eq!(c[0], "/opt/ndi/libndi.dylib");
        assert!(c.contains(&"/usr/local/lib/libndi.dylib".to_string()));
        assert!(c.iter().any(|p| p.contains("libndi_advanced")));
    }

    #[test]
    fn missing_runtime_is_an_error_not_a_crash() {
        let e = Lib::load_from(&["/nonexistent/libndi.dylib".into()])
            .err()
            .unwrap();
        assert!(e.contains("not found"), "{e}");
    }

    #[test]
    fn splits_ndi_names() {
        let s = to_source("MACBOOK (OBS)");
        assert_eq!((s.app.as_str(), s.name.as_str()), ("MACBOOK", "OBS"));
        let s = to_source("plain");
        assert_eq!((s.app.as_str(), s.name.as_str()), ("", "plain"));
    }
}
