// Minimal FFGL 2.x style test plugin written from the documented ABI. Built by the tests
// with rustc --crate-type cdylib. Params: 0 Gain (default 0.5, output = input * 2 * gain),
// 1 Crash (>0.5: abort while processing), 2 Hang (>0.5: never return while processing).
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

#[unsafe(no_mangle)]
pub unsafe extern "C" fn plugMain(code: u32, v: FFMixed, _inst: *mut c_void) -> FFMixed {
    unsafe {
        match code {
            0 => FFMixed { p: &INFO as *const Info as *mut c_void },
            1 => FFMixed { u: 0 },
            4 => FFMixed { u: 3 },
            5 => FFMixed { p: NAMES[v.u as usize].as_ptr() as *mut c_void },
            6 => FFMixed { u: (if v.u == 0 { 0.5f32 } else { 0.0 }).to_bits() },
            15 => FFMixed { u: if v.u == 0 { 10 } else { 0 } },
            8 => {
                let s = &*(v.p as *const SetP);
                VALS[s.index as usize] = f32::from_bits(s.value.u);
                FFMixed { u: 0 }
            }
            9 => FFMixed { u: VALS[v.u as usize].to_bits() },
            0x7F00 => {
                let c = &*(v.p as *const Cpu);
                if VALS[1] > 0.5 {
                    std::process::abort();
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
