// Minimal CLAP effect written from the documented ABI (no header, no crate). Built by the
// tests with rustc --crate-type cdylib. Params (CLAP ids 0..3):
// 0 Gain (0..4, default 1.0: output = input * gain), 1 Crash (>0.5: abort while processing),
// 2 Hang (>0.5: sleep 30 s while processing).
use std::ffi::{c_char, c_void};

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Version {
    major: u32,
    minor: u32,
    revision: u32,
}
const V: Version = Version { major: 1, minor: 2, revision: 0 };

#[repr(C)]
pub struct Entry {
    version: Version,
    init: unsafe extern "C" fn(*const c_char) -> bool,
    deinit: unsafe extern "C" fn(),
    get_factory: unsafe extern "C" fn(*const c_char) -> *const c_void,
}
unsafe impl Sync for Entry {}

#[repr(C)]
struct Factory {
    count: unsafe extern "C" fn(*const Factory) -> u32,
    desc: unsafe extern "C" fn(*const Factory, u32) -> *const Desc,
    create: unsafe extern "C" fn(*const Factory, *const c_void, *const c_char) -> *const Plugin,
}
unsafe impl Sync for Factory {}

#[repr(C)]
struct Desc {
    version: Version,
    id: *const c_char,
    name: *const c_char,
    vendor: *const c_char,
    url: *const c_char,
    manual: *const c_char,
    support: *const c_char,
    plugin_version: *const c_char,
    description: *const c_char,
    features: *const *const c_char,
}
unsafe impl Sync for Desc {}

#[repr(C)]
struct Plugin {
    desc: *const Desc,
    data: *mut c_void,
    init: unsafe extern "C" fn(*const Plugin) -> bool,
    destroy: unsafe extern "C" fn(*const Plugin),
    activate: unsafe extern "C" fn(*const Plugin, f64, u32, u32) -> bool,
    deactivate: unsafe extern "C" fn(*const Plugin),
    start: unsafe extern "C" fn(*const Plugin) -> bool,
    stop: unsafe extern "C" fn(*const Plugin),
    reset: unsafe extern "C" fn(*const Plugin),
    process: unsafe extern "C" fn(*const Plugin, *const Process) -> i32,
    ext: unsafe extern "C" fn(*const Plugin, *const c_char) -> *const c_void,
    main_thread: unsafe extern "C" fn(*const Plugin),
}

#[repr(C)]
struct AudioBuf {
    d32: *mut *mut f32,
    d64: *mut *mut f64,
    channels: u32,
    latency: u32,
    mask: u64,
}
#[repr(C)]
struct Header {
    size: u32,
    time: u32,
    space: u16,
    ty: u16,
    flags: u32,
}
#[repr(C)]
struct ParamValue {
    h: Header,
    id: u32,
    cookie: *mut c_void,
    note_id: i32,
    port: i16,
    channel: i16,
    key: i16,
    value: f64,
}
#[repr(C)]
struct InEvents {
    ctx: *mut c_void,
    size: unsafe extern "C" fn(*const InEvents) -> u32,
    get: unsafe extern "C" fn(*const InEvents, u32) -> *const Header,
}
#[repr(C)]
struct Process {
    steady: i64,
    frames: u32,
    transport: *const c_void,
    inputs: *const AudioBuf,
    outputs: *mut AudioBuf,
    n_in: u32,
    n_out: u32,
    in_events: *const InEvents,
    out_events: *const c_void,
}

#[repr(C)]
struct ParamsExt {
    count: unsafe extern "C" fn(*const Plugin) -> u32,
    info: unsafe extern "C" fn(*const Plugin, u32, *mut ParamInfo) -> bool,
    get: unsafe extern "C" fn(*const Plugin, u32, *mut f64) -> bool,
    to_text: unsafe extern "C" fn(*const Plugin, u32, f64, *mut c_char, u32) -> bool,
    from_text: unsafe extern "C" fn(*const Plugin, u32, *const c_char, *mut f64) -> bool,
    flush: unsafe extern "C" fn(*const Plugin, *const InEvents, *const c_void),
}
unsafe impl Sync for ParamsExt {}
#[repr(C)]
struct ParamInfo {
    id: u32,
    flags: u32,
    cookie: *mut c_void,
    name: [u8; 256],
    module: [u8; 1024],
    min: f64,
    max: f64,
    default: f64,
}

const NAMES: [&str; 3] = ["Gain", "Crash", "Hang"];
const MAXS: [f64; 3] = [4.0, 1.0, 1.0];
const DEFAULTS: [f64; 3] = [1.0, 0.0, 0.0];

struct State {
    vals: [f64; 3],
}

static BROKEN: Desc = Desc {
    version: V,
    id: c"org.vjcraft.test.broken".as_ptr(),
    name: c"VJC Test Broken".as_ptr(),
    vendor: c"test".as_ptr(),
    url: c"".as_ptr(),
    manual: c"".as_ptr(),
    support: c"".as_ptr(),
    plugin_version: c"1".as_ptr(),
    description: c"".as_ptr(),
    features: NO_FEATURES.as_ptr() as *const *const c_char,
};

static NO_FEATURES: [usize; 1] = [0];

static DESC: Desc = Desc {
    version: V,
    id: c"org.vjcraft.test.gain".as_ptr(),
    name: c"VJC Test Gain".as_ptr(),
    vendor: c"test".as_ptr(),
    url: c"".as_ptr(),
    manual: c"".as_ptr(),
    support: c"".as_ptr(),
    plugin_version: c"1".as_ptr(),
    description: c"".as_ptr(),
    features: NO_FEATURES.as_ptr() as *const *const c_char,
};

unsafe extern "C" fn e_init(_: *const c_char) -> bool {
    true
}
unsafe extern "C" fn e_deinit() {}
unsafe extern "C" fn e_factory(id: *const c_char) -> *const c_void {
    if unsafe { std::ffi::CStr::from_ptr(id) } == c"clap.plugin-factory" {
        &FACTORY as *const Factory as *const c_void
    } else {
        std::ptr::null()
    }
}
// --cfg slow: listing the plugins takes 2 s. --cfg two: a second descriptor first in the
// list whose create fails.
unsafe extern "C" fn f_count(_: *const Factory) -> u32 {
    if cfg!(slow) {
        std::thread::sleep(std::time::Duration::from_secs(2));
    }
    if cfg!(two) { 2 } else { 1 }
}
unsafe extern "C" fn f_desc(_: *const Factory, i: u32) -> *const Desc {
    match (cfg!(two), i) {
        (true, 0) => &BROKEN,
        (true, 1) | (false, 0) => &DESC,
        _ => std::ptr::null(),
    }
}
unsafe extern "C" fn f_create(_: *const Factory, _: *const c_void, id: *const c_char) -> *const Plugin {
    if unsafe { std::ffi::CStr::from_ptr(id) } != c"org.vjcraft.test.gain" {
        return std::ptr::null();
    }
    let st = Box::into_raw(Box::new(State { vals: DEFAULTS }));
    Box::into_raw(Box::new(Plugin {
        desc: &DESC,
        data: st as *mut c_void,
        init: p_init,
        destroy: p_destroy,
        activate: p_activate,
        deactivate: p_noop,
        start: p_start,
        stop: p_noop,
        reset: p_noop,
        process: p_process,
        ext: p_ext,
        main_thread: p_noop,
    }))
}
static FACTORY: Factory = Factory { count: f_count, desc: f_desc, create: f_create };

#[unsafe(no_mangle)]
pub static clap_entry: Entry = Entry { version: V, init: e_init, deinit: e_deinit, get_factory: e_factory };

unsafe extern "C" fn p_init(_: *const Plugin) -> bool {
    true
}
unsafe extern "C" fn p_destroy(p: *const Plugin) {
    unsafe {
        let b = Box::from_raw(p as *mut Plugin);
        drop(Box::from_raw(b.data as *mut State));
    }
}
// --cfg slowload: activating takes 6 s (longer than a control call may take).
unsafe extern "C" fn p_activate(_: *const Plugin, _: f64, _: u32, _: u32) -> bool {
    if cfg!(slowload) {
        std::thread::sleep(std::time::Duration::from_secs(6));
    }
    true
}
unsafe extern "C" fn p_start(_: *const Plugin) -> bool {
    true
}
unsafe extern "C" fn p_noop(_: *const Plugin) {}

fn apply(st: &mut State, ev: *const InEvents) {
    unsafe {
        if ev.is_null() {
            return;
        }
        for i in 0..((*ev).size)(ev) {
            let h = ((*ev).get)(ev, i);
            if (*h).space == 0 && (*h).ty == 5 {
                let pv = &*(h as *const ParamValue);
                if (pv.id as usize) < 3 {
                    st.vals[pv.id as usize] = pv.value;
                }
            }
        }
    }
}

unsafe extern "C" fn p_process(p: *const Plugin, pr: *const Process) -> i32 {
    unsafe {
        let st = &mut *((*p).data as *mut State);
        let pr = &*pr;
        apply(st, pr.in_events);
        if st.vals[1] > 0.5 {
            std::process::abort();
        }
        if st.vals[2] > 0.5 {
            std::thread::sleep(std::time::Duration::from_secs(30));
        }
        let (i, o) = (&*pr.inputs, &*pr.outputs);
        for c in 0..(i.channels.min(o.channels) as usize) {
            let src = *i.d32.add(c);
            let dst = *o.d32.add(c);
            for k in 0..pr.frames as usize {
                *dst.add(k) = *src.add(k) * st.vals[0] as f32;
            }
        }
        1
    }
}

unsafe extern "C" fn x_count(_: *const Plugin) -> u32 {
    3
}
unsafe extern "C" fn x_info(_: *const Plugin, i: u32, out: *mut ParamInfo) -> bool {
    if i >= 3 {
        return false;
    }
    unsafe {
        let o = &mut *out;
        o.id = i;
        o.flags = 0;
        o.cookie = std::ptr::null_mut();
        o.name = [0; 256];
        o.module = [0; 1024];
        o.name[..NAMES[i as usize].len()].copy_from_slice(NAMES[i as usize].as_bytes());
        o.min = 0.0;
        o.max = MAXS[i as usize];
        o.default = DEFAULTS[i as usize];
    }
    true
}
unsafe extern "C" fn x_get(p: *const Plugin, id: u32, v: *mut f64) -> bool {
    if id >= 3 {
        return false;
    }
    unsafe { *v = (*((*p).data as *mut State)).vals[id as usize] };
    true
}
unsafe extern "C" fn x_to_text(_: *const Plugin, _: u32, _: f64, _: *mut c_char, _: u32) -> bool {
    false
}
unsafe extern "C" fn x_from_text(_: *const Plugin, _: u32, _: *const c_char, _: *mut f64) -> bool {
    false
}
unsafe extern "C" fn x_flush(p: *const Plugin, ev: *const InEvents, _: *const c_void) {
    unsafe { apply(&mut *((*p).data as *mut State), ev) }
}
static PARAMS: ParamsExt = ParamsExt {
    count: x_count,
    info: x_info,
    get: x_get,
    to_text: x_to_text,
    from_text: x_from_text,
    flush: x_flush,
};
// clap.audio-ports: one input and one output port; stereo, or mono when built with --cfg mono.
#[repr(C)]
struct PortInfo {
    id: u32,
    name: [u8; 256],
    flags: u32,
    channel_count: u32,
    port_type: *const c_char,
    in_place_pair: u32,
}
#[repr(C)]
struct PortsExt {
    count: unsafe extern "C" fn(*const Plugin, bool) -> u32,
    get: unsafe extern "C" fn(*const Plugin, u32, bool, *mut PortInfo) -> bool,
}
unsafe extern "C" fn pt_count(_: *const Plugin, _: bool) -> u32 {
    1
}
unsafe extern "C" fn pt_get(_: *const Plugin, i: u32, _: bool, out: *mut PortInfo) -> bool {
    if i != 0 {
        return false;
    }
    unsafe {
        (*out).id = 0;
        (*out).name = [0; 256];
        (*out).flags = 1;
        (*out).channel_count = if cfg!(mono) { 1 } else { 2 };
        (*out).port_type = if cfg!(mono) { c"mono".as_ptr() } else { c"stereo".as_ptr() };
        (*out).in_place_pair = u32::MAX;
    }
    true
}
static PORTS: PortsExt = PortsExt { count: pt_count, get: pt_get };
unsafe extern "C" fn p_ext(_: *const Plugin, id: *const c_char) -> *const c_void {
    let id = unsafe { std::ffi::CStr::from_ptr(id) };
    if id == c"clap.audio-ports" {
        &PORTS as *const PortsExt as *const c_void
    } else if id == c"clap.params" {
        &PARAMS as *const ParamsExt as *const c_void
    } else {
        std::ptr::null()
    }
}
