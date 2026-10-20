// FFGL 2 style test plugin that really draws with OpenGL. GL entry points are resolved
// with dlsym. PROCESS_OPENGL reads the input texture with glGetTexImage, inverts RGB on
// the CPU, uploads the result to a scratch texture and blits it into the host FBO.
use std::ffi::{c_char, c_void};

#[repr(C)]
#[derive(Clone, Copy)]
pub union FFMixed {
    u: u32,
    p: *mut c_void,
}
#[repr(C)]
struct Info {
    major: u32,
    minor: u32,
    id: [u8; 4],
    name: [u8; 16],
    ty: u32,
}
#[repr(C)]
struct Tex {
    w: u32,
    h: u32,
    hw: u32,
    hh: u32,
    handle: u32,
}
#[repr(C)]
struct ProcGl {
    n: u32,
    inputs: *mut *mut Tex,
    host_fbo: u32,
}
#[repr(C)]
struct Viewport {
    x: u32,
    y: u32,
    w: u32,
    h: u32,
}

static INFO: Info = Info { major: 2, minor: 0, id: *b"TSTI", name: *b"VJC Test Invert ", ty: 0 };
static TOKEN: u8 = 0;

unsafe extern "C" {
    fn dlsym(h: *mut c_void, name: *const c_char) -> *mut c_void;
}

fn sym(name: &str) -> *mut c_void {
    let c = std::ffi::CString::new(name).unwrap();
    unsafe { dlsym(-2isize as *mut c_void, c.as_ptr()) } // RTLD_DEFAULT
}

macro_rules! gl {
    ($name:literal, fn($($a:ty),*) $(-> $r:ty)?) => {{
        let p = sym($name);
        assert!(!p.is_null(), concat!("missing ", $name));
        std::mem::transmute::<*mut c_void, unsafe extern "C" fn($($a),*) $(-> $r)?>(p)
    }};
}

const TEX2D: u32 = 0x0DE1;
const RGBA: u32 = 0x1908;
const UBYTE: u32 = 0x1401;
const READ_FB: u32 = 0x8CA8;
const DRAW_FB: u32 = 0x8CA9;
const COLOR0: u32 = 0x8CE0;
const NEAREST: i32 = 0x2600;
const MIN_F: u32 = 0x2801;
const MAG_F: u32 = 0x2800;
const COLOR_BIT: u32 = 0x4000;

unsafe fn invert(t: &Tex, host_fbo: u32) {
    unsafe {
        let (w, h) = (t.w as i32, t.h as i32);
        let bind_tex = gl!("glBindTexture", fn(u32, u32));
        let get_img = gl!("glGetTexImage", fn(u32, i32, u32, u32, *mut c_void));
        let tex_img = gl!("glTexImage2D", fn(u32, i32, i32, i32, i32, i32, u32, u32, *const c_void));
        let gen_tex = gl!("glGenTextures", fn(i32, *mut u32));
        let del_tex = gl!("glDeleteTextures", fn(i32, *const u32));
        let gen_fb = gl!("glGenFramebuffers", fn(i32, *mut u32));
        let del_fb = gl!("glDeleteFramebuffers", fn(i32, *const u32));
        let bind_fb = gl!("glBindFramebuffer", fn(u32, u32));
        let fb_tex = gl!("glFramebufferTexture2D", fn(u32, u32, u32, u32, i32));
        let blit = gl!("glBlitFramebuffer", fn(i32, i32, i32, i32, i32, i32, i32, i32, u32, u32));
        let param = gl!("glTexParameteri", fn(u32, u32, i32));
        let pixel_store = gl!("glPixelStorei", fn(u32, i32));
        pixel_store(0x0D05, 1);
        pixel_store(0x0CF5, 1);
        let mut px = vec![0u8; (w * h * 4) as usize];
        bind_tex(TEX2D, t.handle);
        get_img(TEX2D, 0, RGBA, UBYTE, px.as_mut_ptr() as *mut c_void);
        for c in px.chunks_exact_mut(4) {
            c[0] = 255 - c[0];
            c[1] = 255 - c[1];
            c[2] = 255 - c[2];
        }
        let mut scratch = 0u32;
        gen_tex(1, &mut scratch);
        bind_tex(TEX2D, scratch);
        param(TEX2D, MIN_F, NEAREST);
        param(TEX2D, MAG_F, NEAREST);
        tex_img(TEX2D, 0, 0x8058, w, h, 0, RGBA, UBYTE, px.as_ptr() as *const c_void);
        let mut rfb = 0u32;
        gen_fb(1, &mut rfb);
        bind_fb(READ_FB, rfb);
        fb_tex(READ_FB, COLOR0, TEX2D, scratch, 0);
        bind_fb(DRAW_FB, host_fbo);
        blit(0, 0, w, h, 0, 0, w, h, COLOR_BIT, NEAREST as u32);
        bind_fb(READ_FB, host_fbo);
        del_fb(1, &rfb);
        del_tex(1, &scratch);
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn plugMain(code: u32, v: FFMixed, inst: *mut c_void) -> FFMixed {
    let has_inst = inst as *const u8 == &TOKEN as *const u8;
    if matches!(code, 17 | 19 | 20 | 23) && !has_inst {
        return FFMixed { u: 0xFFFF_FFFF };
    }
    unsafe {
        match code {
            0 => FFMixed { p: &INFO as *const Info as *mut c_void },
            34 | 2 | 19 | 20 | 4 => FFMixed { u: 0 },
            18 => {
                let vp = &*(v.p as *const Viewport);
                if vp.w == 0 || vp.h == 0 {
                    return FFMixed { u: 0xFFFF_FFFF };
                }
                FFMixed { p: &TOKEN as *const u8 as *mut c_void }
            }
            23 => FFMixed { u: 0 },
            17 => {
                let p = &*(v.p as *const ProcGl);
                if p.n < 1 {
                    return FFMixed { u: 0xFFFF_FFFF };
                }
                invert(&**p.inputs, p.host_fbo);
                FFMixed { u: 0 }
            }
            _ => FFMixed { u: 0xFFFF_FFFF },
        }
    }
}
