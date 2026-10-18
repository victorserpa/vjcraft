//! CLAP C ABI as documented by the public specification (MIT licensed headers): plain
//! `#[repr(C)]` structs and function pointers. Written from the documented layout only; no
//! header or crate is vendored. Only what an audio effect host needs: entry, factory,
//! descriptor, plugin, parameters extension, audio buffers and parameter-value events.
use std::ffi::{c_char, c_void};

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub revision: u32,
}
pub const VERSION: Version = Version {
    major: 1,
    minor: 2,
    revision: 0,
};

/// Exported by the library as the data symbol `clap_entry`.
#[repr(C)]
pub struct Entry {
    pub version: Version,
    pub init: unsafe extern "C" fn(*const c_char) -> bool,
    pub deinit: unsafe extern "C" fn(),
    pub get_factory: unsafe extern "C" fn(*const c_char) -> *const c_void,
}

pub const FACTORY_ID: &std::ffi::CStr = c"clap.plugin-factory";
pub const PARAMS_EXT_ID: &std::ffi::CStr = c"clap.params";

#[repr(C)]
pub struct Factory {
    pub count: unsafe extern "C" fn(*const Factory) -> u32,
    pub descriptor: unsafe extern "C" fn(*const Factory, u32) -> *const Descriptor,
    pub create: unsafe extern "C" fn(*const Factory, *const Host, *const c_char) -> *const Plugin,
}

#[repr(C)]
pub struct Descriptor {
    pub version: Version,
    pub id: *const c_char,
    pub name: *const c_char,
    pub vendor: *const c_char,
    pub url: *const c_char,
    pub manual_url: *const c_char,
    pub support_url: *const c_char,
    pub plugin_version: *const c_char,
    pub description: *const c_char,
    pub features: *const *const c_char,
}

#[repr(C)]
pub struct Host {
    pub version: Version,
    pub host_data: *mut c_void,
    pub name: *const c_char,
    pub vendor: *const c_char,
    pub url: *const c_char,
    pub host_version: *const c_char,
    pub get_extension: unsafe extern "C" fn(*const Host, *const c_char) -> *const c_void,
    pub request_restart: unsafe extern "C" fn(*const Host),
    pub request_process: unsafe extern "C" fn(*const Host),
    pub request_callback: unsafe extern "C" fn(*const Host),
}
unsafe impl Sync for Host {}

#[repr(C)]
pub struct Plugin {
    pub desc: *const Descriptor,
    pub plugin_data: *mut c_void,
    pub init: unsafe extern "C" fn(*const Plugin) -> bool,
    pub destroy: unsafe extern "C" fn(*const Plugin),
    pub activate: unsafe extern "C" fn(*const Plugin, f64, u32, u32) -> bool,
    pub deactivate: unsafe extern "C" fn(*const Plugin),
    pub start_processing: unsafe extern "C" fn(*const Plugin) -> bool,
    pub stop_processing: unsafe extern "C" fn(*const Plugin),
    pub reset: unsafe extern "C" fn(*const Plugin),
    pub process: unsafe extern "C" fn(*const Plugin, *const Process) -> i32,
    pub get_extension: unsafe extern "C" fn(*const Plugin, *const c_char) -> *const c_void,
    pub on_main_thread: unsafe extern "C" fn(*const Plugin),
}

pub const PROCESS_ERROR: i32 = 0;

#[repr(C)]
pub struct AudioBuffer {
    pub data32: *mut *mut f32,
    pub data64: *mut *mut f64,
    pub channel_count: u32,
    pub latency: u32,
    pub constant_mask: u64,
}

#[repr(C)]
pub struct EventHeader {
    pub size: u32,
    pub time: u32,
    pub space_id: u16,
    pub kind: u16,
    pub flags: u32,
}
pub const CORE_EVENT_SPACE: u16 = 0;
pub const EVENT_PARAM_VALUE: u16 = 5;

#[repr(C)]
pub struct ParamValueEvent {
    pub header: EventHeader,
    pub param_id: u32,
    pub cookie: *mut c_void,
    pub note_id: i32,
    pub port_index: i16,
    pub channel: i16,
    pub key: i16,
    pub value: f64,
}

#[repr(C)]
pub struct InputEvents {
    pub ctx: *mut c_void,
    pub size: unsafe extern "C" fn(*const InputEvents) -> u32,
    pub get: unsafe extern "C" fn(*const InputEvents, u32) -> *const EventHeader,
}

#[repr(C)]
pub struct OutputEvents {
    pub ctx: *mut c_void,
    pub try_push: unsafe extern "C" fn(*const OutputEvents, *const EventHeader) -> bool,
}

#[repr(C)]
pub struct Process {
    pub steady_time: i64,
    pub frames_count: u32,
    pub transport: *const c_void,
    pub audio_inputs: *const AudioBuffer,
    pub audio_outputs: *mut AudioBuffer,
    pub audio_inputs_count: u32,
    pub audio_outputs_count: u32,
    pub in_events: *const InputEvents,
    pub out_events: *const OutputEvents,
}

#[repr(C)]
pub struct ParamsExt {
    pub count: unsafe extern "C" fn(*const Plugin) -> u32,
    pub get_info: unsafe extern "C" fn(*const Plugin, u32, *mut ParamInfoRaw) -> bool,
    pub get_value: unsafe extern "C" fn(*const Plugin, u32, *mut f64) -> bool,
    pub value_to_text: unsafe extern "C" fn(*const Plugin, u32, f64, *mut c_char, u32) -> bool,
    pub text_to_value: unsafe extern "C" fn(*const Plugin, u32, *const c_char, *mut f64) -> bool,
    pub flush: unsafe extern "C" fn(*const Plugin, *const InputEvents, *const OutputEvents),
}

#[repr(C)]
pub struct ParamInfoRaw {
    pub id: u32,
    pub flags: u32,
    pub cookie: *mut c_void,
    pub name: [u8; 256],
    pub module: [u8; 1024],
    pub min_value: f64,
    pub max_value: f64,
    pub default_value: f64,
}
