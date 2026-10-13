//! LTC (linear timecode) decoder, pure: biphase-mark audio samples in, timecodes out.
//!
//! Transitions are found with a Schmitt trigger (so polarity does not matter: an inverted
//! signal decodes the same), their spacing is classified against an adaptive bit-cell
//! length (so +-10% speed works), and the 80-bit frame is accepted when the last 16 bits
//! are the sync word 0x3FFD.

use crate::timecode::{TcFps, Timecode};

/// Sync word, transmitted as the last 16 bits of every frame.
pub const SYNC_WORD: u16 = 0x3FFD;
/// Initial guess of the bit-cell length, as a bit rate (between 24 and 30 fps).
const INITIAL_BIT_RATE: f64 = 2300.0;
/// Below this fraction of the cell a transition is a glitch.
const GLITCH: f64 = 0.3;
/// Above this many cells without a transition the signal is gone.
const GAP: f64 = 3.0;
/// Intervals shorter than this fraction of the cell are half cells (a "1").
const HALF_LIMIT: f64 = 0.75;

#[derive(Debug, Clone)]
pub struct LtcDecoder {
    sample_rate: f64,
    /// Fixed frame rate; None = detect from the frame numbers and the bit rate.
    fps: Option<TcFps>,
    pos: f64,
    high: Option<bool>,
    peak: f32,
    prev: f32,
    last_edge: Option<f64>,
    cell: f64,
    half_pending: bool,
    reg: u128,
    bits: u32,
    max_frame: u8,
}

impl LtcDecoder {
    pub fn new(sample_rate: u32) -> LtcDecoder {
        let sample_rate = f64::from(sample_rate.max(1));
        LtcDecoder {
            sample_rate,
            fps: None,
            pos: 0.0,
            high: None,
            peak: 0.0,
            prev: 0.0,
            last_edge: None,
            cell: sample_rate / INITIAL_BIT_RATE,
            half_pending: false,
            reg: 0,
            bits: 0,
            max_frame: 0,
        }
    }

    /// Forces the frame rate (None = automatic). Digits are unaffected; only the rate and
    /// drop-frame interpretation change.
    pub fn set_fps(&mut self, fps: Option<TcFps>) {
        self.fps = fps;
    }

    fn reset_bits(&mut self) {
        self.half_pending = false;
        self.bits = 0;
        self.reg = 0;
    }

    /// Decodes a block of samples (any chunking); calls `out` once per frame found. The
    /// value is the label of the frame that just ended.
    pub fn feed(&mut self, samples: &[f32], mut out: impl FnMut(Timecode)) {
        for &x in samples {
            // Adaptive threshold: 10% of a slowly decaying peak, never below noise floor.
            self.peak = (self.peak * 0.9999).max(x.abs());
            let thr = (self.peak * 0.1).max(0.003);
            let crossed = match self.high {
                None => {
                    if x > thr {
                        self.high = Some(true);
                    } else if x < -thr {
                        self.high = Some(false);
                    }
                    None
                }
                Some(true) if x < -thr => Some(false),
                Some(false) if x > thr => Some(true),
                _ => None,
            };
            if let Some(h) = crossed {
                self.high = Some(h);
                // Sub-sample position of the zero crossing.
                let edge = if (x - self.prev).abs() > f32::EPSILON {
                    self.pos - 1.0 + f64::from(self.prev / (self.prev - x))
                } else {
                    self.pos
                };
                self.edge(edge, &mut out);
            }
            if let Some(e) = self.last_edge
                && self.pos - e > GAP * self.cell
            {
                self.last_edge = None;
                self.reset_bits();
            }
            self.prev = x;
            self.pos += 1.0;
        }
    }

    fn edge(&mut self, at: f64, out: &mut impl FnMut(Timecode)) {
        let Some(last) = self.last_edge else {
            self.last_edge = Some(at);
            return;
        };
        let d = at - last;
        if d < GLITCH * self.cell {
            return;
        }
        self.last_edge = Some(at);
        if d < HALF_LIMIT * self.cell {
            self.cell += 0.1 * (2.0 * d - self.cell);
            if self.half_pending {
                self.half_pending = false;
                self.push_bit(true, out);
            } else {
                self.half_pending = true;
            }
        } else {
            self.cell += 0.1 * (d - self.cell);
            if self.half_pending {
                // A lone half cell: out of phase, wait for the next sync.
                self.reset_bits();
            } else {
                self.push_bit(false, out);
            }
        }
    }

    fn push_bit(&mut self, bit: bool, out: &mut impl FnMut(Timecode)) {
        self.reg = (self.reg << 1 | u128::from(bit)) & ((1u128 << 80) - 1);
        self.bits = (self.bits + 1).min(80);
        if self.bits == 80 && (self.reg & 0xFFFF) as u16 == SYNC_WORD {
            if let Some(tc) = self.frame() {
                out(tc);
            }
            self.bits = 0;
        }
    }

    fn bit(&self, n: u32) -> u8 {
        ((self.reg >> (79 - n)) & 1) as u8
    }

    fn field(&self, start: u32, len: u32) -> u8 {
        (0..len).fold(0, |v, i| v | self.bit(start + i) << i)
    }

    fn frame(&mut self) -> Option<Timecode> {
        let f = self.field(0, 4) + 10 * self.field(8, 2);
        let s = self.field(16, 4) + 10 * self.field(24, 3);
        let m = self.field(32, 4) + 10 * self.field(40, 3);
        let h = self.field(48, 4) + 10 * self.field(56, 2);
        let drop = self.bit(10) == 1;
        self.max_frame = self.max_frame.max(f);
        let measured = self.sample_rate / (80.0 * self.cell);
        let fps = self.fps.unwrap_or(if drop {
            TcFps::F2997Df
        } else if self.max_frame >= 25 || measured > 27.0 {
            TcFps::F30
        } else if self.max_frame == 24 || measured > 24.5 {
            TcFps::F25
        } else {
            TcFps::F24
        });
        let tc = Timecode::new(h, m, s, f, fps);
        tc.is_valid().then_some(tc)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: u32 = 48_000;

    /// 80 bits of an LTC frame, in transmission order.
    fn frame_bits(tc: Timecode) -> Vec<bool> {
        let mut v = [false; 80];
        let mut put = |start: usize, len: usize, val: u8| {
            for i in 0..len {
                v[start + i] = val >> i & 1 == 1;
            }
        };
        put(0, 4, tc.f % 10);
        put(8, 2, tc.f / 10);
        put(10, 1, u8::from(tc.fps == TcFps::F2997Df));
        put(16, 4, tc.s % 10);
        put(24, 3, tc.s / 10);
        put(32, 4, tc.m % 10);
        put(40, 3, tc.m / 10);
        put(48, 4, tc.h % 10);
        put(56, 2, tc.h / 10);
        for i in 0..16 {
            v[64 + i] = SYNC_WORD >> (15 - i) & 1 == 1;
        }
        v.to_vec()
    }

    /// Biphase-mark samples of consecutive frames starting at `start`; `speed` scales the
    /// bit rate, `invert` flips polarity, `lead` bits of silence-free preroll precede.
    fn encode(start: Timecode, frames: usize, speed: f64, invert: bool) -> Vec<f32> {
        let cell = f64::from(SR) / (start.fps.rate() * 80.0 * speed);
        let mut edges = vec![];
        let mut t = 0.0;
        let mut tc = start;
        for _ in 0..frames {
            for bit in frame_bits(tc) {
                edges.push(t); // every bit starts with a transition
                if bit {
                    edges.push(t + cell / 2.0);
                }
                t += cell;
            }
            tc = tc.add_frames(1);
        }
        let n = t.ceil() as usize + 10;
        let (mut e, mut level) = (0, false);
        (0..n)
            .map(|i| {
                while e < edges.len() && edges[e] <= i as f64 {
                    level = !level;
                    e += 1;
                }
                let v = if level != invert { 0.5 } else { -0.5 };
                v as f32
            })
            .collect()
    }

    fn decode(samples: &[f32], fps: Option<TcFps>) -> Vec<Timecode> {
        let mut d = LtcDecoder::new(SR);
        d.set_fps(fps);
        let mut out = vec![];
        // Odd chunking on purpose.
        for c in samples.chunks(777) {
            d.feed(c, |tc| out.push(tc));
        }
        out
    }

    /// Frames 1.. of the run must all be decoded, in order (frame 0 may be eaten by sync).
    fn assert_run(got: &[Timecode], start: Timecode, frames: usize) {
        assert!(
            got.len() >= frames - 2,
            "only {} frames: {got:?}",
            got.len()
        );
        for g in got {
            let n = g.frames().wrapping_sub(start.frames());
            assert!(n < frames as u64, "unexpected {g}");
        }
        for w in got.windows(2) {
            assert_eq!(w[1].frames(), w[0].frames() + 1, "gap {} -> {}", w[0], w[1]);
        }
    }

    #[test]
    fn decodes_25_fps() {
        let start = Timecode::new(1, 2, 3, 20, TcFps::F25);
        let got = decode(&encode(start, 30, 1.0, false), None);
        assert_run(&got, start, 30);
        // Auto detection settles on 25 once frame 24 has been seen.
        assert_eq!(got.last().unwrap().fps, TcFps::F25);
    }

    #[test]
    fn decodes_29_97_drop_frame_across_the_minute_drop() {
        let start = Timecode::new(0, 1, 59, 25, TcFps::F2997Df);
        let got = decode(&encode(start, 12, 1.0, false), None);
        assert_run(&got, start, 12);
        assert!(got.iter().all(|t| t.fps == TcFps::F2997Df));
        assert!(
            got.contains(&Timecode::new(0, 2, 0, 2, TcFps::F2997Df)),
            "{got:?}"
        );
    }

    #[test]
    fn tolerates_speed_changes() {
        let start = Timecode::new(10, 0, 0, 0, TcFps::F25);
        for speed in [0.9, 0.95, 1.05, 1.1] {
            let got = decode(&encode(start, 30, speed, false), Some(TcFps::F25));
            assert_run(&got, start, 30);
        }
        let start = Timecode::new(0, 1, 59, 25, TcFps::F2997Df);
        for speed in [0.95, 1.05] {
            let got = decode(&encode(start, 12, speed, false), None);
            assert_run(&got, start, 12);
        }
    }

    #[test]
    fn tolerates_inverted_signal() {
        let start = Timecode::new(22, 59, 59, 20, TcFps::F25);
        let got = decode(&encode(start, 12, 1.0, true), Some(TcFps::F25));
        assert_run(&got, start, 12);
        let got = decode(&encode(start, 12, 0.95, true), Some(TcFps::F25));
        assert_run(&got, start, 12);
    }

    #[test]
    fn noise_and_silence_decode_nothing() {
        assert!(decode(&vec![0.0; 48_000], None).is_empty());
        // Deterministic pseudo-noise.
        let mut x = 12345u32;
        let noise: Vec<f32> = (0..96_000)
            .map(|_| {
                x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                ((x >> 16) as f32 / 65_536.0 - 0.5) * 0.002
            })
            .collect();
        assert!(decode(&noise, None).is_empty());
    }

    #[test]
    fn resyncs_after_dropout() {
        let start = Timecode::new(0, 0, 10, 0, TcFps::F25);
        let mut s = encode(start, 10, 1.0, false);
        let cut = s.len() / 2;
        s[cut..cut + 5000].fill(0.0);
        let got = decode(&s, Some(TcFps::F25));
        assert!(got.len() >= 4, "{got:?}");
        assert!(got.first().unwrap().frames() < got.last().unwrap().frames());
    }
}
