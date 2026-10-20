//! Helper process: the only place plugin code runs. A crash or hang here never reaches the app.
use libloading::{Library, Symbol};
use memmap2::MmapMut;
use std::ffi::c_void;
use std::io::{BufRead, Write};
use std::time::Instant;
use vjc_ffgl::abi::*;
use vjc_ffgl::glhost::GlHost;
use vjc_ffgl::proto::*;

/// Lifecycle per the documented ABI: INITIALISE, INSTANTIATE (instance id kept and passed on
/// every per-instance call), then DEINSTANTIATE + DEINITIALISE on drop.
struct Plugin {
    main: PlugMain,
    initialised: bool,
    instance: *mut c_void,
    /// OpenGL context (Load only). Declared before the library so it outlives nothing it needs.
    gl: Option<GlHost>,
    gl_error: String,
    /// Viewport the instance currently knows about.
    viewport: (u32, u32),
    mode: Mode,
    started: Instant,
    _lib: Library,
}

/// How frames are processed. Decided on the first frame: a plugin that answers the VJCraft CPU
/// test code (fixture only) stays on the CPU path, everything else is driven over OpenGL.
#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Cpu,
    Gl,
}

/// Initial viewport for INSTANTIATE_GL; the first frame sends RESIZE if the size differs.
const DEFAULT_VIEWPORT: (u32, u32) = (640, 480);

impl Drop for Plugin {
    fn drop(&mut self) {
        unsafe {
            if !self.instance.is_null() {
                (self.main)(FF_DEINSTANTIATE_GL, FFMixed { u: 0 }, self.instance);
            }
            if self.initialised {
                (self.main)(FF_DEINITIALISE, FFMixed { u: 0 }, std::ptr::null_mut());
            }
        }
    }
}

impl Plugin {
    fn open(path: &str) -> Result<Plugin, String> {
        unsafe {
            let lib = Library::new(vjc_ffgl::resolve_binary(std::path::Path::new(path)))
                .map_err(|e| e.to_string())?;
            let main: Symbol<PlugMain> = lib.get(b"plugMain\0").map_err(|e| e.to_string())?;
            let main = *main;
            // Only VJCraft's own test fixtures export this marker; real plugins are never sent
            // the private CPU process code.
            let cpu = lib.get::<*const u8>(b"vjcraft_cpu_test_plugin\0").is_ok();
            Ok(Plugin {
                main,
                initialised: false,
                instance: std::ptr::null_mut(),
                gl: None,
                gl_error: String::new(),
                viewport: DEFAULT_VIEWPORT,
                mode: if cpu { Mode::Cpu } else { Mode::Gl },
                started: Instant::now(),
                _lib: lib,
            })
        }
    }
    fn call(&self, code: u32, v: FFMixed) -> FFMixed {
        unsafe { (self.main)(code, v, self.instance) }
    }
    fn initialise(&mut self) -> Result<(), String> {
        // The SDK ignores INITIALISE_V2's input value; pass null.
        let r = self.call(
            FF_INITIALISE_V2,
            FFMixed {
                p: std::ptr::null_mut(),
            },
        );
        if unsafe { r.u } == FF_FAIL {
            return Err("INITIALISE failed".into());
        }
        self.initialised = true;
        Ok(())
    }
    fn instantiate(&mut self) -> Result<(), String> {
        let vp = ViewportStruct {
            x: 0,
            y: 0,
            width: self.viewport.0,
            height: self.viewport.1,
        };
        let r = self.call(
            FF_INSTANTIATE_GL,
            FFMixed {
                p: &vp as *const _ as *mut c_void,
            },
        );
        let p = unsafe { r.p };
        if p.is_null() || p as usize == FF_FAIL as usize {
            return Err("INSTANTIATE_GL failed".into());
        }
        self.instance = p;
        Ok(())
    }
    /// One frame over OpenGL: RESIZE when the size changed, SET_TIME, then PROCESS_OPENGL with
    /// one input texture and the host FBO.
    fn process_gl(
        &mut self,
        w: u32,
        h: u32,
        input: &[u8],
        output: &mut [u8],
    ) -> Result<(), String> {
        if self.gl.is_none() {
            return Err(if self.gl_error.is_empty() {
                "no OpenGL context".into()
            } else {
                self.gl_error.clone()
            });
        }
        if self.viewport != (w, h) {
            let vp = ViewportStruct {
                x: 0,
                y: 0,
                width: w,
                height: h,
            };
            let r = self.call(
                FF_RESIZE,
                FFMixed {
                    p: &vp as *const _ as *mut c_void,
                },
            );
            if unsafe { r.u } == FF_FAIL {
                return Err("RESIZE failed".into());
            }
            self.viewport = (w, h);
        }
        let mut t = self.started.elapsed().as_secs_f64();
        self.call(
            FF_SET_TIME,
            FFMixed {
                p: &mut t as *mut f64 as *mut c_void,
            },
        );
        let (main, instance) = (self.main, self.instance);
        let mut ok = true;
        let gl = self.gl.as_mut().unwrap();
        gl.frame(w, h, input, output, |tex, fbo| {
            let mut ts = TextureStruct {
                width: w,
                height: h,
                hardware_width: w,
                hardware_height: h,
                handle: tex,
            };
            let mut tp: *mut TextureStruct = &mut ts;
            let ps = ProcessOpenGlStruct {
                num_input_textures: 1,
                input_textures: &mut tp,
                host_fbo: fbo,
            };
            let r = unsafe {
                main(
                    FF_PROCESS_OPENGL,
                    FFMixed {
                        p: &ps as *const _ as *mut c_void,
                    },
                    instance,
                )
            };
            ok = unsafe { r.u } != FF_FAIL;
        })?;
        if ok {
            Ok(())
        } else {
            Err("PROCESS_OPENGL failed".into())
        }
    }
    fn uint(&self, code: u32, v: u32) -> u32 {
        unsafe { self.call(code, FFMixed { u: v }).u }
    }
    fn name(&self, index: u32) -> String {
        let p = unsafe { self.call(FF_GETPARAMETERNAME, FFMixed { u: index }).p } as *const u8;
        if p.is_null() || p as usize == FF_FAIL as usize {
            return String::new();
        }
        let b = unsafe { std::slice::from_raw_parts(p, 16) };
        let n = b.iter().position(|c| *c == 0).unwrap_or(16);
        String::from_utf8_lossy(&b[..n]).trim().to_string()
    }
    fn info(&mut self) -> Result<PluginInfo, String> {
        let p = unsafe { self.call(FF_GETINFO, FFMixed { u: 0 }).p } as *const PluginInfoStruct;
        if p.is_null() || p as usize == FF_FAIL as usize {
            return Err("GETINFO failed".into());
        }
        let i = unsafe { &*p };
        let text = |b: &[u8]| {
            String::from_utf8_lossy(b)
                .trim_end_matches(['\0', ' '])
                .to_string()
        };
        self.initialise()?;
        let n = self.uint(FF_GETNUMPARAMETERS, 0);
        let mut params = Vec::new();
        for k in 0..n {
            let ty = self.uint(FF_GETPARAMETERTYPE, k);
            let kind = if ty == FF_TYPE_BOOLEAN {
                ParamKind::Bool
            } else {
                ParamKind::Float
            };
            let default = f32::from_bits(self.uint(FF_GETPARAMETERDEFAULT, k));
            params.push(ParamInfo {
                name: self.name(k),
                kind,
                default,
            });
        }
        Ok(PluginInfo {
            id: text(&i.plugin_unique_id),
            name: text(&i.plugin_name),
            kind: if i.plugin_type == FF_SOURCE {
                Kind::Source
            } else {
                Kind::Effect
            },
            params,
        })
    }
}

fn handle(req: Request, cur: &mut Option<Plugin>) -> Response {
    match req {
        Request::Info { path } => match Plugin::open(&path).and_then(|mut p| p.info()) {
            Ok(info) => Response::Info { info },
            Err(msg) => Response::Err { msg },
        },
        Request::Load { path } => {
            *cur = None; // DEINSTANTIATE + DEINITIALISE the old plugin first
            match Plugin::open(&path) {
                Ok(mut p) => {
                    // The context must be current before INSTANTIATE_GL. Without one the CPU
                    // fixture still works; a GL frame then reports why it cannot run.
                    match GlHost::new() {
                        Ok(g) => p.gl = Some(g),
                        Err(e) => p.gl_error = e,
                    }
                    // A GL plugin must not be instantiated without a current context.
                    if p.mode == Mode::Gl && p.gl.is_none() {
                        return Response::Err {
                            msg: format!("no OpenGL context: {}", p.gl_error),
                        };
                    }
                    match p.info().and_then(|i| p.instantiate().map(|_| i)) {
                        Ok(info) => {
                            *cur = Some(p);
                            Response::Info { info }
                        }
                        Err(msg) => Response::Err { msg },
                    }
                }
                Err(msg) => Response::Err { msg },
            }
        }
        Request::Set { index, value } => match cur {
            Some(p) => {
                let s = SetParameterStruct {
                    index,
                    value: FFMixed { u: value.to_bits() },
                };
                let r = p.call(
                    FF_SETPARAMETER,
                    FFMixed {
                        p: &s as *const _ as *mut c_void,
                    },
                );
                if unsafe { r.u } == FF_FAIL {
                    return Response::Err {
                        msg: "SETPARAMETER failed".into(),
                    };
                }
                Response::Done
            }
            None => Response::Err {
                msg: "no plugin loaded".into(),
            },
        },
        Request::Get { index } => match cur {
            Some(p) => match p.uint(FF_GETPARAMETER, index) {
                FF_FAIL => Response::Err {
                    msg: "GETPARAMETER failed".into(),
                },
                bits => Response::Value {
                    value: f32::from_bits(bits),
                },
            },
            None => Response::Err {
                msg: "no plugin loaded".into(),
            },
        },
        Request::Process { shm, w, h } => {
            let Some(p) = cur else {
                return Response::Err {
                    msg: "no plugin loaded".into(),
                };
            };
            let n = w as usize * h as usize * 4;
            let file = match std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&shm)
            {
                Ok(f) => f,
                Err(e) => return Response::Err { msg: e.to_string() },
            };
            let mut map = match unsafe { MmapMut::map_mut(&file) } {
                Ok(m) => m,
                Err(e) => return Response::Err { msg: e.to_string() },
            };
            if map.len() < 2 * n {
                return Response::Err {
                    msg: "mapping too small".into(),
                };
            }
            let (a, b) = map.split_at_mut(n);
            if p.mode == Mode::Cpu {
                let s = CpuFrameStruct {
                    input: a.as_ptr(),
                    output: b.as_mut_ptr(),
                    width: w,
                    height: h,
                };
                let r = p.call(
                    FF_PROCESS_CPU_EXT,
                    FFMixed {
                        p: &s as *const _ as *mut c_void,
                    },
                );
                if unsafe { r.u } != FF_FAIL {
                    return Response::Done;
                }
                return Response::Err {
                    msg: "CPU process failed".into(),
                };
            }
            match p.process_gl(w, h, a, &mut b[..n]) {
                Ok(()) => Response::Done,
                Err(msg) => Response::Err { msg },
            }
        }
        Request::Quit => {
            *cur = None;
            std::process::exit(0)
        }
    }
}

fn main() {
    let stdin = std::io::stdin();
    let mut cur: Option<Plugin> = None;
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let resp = match serde_json::from_str::<Request>(&line) {
            Ok(r) => handle(r, &mut cur),
            Err(e) => Response::Err { msg: e.to_string() },
        };
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "{}", serde_json::to_string(&resp).unwrap());
        let _ = out.flush();
    }
}
