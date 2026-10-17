//! FFGL 2.x C ABI as documented by the public specification: one exported entry point,
//! `plugMain(functionCode, inputValue, instanceID)`, and a handful of plain structs.
//! Written from the documented layout only; no SDK header or code is used or vendored.
//! Constants are from the documented function-code table (not machine-checked against the SDK).
use std::ffi::c_void;

/// Documented `FFMixed`: a 32-bit unsigned or a pointer.
#[repr(C)]
#[derive(Clone, Copy)]
pub union FFMixed {
    pub u: u32,
    pub p: *mut c_void,
}

pub type PlugMain = unsafe extern "C" fn(u32, FFMixed, *mut c_void) -> FFMixed;

pub const FF_FAIL: u32 = 0xFFFF_FFFF;
pub const FF_GETINFO: u32 = 0;
pub const FF_INITIALISE: u32 = 1;
pub const FF_DEINITIALISE: u32 = 2;
pub const FF_GETNUMPARAMETERS: u32 = 4;
pub const FF_GETPARAMETERNAME: u32 = 5;
pub const FF_GETPARAMETERDEFAULT: u32 = 6;
pub const FF_SETPARAMETER: u32 = 8;
pub const FF_GETPARAMETER: u32 = 9;
pub const FF_GETPARAMETERTYPE: u32 = 15;
/// VJCraft stage-1 extension (not part of FFGL): process an RGBA8 frame on the CPU.
/// Lets the helper be tested without an OpenGL context; real plugins answer FF_FAIL.
pub const FF_PROCESS_CPU_EXT: u32 = 0x7F00;

pub const FF_EFFECT: u32 = 0;
pub const FF_SOURCE: u32 = 1;
pub const FF_TYPE_BOOLEAN: u32 = 0;
pub const FF_TYPE_STANDARD: u32 = 10;

#[repr(C)]
pub struct PluginInfoStruct {
    pub api_major: u32,
    pub api_minor: u32,
    pub plugin_unique_id: [u8; 4],
    pub plugin_name: [u8; 16],
    pub plugin_type: u32,
}

#[repr(C)]
pub struct SetParameterStruct {
    pub index: u32,
    pub value: FFMixed,
}

#[repr(C)]
pub struct CpuFrameStruct {
    pub input: *const u8,
    pub output: *mut u8,
    pub width: u32,
    pub height: u32,
}
