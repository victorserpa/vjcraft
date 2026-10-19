//! HAP frame parsing (public HAP spec): section headers, snappy and chunked
//! "decode instructions". The output is block-compressed texture data ready for the GPU;
//! no pixel is decoded on the CPU.
//!
//! Formats: Hap (BC1), Hap Alpha (BC3), Hap Q (YCoCg in BC3), Hap Q Alpha (YCoCg BC3 +
//! alpha BC4), Hap R (BC7) and Hap Alpha-only (BC4).

use std::sync::atomic::{AtomicBool, Ordering};

static GPU_BC: AtomicBool = AtomicBool::new(false);

/// The app tells the media layer whether the GPU device has `TEXTURE_COMPRESSION_BC`.
/// Without it HAP clips take the FFmpeg RGBA path.
pub fn set_gpu_supported(on: bool) {
    GPU_BC.store(on, Ordering::Relaxed);
}

pub fn gpu_supported() -> bool {
    GPU_BC.load(Ordering::Relaxed)
}

/// How the texture data of a HAP frame is laid out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HapFormat {
    /// Hap: RGB, BC1.
    Dxt1,
    /// Hap Alpha: RGBA, BC3.
    Dxt5,
    /// Hap Q: scaled YCoCg in BC3.
    YcoCg,
    /// Hap Q Alpha: scaled YCoCg in BC3 plus an alpha plane in BC4.
    YcoCgAlpha,
    /// Hap R: RGBA, BC7.
    Bc7,
    /// Hap Alpha-only: alpha in BC4.
    AlphaOnly,
}

impl HapFormat {
    /// Bytes of one 4x4 block of the main texture.
    pub fn block_bytes(self) -> usize {
        match self {
            HapFormat::Dxt1 | HapFormat::AlphaOnly => 8,
            _ => 16,
        }
    }
}

/// One frame of block-compressed data, plus the picture size it crops to.
pub struct HapFrame {
    pub format: HapFormat,
    /// Picture size (what the layer shows).
    pub width: u32,
    pub height: u32,
    /// Texture size: the picture size rounded up to multiples of 4.
    pub tex_w: u32,
    pub tex_h: u32,
    /// Main texture blocks (all formats).
    pub color: Vec<u8>,
    /// Alpha BC4 blocks (Hap Q Alpha only).
    pub alpha: Option<Vec<u8>>,
}

impl HapFrame {
    pub fn bytes(&self) -> usize {
        self.color.len() + self.alpha.as_ref().map_or(0, Vec::len)
    }
}

const COMP_NONE: u8 = 0xA;
const COMP_SNAPPY: u8 = 0xB;
const COMP_COMPLEX: u8 = 0xC;

const TEX_DXT1: u8 = 0xB;
const TEX_DXT5: u8 = 0xE;
const TEX_YCOCG: u8 = 0xF;
const TEX_BC4: u8 = 0x1;
const TEX_BC7: u8 = 0xC;

const SEC_MULTI: u8 = 0x0D;
const SEC_INSTR: u8 = 0x01;
const SEC_COMPRESSORS: u8 = 0x02;
const SEC_SIZES: u8 = 0x03;
const SEC_OFFSETS: u8 = 0x04;

/// Reads a section header: `(header length, payload length, type byte)`.
fn section(b: &[u8]) -> Result<(usize, usize, u8), String> {
    if b.len() < 4 {
        return Err("hap: truncated section header".into());
    }
    let len = b[0] as usize | (b[1] as usize) << 8 | (b[2] as usize) << 16;
    let ty = b[3];
    let (hdr, len): (usize, usize) = if len == 0 {
        if b.len() < 8 {
            return Err("hap: truncated long section header".into());
        }
        (8, u32::from_le_bytes([b[4], b[5], b[6], b[7]]) as usize)
    } else {
        (4, len)
    };
    if hdr.checked_add(len).is_none_or(|e| e > b.len()) {
        return Err("hap: section longer than the frame".into());
    }
    Ok((hdr, len, ty))
}

fn u32s(b: &[u8]) -> Vec<u32> {
    b.as_chunks::<4>()
        .0
        .iter()
        .map(|c| u32::from_le_bytes(*c))
        .collect()
}

/// Bytes a declared size may exceed the expected plane by.
const SLACK: usize = 4096;

/// One piece of second-stage data: where it comes from, how it is packed.
struct Chunk<'a> {
    src: &'a [u8],
    snappy: bool,
    size: usize,
}

fn unpack(chunk: &Chunk, dst: &mut [u8]) -> Result<(), String> {
    if chunk.snappy {
        let n = snap::raw::Decoder::new()
            .decompress(chunk.src, dst)
            .map_err(|e| format!("hap: snappy: {e}"))?;
        if n != dst.len() {
            return Err("hap: snappy chunk has the wrong size".into());
        }
    } else {
        if chunk.src.len() != dst.len() {
            return Err("hap: raw chunk has the wrong size".into());
        }
        dst.copy_from_slice(chunk.src);
    }
    Ok(())
}

/// Second stage over all chunks, spread over threads when there are several.
fn unpack_all(chunks: &[Chunk], max: usize) -> Result<Vec<u8>, String> {
    let total = chunks
        .iter()
        .try_fold(0usize, |a, c| a.checked_add(c.size))
        .unwrap_or(usize::MAX);
    // The sizes come from the file: check them before allocating anything.
    if total > max.saturating_add(SLACK) {
        return Err(format!(
            "hap: declares {total} bytes, the picture needs {max}"
        ));
    }
    let mut out = vec![0u8; total];
    let mut jobs: Vec<(&Chunk, &mut [u8])> = Vec::with_capacity(chunks.len());
    let mut rest = out.as_mut_slice();
    for c in chunks {
        let (head, tail) = rest.split_at_mut(c.size);
        jobs.push((c, head));
        rest = tail;
    }
    let threads = std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(jobs.len());
    if threads <= 1 || total < 64 * 1024 {
        for (c, dst) in jobs {
            unpack(c, dst)?;
        }
        return Ok(out);
    }
    let per = jobs.len().div_ceil(threads);
    let results: Vec<Result<(), String>> = std::thread::scope(|s| {
        let handles: Vec<_> = jobs
            .chunks_mut(per)
            .map(|group| {
                s.spawn(move || {
                    for (c, dst) in group.iter_mut() {
                        unpack(c, dst)?;
                    }
                    Ok(())
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| {
                h.join()
                    .unwrap_or_else(|_| Err("hap: decoder panicked".into()))
            })
            .collect()
    });
    results.into_iter().collect::<Result<(), _>>()?;
    Ok(out)
}

/// Payload of a texture section after both stages, for the compressor in `ty`'s high nibble.
fn texture_data(ty: u8, payload: &[u8], max: usize) -> Result<Vec<u8>, String> {
    match ty >> 4 {
        COMP_NONE if payload.len() > max.saturating_add(SLACK) => {
            Err("hap: texture longer than the picture needs".into())
        }
        COMP_NONE => Ok(payload.to_vec()),
        COMP_SNAPPY => {
            let size = snap::raw::decompress_len(payload).map_err(|e| format!("hap: {e}"))?;
            unpack_all(
                &[Chunk {
                    src: payload,
                    snappy: true,
                    size,
                }],
                max,
            )
        }
        COMP_COMPLEX => {
            let (hdr, len, t) = section(payload)?;
            if t != SEC_INSTR {
                return Err("hap: missing decode instructions".into());
            }
            let mut inner = &payload[hdr..hdr + len];
            let data = &payload[hdr + len..];
            let (mut kinds, mut sizes, mut offsets) = (None, None, None);
            while !inner.is_empty() {
                let (h, l, t) = section(inner)?;
                let body = &inner[h..h + l];
                match t {
                    SEC_COMPRESSORS => kinds = Some(body),
                    SEC_SIZES => sizes = Some(u32s(body)),
                    SEC_OFFSETS => offsets = Some(u32s(body)),
                    _ => {}
                }
                inner = &inner[h + l..];
            }
            let (kinds, sizes) = kinds
                .zip(sizes)
                .ok_or("hap: incomplete decode instructions")?;
            if kinds.len() != sizes.len() || kinds.is_empty() {
                return Err("hap: chunk tables disagree".into());
            }
            let n = kinds.len();
            // Sizes are the stored (compressed) sizes. Without an offset table the chunks
            // follow each other.
            let mut starts = Vec::with_capacity(n);
            match offsets {
                Some(o) if o.len() == n => starts.extend(o.iter().map(|&x| x as usize)),
                Some(_) => return Err("hap: chunk tables disagree".into()),
                None => {
                    let mut at = 0usize;
                    for &sz in &sizes {
                        starts.push(at);
                        at += sz as usize;
                    }
                }
            }
            let mut chunks = Vec::with_capacity(n);
            for i in 0..n {
                let end = starts[i].saturating_add(sizes[i] as usize);
                if end > data.len() {
                    return Err("hap: chunk offsets out of range".into());
                }
                let src = &data[starts[i]..end];
                let (snappy, size) = match kinds[i] {
                    COMP_SNAPPY => (
                        true,
                        snap::raw::decompress_len(src).map_err(|e| format!("hap: {e}"))?,
                    ),
                    COMP_NONE => (false, src.len()),
                    k => return Err(format!("hap: unknown chunk compressor {k:#x}")),
                };
                chunks.push(Chunk { src, snappy, size });
            }
            unpack_all(&chunks, max)
        }
        c => Err(format!("hap: unknown compressor {c:#x}")),
    }
}

/// Bytes of a block-compressed plane of `w` x `h` pixels (both multiples of 4).
fn plane_bytes(w: u32, h: u32, block: usize) -> usize {
    (w / 4) as usize * (h / 4) as usize * block
}

/// Parses one HAP packet for a `width` x `height` picture.
pub fn parse(packet: &[u8], width: u32, height: u32) -> Result<HapFrame, String> {
    if width == 0 || height == 0 {
        return Err("hap: empty picture".into());
    }
    let (tw, th) = (width.div_ceil(4) * 4, height.div_ceil(4) * 4);
    let (hdr, len, ty) = section(packet)?;
    let body = &packet[hdr..hdr + len];
    let (format, color, alpha) = if ty == SEC_MULTI {
        let (h1, l1, t1) = section(body)?;
        let rest = &body[h1 + l1..];
        let (h2, l2, t2) = section(rest)?;
        if t1 & 0xF != TEX_YCOCG || t2 & 0xF != TEX_BC4 {
            return Err("hap: unsupported multi-image combination".into());
        }
        (
            HapFormat::YcoCgAlpha,
            texture_data(t1, &body[h1..h1 + l1], plane_bytes(tw, th, 16))?,
            Some(texture_data(
                t2,
                &rest[h2..h2 + l2],
                plane_bytes(tw, th, 8),
            )?),
        )
    } else {
        let format = match ty & 0xF {
            TEX_DXT1 => HapFormat::Dxt1,
            TEX_DXT5 => HapFormat::Dxt5,
            TEX_YCOCG => HapFormat::YcoCg,
            TEX_BC7 => HapFormat::Bc7,
            TEX_BC4 => HapFormat::AlphaOnly,
            t => return Err(format!("hap: unsupported texture format {t:#x}")),
        };
        (
            format,
            texture_data(ty, body, plane_bytes(tw, th, format.block_bytes()))?,
            None,
        )
    };
    if color.len() != plane_bytes(tw, th, format.block_bytes()) {
        return Err(format!(
            "hap: texture has {} bytes, expected {}",
            color.len(),
            plane_bytes(tw, th, format.block_bytes())
        ));
    }
    if alpha
        .as_ref()
        .is_some_and(|a| a.len() != plane_bytes(tw, th, 8))
    {
        return Err("hap: alpha plane has the wrong size".into());
    }
    Ok(HapFrame {
        format,
        width,
        height,
        tex_w: tw,
        tex_h: th,
        color,
        alpha,
    })
}

/// Builds HAP files for tests (no HAP encoder is needed on the machine).
#[doc(hidden)]
pub mod testgen {
    use super::*;

    #[derive(Clone, Copy, PartialEq)]
    pub enum Kind {
        Hap,
        HapAlpha,
        HapQ,
    }

    /// Wraps `payload` in a section header of type `ty`.
    pub fn wrap(ty: u8, payload: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        if payload.len() < 1 << 24 {
            v.extend_from_slice(&(payload.len() as u32).to_le_bytes()[..3]);
            v.push(ty);
        } else {
            v.extend_from_slice(&[0, 0, 0, ty]);
            v.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        }
        v.extend_from_slice(payload);
        v
    }

    /// Texture section with the second stage split in `chunks` snappy chunks
    /// (1 = plain snappy section, 0 = no compressor).
    pub fn texture_section(tex_ty: u8, blocks: &[u8], chunks: usize) -> Vec<u8> {
        if chunks == 0 {
            return wrap(0xA0 | tex_ty, blocks);
        }
        if chunks == 1 {
            let c = snap::raw::Encoder::new().compress_vec(blocks).unwrap();
            return wrap(0xB0 | tex_ty, &c);
        }
        let per = blocks.len().div_ceil(chunks);
        let (mut data, mut sizes, mut offsets, mut kinds) = (vec![], vec![], vec![], vec![]);
        for part in blocks.chunks(per) {
            offsets.extend_from_slice(&(data.len() as u32).to_le_bytes());
            let packed = snap::raw::Encoder::new().compress_vec(part).unwrap();
            sizes.extend_from_slice(&(packed.len() as u32).to_le_bytes());
            kinds.push(COMP_SNAPPY);
            data.extend(packed);
        }
        let mut instr = wrap(SEC_COMPRESSORS, &kinds);
        instr.extend(wrap(SEC_SIZES, &sizes));
        instr.extend(wrap(SEC_OFFSETS, &offsets));
        let mut body = wrap(SEC_INSTR, &instr);
        body.extend(data);
        wrap(0xC0 | tex_ty, &body)
    }

    fn rgb565(c: [u8; 3]) -> u16 {
        (u16::from(c[0] >> 3) << 11) | (u16::from(c[1] >> 2) << 5) | u16::from(c[2] >> 3)
    }

    fn unpack565(v: u16) -> [i32; 3] {
        let (r, g, b) = (
            i32::from(v >> 11),
            i32::from((v >> 5) & 63),
            i32::from(v & 31),
        );
        [
            (r << 3) | (r >> 2),
            (g << 2) | (g >> 4),
            (b << 3) | (b >> 2),
        ]
    }

    fn bc1_block(px: &[[u8; 4]; 16]) -> [u8; 8] {
        let mut lo = [255u8; 3];
        let mut hi = [0u8; 3];
        for p in px {
            for k in 0..3 {
                lo[k] = lo[k].min(p[k]);
                hi[k] = hi[k].max(p[k]);
            }
        }
        let (mut c0, mut c1) = (rgb565(hi), rgb565(lo));
        if c0 < c1 {
            std::mem::swap(&mut c0, &mut c1);
        }
        let (a, b) = (unpack565(c0), unpack565(c1));
        let pal: Vec<[i32; 3]> = if c0 > c1 {
            vec![
                a,
                b,
                std::array::from_fn(|k| (2 * a[k] + b[k]) / 3),
                std::array::from_fn(|k| (a[k] + 2 * b[k]) / 3),
            ]
        } else {
            vec![a, b, a, b]
        };
        let mut bits = 0u32;
        for (i, p) in px.iter().enumerate() {
            let best = (0..4)
                .min_by_key(|&j| {
                    (0..3)
                        .map(|k| (pal[j][k] - i32::from(p[k])).pow(2))
                        .sum::<i32>()
                })
                .unwrap();
            bits |= (best as u32) << (2 * i);
        }
        let mut out = [0u8; 8];
        out[..2].copy_from_slice(&c0.to_le_bytes());
        out[2..4].copy_from_slice(&c1.to_le_bytes());
        out[4..].copy_from_slice(&bits.to_le_bytes());
        out
    }

    /// 8-value (a0 > a1) BC4 block over one channel.
    fn bc4_block(v: &[u8; 16]) -> [u8; 8] {
        let (lo, hi) = (*v.iter().min().unwrap(), *v.iter().max().unwrap());
        let (a0, a1) = if hi == lo {
            (hi, lo.saturating_sub(1))
        } else {
            (hi, lo)
        };
        let mut pal = [0i32; 8];
        pal[0] = i32::from(a0);
        pal[1] = i32::from(a1);
        for i in 1..7 {
            pal[i + 1] = ((7 - i as i32) * pal[0] + i as i32 * pal[1]) / 7;
        }
        let mut bits = 0u64;
        for (i, &x) in v.iter().enumerate() {
            let best = (0..8)
                .min_by_key(|&j| (pal[j] - i32::from(x)).abs())
                .unwrap();
            bits |= (best as u64) << (3 * i);
        }
        let mut out = [0u8; 8];
        out[0] = a0;
        out[1] = a1;
        out[2..].copy_from_slice(&bits.to_le_bytes()[..6]);
        out
    }

    /// Encodes padded RGBA (`tw` x `th`) into BC1 (`alpha` false) or BC3 blocks.
    fn encode(rgba: &[u8], tw: usize, th: usize, bc3: bool) -> Vec<u8> {
        let mut out = Vec::new();
        for by in 0..th / 4 {
            for bx in 0..tw / 4 {
                let mut px = [[0u8; 4]; 16];
                for (i, p) in px.iter_mut().enumerate() {
                    let o = ((by * 4 + i / 4) * tw + bx * 4 + i % 4) * 4;
                    p.copy_from_slice(&rgba[o..o + 4]);
                }
                if bc3 {
                    out.extend(bc4_block(&px.map(|p| p[3])));
                }
                out.extend(bc1_block(&px));
            }
        }
        out
    }

    /// RGBA test picture: smooth ramps, so block compression stays accurate.
    pub fn picture(w: usize, h: usize, t: usize, alpha: bool) -> Vec<u8> {
        let mut v = Vec::with_capacity(w * h * 4);
        for y in 0..h {
            for x in 0..w {
                v.extend_from_slice(&[
                    (x * 255 / w.max(1)) as u8,
                    (y * 255 / h.max(1)) as u8,
                    ((x + y + t * 8) * 255 / (w + h)).min(255) as u8,
                    if alpha {
                        (255 - x * 200 / w.max(1)) as u8
                    } else {
                        255
                    },
                ]);
            }
        }
        v
    }

    /// One HAP packet from an RGBA picture of `w` x `h` (edge pixels repeat into the padding).
    pub fn packet(rgba: &[u8], w: usize, h: usize, kind: Kind, chunks: usize) -> Vec<u8> {
        let (tw, th) = (w.div_ceil(4) * 4, h.div_ceil(4) * 4);
        let mut pad = vec![0u8; tw * th * 4];
        for y in 0..th {
            for x in 0..tw {
                let s = (y.min(h - 1) * w + x.min(w - 1)) * 4;
                let d = (y * tw + x) * 4;
                let px = &rgba[s..s + 4];
                if kind == Kind::HapQ {
                    let (r, g, b) = (i32::from(px[0]), i32::from(px[1]), i32::from(px[2]));
                    let co = ((r - b) / 2 + 128).clamp(0, 255);
                    let cg = ((-r + 2 * g - b) / 4 + 128).clamp(0, 255);
                    let y = ((r + 2 * g + b) / 4).clamp(0, 255);
                    pad[d..d + 4].copy_from_slice(&[co as u8, cg as u8, 0, y as u8]);
                } else {
                    pad[d..d + 4].copy_from_slice(px);
                }
            }
        }
        match kind {
            Kind::Hap => texture_section(TEX_DXT1, &encode(&pad, tw, th, false), chunks),
            Kind::HapAlpha => texture_section(TEX_DXT5, &encode(&pad, tw, th, true), chunks),
            Kind::HapQ => texture_section(TEX_YCOCG, &encode(&pad, tw, th, true), chunks),
        }
    }

    fn bx(ty: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut v = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
        v.extend_from_slice(ty);
        v.extend_from_slice(payload);
        v
    }

    fn be32(x: u32) -> [u8; 4] {
        x.to_be_bytes()
    }

    /// Minimal QuickTime file holding `packets` as a HAP video track.
    pub fn mov(packets: &[Vec<u8>], w: u32, h: u32, fps: u32, fourcc: &[u8; 4]) -> Vec<u8> {
        let ftyp = bx(b"ftyp", b"qt  \0\0\0\0qt  ");
        let mdat_payload: Vec<u8> = packets.concat();
        let mdat = bx(b"mdat", &mdat_payload);
        let data_at = (ftyp.len() + 8) as u32;
        let n = packets.len() as u32;
        let matrix: Vec<u8> = [0x10000u32, 0, 0, 0, 0x10000, 0, 0, 0, 0x40000000]
            .iter()
            .flat_map(|x| x.to_be_bytes())
            .collect();
        let mut mvhd = vec![0u8; 4];
        mvhd.extend([0u8; 8]);
        mvhd.extend(be32(fps));
        mvhd.extend(be32(n));
        mvhd.extend(be32(0x10000));
        mvhd.extend([1, 0]);
        mvhd.extend([0u8; 10]);
        mvhd.extend(&matrix);
        mvhd.extend([0u8; 24]);
        mvhd.extend(be32(2));
        let mut tkhd = vec![0, 0, 0, 3];
        tkhd.extend([0u8; 8]);
        tkhd.extend(be32(1));
        tkhd.extend([0u8; 4]);
        tkhd.extend(be32(n));
        tkhd.extend([0u8; 8]);
        tkhd.extend([0u8; 4]);
        tkhd.extend([0u8; 4]);
        tkhd.extend(&matrix);
        tkhd.extend(be32(w << 16));
        tkhd.extend(be32(h << 16));
        let mut mdhd = vec![0u8; 12];
        mdhd.extend(be32(fps));
        mdhd.extend(be32(n));
        mdhd.extend([0x55, 0xC4, 0, 0]);
        let mut hdlr = vec![0u8; 8];
        hdlr.extend(b"vide");
        hdlr.extend([0u8; 12]);
        hdlr.extend(b"v\0");
        let mut vmhd = vec![0, 0, 0, 1];
        vmhd.extend([0u8; 8]);
        let url = bx(b"url ", &[0, 0, 0, 1]);
        let mut dref = vec![0, 0, 0, 0];
        dref.extend(be32(1));
        dref.extend(&url);
        let dinf = bx(b"dinf", &bx(b"dref", &dref));
        let mut entry = vec![0u8; 6];
        entry.extend([0, 1]);
        entry.extend([0u8; 16]);
        entry.extend((w as u16).to_be_bytes());
        entry.extend((h as u16).to_be_bytes());
        entry.extend(be32(0x0048_0000));
        entry.extend(be32(0x0048_0000));
        entry.extend([0u8; 4]);
        entry.extend([0, 1]);
        entry.extend([0u8; 32]);
        entry.extend([0, 24, 0xFF, 0xFF]);
        let mut stsd = vec![0, 0, 0, 0];
        stsd.extend(be32(1));
        stsd.extend(bx(fourcc, &entry));
        let mut stts = vec![0, 0, 0, 0];
        stts.extend(be32(1));
        stts.extend(be32(n));
        stts.extend(be32(1));
        let mut stsc = vec![0, 0, 0, 0];
        stsc.extend(be32(1));
        stsc.extend(be32(1));
        stsc.extend(be32(n));
        stsc.extend(be32(1));
        let mut stsz = vec![0u8; 8];
        stsz.extend(be32(n));
        for p in packets {
            stsz.extend(be32(p.len() as u32));
        }
        let mut stco = vec![0, 0, 0, 0];
        stco.extend(be32(1));
        stco.extend(be32(data_at));
        let stbl = [
            bx(b"stsd", &stsd),
            bx(b"stts", &stts),
            bx(b"stsc", &stsc),
            bx(b"stsz", &stsz),
            bx(b"stco", &stco),
        ]
        .concat();
        let minf = [bx(b"vmhd", &vmhd), dinf, bx(b"stbl", &stbl)].concat();
        let mdia = [bx(b"mdhd", &mdhd), bx(b"hdlr", &hdlr), bx(b"minf", &minf)].concat();
        let trak = [bx(b"tkhd", &tkhd), bx(b"mdia", &mdia)].concat();
        let moov = bx(b"moov", &[bx(b"mvhd", &mvhd), bx(b"trak", &trak)].concat());
        [ftyp, mdat, moov].concat()
    }

    /// Writes a short HAP clip of `frames` frames; returns the first RGBA picture.
    pub fn write_clip(
        path: &std::path::Path,
        kind: Kind,
        (w, h): (usize, usize),
        frames: usize,
        chunks: usize,
    ) -> std::io::Result<()> {
        let alpha = kind == Kind::HapAlpha;
        let packets: Vec<Vec<u8>> = (0..frames)
            .map(|t| packet(&picture(w, h, t, alpha), w, h, kind, chunks))
            .collect();
        let fourcc = match kind {
            Kind::Hap => b"Hap1",
            Kind::HapAlpha => b"Hap5",
            Kind::HapQ => b"HapY",
        };
        std::fs::write(path, mov(&packets, w as u32, h as u32, 25, fourcc))
    }
}

#[cfg(test)]
mod tests {
    use super::testgen::*;
    use super::*;

    #[test]
    fn parses_every_compressor_layout() {
        let (w, h) = (30usize, 18usize);
        let pic = picture(w, h, 0, true);
        for kind in [Kind::Hap, Kind::HapAlpha, Kind::HapQ] {
            let reference = parse(&packet(&pic, w, h, kind, 0), w as u32, h as u32).unwrap();
            assert_eq!((reference.tex_w, reference.tex_h), (32, 20));
            for chunks in [1, 2, 3, 7] {
                let f = parse(&packet(&pic, w, h, kind, chunks), w as u32, h as u32).unwrap();
                assert_eq!(f.color, reference.color, "chunks={chunks}");
                assert_eq!(f.format, reference.format);
            }
        }
    }

    #[test]
    fn chunked_snappy_header_is_read_from_the_spec_layout() {
        // Hand-built Hap frame: two chunks, one snappy and one raw, long (8 byte) header.
        let blocks: Vec<u8> = (0..32u8).collect(); // 4 blocks of BC1 = 8x8 picture
        let snapped = snap::raw::Encoder::new()
            .compress_vec(&blocks[..16])
            .unwrap();
        let mut data = snapped.clone();
        data.extend_from_slice(&blocks[16..]);
        let mut instr = wrap(0x02, &[0x0B, 0x0A]);
        let mut sz = (snapped.len() as u32).to_le_bytes().to_vec();
        sz.extend(16u32.to_le_bytes());
        instr.extend(wrap(0x03, &sz));
        let mut offs = 0u32.to_le_bytes().to_vec();
        offs.extend((snapped.len() as u32).to_le_bytes());
        instr.extend(wrap(0x04, &offs));
        let mut body = wrap(0x01, &instr);
        body.extend(&data);
        let mut frame = vec![0, 0, 0, 0xCB];
        frame.extend((body.len() as u32).to_le_bytes());
        frame.extend(&body);
        let f = parse(&frame, 8, 8).unwrap();
        assert_eq!(f.format, HapFormat::Dxt1);
        assert_eq!(f.color, blocks);
    }

    #[test]
    fn rejects_bad_offsets_and_sizes() {
        let pic = picture(16, 16, 0, false);
        let good = packet(&pic, 16, 16, Kind::Hap, 2);
        assert!(parse(&good, 16, 16).is_ok());
        assert!(parse(&good, 32, 32).is_err(), "size mismatch");
        assert!(parse(&good[..good.len() / 2], 16, 16).is_err(), "truncated");
        assert!(parse(&[1, 2], 16, 16).is_err());
    }

    #[test]
    fn rejects_a_huge_snappy_declaration_before_allocating() {
        // Snappy preamble: varint 2^32-1 (4 GB), then a few junk bytes.
        let lie = [0xFF, 0xFF, 0xFF, 0xFF, 0x0F, 0x00, 0x01, 0x02];
        let pkt = testgen::wrap(0xB1, &lie);
        let e = parse(&pkt, 16, 16).err().expect("must be refused");
        assert!(e.contains("declares"), "{e}");
        // Also through the chunked layout.
        let mut instr = testgen::wrap(SEC_COMPRESSORS, &[COMP_SNAPPY]);
        instr.extend(testgen::wrap(SEC_SIZES, &(lie.len() as u32).to_le_bytes()));
        let mut body = testgen::wrap(SEC_INSTR, &instr);
        body.extend(lie);
        assert!(parse(&testgen::wrap(0xC1, &body), 16, 16).is_err());
    }

    #[test]
    fn ffmpeg_decodes_what_the_generator_writes() {
        for kind in [Kind::Hap, Kind::HapAlpha, Kind::HapQ] {
            for chunks in [0, 1, 2] {
                let p = temp("gen.mov");
                write_clip(&p, kind, (64, 32), 3, chunks).unwrap();
                let n = crate::decode_n(&p, 3).map(|f| f.len());
                assert_eq!(n.ok(), Some(3), "kind={} chunks={chunks}", kind as u8);
            }
        }
    }

    static FLAG: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn temp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("vjc-hap-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d.join(name)
    }

    fn wait_frame(pl: &crate::Player) -> crate::Frame {
        let t = std::time::Instant::now();
        loop {
            if let Some(f) = pl.latest_frame() {
                return f;
            }
            assert!(t.elapsed().as_secs() < 10, "{:?}", pl.error());
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    #[test]
    fn player_hands_out_compressed_frames_only_when_gpu_can_take_them() {
        let _g = FLAG.lock().unwrap_or_else(|e| e.into_inner());
        let p = temp("player.mov");
        write_clip(&p, Kind::HapAlpha, (62, 34), 5, 2).unwrap();
        set_gpu_supported(true);
        let on = crate::Player::open_scrub(&p);
        let f = wait_frame(&on);
        assert!(f.hap.is_some(), "{:?}", on.error());
        assert_eq!((f.width, f.height), (62, 34));
        set_gpu_supported(false);
        let off = crate::Player::open_scrub(&p);
        let f = wait_frame(&off);
        assert!(f.hap.is_none(), "fallback must decode to RGBA");
        assert_eq!(f.data.len(), 62 * 34 * 4);
    }

    #[test]
    fn snapshot_frame_is_rgba_even_for_a_compressed_hap_player() {
        let _g = FLAG.lock().unwrap_or_else(|e| e.into_inner());
        let p = temp("snap.mov");
        write_clip(&p, Kind::HapAlpha, (62, 34), 5, 2).unwrap();
        set_gpu_supported(true);
        let f = crate::snapshot_frame(&p, 0.5).expect("frame");
        set_gpu_supported(false);
        assert!(f.hap.is_none());
        assert_eq!((f.width, f.height), (62, 34));
        assert_eq!(f.data.len(), 62 * 34 * 4);
        assert!(f.data.iter().any(|b| *b != 0), "picture must not be empty");
    }

    /// Prints CPU time per 1080p frame: HAP parse (snappy) against FFmpeg RGBA decode.
    #[test]
    #[ignore = "benchmark; cargo test -p vjc-media hap_bench --release -- --ignored --nocapture"]
    fn hap_bench() {
        let (w, h) = (1920usize, 1080usize);
        let p = temp("bench.mov");
        write_clip(&p, Kind::HapAlpha, (w, h), 30, 4).unwrap();
        let pic = picture(w, h, 3, true);
        let pkt = packet(&pic, w, h, Kind::HapAlpha, 4);
        let t = std::time::Instant::now();
        for _ in 0..60 {
            std::hint::black_box(parse(&pkt, w as u32, h as u32).unwrap());
        }
        let hap_wall = t.elapsed().as_secs_f64() / 60.0;
        let t = std::time::Instant::now();
        let frames = crate::decode_n(&p, 30).unwrap();
        let ff = t.elapsed().as_secs_f64() / frames.len() as f64;
        eprintln!(
            "1080p Hap Alpha, per frame: new path parse+snappy {:.2} ms wall ({} KB), FFmpeg RGBA {:.2} ms",
            hap_wall * 1e3,
            pkt.len() / 1024,
            ff * 1e3
        );
    }
}
