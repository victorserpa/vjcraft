//! Helper process: the only place plugin code runs. A crash or hang here never reaches the app.
use libloading::{Library, Symbol};
use memmap2::MmapMut;
use std::collections::BTreeMap;
use std::ffi::{CStr, CString, c_char, c_void};
use std::io::{BufRead, Write};
use vjc_clap::abi::*;
use vjc_clap::proto::{self, *};

unsafe extern "C" fn host_ext(_: *const Host, _: *const c_char) -> *const c_void {
    std::ptr::null()
}
unsafe extern "C" fn host_noop(_: *const Host) {}

static HOST: Host = Host {
    version: VERSION,
    host_data: std::ptr::null_mut(),
    name: c"VJCraft".as_ptr(),
    vendor: c"VJCraft".as_ptr(),
    url: c"".as_ptr(),
    host_version: c"0".as_ptr(),
    get_extension: host_ext,
    request_restart: host_noop,
    request_process: host_noop,
    request_callback: host_noop,
};

/// A loaded library: entry initialised, factory looked up. Deinit on drop.
struct Lib {
    entry: *const Entry,
    factory: *const Factory,
    _lib: Library,
}

impl Lib {
    fn open(path: &str) -> Result<Lib, String> {
        unsafe {
            let bin = vjc_clap::resolve_binary(std::path::Path::new(path));
            let lib = Library::new(&bin).map_err(|e| e.to_string())?;
            let sym: Symbol<*const Entry> = lib.get(b"clap_entry\0").map_err(|e| e.to_string())?;
            let entry: *const Entry = *sym;
            if entry.is_null() {
                return Err("no clap_entry".into());
            }
            let cpath =
                CString::new(bin.to_string_lossy().as_bytes()).map_err(|e| e.to_string())?;
            if !((*entry).init)(cpath.as_ptr()) {
                return Err("entry init failed".into());
            }
            let factory = ((*entry).get_factory)(FACTORY_ID.as_ptr()) as *const Factory;
            if factory.is_null() {
                ((*entry).deinit)();
                return Err("no plugin factory".into());
            }
            Ok(Lib {
                entry,
                factory,
                _lib: lib,
            })
        }
    }
}

impl Drop for Lib {
    fn drop(&mut self) {
        unsafe { ((*self.entry).deinit)() }
    }
}

fn text(p: *const c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

fn fixed(b: &[u8]) -> String {
    let n = b.iter().position(|c| *c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..n]).into_owned()
}

/// A created plugin. Field order matters: the plugin goes before the library.
struct Instance {
    plugin: *const Plugin,
    params: *const ParamsExt,
    processing: bool,
    active: bool,
    pending: BTreeMap<u32, f64>,
    infos: Vec<proto::ParamInfo>,
    ins: Vec<Vec<f32>>,
    outs: Vec<Vec<f32>>,
    steady: i64,
    _lib: Lib,
}

impl Drop for Instance {
    fn drop(&mut self) {
        unsafe {
            if self.processing {
                ((*self.plugin).stop_processing)(self.plugin);
            }
            if self.active {
                ((*self.plugin).deactivate)(self.plugin);
            }
            ((*self.plugin).destroy)(self.plugin);
        }
    }
}

fn param_infos(plugin: *const Plugin, params: *const ParamsExt) -> Vec<proto::ParamInfo> {
    let mut v = Vec::new();
    if params.is_null() {
        return v;
    }
    unsafe {
        let n = ((*params).count)(plugin);
        for i in 0..n.min(1024) {
            let mut raw: ParamInfoRaw = std::mem::zeroed();
            if ((*params).get_info)(plugin, i, &mut raw) {
                v.push(proto::ParamInfo {
                    id: raw.id,
                    name: fixed(&raw.name),
                    min: raw.min_value,
                    max: raw.max_value,
                    default: raw.default_value,
                });
            }
        }
    }
    v
}

/// Create plugin `id` (init done). Returns the raw plugin and its params extension.
fn create(lib: &Lib, id: &str) -> Result<(*const Plugin, *const ParamsExt), String> {
    let cid = CString::new(id).map_err(|e| e.to_string())?;
    unsafe {
        let p = ((*lib.factory).create)(lib.factory, &HOST, cid.as_ptr());
        if p.is_null() {
            return Err(format!("no plugin with id {id}"));
        }
        if !((*p).init)(p) {
            ((*p).destroy)(p);
            return Err("plugin init failed".into());
        }
        let ext = ((*p).get_extension)(p, PARAMS_EXT_ID.as_ptr()) as *const ParamsExt;
        Ok((p, ext))
    }
}

/// True when the plugin has a stereo input port and a stereo output port (the layout the host
/// feeds). A plugin without the audio-ports extension has no ports to feed.
fn has_stereo_ports(p: *const Plugin) -> bool {
    unsafe {
        let ext = ((*p).get_extension)(p, AUDIO_PORTS_EXT_ID.as_ptr()) as *const AudioPortsExt;
        if ext.is_null() {
            return false;
        }
        [true, false].into_iter().all(|input| {
            (0..((*ext).count)(p, input).min(64)).any(|i| {
                let mut info: AudioPortInfo = std::mem::zeroed();
                ((*ext).get)(p, i, input, &mut info) && info.channel_count == 2
            })
        })
    }
}

fn info(path: &str) -> Result<Vec<PluginInfo>, String> {
    let lib = Lib::open(path)?;
    let mut out = Vec::new();
    unsafe {
        for i in 0..((*lib.factory).count)(lib.factory).min(256) {
            let d = ((*lib.factory).descriptor)(lib.factory, i);
            if d.is_null() {
                continue;
            }
            let (id, name) = (text((*d).id), text((*d).name));
            let (p, ext) = create(&lib, &id)?;
            let params = param_infos(p, ext);
            let stereo = has_stereo_ports(p);
            ((*p).destroy)(p);
            out.push(PluginInfo {
                id,
                name,
                params,
                stereo,
            });
        }
    }
    Ok(out)
}

fn load(path: &str, id: &str, rate: u32, max_frames: u32) -> Result<Instance, String> {
    let lib = Lib::open(path)?;
    let (plugin, params) = create(&lib, id)?;
    let infos = param_infos(plugin, params);
    let mut inst = Instance {
        plugin,
        params,
        processing: false,
        active: false,
        pending: BTreeMap::new(),
        infos,
        ins: vec![vec![0.0; max_frames as usize]; 2],
        outs: vec![vec![0.0; max_frames as usize]; 2],
        steady: 0,
        _lib: lib,
    };
    if !has_stereo_ports(plugin) {
        return Err("plugin has no stereo audio port (mono or surround only); it cannot be used here".into());
    }
    unsafe {
        if !((*plugin).activate)(plugin, rate as f64, 1, max_frames) {
            return Err("activate failed".into());
        }
        inst.active = true;
        if !((*plugin).start_processing)(plugin) {
            return Err("start_processing failed".into());
        }
        inst.processing = true;
    }
    Ok(inst)
}

unsafe extern "C" fn ev_size(l: *const InputEvents) -> u32 {
    unsafe { (*((*l).ctx as *const Vec<ParamValueEvent>)).len() as u32 }
}
unsafe extern "C" fn ev_get(l: *const InputEvents, i: u32) -> *const EventHeader {
    unsafe {
        let v = &*((*l).ctx as *const Vec<ParamValueEvent>);
        match v.get(i as usize) {
            Some(e) => &e.header,
            None => std::ptr::null(),
        }
    }
}
unsafe extern "C" fn out_push(_: *const OutputEvents, _: *const EventHeader) -> bool {
    true
}

impl Instance {
    fn process(&mut self, shm: &str, frames: u32) -> Result<(), String> {
        let n = frames as usize;
        if n == 0 || n > self.ins[0].len() {
            return Err("bad frame count".into());
        }
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(shm)
            .map_err(|e| e.to_string())?;
        let mut map = unsafe { MmapMut::map_mut(&file) }.map_err(|e| e.to_string())?;
        if map.len() < n * 16 {
            return Err("mapping too small".into());
        }
        for i in 0..n {
            for c in 0..2 {
                let o = (i * 2 + c) * 4;
                self.ins[c][i] = f32::from_le_bytes(map[o..o + 4].try_into().unwrap());
            }
        }
        let events: Vec<ParamValueEvent> = std::mem::take(&mut self.pending)
            .into_iter()
            .map(|(param_id, value)| ParamValueEvent {
                header: EventHeader {
                    size: std::mem::size_of::<ParamValueEvent>() as u32,
                    time: 0,
                    space_id: CORE_EVENT_SPACE,
                    kind: EVENT_PARAM_VALUE,
                    flags: 0,
                },
                param_id,
                cookie: std::ptr::null_mut(),
                note_id: -1,
                port_index: -1,
                channel: -1,
                key: -1,
                value,
            })
            .collect();
        let in_events = InputEvents {
            ctx: &events as *const _ as *mut c_void,
            size: ev_size,
            get: ev_get,
        };
        let out_events = OutputEvents {
            ctx: std::ptr::null_mut(),
            try_push: out_push,
        };
        let mut in_ptrs = [self.ins[0].as_mut_ptr(), self.ins[1].as_mut_ptr()];
        let mut out_ptrs = [self.outs[0].as_mut_ptr(), self.outs[1].as_mut_ptr()];
        let input = AudioBuffer {
            data32: in_ptrs.as_mut_ptr(),
            data64: std::ptr::null_mut(),
            channel_count: 2,
            latency: 0,
            constant_mask: 0,
        };
        let mut output = AudioBuffer {
            data32: out_ptrs.as_mut_ptr(),
            data64: std::ptr::null_mut(),
            channel_count: 2,
            latency: 0,
            constant_mask: 0,
        };
        let pr = Process {
            steady_time: self.steady,
            frames_count: frames,
            transport: std::ptr::null(),
            audio_inputs: &input,
            audio_outputs: &mut output,
            audio_inputs_count: 1,
            audio_outputs_count: 1,
            in_events: &in_events,
            out_events: &out_events,
        };
        self.steady += n as i64;
        let st = unsafe { ((*self.plugin).process)(self.plugin, &pr) };
        if st == PROCESS_ERROR {
            return Err("plugin reported a processing error".into());
        }
        let base = n * 8;
        for i in 0..n {
            for c in 0..2 {
                let o = base + (i * 2 + c) * 4;
                map[o..o + 4].copy_from_slice(&self.outs[c][i].to_le_bytes());
            }
        }
        Ok(())
    }
}

fn err(msg: impl Into<String>) -> Response {
    Response::Err { msg: msg.into() }
}

fn handle(req: Request, cur: &mut Option<Instance>) -> Response {
    match req {
        Request::Info { path } => match info(&path) {
            Ok(plugins) => Response::Info { plugins },
            Err(m) => err(m),
        },
        Request::Load {
            path,
            id,
            rate,
            max_frames,
        } => {
            *cur = None;
            match load(&path, &id, rate, max_frames.clamp(1, 1 << 16)) {
                Ok(i) => {
                    let params = i.infos.clone();
                    *cur = Some(i);
                    Response::Loaded { params }
                }
                Err(m) => err(m),
            }
        }
        Request::Set { index, value } => match cur {
            Some(i) => match i.infos.get(index as usize) {
                Some(p) => {
                    i.pending.insert(p.id, value);
                    Response::Done
                }
                None => err("parameter index out of range"),
            },
            None => err("no plugin loaded"),
        },
        Request::Get { index } => match cur {
            Some(i) => {
                let Some(p) = i.infos.get(index as usize) else {
                    return err("parameter index out of range");
                };
                if let Some(v) = i.pending.get(&p.id) {
                    return Response::Value { value: *v };
                }
                let mut v = 0.0f64;
                let ok = !i.params.is_null()
                    && unsafe { ((*i.params).get_value)(i.plugin, p.id, &mut v) };
                if ok {
                    Response::Value { value: v }
                } else {
                    err("get_value failed")
                }
            }
            None => err("no plugin loaded"),
        },
        Request::Process { shm, frames } => match cur {
            Some(i) => match i.process(&shm, frames) {
                Ok(()) => Response::Done,
                Err(m) => err(m),
            },
            None => err("no plugin loaded"),
        },
        Request::Quit => {
            *cur = None;
            std::process::exit(0)
        }
    }
}

fn main() {
    let mut cur: Option<Instance> = None;
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        let resp = match serde_json::from_str::<Request>(&line) {
            Ok(r) => handle(r, &mut cur),
            Err(e) => err(e.to_string()),
        };
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "{}", serde_json::to_string(&resp).unwrap());
        let _ = out.flush();
    }
}
