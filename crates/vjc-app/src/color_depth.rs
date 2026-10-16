//! Texture formats for the composition bit depth and the output surface.

use eframe::egui_wgpu::wgpu::TextureFormat;

/// Format of the composition's render textures for a bit depth (16 = half float).
pub fn pool_format(bit_depth: u8) -> TextureFormat {
    if bit_depth == 16 {
        TextureFormat::Rgba16Float
    } else {
        TextureFormat::Rgba8Unorm
    }
}

/// Picks the format of an output surface from the ones it supports. With `want_10bit` and
/// `Rgb10a2Unorm` available that is the one; otherwise an 8-bit format, and the second
/// value says why the request was not met (for the log).
pub fn select_output_format(
    want_10bit: bool,
    supported: &[TextureFormat],
) -> (TextureFormat, Option<String>) {
    if want_10bit && supported.contains(&TextureFormat::Rgb10a2Unorm) {
        return (TextureFormat::Rgb10a2Unorm, None);
    }
    let eight = [TextureFormat::Bgra8Unorm, TextureFormat::Rgba8Unorm]
        .into_iter()
        .find(|f| supported.contains(f))
        .or_else(|| supported.first().copied())
        .unwrap_or(TextureFormat::Bgra8Unorm);
    let note = want_10bit.then(|| {
        format!("10-bit output unavailable (surface offers {supported:?}); using {eight:?}")
    });
    (eight, note)
}

#[cfg(test)]
mod tests {
    use super::*;
    use TextureFormat::{Bgra8Unorm, Rgb10a2Unorm, Rgba8Unorm, Rgba16Float};

    #[test]
    fn pool_follows_the_bit_depth() {
        assert_eq!(pool_format(8), Rgba8Unorm);
        assert_eq!(pool_format(16), Rgba16Float);
        assert_eq!(pool_format(0), Rgba8Unorm);
    }

    #[test]
    fn ten_bit_is_used_only_when_asked_and_supported() {
        let both = [Bgra8Unorm, Rgb10a2Unorm];
        assert_eq!(select_output_format(true, &both), (Rgb10a2Unorm, None));
        // Not asked: stays 8-bit and says nothing.
        assert_eq!(select_output_format(false, &both), (Bgra8Unorm, None));
        // Asked but missing: falls back and explains.
        let (f, note) = select_output_format(true, &[Rgba8Unorm]);
        assert_eq!(f, Rgba8Unorm);
        assert!(note.unwrap().contains("10-bit output unavailable"));
        let (f, note) = select_output_format(true, &[]);
        assert_eq!(f, Bgra8Unorm);
        assert!(note.is_some());
    }
}
