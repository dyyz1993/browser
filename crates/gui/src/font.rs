//! M7.4.2: FontCache — fontduerasterization → alpha mask.

use fontdue::Font;

/// Font cache using fontdue for glyph rasterization.
#[derive(Debug)]
pub struct FontCache {
    font: Font,
}

impl FontCache {
    /// Create a new cache with the default font (fontdue's built-in
    /// fallback). M7.4.1 will embed a real font file.
    pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
        // fontdue 0.9 includes a default font (IBMPlexSans-Regular.ttf
        // or similar) as a const byte slice.
        let font_data = include_bytes!("../assets/font.ttf");
        let font = Font::from_bytes(font_data.as_ref(), fontdue::FontSettings::default())?;
        Ok(Self { font })
    }

    /// Rasterize a character into an 8-bit alpha mask (1 byte per pixel).
    /// Returns (width, height, mask_vec) where mask_vec.len() = width * height.
    pub fn rasterize_char(&self, c: char) -> Option<(usize, usize, Vec<u8>)> {
        self.font
            .rasterize(c, fontdue::FontSettings::default())
            .map(|(metrics, bitmap)| {
                (
                    metrics.width,
                    metrics.height,
                    bitmap, // Vec<u8> already.
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn font_cache_rasterizes_a() {
        let cache = FontCache::new().expect("font file missing");
        let (w, h, mask) = cache
            .rasterize_char('A')
            .expect("A should rasterize");
        // Check non-empty.
        assert!(w > 0 && h > 0);
        assert_eq!(mask.len(), w * h);
        // Check at least one non-zero pixel.
        assert!(mask.iter().any(|&b| b > 0));
    }

    #[test]
    fn font_cache_handles_unknown_char() {
        let cache = FontCache::new().expect("font file missing");
        // U+1F4A9 (💩) unlikely in embedded font.
        let result = cache.rasterize_char('\u{1F4A9}');
        assert!(result.is_none());
    }
}
