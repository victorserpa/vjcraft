//! Helper process: the only place plugin code runs. A crash or hang here never reaches the app.
use libloading::{Library, Symbol};
use memmap2::MmapMut;
use std::ffi::c_void;
use std::io::{BufRead, Write};
use vjc_ffgl::abi::*;
use vjc_ffgl::proto::*;

/// Lifecycle per the documented ABI: INITIALISE, INSTANTIATE (instance id kept and passed on
/// every per-instance call), then DEINSTANTIATE + DEINITIALISE on drop.
struct Plugin {
    main: PlugMain,
    initialised: bool,
    instance: *mut c_void,
    _lib: Library,
}

impl Drop for Plugin {
    fn drop(&mut self) {
        unsafe {
            if !self.instance.is_null() {
                (self.main)(FF_DEINSTANTIATE, FFMixed { u: 0 }, self.instance);
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
            Ok(Plugin {
                main,
                initialised: false,
                instance: std::ptr::null_mut(),
                _lib: lib,
            })
        }
    }
    fn call(&self, code: u32, v: FFMixed) -> FFMixed {
        unsafe { (self.main)(code, v, self.instance) }
    }
    fn initialise(&mut self) -> Result<(), String> {
        if self.uint(FF_INITIALISE, 0) == FF_FAIL {
            return Err("INITIALISE failed".into());
        }
        self.initialised = true;
        Ok(())
    }
    fn instantiate(&mut self) -> Result<(), String> {
        let vp = ViewportStruct {
            x: 0,
            y: 0,
            width: 0,
            height: 0,
        };
        let r = self.call(
            FF_INSTANTIATE,
            FFMixed {
                p: &vp as *const _ as *mut c_void,
            },
        );
        let p = unsafe { r.p };
        if p.is_null() || p as usize == FF_FAIL as usize {
            return Err("INSTANTIATE failed".into());
        }
        self.instance = p;
        Ok(())
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
                Ok(mut p) => match p.info().and_then(|i| p.instantiate().map(|_| i)) {
                    Ok(info) => {
                        *cur = Some(p);
                        Response::Info { info }
                    }
                    Err(msg) => Response::Err { msg },
                },
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
            if unsafe { r.u } == FF_FAIL {
                return Response::Err {
                    msg: "plugin has no CPU path (needs the OpenGL host, stage 2)".into(),
                };
            }
            Response::Done
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
