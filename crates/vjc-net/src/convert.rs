//! Pixel format helpers shared by the backends. Pure, so they are unit-tested.

/// Copies `h` rows of `w` pixels (4 bytes each) out of a buffer with `stride` bytes per row.
/// Returns None if the buffer is too short or the geometry is invalid.
pub fn pack_rows(src: &[u8], w: u32, h: u32, stride: u32) -> Option<Vec<u8>> {
    let (w, h, stride) = (w as usize, h as usize, stride as usize);
    let row = w.checked_mul(4)?;
    if row == 0 || h == 0 || stride < row {
        return None;
    }
    let need = stride.checked_mul(h - 1)?.checked_add(row)?;
    if src.len() < need {
        return None;
    }
    let mut out = Vec::with_capacity(row * h);
    for y in 0..h {
        out.extend_from_slice(&src[y * stride..y * stride + row]);
    }
    Some(out)
}

/// Swaps B and R in place (BGRA <-> RGBA).
pub fn swap_rb(px: &mut [u8]) {
    for p in px.as_chunks_mut::<4>().0 {
        p.swap(0, 2);
    }
}

/// Sets alpha to 255 (for X-channel formats whose fourth byte is undefined).
pub fn opaque(px: &mut [u8]) {
    for p in px.as_chunks_mut::<4>().0 {
        p[3] = 255;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn swaps_red_and_blue() {
        let mut p = vec![1, 2, 3, 4, 5, 6, 7, 8];
        swap_rb(&mut p);
        assert_eq!(p, vec![3, 2, 1, 4, 7, 6, 5, 8]);
    }

    #[test]
    fn packs_strided_rows() {
        let mut src = vec![0u8; 12 * 2];
        src[..8].fill(1);
        src[12..20].fill(2);
        let out = pack_rows(&src, 2, 2, 12).unwrap();
        assert_eq!(out, [vec![1u8; 8], vec![2u8; 8]].concat());
    }

    #[test]
    fn pack_rejects_bad_geometry() {
        assert!(pack_rows(&[0; 8], 2, 2, 8).is_none()); // too short
        assert!(pack_rows(&[0; 64], 2, 2, 4).is_none()); // stride < row
        assert!(pack_rows(&[0; 64], 0, 2, 8).is_none());
    }

    #[test]
    fn opaque_sets_alpha() {
        let mut p = vec![9, 9, 9, 0];
        opaque(&mut p);
        assert_eq!(p[3], 255);
    }
}
