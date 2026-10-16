//! Minimal QR Code encoder (ISO 18004): byte mode, error correction L, versions 1 to 5
//! (one block, up to 106 bytes). Enough for a `http://ip:port/?pin=` URL. No dependencies.

/// Data codewords and error-correction codewords per version 1..=5 at level L.
const VERSIONS: [(usize, usize); 5] = [(19, 7), (34, 10), (55, 15), (80, 20), (108, 26)];

/// A square grid of modules, `true` = dark.
pub struct Qr {
    pub size: usize,
    pub dark: Vec<bool>,
}

impl Qr {
    pub fn get(&self, x: usize, y: usize) -> bool {
        self.dark[y * self.size + x]
    }
}

fn gf_mul(a: u8, b: u8) -> u8 {
    let (mut r, mut a, mut b) = (0u16, u16::from(a), b);
    while b != 0 {
        if b & 1 != 0 {
            r ^= a;
        }
        a <<= 1;
        if a & 0x100 != 0 {
            a ^= 0x11D;
        }
        b >>= 1;
    }
    r as u8
}

/// Reed-Solomon remainder of `data` for `ec` check codewords.
fn rs_remainder(data: &[u8], ec: usize) -> Vec<u8> {
    let mut gen_poly = vec![0u8; ec];
    gen_poly[ec - 1] = 1;
    let mut root = 1u8;
    for _ in 0..ec {
        for j in 0..ec {
            gen_poly[j] = gf_mul(gen_poly[j], root);
            if j + 1 < ec {
                gen_poly[j] ^= gen_poly[j + 1];
            }
        }
        root = gf_mul(root, 2);
    }
    let mut rem = vec![0u8; ec];
    for &b in data {
        let f = b ^ rem.remove(0);
        rem.push(0);
        for (r, g) in rem.iter_mut().zip(&gen_poly) {
            *r ^= gf_mul(*g, f);
        }
    }
    rem
}

fn format_bits(mask: u32) -> u32 {
    let data = (1u32 << 3) | mask; // level L = 01
    let mut rem = data;
    for _ in 0..10 {
        rem = (rem << 1) ^ ((rem >> 9) * 0x537);
    }
    ((data << 10) | rem) ^ 0x5412
}

struct Grid {
    n: usize,
    m: Vec<bool>,
    func: Vec<bool>,
}

impl Grid {
    fn set(&mut self, x: i32, y: i32, v: bool) {
        if x >= 0 && y >= 0 && (x as usize) < self.n && (y as usize) < self.n {
            let i = y as usize * self.n + x as usize;
            self.m[i] = v;
            self.func[i] = true;
        }
    }

    fn format(&mut self, mask: u32) {
        let b = format_bits(mask);
        let bit = |i: u32| (b >> i) & 1 != 0;
        let n = self.n as i32;
        for i in 0..=5 {
            self.set(8, i, bit(i as u32));
        }
        self.set(8, 7, bit(6));
        self.set(8, 8, bit(7));
        self.set(7, 8, bit(8));
        for i in 9..15 {
            self.set(14 - i, 8, bit(i as u32));
        }
        for i in 0..8 {
            self.set(n - 1 - i, 8, bit(i as u32));
        }
        for i in 8..15 {
            self.set(8, n - 15 + i, bit(i as u32));
        }
        self.set(8, n - 8, true);
    }
}

fn masked(mask: u32, x: usize, y: usize) -> bool {
    let (x, y) = (x as u32, y as u32);
    match mask {
        0 => (x + y) % 2 == 0,
        1 => y % 2 == 0,
        2 => x % 3 == 0,
        3 => (x + y) % 3 == 0,
        4 => (x / 3 + y / 2) % 2 == 0,
        5 => x * y % 2 + x * y % 3 == 0,
        6 => (x * y % 2 + x * y % 3) % 2 == 0,
        _ => ((x + y) % 2 + x * y % 3) % 2 == 0,
    }
}

/// Runs of 5+ (3 + extra), 2x2 blocks (3 each) and dark balance (10 per 5% off 50%).
fn penalty(m: &[bool], n: usize) -> u32 {
    let mut p = 0;
    for horizontal in [true, false] {
        for a in 0..n {
            let mut run = 1;
            for b in 1..n {
                let at = |i: usize| {
                    if horizontal {
                        m[a * n + i]
                    } else {
                        m[i * n + a]
                    }
                };
                if at(b) == at(b - 1) {
                    run += 1;
                    if run == 5 {
                        p += 3;
                    } else if run > 5 {
                        p += 1;
                    }
                } else {
                    run = 1;
                }
            }
        }
    }
    for y in 0..n - 1 {
        for x in 0..n - 1 {
            let c = m[y * n + x];
            if c == m[y * n + x + 1] && c == m[(y + 1) * n + x] && c == m[(y + 1) * n + x + 1] {
                p += 3;
            }
        }
    }
    let dark = m.iter().filter(|d| **d).count() as i64;
    let total = (n * n) as i64;
    let off = (dark * 20 - total * 10).abs() / total;
    p + 10 * off as u32
}

/// Encodes `data`; None if it needs more than version 5 (106 bytes).
pub fn encode(data: &[u8]) -> Option<Qr> {
    let (ver, &(dcw, ecw)) = VERSIONS
        .iter()
        .enumerate()
        .find(|(_, (d, _))| data.len() + 2 <= *d)?;
    let ver = ver + 1;
    let mut bits: Vec<bool> = vec![];
    let push = |bits: &mut Vec<bool>, v: u32, len: u32| {
        bits.extend((0..len).rev().map(|i| (v >> i) & 1 != 0));
    };
    push(&mut bits, 0b0100, 4);
    push(&mut bits, data.len() as u32, 8);
    for &b in data {
        push(&mut bits, u32::from(b), 8);
    }
    let cap = dcw * 8;
    let term = 4.min((cap - bits.len()) as u32);
    push(&mut bits, 0, term);
    while !bits.len().is_multiple_of(8) {
        bits.push(false);
    }
    let mut cw: Vec<u8> = bits
        .chunks(8)
        .map(|c| c.iter().fold(0u8, |a, b| a << 1 | u8::from(*b)))
        .collect();
    for pad in [0xECu8, 0x11].iter().cycle() {
        if cw.len() >= dcw {
            break;
        }
        cw.push(*pad);
    }
    cw.extend(rs_remainder(&cw, ecw));

    let n = 17 + 4 * ver;
    let mut g = Grid {
        n,
        m: vec![false; n * n],
        func: vec![false; n * n],
    };
    let ni = n as i32;
    for i in 0..ni {
        g.set(6, i, i % 2 == 0);
        g.set(i, 6, i % 2 == 0);
    }
    for (cx, cy) in [(3, 3), (ni - 4, 3), (3, ni - 4)] {
        for dy in -4i32..=4 {
            for dx in -4i32..=4 {
                let d = dx.abs().max(dy.abs());
                g.set(cx + dx, cy + dy, d != 2 && d != 4);
            }
        }
    }
    if ver >= 2 {
        let c = ni - 7;
        for dy in -2i32..=2 {
            for dx in -2i32..=2 {
                g.set(c + dx, c + dy, dx.abs().max(dy.abs()) != 1);
            }
        }
    }
    g.format(0);
    // Data, zigzag from the bottom right, skipping the timing column.
    let mut i = 0usize;
    let mut x = ni - 1;
    let mut up = true;
    while x > 0 {
        if x == 6 {
            x -= 1;
        }
        for k in 0..n {
            let y = if up { n - 1 - k } else { k };
            for dx in 0..2 {
                let xx = (x - dx) as usize;
                if !g.func[y * n + xx] {
                    g.m[y * n + xx] = cw.get(i / 8).is_some_and(|b| (b >> (7 - i % 8)) & 1 != 0);
                    i += 1;
                }
            }
        }
        up = !up;
        x -= 2;
    }
    let data_only = g.m.clone();
    let mut best: Option<(u32, u32, Vec<bool>)> = None;
    for mask in 0..8 {
        let mut m = data_only.clone();
        for y in 0..n {
            for x in 0..n {
                if !g.func[y * n + x] && masked(mask, x, y) {
                    m[y * n + x] = !m[y * n + x];
                }
            }
        }
        let mut t = Grid {
            n,
            m,
            func: g.func.clone(),
        };
        t.format(mask);
        let pen = penalty(&t.m, n);
        if best.as_ref().is_none_or(|(p, _, _)| pen < *p) {
            best = Some((pen, mask, t.m));
        }
    }
    let (_, _, dark) = best?;
    Some(Qr { size: n, dark })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reed_solomon_matches_the_spec_example() {
        // ISO 18004 annex / common tutorial: "HELLO WORLD", 1-M (10 check codewords).
        let data = [
            32, 91, 11, 120, 209, 114, 220, 77, 67, 64, 236, 17, 236, 17, 236, 17,
        ];
        assert_eq!(
            rs_remainder(&data, 10),
            [196, 35, 39, 119, 235, 215, 231, 226, 93, 23]
        );
    }

    #[test]
    fn format_info_known_values() {
        assert_eq!(format_bits(0), 0b111011111000100);
        assert_eq!(format_bits(4), 0b110011000101111);
    }

    #[test]
    fn sizes_and_limits() {
        assert_eq!(encode(b"hi").unwrap().size, 21);
        assert_eq!(encode(&[b'a'; 30]).unwrap().size, 25);
        assert_eq!(encode(&[b'a'; 40]).unwrap().size, 29);
        assert_eq!(encode(&[b'a'; 106]).unwrap().size, 37);
        assert!(encode(&[b'a'; 107]).is_none());
        let q = encode(b"http://192.168.1.20:8090/?pin=1234").unwrap();
        // Finder corner and the always-dark module.
        assert!(q.get(0, 0) && q.get(6, 6) && !q.get(1, 1) && q.get(8, q.size - 8));
    }
}
