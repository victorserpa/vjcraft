//! OpenGL context for the helper (stage 2a). macOS: CGL, an offscreen 4.1 core context, symbols
//! loaded from OpenGL.framework with dlopen/dlsym. Elsewhere `GlHost::new` reports "no OpenGL context".
//! Written from the documented CGL/GL APIs; no SDK code.

pub struct GlHost {
    #[cfg(target_os = "macos")]
    inner: mac::Inner,
}

impl GlHost {
    /// Create the context and make it current on this thread.
    pub fn new() -> Result<GlHost, String> {
        #[cfg(target_os = "macos")]
        {
            Ok(GlHost {
                inner: mac::Inner::new()?,
            })
        }
        #[cfg(not(target_os = "macos"))]
        {
            Err("no OpenGL context (unsupported platform)".into())
        }
    }

    /// Run one frame through `draw`, which receives (input texture handle, FBO). The input RGBA8
    /// frame is uploaded first; the FBO (RGBA8 colour texture, w x h) is bound and the result is
    /// read back into `output`.
    pub fn frame(
        &mut self,
        w: u32,
        h: u32,
        input: &[u8],
        output: &mut [u8],
        draw: impl FnOnce(u32, u32),
    ) -> Result<(), String> {
        #[cfg(target_os = "macos")]
        {
            self.inner.frame(w, h, input, output, draw)
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (w, h, input, output, draw);
            Err("no OpenGL context".into())
        }
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use std::ffi::{CString, c_char, c_void};

    unsafe extern "C" {
        fn dlopen(path: *const c_char, mode: i32) -> *mut c_void;
        fn dlsym(h: *mut c_void, name: *const c_char) -> *mut c_void;
    }

    const FRAMEWORK: &str = "/System/Library/Frameworks/OpenGL.framework/OpenGL";
    const RTLD_NOW: i32 = 2;
    const RTLD_GLOBAL: i32 = 8;

    const PFA_ACCELERATED: i32 = 73;
    const PFA_ALLOW_OFFLINE: i32 = 96;
    const PFA_PROFILE: i32 = 99;
    const PROFILE_3_2_CORE: i32 = 0x3200; // gives a 4.1 core context on macOS

    const TEX2D: u32 = 0x0DE1;
    const RGBA: u32 = 0x1908;
    const RGBA8: i32 = 0x8058;
    const UBYTE: u32 = 0x1401;
    const FB: u32 = 0x8D40;
    const COLOR0: u32 = 0x8CE0;
    const COMPLETE: u32 = 0x8CD5;
    const NEAREST: i32 = 0x2600;
    const MIN_F: u32 = 0x2801;
    const MAG_F: u32 = 0x2800;
    const PACK_ALIGN: u32 = 0x0D05;
    const UNPACK_ALIGN: u32 = 0x0CF5;

    type Fn1<T> = unsafe extern "C" fn(i32, *mut T);

    struct Gl {
        gen_textures: Fn1<u32>,
        delete_textures: unsafe extern "C" fn(i32, *const u32),
        bind_texture: unsafe extern "C" fn(u32, u32),
        tex_image: unsafe extern "C" fn(u32, i32, i32, i32, i32, i32, u32, u32, *const c_void),
        tex_sub_image: unsafe extern "C" fn(u32, i32, i32, i32, i32, i32, u32, u32, *const c_void),
        tex_parameter: unsafe extern "C" fn(u32, u32, i32),
        gen_fbos: Fn1<u32>,
        delete_fbos: unsafe extern "C" fn(i32, *const u32),
        bind_fbo: unsafe extern "C" fn(u32, u32),
        fbo_texture: unsafe extern "C" fn(u32, u32, u32, u32, i32),
        check_fbo: unsafe extern "C" fn(u32) -> u32,
        read_pixels: unsafe extern "C" fn(i32, i32, i32, i32, u32, u32, *mut c_void),
        viewport: unsafe extern "C" fn(i32, i32, i32, i32),
        pixel_store: unsafe extern "C" fn(u32, i32),
        finish: unsafe extern "C" fn(),
    }

    type ChoosePf = unsafe extern "C" fn(*const i32, *mut *mut c_void, *mut i32) -> i32;
    type CreateCtx = unsafe extern "C" fn(*mut c_void, *mut c_void, *mut *mut c_void) -> i32;
    type SetCurrent = unsafe extern "C" fn(*mut c_void) -> i32;
    type Destroy = unsafe extern "C" fn(*mut c_void);

    pub struct Inner {
        gl: Gl,
        ctx: *mut c_void,
        destroy_ctx: Destroy,
        fbo: u32,
        tex_out: u32,
        tex_in: u32,
        size: (u32, u32),
    }

    unsafe fn load<T: Copy>(lib: *mut c_void, name: &str) -> Result<T, String> {
        let c = CString::new(name).unwrap();
        let p = unsafe { dlsym(lib, c.as_ptr()) };
        if p.is_null() {
            return Err(format!("no OpenGL context (missing symbol {name})"));
        }
        Ok(unsafe { std::mem::transmute_copy::<*mut c_void, T>(&p) })
    }

    impl Inner {
        pub fn new() -> Result<Inner, String> {
            unsafe {
                let path = CString::new(FRAMEWORK).unwrap();
                let lib = dlopen(path.as_ptr(), RTLD_NOW | RTLD_GLOBAL);
                if lib.is_null() {
                    return Err("no OpenGL context (OpenGL.framework not loadable)".into());
                }
                let choose: ChoosePf = load(lib, "CGLChoosePixelFormat")?;
                let create: CreateCtx = load(lib, "CGLCreateContext")?;
                let set_current: SetCurrent = load(lib, "CGLSetCurrentContext")?;
                let destroy_pf: Destroy = load(lib, "CGLDestroyPixelFormat")?;
                let destroy_ctx: Destroy = load(lib, "CGLDestroyContext")?;
                // Try with the accelerated flag first, then let CGL fall back to any renderer.
                let mut pf: *mut c_void = std::ptr::null_mut();
                for accel in [true, false] {
                    let mut a = vec![PFA_ALLOW_OFFLINE, PFA_PROFILE, PROFILE_3_2_CORE];
                    if accel {
                        a.push(PFA_ACCELERATED);
                    }
                    a.push(0);
                    let mut n = 0;
                    if choose(a.as_ptr(), &mut pf, &mut n) == 0 && !pf.is_null() {
                        break;
                    }
                    pf = std::ptr::null_mut();
                }
                if pf.is_null() {
                    return Err("no OpenGL context (no pixel format)".into());
                }
                let mut ctx: *mut c_void = std::ptr::null_mut();
                let err = create(pf, std::ptr::null_mut(), &mut ctx);
                destroy_pf(pf);
                if err != 0 || ctx.is_null() {
                    return Err(format!("no OpenGL context (CGLCreateContext error {err})"));
                }
                if set_current(ctx) != 0 {
                    destroy_ctx(ctx);
                    return Err("no OpenGL context (CGLSetCurrentContext failed)".into());
                }
                let gl = Gl {
                    gen_textures: load(lib, "glGenTextures")?,
                    delete_textures: load(lib, "glDeleteTextures")?,
                    bind_texture: load(lib, "glBindTexture")?,
                    tex_image: load(lib, "glTexImage2D")?,
                    tex_sub_image: load(lib, "glTexSubImage2D")?,
                    tex_parameter: load(lib, "glTexParameteri")?,
                    gen_fbos: load(lib, "glGenFramebuffers")?,
                    delete_fbos: load(lib, "glDeleteFramebuffers")?,
                    bind_fbo: load(lib, "glBindFramebuffer")?,
                    fbo_texture: load(lib, "glFramebufferTexture2D")?,
                    check_fbo: load(lib, "glCheckFramebufferStatus")?,
                    read_pixels: load(lib, "glReadPixels")?,
                    viewport: load(lib, "glViewport")?,
                    pixel_store: load(lib, "glPixelStorei")?,
                    finish: load(lib, "glFinish")?,
                };
                (gl.pixel_store)(PACK_ALIGN, 1);
                (gl.pixel_store)(UNPACK_ALIGN, 1);
                Ok(Inner {
                    gl,
                    ctx,
                    destroy_ctx,
                    fbo: 0,
                    tex_out: 0,
                    tex_in: 0,
                    size: (0, 0),
                })
            }
        }

        unsafe fn make_texture(&self, w: u32, h: u32) -> u32 {
            unsafe {
                let mut t = 0;
                (self.gl.gen_textures)(1, &mut t);
                (self.gl.bind_texture)(TEX2D, t);
                (self.gl.tex_parameter)(TEX2D, MIN_F, NEAREST);
                (self.gl.tex_parameter)(TEX2D, MAG_F, NEAREST);
                (self.gl.tex_image)(
                    TEX2D,
                    0,
                    RGBA8,
                    w as i32,
                    h as i32,
                    0,
                    RGBA,
                    UBYTE,
                    std::ptr::null(),
                );
                t
            }
        }

        unsafe fn free_targets(&mut self) {
            unsafe {
                if self.fbo != 0 {
                    (self.gl.delete_fbos)(1, &self.fbo);
                    (self.gl.delete_textures)(1, &self.tex_out);
                    (self.gl.delete_textures)(1, &self.tex_in);
                }
                self.fbo = 0;
                self.size = (0, 0);
            }
        }

        unsafe fn ensure_targets(&mut self, w: u32, h: u32) -> Result<(), String> {
            unsafe {
                if self.size == (w, h) && self.fbo != 0 {
                    return Ok(());
                }
                self.free_targets();
                self.tex_out = self.make_texture(w, h);
                self.tex_in = self.make_texture(w, h);
                (self.gl.gen_fbos)(1, &mut self.fbo);
                (self.gl.bind_fbo)(FB, self.fbo);
                (self.gl.fbo_texture)(FB, COLOR0, TEX2D, self.tex_out, 0);
                if (self.gl.check_fbo)(FB) != COMPLETE {
                    self.free_targets();
                    return Err("framebuffer incomplete".into());
                }
                self.size = (w, h);
                Ok(())
            }
        }

        pub fn frame(
            &mut self,
            w: u32,
            h: u32,
            input: &[u8],
            output: &mut [u8],
            draw: impl FnOnce(u32, u32),
        ) -> Result<(), String> {
            let n = w as usize * h as usize * 4;
            if w == 0 || h == 0 || input.len() < n || output.len() < n {
                return Err("bad frame size".into());
            }
            unsafe {
                self.ensure_targets(w, h)?;
                (self.gl.bind_texture)(TEX2D, self.tex_in);
                (self.gl.tex_sub_image)(
                    TEX2D,
                    0,
                    0,
                    0,
                    w as i32,
                    h as i32,
                    RGBA,
                    UBYTE,
                    input.as_ptr() as *const c_void,
                );
                (self.gl.bind_fbo)(FB, self.fbo);
                (self.gl.viewport)(0, 0, w as i32, h as i32);
                draw(self.tex_in, self.fbo);
                (self.gl.finish)();
                (self.gl.bind_fbo)(FB, self.fbo);
                (self.gl.read_pixels)(
                    0,
                    0,
                    w as i32,
                    h as i32,
                    RGBA,
                    UBYTE,
                    output.as_mut_ptr() as *mut c_void,
                );
            }
            Ok(())
        }
    }

    impl Drop for Inner {
        fn drop(&mut self) {
            unsafe {
                self.free_targets();
                (self.destroy_ctx)(self.ctx);
            }
        }
    }
}
