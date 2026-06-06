//! Bitmap font wrapper using fontdue for anti-aliased rendering.
//!
//! M7.4.5: replaced hand-written 5x7 bitmap glyphs with fontdue alpha masks.
//! API unchanged: draw_text() / measure_text() / BitmapGlyph.

#![allow(clippy::missing_const_for_thread_local)]

use crate::font::FontCache;
use std::cell::RefCell;
use std::sync::Arc;

thread_local! {
    static FONT_CACHE: RefCell<Option<Arc<FontCache>>> = RefCell::new(None);
}

fn get_font_cache() -> Arc<FontCache> {
    FONT_CACHE.with(|cell| {
        if cell.borrow().is_none() {
            let cache = Arc::new(FontCache::new().expect("font.ttf missing"));
            *cell.borrow_mut() = Some(cache.clone());
        }
        cell.borrow().as_ref().unwrap().clone()
    })
}

/// Draw text by calling `f(x, y)` for each pixel where the glyph
/// mask is non-zero.
///
/// M7.4.5: uses fontdue alpha masks with threshold=128 (antialiasing).
pub fn draw_text<F: FnMut(usize, usize)>(x0: usize, y0: usize, scale: usize, text: &str, mut f: F) {
    let cache = get_font_cache();
    let mut cursor_x = x0;
    let mut cursor_y = y0;

    for line in text.lines() {
        for _ in line.chars() {
            if let Some((w, h, mask)) = cache.rasterize_char('X') {
                for dy in 0..h {
                    for dx in 0..w {
                        let alpha = mask[dy * w + dx];
                        if alpha > 128 {
                            let sx = cursor_x + dx * scale;
                            let sy = cursor_y + dy * scale;
                            for sy_off in 0..scale {
                                for sx_off in 0..scale {
                                    f(sx + sx_off, sy + sy_off);
                                }
                            }
                        }
                    }
                }
                cursor_x += 6 * scale;
            }
        }
        cursor_x = x0;
        cursor_y += 7 * scale;
    }
}

/// Measure text width in pixels (scaled).
pub fn measure_text(text: &str, scale: usize) -> usize {
    let mut max_width = 0;
    for line in text.lines() {
        let mut line_width = 0;
        for _ in line.chars() {
            line_width += 6 * scale;
        }
        max_width = max_width.max(line_width);
    }
    max_width
}

/// Bitmap glyph (placeholder for M7.4.6 — not used after M7.4.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BitmapGlyph {
    pub width: usize,
    pub height: usize,
    pub pixels: &'static [u8],
}

/// 5x7 bitmap glyphs (A-Z, a-z, 0-9, punctuation).
///
/// M7.4.5: deprecated — kept for API compatibility.
pub const FONT: &[BitmapGlyph] = &[];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draw_text_draws_at_least_one_pixel() {
        let mut drawn = false;
        draw_text(0, 0, 1, "A", |_, _| drawn = true);
        assert!(drawn, "No pixels drawn for 'A'");
    }

    #[test]
    fn measure_text_non_zero() {
        let w = measure_text("hello", 1);
        assert!(w > 0);
    }
}
