// Minimal FFGL 2.x style test plugin written from the documented ABI. Built by the tests
// with rustc --crate-type cdylib. Params: 0 Gain (default 0.5, output = input * 2 * gain),
// 1 Crash (>0.5: abort while processing; exactly 0.75: abort while setting, exactly 0.25: SETPARAMETER returns FF_FAIL), 2 Hang (>0.5: never return while processing).
use std::ffi::c_void;

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
struct SetP {
    index: u32,
    value: FFMixed,
}
#[repr(C)]
struct Cpu {
    input: *const u8,
    output: *mut u8,
    w: u32,
    h: u32,
}

static INFO: Info = Info { major: 2, minor: 0, id: *b"TSTG", name: *b"VJC Test Gain   ", ty: 0 };
static NAMES: [[u8; 16]; 3] = [*b"Gain\0\0\0\0\0\0\0\0\0\0\0\0", *b"Crash\0\0\0\0\0\0\0\0\0\0\0", *b"Hang\0\0\0\0\0\0\0\0\0\0\0\0"];
static mut VALS: [f32; 3] = [0.5, 0.0, 0.0];

static TOKEN: u8 = 0;

#[repr(C)]
struct DlInfo {
    fname: *const std::ffi::c_char,
    fbase: *mut c_void,
    sname: *const std::ffi::c_char,
    saddr: *mut c_void,
}
unsafe extern "C" {
    fn dladdr(addr: *const c_void, info: *mut DlInfo) -> i32;
}

/// True when `<this library>.<ext>` exists.
fn sibling_exists(ext: &str) -> bool {
    unsafe {
        let mut i = DlInfo { fname: std::ptr::null(), fbase: std::ptr::null_mut(), sname: std::ptr::null(), saddr: std::ptr::null_mut() };
        if dladdr(plugMain as *const c_void, &mut i) == 0 || i.fname.is_null() {
            return false;
        }
        let mut p = std::ffi::CStr::from_ptr(i.fname).to_string_lossy().to_string();
        p.push('.');
        p.push_str(ext);
        std::path::Path::new(&p).exists()
    }
}

/// Lifecycle log: appends the function code to `<this library>.log`, only if that file exists.
fn log(code: u32) {
    unsafe {
        let mut i = DlInfo { fname: std::ptr::null(), fbase: std::ptr::null_mut(), sname: std::ptr::null(), saddr: std::ptr::null_mut() };
        if dladdr(plugMain as *const c_void, &mut i) == 0 || i.fname.is_null() {
            return;
        }
        let mut p = std::ffi::CStr::from_ptr(i.fname).to_string_lossy().to_string();
        p.push_str(".log");
        if std::path::Path::new(&p).exists() {
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new().append(true).open(&p) {
                let _ = writeln!(f, "{code}");
            }
        }
    }
}

/// Marks this as a VJCraft test fixture: the helper drives it with the private CPU code.
#[unsafe(no_mangle)]
pub static vjcraft_cpu_test_plugin: u8 = 1;

// Needs an instance id (the one INSTANTIATE returned) for everything per-instance, like FFGL 2.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn plugMain(code: u32, v: FFMixed, inst: *mut c_void) -> FFMixed {
    log(code);
    if code == 0 && sibling_exists("slow") {
        std::thread::sleep(std::time::Duration::from_millis(1500)); // slow GETINFO
    }
    let has_inst = inst as *const u8 == &TOKEN as *const u8;
    if matches!(code, 8 | 9 | 19 | 0x7F00) && !has_inst {
        return FFMixed { u: 0xFFFF_FFFF };
    }
    unsafe {
        match code {
            18 => FFMixed { p: &TOKEN as *const u8 as *mut c_void },
            19 | 2 => FFMixed { u: 0 },
            0 => FFMixed { p: &INFO as *const Info as *mut c_void },
            34 => FFMixed { u: 0 },
            4 => FFMixed { u: 3 },
            5 => FFMixed { p: NAMES[v.u as usize].as_ptr() as *mut c_void },
            6 => FFMixed { u: (if v.u == 0 { 0.5f32 } else { 0.0 }).to_bits() },
            15 => FFMixed { u: if v.u == 0 { 10 } else { 0 } },
            8 => {
                let s = &*(v.p as *const SetP);
                if s.index == 1 && f32::from_bits(s.value.u) == 0.75 {
                    std::process::abort(); // crash while setting
                }
                if s.index == 1 && f32::from_bits(s.value.u) == 0.25 {
                    return FFMixed { u: 0xFFFF_FFFF }; // rejects this single value
                }
                VALS[s.index as usize] = f32::from_bits(s.value.u);
                FFMixed { u: 0 }
            }
            9 => FFMixed { u: VALS[v.u as usize].to_bits() },
            0x7F00 => {
                let c = &*(v.p as *const Cpu);
                if VALS[1] > 0.5 {
                    std::process::abort();
                }
                if VALS[2] == 0.25 {
                    std::thread::sleep(std::time::Duration::from_millis(50)); // slow plugin
                }
                if VALS[2] > 0.5 {
                    loop {
                        std::thread::sleep(std::time::Duration::from_secs(1));
                    }
                }
                for i in 0..(c.w * c.h) as usize {
                    for k in 0..4 {
                        let x = *c.input.add(i * 4 + k);
                        *c.output.add(i * 4 + k) =
                            if k == 3 { x } else { (x as f32 * 2.0 * VALS[0]).min(255.0) as u8 };
                    }
                }
                FFMixed { u: 0 }
            }
            _ => FFMixed { u: 0xFFFF_FFFF },
        }
    }
}
