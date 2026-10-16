//! `.cube` 3D LUT files: parser and the strip layout the GPU samples.
//!
//! The effect shader reads the LUT as a 2D strip (`size * size` by `size` texels, one
//! `size x size` slice per blue step) through the effect bind group's spare texture slot,
//! and blends two slices for the blue axis: a trilinear lookup without a 3D texture binding.

use crate::FxError;

/// A cubic LUT of `size`^3 entries, red varying fastest, then green, then blue.
#[derive(Debug, Clone, PartialEq)]
pub struct Lut {
    pub size: u32,
    pub data: Vec<[f32; 3]>,
    /// A plain RGBA8 picture (width, height, pixels) travelling through the same texture
    /// slot, for the image mask effect. `size` is 0 and `data` empty then.
    pub image: Option<(u32, u32, Vec<u8>)>,
}

/// The LUT uploads as an n*n-wide strip: 64 gives 4096 px, inside every GPU's 2D limit (8192).
pub const MAX_SIZE: u32 = 64;

/// Parses the text of a `.cube` file. Only 3D LUTs with the default 0..1 domain are
/// supported; anything else is an error naming the problem.
pub fn parse_cube(text: &str) -> Result<Lut, FxError> {
    let err = |m: String| Err(FxError(format!(".cube: {m}")));
    let mut size: Option<u32> = None;
    let mut data: Vec<[f32; 3]> = vec![];
    for (n, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut it = line.split_whitespace();
        let key = it.next().unwrap_or("");
        match key {
            "TITLE" | "LUT_3D_INPUT_RANGE" => {}
            "LUT_1D_SIZE" => return err("1D LUTs are not supported".into()),
            "LUT_3D_SIZE" => {
                let s = it.next().and_then(|v| v.parse::<u32>().ok());
                match s {
                    Some(s) if (2..=MAX_SIZE).contains(&s) => size = Some(s),
                    _ => return err(format!("line {}: bad LUT_3D_SIZE", n + 1)),
                }
            }
            "DOMAIN_MIN" | "DOMAIN_MAX" => {
                let want = if key == "DOMAIN_MIN" { 0.0 } else { 1.0 };
                let v: Vec<f32> = it.filter_map(|v| v.parse().ok()).collect();
                if v.len() != 3 || v.iter().any(|x| (x - want).abs() > 1e-6) {
                    return err("only the 0..1 domain is supported".into());
                }
            }
            _ => {
                let v: Vec<f32> = line
                    .split_whitespace()
                    .filter_map(|v| v.parse().ok())
                    .collect();
                if v.len() != 3 || v.iter().any(|x| !x.is_finite()) {
                    return err(format!("line {}: bad entry", n + 1));
                }
                data.push([v[0], v[1], v[2]]);
            }
        }
    }
    let size = size.ok_or_else(|| FxError(".cube: LUT_3D_SIZE missing".into()))?;
    let want = (size as usize).pow(3);
    if data.len() != want {
        return err(format!("expected {want} entries, found {}", data.len()));
    }
    Ok(Lut {
        size,
        data,
        image: None,
    })
}

impl Lut {
    /// A picture (RGBA8, top row first) sent to the GPU like a LUT strip.
    pub fn picture(w: u32, h: u32, rgba: Vec<u8>) -> Lut {
        Lut {
            size: 0,
            data: vec![],
            image: Some((w, h, rgba)),
        }
    }

    /// The LUT that maps every colour to itself.
    pub fn identity(size: u32) -> Lut {
        let s = size as usize;
        let k = (s - 1) as f32;
        let mut data = Vec::with_capacity(s * s * s);
        for b in 0..s {
            for g in 0..s {
                for r in 0..s {
                    data.push([r as f32 / k, g as f32 / k, b as f32 / k]);
                }
            }
        }
        Lut {
            size,
            data,
            image: None,
        }
    }

    /// RGBA8 strip: width `size * size`, height `size`; texel (b * size + r, g).
    pub fn strip_rgba8(&self) -> (u32, u32, Vec<u8>) {
        if let Some((w, h, px)) = &self.image {
            return (*w, *h, px.clone());
        }
        let s = self.size as usize;
        let (w, h) = (s * s, s);
        let mut px = vec![255u8; w * h * 4];
        for (i, c) in self.data.iter().enumerate() {
            let (r, g, b) = (i % s, (i / s) % s, i / (s * s));
            let o = (g * w + b * s + r) * 4;
            for k in 0..3 {
                px[o + k] = (c[k].clamp(0.0, 1.0) * 255.0).round() as u8;
            }
        }
        (w as u32, h as u32, px)
    }

    /// Trilinear lookup of `rgb` (0..1), the CPU twin of the shader.
    pub fn sample(&self, rgb: [f32; 3]) -> [f32; 3] {
        let s = self.size as usize;
        let k = (s - 1) as f32;
        let p = rgb.map(|c| c.clamp(0.0, 1.0) * k);
        let i0 = p.map(|v| v.floor() as usize);
        let f = [
            p[0] - i0[0] as f32,
            p[1] - i0[1] as f32,
            p[2] - i0[2] as f32,
        ];
        let at = |r: usize, g: usize, b: usize| {
            self.data[r.min(s - 1) + s * (g.min(s - 1) + s * b.min(s - 1))]
        };
        let mut out = [0.0; 3];
        for (corner, w) in (0..8).map(|c| {
            let (dr, dg, db) = (c & 1, (c >> 1) & 1, (c >> 2) & 1);
            let w = (if dr == 1 { f[0] } else { 1.0 - f[0] })
                * (if dg == 1 { f[1] } else { 1.0 - f[1] })
                * (if db == 1 { f[2] } else { 1.0 - f[2] });
            (at(i0[0] + dr, i0[1] + dg, i0[2] + db), w)
        }) {
            for k in 0..3 {
                out[k] += corner[k] * w;
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cube(size: u32, f: impl Fn(f32, f32, f32) -> [f32; 3]) -> String {
        let mut s = format!(
            "# synthetic\nTITLE \"t\"\nLUT_3D_SIZE {size}\nDOMAIN_MIN 0 0 0\nDOMAIN_MAX 1 1 1\n"
        );
        let k = (size - 1) as f32;
        for b in 0..size {
            for g in 0..size {
                for r in 0..size {
                    let c = f(r as f32 / k, g as f32 / k, b as f32 / k);
                    s += &format!("{} {} {}\n", c[0], c[1], c[2]);
                }
            }
        }
        s
    }

    #[test]
    fn parses_a_small_cube_with_red_fastest() {
        let lut = parse_cube(&cube(2, |r, g, b| [r, g, b])).unwrap();
        assert_eq!(lut.size, 2);
        assert_eq!(lut.data.len(), 8);
        assert_eq!(lut.data[1], [1.0, 0.0, 0.0], "red varies fastest");
        assert_eq!(lut.data[2], [0.0, 1.0, 0.0]);
        assert_eq!(lut.data[4], [0.0, 0.0, 1.0]);
        assert_eq!(lut, Lut::identity(2));
    }

    #[test]
    fn rejects_bad_files() {
        assert!(parse_cube("LUT_1D_SIZE 4\n0 0 0\n").is_err());
        assert!(parse_cube("0 0 0\n").is_err(), "size missing");
        assert!(
            parse_cube("LUT_3D_SIZE 2\n0 0 0\n").is_err(),
            "too few entries"
        );
        assert!(parse_cube("LUT_3D_SIZE 1\n0 0 0\n").is_err());
        assert!(
            parse_cube("LUT_3D_SIZE 96\n0 0 0\n").is_err(),
            "too wide for the strip"
        );
        assert!(parse_cube("LUT_3D_SIZE 2\nDOMAIN_MAX 2 2 2\n").is_err());
        let mut s = cube(2, |r, g, b| [r, g, b]);
        s.push_str("nope\n");
        assert!(parse_cube(&s).is_err());
    }

    #[test]
    fn identity_samples_to_itself_and_a_swap_lut_swaps() {
        let id = Lut::identity(5);
        for c in [[0.1, 0.5, 0.9], [0.0, 1.0, 0.33]] {
            let o = id.sample(c);
            for k in 0..3 {
                assert!((o[k] - c[k]).abs() < 1e-5);
            }
        }
        let swap = parse_cube(&cube(5, |r, g, b| [b, g, r])).unwrap();
        let o = swap.sample([0.25, 0.5, 1.0]);
        assert!((o[0] - 1.0).abs() < 1e-5 && (o[2] - 0.25).abs() < 1e-5);
    }

    #[test]
    fn strip_layout_puts_blue_slices_side_by_side() {
        let (w, h, px) = Lut::identity(3).strip_rgba8();
        assert_eq!((w, h), (9, 3));
        // texel (b=2, r=1, g=0): red 0.5, green 0, blue 1
        let o = (2 * 3 + 1) * 4;
        assert_eq!(&px[o..o + 4], &[128, 0, 255, 255]);
    }
}
