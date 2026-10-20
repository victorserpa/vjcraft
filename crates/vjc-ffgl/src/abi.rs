//! FFGL 2.x C ABI as documented by the public specification: one exported entry point,
//! `plugMain(functionCode, inputValue, instanceID)`, and a handful of plain structs.
//! Written from the documented layout only; no SDK header or code is used or vendored.
//! Function codes were checked against the open SDK headers (see docs/specs/ffgl-host.md).
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
pub const FF_DEINITIALISE: u32 = 2;
pub const FF_GETNUMPARAMETERS: u32 = 4;
pub const FF_GETPARAMETERNAME: u32 = 5;
pub const FF_GETPARAMETERDEFAULT: u32 = 6;
pub const FF_SETPARAMETER: u32 = 8;
pub const FF_GETPARAMETER: u32 = 9;
pub const FF_GETPARAMETERTYPE: u32 = 15;
pub const FF_PROCESS_OPENGL: u32 = 17;
pub const FF_INSTANTIATE_GL: u32 = 18;
pub const FF_DEINSTANTIATE_GL: u32 = 19;
pub const FF_SET_TIME: u32 = 20;
pub const FF_RESIZE: u32 = 23;
pub const FF_INITIALISE_V2: u32 = 34;
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

/// Documented `FFGLTextureStruct`.
#[repr(C)]
pub struct TextureStruct {
    pub width: u32,
    pub height: u32,
    pub hardware_width: u32,
    pub hardware_height: u32,
    pub handle: u32,
}

/// Documented `ProcessOpenGLStruct`.
#[repr(C)]
pub struct ProcessOpenGlStruct {
    pub num_input_textures: u32,
    pub input_textures: *mut *mut TextureStruct,
    pub host_fbo: u32,
}

/// Host info handed to INITIALISE_V2 (name, version). Layout not machine-checked.
#[repr(C)]
pub struct HostInfoStruct {
    pub name: *const std::ffi::c_char,
    pub major: u32,
    pub minor: u32,
}

/// Documented viewport passed to INSTANTIATE_GL and RESIZE.
#[repr(C)]
pub struct ViewportStruct {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}
