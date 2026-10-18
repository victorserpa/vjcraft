//! Audio effect chains (clip, layer and master), pure Rust DSP on interleaved stereo f32.
//!
//! Threading design: the control thread describes a chain with `FxChain::set` (kinds, bypass,
//! dry/wet, up to `MAX_PARAMS` numbers per effect). Numbers travel in atomics; the DSP state
//! (delay lines, filter memory) lives in a `Mutex<ChainState>` that the audio callback only
//! `try_lock`s: it never blocks, never allocates, and a block that loses the race is passed
//! through dry. All buffers are allocated by `set` on the control thread, outside the lock,
//! and swapped in when the list of kinds changes (add, remove, reorder).

use std::f64::consts::PI;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};

/// Longest chain per scope.
pub const MAX_FX: usize = 8;
/// Numbers per effect.
pub const MAX_PARAMS: usize = 6;
/// Most layer buses the mixer offers.
pub const MAX_BUSES: usize = 32;
/// Longest delay line, seconds.
pub const MAX_DELAY_SECS: f32 = 2.0;
/// Frames processed per dry/wet chunk.
const CHUNK_FRAMES: usize = 4096;

/// One tweakable number of an effect.
#[derive(Debug, Clone, Copy)]
pub struct ParamSpec {
    pub name: &'static str,
    pub min: f32,
    pub max: f32,
    pub default: f32,
}

const fn ps(name: &'static str, min: f32, max: f32, default: f32) -> ParamSpec {
    ParamSpec {
        name,
        min,
        max,
        default,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FxKind {
    Gain,
    Pan,
    Eq3,
    LowPass,
    HighPass,
    Delay,
    Reverb,
    Compressor,
    Limiter,
    Bitcrusher,
    /// A CLAP plugin (docs/specs/audio-plugins.md); not in `ALL`, the plugin comes with the spec.
    Clap,
}

impl FxKind {
    pub const ALL: [FxKind; 10] = [
        FxKind::Gain,
        FxKind::Pan,
        FxKind::Eq3,
        FxKind::LowPass,
        FxKind::HighPass,
        FxKind::Delay,
        FxKind::Reverb,
        FxKind::Compressor,
        FxKind::Limiter,
        FxKind::Bitcrusher,
    ];

    /// Stable id stored in the project.
    pub fn id(self) -> &'static str {
        match self {
            FxKind::Gain => "audio.gain",
            FxKind::Pan => "audio.pan",
            FxKind::Eq3 => "audio.eq3",
            FxKind::LowPass => "audio.lowpass",
            FxKind::HighPass => "audio.highpass",
            FxKind::Delay => "audio.delay",
            FxKind::Reverb => "audio.reverb",
            FxKind::Compressor => "audio.compressor",
            FxKind::Limiter => "audio.limiter",
            FxKind::Bitcrusher => "audio.bitcrusher",
            FxKind::Clap => "clap",
        }
    }

    pub fn from_id(id: &str) -> Option<FxKind> {
        // CLAP effects are stored as `clap:<id>|<path>`
        if id.starts_with("clap:") {
            return Some(FxKind::Clap);
        }
        FxKind::ALL.into_iter().find(|k| k.id() == id)
    }

    pub fn label(self) -> &'static str {
        match self {
            FxKind::Gain => "Gain",
            FxKind::Pan => "Pan",
            FxKind::Eq3 => "3-band EQ",
            FxKind::LowPass => "Low-pass",
            FxKind::HighPass => "High-pass",
            FxKind::Delay => "Delay",
            FxKind::Reverb => "Reverb",
            FxKind::Compressor => "Compressor",
            FxKind::Limiter => "Limiter",
            FxKind::Bitcrusher => "Bitcrusher",
            FxKind::Clap => "CLAP plugin",
        }
    }

    /// Params in the order the DSP reads them.
    pub fn params(self) -> &'static [ParamSpec] {
        const GAIN: [ParamSpec; 1] = [ps("Gain dB", -60.0, 24.0, 0.0)];
        const PAN: [ParamSpec; 1] = [ps("Pan", -1.0, 1.0, 0.0)];
        const EQ: [ParamSpec; 3] = [
            ps("Low dB", -18.0, 18.0, 0.0),
            ps("Mid dB", -18.0, 18.0, 0.0),
            ps("High dB", -18.0, 18.0, 0.0),
        ];
        const FILTER_LP: [ParamSpec; 2] = [
            ps("Cutoff Hz", 20.0, 20000.0, 20000.0),
            ps("Resonance", 0.5, 10.0, 0.707),
        ];
        const FILTER_HP: [ParamSpec; 2] = [
            ps("Cutoff Hz", 20.0, 20000.0, 20.0),
            ps("Resonance", 0.5, 10.0, 0.707),
        ];
        const DELAY: [ParamSpec; 3] = [
            ps("Time ms", 1.0, 2000.0, 250.0),
            ps("Sync beats", 0.0, 8.0, 0.0),
            ps("Feedback", 0.0, 0.95, 0.35),
        ];
        const REVERB: [ParamSpec; 2] = [ps("Room", 0.0, 1.0, 0.5), ps("Damping", 0.0, 1.0, 0.5)];
        const COMP: [ParamSpec; 5] = [
            ps("Threshold dB", -60.0, 0.0, -18.0),
            ps("Ratio", 1.0, 20.0, 4.0),
            ps("Attack ms", 0.1, 200.0, 10.0),
            ps("Release ms", 5.0, 2000.0, 120.0),
            ps("Makeup dB", 0.0, 24.0, 0.0),
        ];
        const LIMIT: [ParamSpec; 2] = [
            ps("Ceiling dB", -24.0, 0.0, -1.0),
            ps("Release ms", 5.0, 2000.0, 80.0),
        ];
        const CRUSH: [ParamSpec; 2] =
            [ps("Bits", 1.0, 16.0, 8.0), ps("Downsample", 1.0, 32.0, 1.0)];
        match self {
            FxKind::Gain => &GAIN,
            FxKind::Pan => &PAN,
            FxKind::Eq3 => &EQ,
            FxKind::LowPass => &FILTER_LP,
            FxKind::HighPass => &FILTER_HP,
            FxKind::Delay => &DELAY,
            FxKind::Reverb => &REVERB,
            FxKind::Compressor => &COMP,
            FxKind::Limiter => &LIMIT,
            FxKind::Bitcrusher => &CRUSH,
            FxKind::Clap => &[],
        }
    }

    /// Dry/wet a new instance starts with: delay and reverb are blends, the rest are inline.
    pub fn default_mix(self) -> f32 {
        match self {
            FxKind::Delay => 0.4,
            FxKind::Reverb => 0.3,
            _ => 1.0,
        }
    }
}

/// What the control side wants from one effect of a chain.
#[derive(Debug, Clone)]
pub struct FxSpec {
    pub kind: FxKind,
    /// The plugin of a `FxKind::Clap` effect (its params live there, not in `params`).
    pub clap: Option<std::sync::Arc<vjc_clap::ClapShared>>,
    pub bypass: bool,
    /// Dry/wet, 0..=1.
    pub mix: f32,
    pub params: [f32; MAX_PARAMS],
}

/// Tempo and rate the DSP needs.
struct Ctx {
    rate: f32,
    bpm: f32,
}

// ---------------------------------------------------------------- biquad

#[derive(Debug, Clone, Copy, Default)]
pub struct BiquadCoef {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
}

impl BiquadCoef {
    fn norm(b: [f64; 3], a: [f64; 3]) -> Self {
        BiquadCoef {
            b0: b[0] / a[0],
            b1: b[1] / a[0],
            b2: b[2] / a[0],
            a1: a[1] / a[0],
            a2: a[2] / a[0],
        }
    }

    fn base(freq: f32, rate: f32) -> (f64, f64) {
        let f = (freq as f64).clamp(10.0, rate as f64 * 0.49);
        let w = 2.0 * PI * f / rate as f64;
        (w.sin(), w.cos())
    }

    pub fn lowpass(freq: f32, q: f32, rate: f32) -> Self {
        let (s, c) = Self::base(freq, rate);
        let al = s / (2.0 * (q as f64).max(0.1));
        Self::norm(
            [(1.0 - c) / 2.0, 1.0 - c, (1.0 - c) / 2.0],
            [1.0 + al, -2.0 * c, 1.0 - al],
        )
    }

    pub fn highpass(freq: f32, q: f32, rate: f32) -> Self {
        let (s, c) = Self::base(freq, rate);
        let al = s / (2.0 * (q as f64).max(0.1));
        Self::norm(
            [(1.0 + c) / 2.0, -(1.0 + c), (1.0 + c) / 2.0],
            [1.0 + al, -2.0 * c, 1.0 - al],
        )
    }

    pub fn peaking(freq: f32, q: f32, db: f32, rate: f32) -> Self {
        let (s, c) = Self::base(freq, rate);
        let a = 10f64.powf(db as f64 / 40.0);
        let al = s / (2.0 * (q as f64).max(0.1));
        Self::norm(
            [1.0 + al * a, -2.0 * c, 1.0 - al * a],
            [1.0 + al / a, -2.0 * c, 1.0 - al / a],
        )
    }

    pub fn low_shelf(freq: f32, db: f32, rate: f32) -> Self {
        let (s, c) = Self::base(freq, rate);
        let a = 10f64.powf(db as f64 / 40.0);
        let k = 2.0 * a.sqrt() * (s / 2.0 * 2f64.sqrt());
        Self::norm(
            [
                a * ((a + 1.0) - (a - 1.0) * c + k),
                2.0 * a * ((a - 1.0) - (a + 1.0) * c),
                a * ((a + 1.0) - (a - 1.0) * c - k),
            ],
            [
                (a + 1.0) + (a - 1.0) * c + k,
                -2.0 * ((a - 1.0) + (a + 1.0) * c),
                (a + 1.0) + (a - 1.0) * c - k,
            ],
        )
    }

    pub fn high_shelf(freq: f32, db: f32, rate: f32) -> Self {
        let (s, c) = Self::base(freq, rate);
        let a = 10f64.powf(db as f64 / 40.0);
        let k = 2.0 * a.sqrt() * (s / 2.0 * 2f64.sqrt());
        Self::norm(
            [
                a * ((a + 1.0) + (a - 1.0) * c + k),
                -2.0 * a * ((a - 1.0) + (a + 1.0) * c),
                a * ((a + 1.0) + (a - 1.0) * c - k),
            ],
            [
                (a + 1.0) - (a - 1.0) * c + k,
                2.0 * ((a - 1.0) - (a + 1.0) * c),
                (a + 1.0) - (a - 1.0) * c - k,
            ],
        )
    }

    /// Gain of the filter at `freq` Hz (linear).
    pub fn magnitude(&self, freq: f32, rate: f32) -> f64 {
        let w = 2.0 * PI * freq as f64 / rate as f64;
        let (c1, s1, c2, s2) = (w.cos(), w.sin(), (2.0 * w).cos(), (2.0 * w).sin());
        let (nr, ni) = (
            self.b0 + self.b1 * c1 + self.b2 * c2,
            -(self.b1 * s1 + self.b2 * s2),
        );
        let (dr, di) = (
            1.0 + self.a1 * c1 + self.a2 * c2,
            -(self.a1 * s1 + self.a2 * s2),
        );
        ((nr * nr + ni * ni) / (dr * dr + di * di)).sqrt()
    }
}

/// Transposed direct form II memory for one channel.
#[derive(Debug, Clone, Copy, Default)]
struct BiquadState {
    z1: f64,
    z2: f64,
}

impl BiquadState {
    #[inline]
    fn tick(&mut self, c: &BiquadCoef, x: f32) -> f32 {
        let x = x as f64;
        let y = c.b0 * x + self.z1;
        self.z1 = c.b1 * x - c.a1 * y + self.z2;
        self.z2 = c.b2 * x - c.a2 * y;
        y as f32
    }
}

/// Runs stages in series on both channels.
fn run_biquads(buf: &mut [f32], coefs: &[BiquadCoef], st: &mut [[BiquadState; 2]]) {
    for fr in buf.as_chunks_mut::<2>().0 {
        for ch in 0..2 {
            let mut x = fr[ch];
            for (c, s) in coefs.iter().zip(st.iter_mut()) {
                x = s[ch].tick(c, x);
            }
            fr[ch] = x;
        }
    }
}

// ---------------------------------------------------------------- effects

struct Comb {
    buf: Vec<f32>,
    i: usize,
    store: f32,
}

struct Allpass {
    buf: Vec<f32>,
    i: usize,
}

/// Freeverb: 8 parallel damped combs and 4 allpasses per channel.
pub struct Reverb {
    combs: [Vec<Comb>; 2],
    all: [Vec<Allpass>; 2],
}

const COMB_LEN: [usize; 8] = [1116, 1188, 1277, 1356, 1422, 1491, 1557, 1617];
const ALL_LEN: [usize; 4] = [556, 441, 341, 225];
const STEREO_SPREAD: usize = 23;

impl Reverb {
    fn new(rate: f32) -> Reverb {
        let sc = |n: usize, ch: usize| {
            (((n + ch * STEREO_SPREAD) as f32 * rate / 44100.0) as usize).max(1)
        };
        let mk = |ch: usize| {
            (
                COMB_LEN
                    .iter()
                    .map(|n| Comb {
                        buf: vec![0.0; sc(*n, ch)],
                        i: 0,
                        store: 0.0,
                    })
                    .collect::<Vec<_>>(),
                ALL_LEN
                    .iter()
                    .map(|n| Allpass {
                        buf: vec![0.0; sc(*n, ch)],
                        i: 0,
                    })
                    .collect::<Vec<_>>(),
            )
        };
        let (c0, a0) = mk(0);
        let (c1, a1) = mk(1);
        Reverb {
            combs: [c0, c1],
            all: [a0, a1],
        }
    }

    fn process(&mut self, buf: &mut [f32], room: f32, damp: f32) {
        let fb = 0.7 + 0.28 * room.clamp(0.0, 1.0);
        let d1 = 0.4 * damp.clamp(0.0, 1.0);
        for fr in buf.as_chunks_mut::<2>().0 {
            let input = (fr[0] + fr[1]) * 0.015;
            for (ch, out_s) in fr.iter_mut().enumerate() {
                let mut sum = 0.0;
                for c in &mut self.combs[ch] {
                    let out = c.buf[c.i];
                    c.store = out * (1.0 - d1) + c.store * d1;
                    c.buf[c.i] = input + c.store * fb;
                    c.i = (c.i + 1) % c.buf.len();
                    sum += out;
                }
                for a in &mut self.all[ch] {
                    let b = a.buf[a.i];
                    let out = b - sum;
                    a.buf[a.i] = sum + b * 0.5;
                    a.i = (a.i + 1) % a.buf.len();
                    sum = out;
                }
                *out_s = sum * 3.0;
            }
        }
    }
}

enum Fx {
    Gain {
        prev: f32,
    },
    Pan,
    Eq3 {
        coefs: [BiquadCoef; 3],
        key: [f32; 3],
        st: [[BiquadState; 2]; 3],
    },
    Filter {
        hp: bool,
        coef: BiquadCoef,
        key: [f32; 2],
        st: [[BiquadState; 2]; 1],
    },
    Delay {
        buf: Vec<f32>,
        w: usize,
    },
    Reverb(Box<Reverb>),
    Comp {
        env: f32,
    },
    Limiter {
        gain: f32,
    },
    Crush {
        hold: [f32; 2],
        n: u32,
    },
    /// None: the spec had no plugin; the audio passes.
    Clap(Option<vjc_clap::ClapLane>),
}

/// Longest a plugin block may take before the effect is bypassed.
const CLAP_BUDGET: std::time::Duration = std::time::Duration::from_millis(40);

fn db_to_lin(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

/// Equal-power pan, unity at the centre: (left, right) gains for a pan in -1..1.
pub fn equal_power_pan(pan: f32) -> (f32, f32) {
    let p = if pan.is_finite() {
        pan.clamp(-1.0, 1.0)
    } else {
        0.0
    };
    let a = (p as f64 + 1.0) * PI / 4.0;
    (
        (a.cos() * std::f64::consts::SQRT_2) as f32,
        (a.sin() * std::f64::consts::SQRT_2) as f32,
    )
}

impl Fx {
    fn new(kind: FxKind, rate: f32, clap: Option<&std::sync::Arc<vjc_clap::ClapShared>>) -> Fx {
        match kind {
            FxKind::Clap => Fx::Clap(clap.map(|c| {
                vjc_clap::ClapLane::spawn(
                    c.clone(),
                    &vjc_clap::helper_path(),
                    CLAP_BUDGET,
                    rate as u32,
                )
            })),
            FxKind::Gain => Fx::Gain { prev: 1.0 },
            FxKind::Pan => Fx::Pan,
            FxKind::Eq3 => Fx::Eq3 {
                coefs: [BiquadCoef::default(); 3],
                key: [f32::NAN; 3],
                st: Default::default(),
            },
            FxKind::LowPass | FxKind::HighPass => Fx::Filter {
                hp: kind == FxKind::HighPass,
                coef: BiquadCoef::default(),
                key: [f32::NAN; 2],
                st: Default::default(),
            },
            FxKind::Delay => Fx::Delay {
                buf: vec![0.0; (MAX_DELAY_SECS * rate) as usize * 2 + 2],
                w: 0,
            },
            FxKind::Reverb => Fx::Reverb(Box::new(Reverb::new(rate))),
            FxKind::Compressor => Fx::Comp { env: 0.0 },
            FxKind::Limiter => Fx::Limiter { gain: 1.0 },
            FxKind::Bitcrusher => Fx::Crush {
                hold: [0.0; 2],
                n: 0,
            },
        }
    }

    fn process(&mut self, p: &[f32; MAX_PARAMS], buf: &mut [f32], cx: &Ctx) {
        let rate = cx.rate;
        match self {
            Fx::Gain { prev } => {
                let target = db_to_lin(p[0]);
                let frames = (buf.len() / 2).max(1) as f32;
                let step = (target - *prev) / frames;
                let mut g = *prev;
                for fr in buf.as_chunks_mut::<2>().0 {
                    g += step;
                    fr[0] *= g;
                    fr[1] *= g;
                }
                *prev = target;
            }
            Fx::Pan => {
                let (gl, gr) = equal_power_pan(p[0]);
                for fr in buf.as_chunks_mut::<2>().0 {
                    fr[0] *= gl;
                    fr[1] *= gr;
                }
            }
            Fx::Eq3 { coefs, key, st } => {
                let k = [p[0], p[1], p[2]];
                if *key != k {
                    *coefs = [
                        BiquadCoef::low_shelf(200.0, k[0], rate),
                        BiquadCoef::peaking(1000.0, 0.7, k[1], rate),
                        BiquadCoef::high_shelf(4000.0, k[2], rate),
                    ];
                    *key = k;
                }
                run_biquads(buf, coefs, st);
            }
            Fx::Filter { hp, coef, key, st } => {
                let k = [p[0], p[1]];
                if *key != k {
                    *coef = if *hp {
                        BiquadCoef::highpass(k[0], k[1], rate)
                    } else {
                        BiquadCoef::lowpass(k[0], k[1], rate)
                    };
                    *key = k;
                }
                run_biquads(buf, std::slice::from_ref(coef), st);
            }
            Fx::Delay { buf: line, w } => {
                let cap = line.len() / 2;
                let secs = if p[1] > 0.0 && cx.bpm > 0.0 {
                    p[1] * 60.0 / cx.bpm
                } else {
                    p[0] / 1000.0
                };
                let d = ((secs * rate) as usize).clamp(1, cap - 1);
                let fb = p[2].clamp(0.0, 0.95);
                for fr in buf.as_chunks_mut::<2>().0 {
                    let r = (*w + cap - d) % cap;
                    let (ol, or) = (line[r * 2], line[r * 2 + 1]);
                    line[*w * 2] = fr[0] + ol * fb;
                    line[*w * 2 + 1] = fr[1] + or * fb;
                    *w = (*w + 1) % cap;
                    fr[0] = ol;
                    fr[1] = or;
                }
            }
            Fx::Reverb(r) => r.process(buf, p[0], p[1]),
            Fx::Clap(lane) => {
                if let Some(l) = lane {
                    l.process(buf);
                }
            }
            Fx::Comp { env } => {
                let coef = |ms: f32| (-1.0 / (ms.max(0.01) * 0.001 * rate)).exp();
                let (att, rel) = (coef(p[2]), coef(p[3]));
                let ratio = p[1].max(1.0);
                let makeup = db_to_lin(p[4]);
                for fr in buf.as_chunks_mut::<2>().0 {
                    let level = fr[0].abs().max(fr[1].abs());
                    let c = if level > *env { att } else { rel };
                    *env = level + c * (*env - level);
                    let over = 20.0 * env.max(1e-6).log10() - p[0];
                    let g = if over > 0.0 {
                        db_to_lin(-over * (1.0 - 1.0 / ratio))
                    } else {
                        1.0
                    } * makeup;
                    fr[0] *= g;
                    fr[1] *= g;
                }
            }
            Fx::Limiter { gain } => {
                let ceil = db_to_lin(p[0].min(0.0));
                let rel = 1.0 - (-1.0 / (p[1].max(1.0) * 0.001 * rate)).exp();
                for fr in buf.as_chunks_mut::<2>().0 {
                    let peak = fr[0].abs().max(fr[1].abs());
                    let target = if peak > ceil { ceil / peak } else { 1.0 };
                    *gain = if target < *gain {
                        target
                    } else {
                        *gain + (target - *gain) * rel
                    };
                    fr[0] = (fr[0] * *gain).clamp(-ceil, ceil);
                    fr[1] = (fr[1] * *gain).clamp(-ceil, ceil);
                }
            }
            Fx::Crush { hold, n } => {
                let levels = (2f32).powf(p[0].clamp(1.0, 16.0) - 1.0);
                let every = p[1].clamp(1.0, 32.0) as u32;
                for fr in buf.as_chunks_mut::<2>().0 {
                    if *n == 0 {
                        hold[0] = (fr[0] * levels).round() / levels;
                        hold[1] = (fr[1] * levels).round() / levels;
                    }
                    *n = (*n + 1) % every;
                    fr[0] = hold[0];
                    fr[1] = hold[1];
                }
            }
        }
    }
}

// ---------------------------------------------------------------- chain

struct Slot {
    bypass: AtomicBool,
    mix: AtomicU32,
    params: [AtomicU32; MAX_PARAMS],
}

#[derive(Default)]
struct ChainState {
    fx: Vec<(FxKind, Fx)>,
    dry: Vec<f32>,
}

/// An ordered list of effects shared between the control thread and the audio callback.
pub struct FxChain {
    n: AtomicUsize,
    bpm: AtomicU32,
    slots: [Slot; MAX_FX],
    /// One state per processing path (master has two: speakers and recording tap).
    states: Vec<Mutex<ChainState>>,
    /// Kinds the states were built for (control thread only).
    built: Mutex<Vec<(FxKind, usize)>>,
}

impl FxChain {
    pub fn new(paths: usize) -> FxChain {
        FxChain {
            n: AtomicUsize::new(0),
            bpm: AtomicU32::new(120f32.to_bits()),
            slots: std::array::from_fn(|_| Slot {
                bypass: AtomicBool::new(false),
                mix: AtomicU32::new(1f32.to_bits()),
                params: std::array::from_fn(|_| AtomicU32::new(0)),
            }),
            states: (0..paths.max(1))
                .map(|_| Mutex::new(ChainState::default()))
                .collect(),
            built: Mutex::new(Vec::new()),
        }
    }

    /// True when there is nothing to run.
    pub fn is_empty(&self) -> bool {
        self.n.load(Ordering::Relaxed) == 0
    }

    /// Control thread: makes the chain match `specs` (extra entries beyond `MAX_FX` are
    /// ignored). Allocates only when the list of kinds changed.
    pub fn set(&self, specs: &[FxSpec], rate: u32, bpm: f32) {
        let specs = &specs[..specs.len().min(MAX_FX)];
        // a CLAP slot is identified by its plugin instance too
        let kinds: Vec<(FxKind, usize)> = specs
            .iter()
            .map(|s| {
                (
                    s.kind,
                    s.clap
                        .as_ref()
                        .map_or(0, |c| std::sync::Arc::as_ptr(c) as usize),
                )
            })
            .collect();
        let mut built = self.built.lock().unwrap();
        if *built != kinds {
            // Bypass the whole chain while states and params change, so the callback never
            // runs a new effect with the previous effect's params (one block plays dry).
            self.n.store(0, Ordering::Release);
            let rate = rate.max(8000) as f32;
            for m in &self.states {
                let mut fresh: Vec<Option<Fx>> = kinds
                    .iter()
                    .enumerate()
                    .map(|(i, k)| {
                        (built.get(i) != Some(k))
                            .then(|| Fx::new(k.0, rate, specs[i].clap.as_ref()))
                    })
                    .collect();
                let dry = if kinds.is_empty() {
                    Vec::new()
                } else {
                    vec![0.0; CHUNK_FRAMES * 2]
                };
                let mut garbage: Vec<(FxKind, Fx)> = Vec::with_capacity(MAX_FX);
                let mut old_dry = Vec::new();
                {
                    let mut st = m.lock().unwrap();
                    for (i, k) in kinds.iter().enumerate() {
                        match (fresh[i].take(), st.fx.get_mut(i)) {
                            (Some(f), Some(slot)) => {
                                garbage.push(std::mem::replace(slot, (k.0, f)))
                            }
                            (Some(f), None) => st.fx.push((k.0, f)),
                            (None, _) => {}
                        }
                    }
                    while st.fx.len() > kinds.len() {
                        garbage.extend(st.fx.pop());
                    }
                    if st.dry.len() != dry.len() {
                        old_dry = std::mem::replace(&mut st.dry, dry);
                    }
                }
                drop((garbage, old_dry));
            }
            *built = kinds;
        }
        for (slot, s) in self.slots.iter().zip(specs) {
            slot.bypass.store(s.bypass, Ordering::Relaxed);
            slot.mix.store(s.mix.to_bits(), Ordering::Relaxed);
            for (a, v) in slot.params.iter().zip(s.params) {
                a.store(v.to_bits(), Ordering::Relaxed);
            }
        }
        self.bpm.store(bpm.to_bits(), Ordering::Relaxed);
        self.n.store(specs.len(), Ordering::Release);
    }

    /// Audio callback: runs the chain over interleaved stereo `buf` using state `path`.
    /// Allocation-free and non-blocking; an empty chain or a contended state is a no-op.
    pub fn process(&self, buf: &mut [f32], path: usize, rate: u32) {
        let n = self.n.load(Ordering::Acquire);
        if n == 0 {
            return;
        }
        let Some(m) = self.states.get(path) else {
            return;
        };
        let Ok(mut st) = m.try_lock() else { return };
        // `set` may have swapped the states while we waited: it zeroes `n` first and
        // republishes it only after the params, so re-read it under the lock.
        if self.n.load(Ordering::Acquire) == 0 {
            return;
        }
        let st = &mut *st;
        let cx = Ctx {
            rate: rate as f32,
            bpm: f32::from_bits(self.bpm.load(Ordering::Relaxed)),
        };
        for (slot, (_, fx)) in self.slots.iter().zip(st.fx.iter_mut()).take(n) {
            if slot.bypass.load(Ordering::Relaxed) {
                continue;
            }
            let mix = f32::from_bits(slot.mix.load(Ordering::Relaxed)).clamp(0.0, 1.0);
            let p: [f32; MAX_PARAMS] =
                std::array::from_fn(|i| f32::from_bits(slot.params[i].load(Ordering::Relaxed)));
            let chunk = st.dry.len().max(2);
            for part in buf.chunks_mut(chunk) {
                if let Fx::Clap(lane) = fx {
                    // the lane mixes against the dry block its wet audio came from
                    if let Some(l) = lane {
                        l.process_mix(part, mix);
                    }
                    continue;
                }
                if mix >= 1.0 || st.dry.len() < part.len() {
                    fx.process(&p, part, &cx);
                } else {
                    let dry = &mut st.dry[..part.len()];
                    dry.copy_from_slice(part);
                    fx.process(&p, part, &cx);
                    for (o, d) in part.iter_mut().zip(dry.iter()) {
                        *o = *d * (1.0 - mix) + *o * mix;
                    }
                }
            }
        }
    }
}

/// Chains of the mixer: one per layer bus plus the master.
pub struct Buses {
    pub layers: Vec<FxChain>,
    pub master: FxChain,
}

impl Buses {
    pub fn new() -> Buses {
        Buses {
            layers: (0..MAX_BUSES).map(|_| FxChain::new(1)).collect(),
            master: FxChain::new(2),
        }
    }
}

impl Default for Buses {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;

    struct Counting;
    thread_local! { static ALLOCS: Cell<usize> = const { Cell::new(0) }; }
    unsafe impl GlobalAlloc for Counting {
        unsafe fn alloc(&self, l: Layout) -> *mut u8 {
            let _ = ALLOCS.try_with(|c| c.set(c.get() + 1));
            unsafe { System.alloc(l) }
        }
        unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
            unsafe { System.dealloc(p, l) }
        }
    }
    #[global_allocator]
    static A: Counting = Counting;

    const R: u32 = 48000;

    fn spec(kind: FxKind, p: &[f32]) -> FxSpec {
        let mut params = [0.0; MAX_PARAMS];
        for (i, s) in kind.params().iter().enumerate() {
            params[i] = p.get(i).copied().unwrap_or(s.default);
        }
        FxSpec {
            kind,
            bypass: false,
            mix: 1.0,
            params,
            clap: None,
        }
    }

    fn run(specs: &[FxSpec], input: &[f32]) -> Vec<f32> {
        let c = FxChain::new(1);
        c.set(specs, R, 120.0);
        let mut b = input.to_vec();
        c.process(&mut b, 0, R);
        b
    }

    #[test]
    fn reordering_never_runs_an_effect_with_the_previous_effects_params() {
        use std::sync::Arc;
        use std::sync::atomic::AtomicBool;
        // A = hard-left pan, B = unity gain. A correct block is (left, 0) or (1, 1); a new
        // effect run with the old slot params gives something else (pan centre ~0.707 on
        // both sides, or gain -1 dB ~0.89 on both).
        let a = [spec(FxKind::Pan, &[-1.0])];
        let b = [spec(FxKind::Gain, &[0.0])];
        let c = Arc::new(FxChain::new(1));
        c.set(&a, R, 120.0);
        let stop = Arc::new(AtomicBool::new(false));
        let bad = {
            let (c, stop) = (c.clone(), stop.clone());
            std::thread::spawn(move || {
                let mut bad = 0u32;
                let mut buf = [0.0f32; 64];
                while !stop.load(Ordering::Relaxed) {
                    buf.fill(1.0);
                    c.process(&mut buf, 0, R);
                    for fr in buf.as_chunks::<2>().0 {
                        let ok = fr[1].abs() < 1e-6
                            || ((fr[0] - 1.0).abs() < 1e-4 && (fr[1] - 1.0).abs() < 1e-4);
                        if !ok {
                            bad += 1;
                        }
                    }
                }
                bad
            })
        };
        for i in 0..20000 {
            c.set(if i % 2 == 0 { &b } else { &a }, R, 120.0);
        }
        stop.store(true, Ordering::Relaxed);
        assert_eq!(bad.join().unwrap(), 0);
    }

    #[test]
    fn biquad_response_at_known_freqs() {
        let r = R as f32;
        let lp = BiquadCoef::lowpass(1000.0, std::f32::consts::FRAC_1_SQRT_2, r);
        assert!(
            (lp.magnitude(1000.0, r) - std::f64::consts::FRAC_1_SQRT_2).abs() < 0.01,
            "-3 dB at cutoff"
        );
        assert!(lp.magnitude(50.0, r) > 0.99);
        assert!(lp.magnitude(10000.0, r) < 0.02);
        let hp = BiquadCoef::highpass(1000.0, std::f32::consts::FRAC_1_SQRT_2, r);
        assert!((hp.magnitude(1000.0, r) - std::f64::consts::FRAC_1_SQRT_2).abs() < 0.01);
        assert!(hp.magnitude(50.0, r) < 0.01 && hp.magnitude(10000.0, r) > 0.98);
        let pk = BiquadCoef::peaking(1000.0, 1.0, 12.0, r);
        assert!((20.0 * pk.magnitude(1000.0, r).log10() - 12.0).abs() < 0.05);
        let ls = BiquadCoef::low_shelf(200.0, 9.0, r);
        assert!((20.0 * ls.magnitude(20.0, r).log10() - 9.0).abs() < 0.3);
        assert!((20.0 * ls.magnitude(10000.0, r).log10()).abs() < 0.1);
        let hs = BiquadCoef::high_shelf(4000.0, -9.0, r);
        assert!((20.0 * hs.magnitude(20000.0, r).log10() + 9.0).abs() < 0.8);
        assert!((20.0 * hs.magnitude(100.0, r).log10()).abs() < 0.1);
    }

    #[test]
    fn filter_effect_attenuates_a_high_tone() {
        let tone: Vec<f32> = (0..4800)
            .flat_map(|i| {
                let s = (2.0 * PI * 10000.0 * i as f64 / R as f64).sin() as f32;
                [s, s]
            })
            .collect();
        let out = run(&[spec(FxKind::LowPass, &[500.0, 0.707])], &tone);
        let rms = |v: &[f32]| (v.iter().map(|x| x * x).sum::<f32>() / v.len() as f32).sqrt();
        assert!(rms(&out[2400..]) < rms(&tone[2400..]) * 0.01);
    }

    #[test]
    fn delay_offsets_by_the_exact_samples_and_feeds_back() {
        let mut x = vec![0.0f32; 2000];
        x[0] = 1.0;
        x[1] = 1.0;
        // 10 ms at 48 kHz = 480 frames.
        let out = run(&[spec(FxKind::Delay, &[10.0, 0.0, 0.5])], &x);
        assert_eq!(out[480 * 2], 1.0);
        assert_eq!(out[480 * 2 + 1], 1.0);
        assert_eq!(out[0], 0.0);
        let mut x2 = vec![0.0f32; 4000];
        x2[0] = 1.0;
        let out = run(&[spec(FxKind::Delay, &[10.0, 0.0, 0.5])], &x2);
        assert_eq!(out[960 * 2], 0.5, "first echo comes back at half level");
    }

    #[test]
    fn delay_syncs_to_bpm() {
        let c = FxChain::new(1);
        c.set(&[spec(FxKind::Delay, &[10.0, 1.0, 0.0])], R, 120.0);
        let mut x = vec![0.0f32; 2 * 30000];
        x[0] = 1.0;
        c.process(&mut x, 0, R);
        // One beat at 120 bpm = 0.5 s = 24000 frames.
        assert_eq!(x[24000 * 2], 1.0);
    }

    #[test]
    fn limiter_never_exceeds_the_ceiling() {
        let x: Vec<f32> = (0..8000)
            .map(|i| if i % 7 < 3 { 3.0 } else { -2.0 } * ((i % 50) as f32 / 50.0 + 0.2))
            .collect();
        let out = run(&[spec(FxKind::Limiter, &[-6.0, 50.0])], &x);
        let ceil = db_to_lin(-6.0);
        assert!(out.iter().all(|s| s.abs() <= ceil + 1e-6));
        assert!(
            out.iter().any(|s| s.abs() > ceil * 0.9),
            "it still passes loud audio"
        );
    }

    #[test]
    fn compressor_reduces_loud_and_keeps_quiet() {
        let loud = vec![0.9f32; 20000];
        let out = run(
            &[spec(FxKind::Compressor, &[-20.0, 8.0, 1.0, 50.0, 0.0])],
            &loud,
        );
        assert!(out[19999] < 0.5);
        let quiet = vec![0.01f32; 2000];
        let out = run(
            &[spec(FxKind::Compressor, &[-20.0, 8.0, 1.0, 50.0, 0.0])],
            &quiet,
        );
        assert!((out[1999] - 0.01).abs() < 1e-6);
    }

    #[test]
    fn pan_law_is_equal_power_with_unity_centre() {
        let (l, r) = equal_power_pan(0.0);
        assert!((l - 1.0).abs() < 1e-6 && (r - 1.0).abs() < 1e-6);
        for p in [-1.0, -0.4, 0.3, 1.0] {
            let (l, r) = equal_power_pan(p);
            assert!((l * l + r * r - 2.0).abs() < 1e-5, "constant power at {p}");
        }
        let (l, r) = equal_power_pan(-1.0);
        assert!(r.abs() < 1e-6 && l > 1.4);
        assert_eq!(equal_power_pan(f32::NAN), equal_power_pan(0.0));
        let out = run(&[spec(FxKind::Pan, &[1.0])], &[1.0, 1.0]);
        assert!(out[0].abs() < 1e-6);
    }

    #[test]
    fn gain_reaches_target_and_bitcrusher_quantizes() {
        let out = run(&[spec(FxKind::Gain, &[-6.0])], &[1.0; 400]);
        assert!((out[399] - db_to_lin(-6.0)).abs() < 1e-4);
        let out = run(
            &[spec(FxKind::Bitcrusher, &[2.0, 2.0])],
            &[0.3, 0.3, 0.9, 0.9, 0.9, 0.9],
        );
        assert_eq!(out, vec![0.5, 0.5, 0.5, 0.5, 1.0, 1.0]);
    }

    #[test]
    fn reverb_has_a_tail_and_mix_blends() {
        let mut x = vec![0.0f32; 2 * 20000];
        x[0] = 1.0;
        x[1] = 1.0;
        let out = run(&[spec(FxKind::Reverb, &[0.8, 0.3])], &x);
        assert!(
            out[2 * 10000..].iter().any(|s| s.abs() > 1e-4),
            "tail rings on"
        );
        let mut s = spec(FxKind::Reverb, &[0.8, 0.3]);
        s.mix = 0.0;
        assert_eq!(run(&[s], &x), x, "mix 0 is dry");
    }

    #[test]
    fn bypass_is_bit_exact_and_empty_chain_too() {
        let x: Vec<f32> = (0..512).map(|i| (i as f32 * 0.37).sin()).collect();
        for k in FxKind::ALL {
            let mut s = spec(k, &[]);
            s.bypass = true;
            assert_eq!(run(&[s], &x), x, "{k:?}");
        }
        assert_eq!(run(&[], &x), x);
    }

    #[test]
    fn chain_runs_in_order() {
        let x = vec![0.5f32; 400];
        let a = run(
            &[
                spec(FxKind::Gain, &[6.0]),
                spec(FxKind::Limiter, &[-6.0, 50.0]),
            ],
            &x,
        );
        let b = run(
            &[
                spec(FxKind::Limiter, &[-6.0, 50.0]),
                spec(FxKind::Gain, &[6.0]),
            ],
            &x,
        );
        assert!(
            a[399] <= db_to_lin(-6.0) + 1e-6,
            "limiter last holds the ceiling"
        );
        assert!(b[399] > db_to_lin(-6.0) + 0.1, "gain last pushes past it");
    }

    #[test]
    fn reorder_and_removal_rebuild_state() {
        let c = FxChain::new(1);
        let (g, d) = (spec(FxKind::Gain, &[0.0]), spec(FxKind::Delay, &[]));
        c.set(&[g.clone(), d.clone()], R, 120.0);
        c.set(&[d, g.clone()], R, 120.0);
        c.set(&[g], R, 120.0);
        let mut b = vec![0.5f32; 64];
        c.process(&mut b, 0, R);
        assert!(b.iter().all(|s| (s - 0.5).abs() < 1e-5));
        c.set(&[], R, 120.0);
        assert!(c.is_empty());
    }

    #[test]
    fn processing_with_changing_params_does_not_allocate() {
        let specs: Vec<FxSpec> = FxKind::ALL.iter().map(|k| spec(*k, &[])).collect();
        let c = FxChain::new(1);
        // Longer than MAX_FX is truncated; run the full catalog in two chains.
        let (a, b) = specs.split_at(MAX_FX);
        let c2 = FxChain::new(1);
        c.set(a, R, 120.0);
        c2.set(b, R, 120.0);
        let mut buf = vec![0.3f32; 2048];
        let before = ALLOCS.with(Cell::get);
        for i in 0..50 {
            let mut s = specs.clone();
            for sp in &mut s {
                sp.mix = 0.5;
                sp.params[0] += i as f32 * 0.01;
            }
            c.set(&s[..MAX_FX], R, 120.0);
            c2.set(&s[MAX_FX..], R, 120.0);
            let p0 = ALLOCS.with(Cell::get);
            c.process(&mut buf, 0, R);
            c2.process(&mut buf, 0, R);
            assert_eq!(ALLOCS.with(Cell::get), p0, "audio callback allocated");
        }
        assert!(ALLOCS.with(Cell::get) > before, "the counter works");
    }

    #[test]
    fn clap_slot_keeps_its_place_and_passes_dry_when_the_plugin_is_gone() {
        use vjc_clap::ClapShared;
        let shared = ClapShared::new("/nonexistent/x.clap", "x.y", &[]);
        let mut clap = spec(FxKind::Clap, &[]);
        clap.clap = Some(shared.clone());
        let gain = spec(FxKind::Gain, &[-20.0]); // x0.1
        let c = FxChain::new(2);
        c.set(&[gain.clone(), clap.clone()], R, 120.0);
        let mut buf = vec![0.4f32; 1024];
        let mut tap = vec![0.4f32; 1024];
        // let the worker notice the missing plugin; the chain never waits for it
        for _ in 0..50 {
            buf.fill(0.4);
            let p0 = ALLOCS.with(Cell::get);
            c.process(&mut buf, 0, R);
            c.process(&mut tap, 1, R);
            assert_eq!(ALLOCS.with(Cell::get), p0, "audio callback allocated");
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(
            buf.iter().all(|v| (v - 0.04).abs() < 1e-4),
            "gain ran, clap passed dry"
        );
        assert!(matches!(
            shared.status(),
            vjc_clap::LaneStatus::Bypassed(_) | vjc_clap::LaneStatus::Starting
        ));
        // same Arc again: nothing is rebuilt
        c.set(&[gain, clap], R, 120.0);
    }
}
