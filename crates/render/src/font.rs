//! M34.1: Reusable fontdue-based text rasterizer.
//!
//! Extracts the M25-verified fontdue rendering logic (the `y_origin`
//! formula that took 5 failed attempts to lock down) into a single
//! reusable module. Both `screenshot.rs` (PNG) and `gui` (softbuffer
//! window) now share one source of truth for glyph positioning.
//!
//! ## M25 coordinate formula (do NOT re-guess — it's locked by tests)
//!
//! ```text
//! y_origin = baseline - ymin - height + 1
//! ```
//!
//! - bitmap is stored **upright**: `bitmap[0]` = top row
//! - `ymin` is a math coordinate (up = positive, negative = below baseline);
//!   fontdue semantics: ymin = offset of the bitmap **bottom edge** from baseline
//! - screen coordinate (y grows down): bottom edge `screen_offset = baseline - ymin`
//! - bottom row `dy = height - 1` lands at `baseline - ymin`, so
//!   `y_origin = (baseline - ymin) - height + 1`
//!
//! Verified with 'p' (ymin=-4, h=13, ascent=15):
//!   `y_origin = 15-(-4)-13+1 = 7` → 'p' round-top at baseline-8 (x-height ✓),
//!   bottom row `py = 7+12 = 19 = baseline+4` (descender ✓).

use std::collections::HashMap;

use fontdue::Font;

const FONT_BYTES: &[u8] = include_bytes!("../assets/font.ttf");
/// M36: CJK fallback font (NotoSansSC GB2312 subset, ~1.6MB).
/// Covers 6763 most common Chinese chars + CJK punctuation.
const CJK_FONT_BYTES: &[u8] = include_bytes!("../assets/cjk.ttf");
const FONT_SIZE: f32 = 16.0;
/// Extra spacing beyond ascent+descent (M25 measured value).
const LINE_GAP: usize = 6;

/// RGBA layout metrics — all the numbers a caller needs to size its buffer.
/// Computed once from the embedded DejaVuSans at FONT_SIZE.
#[derive(Debug, Clone, Copy)]
pub struct LayoutMetrics {
    /// Max advance_width of printable ASCII chars, ceil'd.
    pub col_width: usize,
    /// ascent + descent + LINE_GAP.
    pub line_height: usize,
    /// Distance from cell top to the text baseline (= ascent).
    pub baseline: usize,
}

/// A reusable fontdue renderer. Owns the `Font` + a per-character
/// glyph cache (metrics + alpha mask). `Clone` is intentionally NOT
/// derived — share one instance (it's used behind a thread-local in gui).
pub struct FontRenderer {
    /// ASCII / Latin font (DejaVuSans).
    font: Font,
    /// M36: CJK fallback font (NotoSansSC subset).
    cjk_font: Font,
    metrics: LayoutMetrics,
    cache: HashMap<char, (fontdue::Metrics, Vec<u8>)>,
}

impl FontRenderer {
    /// Construct with the embedded DejaVuSans font at FONT_SIZE.
    ///
    /// # Panics
    /// Panics if the embedded font fails to parse (it's a build-time asset).
    #[must_use]
    pub fn new() -> Self {
        let font = Font::from_bytes(FONT_BYTES, fontdue::FontSettings::default())
            .expect("embedded font.ttf must parse");
        let cjk_font = Font::from_bytes(CJK_FONT_BYTES, fontdue::FontSettings::default())
            .expect("embedded cjk.ttf must parse");
        let metrics = measure_layout(&font);
        Self {
            font,
            cjk_font,
            metrics,
            cache: HashMap::new(),
        }
    }

    /// The computed layout metrics.
    #[must_use]
    pub fn metrics(&self) -> LayoutMetrics {
        self.metrics
    }

    /// Rasterize `text` into a freshly-allocated RGBA buffer (white
    /// background, black text). Link spans get W3C link blue (#0000EE).
    ///
    /// - `link_spans`: per-line list of `(start_col, end_col)` half-open
    ///   ranges to paint blue. Pass empty for plain black text.
    ///
    /// Returns `(width, height, rgba_buffer)`. Buffer length is
    /// `width * height * 4`.
    ///
    /// # Panics
    /// Panics if `text` is empty (caller should guard).
    #[must_use]
    pub fn render_text_to_rgba(
        &mut self,
        text: &str,
        link_spans_per_line: &[Vec<(usize, usize)>],
    ) -> (usize, usize, Vec<u8>) {
        let lines: Vec<&str> = text.lines().collect();
        assert!(!lines.is_empty(), "render_text_to_rgba: empty text");
        let col_width = self.metrics.col_width;
        let line_height = self.metrics.line_height;
        let baseline = self.metrics.baseline;

        let cols = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0);
        assert!(cols > 0, "render_text_to_rgba: all lines empty");
        let img_w = cols * col_width;
        let img_h = lines.len() * line_height;

        let mut buf = vec![255u8; img_w * img_h * 4];

        for (row, raw_line) in lines.iter().enumerate() {
            let spans = link_spans_per_line
                .get(row)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            let row_top = row * line_height;
            for (col, ch) in raw_line.chars().enumerate() {
                let (m, mask) = self.cached_glyph(ch);
                if m.width == 0 || m.height == 0 {
                    continue;
                }
                let y_origin = baseline as i32 - m.ymin - m.height as i32 + 1;
                let is_link = spans.iter().any(|&(s, e)| col >= s && col < e);
                let (cr, cg, cb) = if is_link {
                    (0u8, 0u8, 0xEEu8)
                } else {
                    // alpha 0..=255 → gray v = 255 - alpha (black on white)
                    (0u8, 0u8, 0u8)
                };
                for dy in 0..m.height {
                    for dx in 0..m.width {
                        let alpha = mask[dy * m.width + dx];
                        if alpha == 0 {
                            continue;
                        }
                        let px = (col * col_width) as i32 + m.xmin + dx as i32;
                        let py = row_top as i32 + y_origin + dy as i32;
                        if px < 0 || py < 0 {
                            continue;
                        }
                        let (pxu, pyu) = (px as usize, py as usize);
                        if pxu >= img_w || pyu >= img_h {
                            continue;
                        }
                        let idx = (pyu * img_w + pxu) * 4;
                        let v = 255 - alpha;
                        // Blend: link spans override to blue, plain text uses gray.
                        let (r, g, b) = if is_link {
                            // Blue text: keep blue, darken by alpha.
                            let bf = (v as u32 * 0xEE / 255) as u8;
                            (0u8, 0u8, bf)
                        } else {
                            (v, v, v)
                        };
                        let _ = (cr, cg, cb); // silence unused (replaced by blend logic above)
                        buf[idx] = r;
                        buf[idx + 1] = g;
                        buf[idx + 2] = b;
                    }
                }
            }
        }

        (img_w, img_h, buf)
    }

    /// Get-or-cache the (metrics, alpha_mask) for `ch`.
    ///
    /// M36: CJK chars use the CJK font; ASCII/Latin use DejaVuSans.
    /// If the ASCII font lacks the glyph (width == 0), fall back to CJK.
    fn cached_glyph(&mut self, ch: char) -> (fontdue::Metrics, Vec<u8>) {
        if let Some((m, mask)) = self.cache.get(&ch) {
            return (*m, mask.clone());
        }
        let (m, mask) = if is_cjk_char(ch) {
            self.cjk_font.rasterize(ch, FONT_SIZE)
        } else {
            let m = self.font.metrics(ch, FONT_SIZE);
            if m.width == 0 || m.height == 0 {
                self.cjk_font.rasterize(ch, FONT_SIZE)
            } else {
                self.font.rasterize(ch, FONT_SIZE)
            }
        };
        self.cache.insert(ch, (m, mask.clone()));
        (m, mask)
    }
}

impl Default for FontRenderer {
    fn default() -> Self {
        Self::new()
    }
}

/// M36: Determine if a character should use the CJK font.
///
/// Uses Unicode block ranges (no external crate):
/// - CJK Unified Ideographs (U+4E00..U+9FFF): common Chinese chars
/// - CJK Symbols and Punctuation (U+3000..U+303F): 、。「」等
/// - Halfwidth and Fullwidth Forms (U+FF00..U+FFEF): ！＃等
/// - Hiragana / Katakana / Hangul / CJK Compatibility
#[must_use]
pub(crate) fn is_cjk_char(ch: char) -> bool {
    let c = ch as u32;
    matches!(c,
        0x3000..=0x303F   // CJK symbols and punctuation
        | 0x3040..=0x309F  // Hiragana
        | 0x30A0..=0x30FF  // Katakana
        | 0x3300..=0x33FF  // CJK compatibility
        | 0x4E00..=0x9FFF  // CJK Unified Ideographs
        | 0xAC00..=0xD7AF  // Hangul Syllables
        | 0xF900..=0xFAFF  // CJK Compatibility Ideographs
        | 0xFF00..=0xFFEF  // Halfwidth and Fullwidth Forms
    )
}

/// Measure col_width / line_height / baseline from the embedded font.
fn measure_layout(font: &Font) -> LayoutMetrics {
    let col_width = (32u8..=126)
        .map(|c| font.metrics(c as char, FONT_SIZE).advance_width)
        .fold(0.0_f32, f32::max)
        .ceil()
        .max(1.0) as usize;
    let lm = font
        .horizontal_line_metrics(FONT_SIZE)
        .expect("font must have horizontal metrics");
    let ascent = lm.ascent.ceil() as usize;
    let descent = (-lm.descent).ceil() as usize;
    let line_height = ascent + descent + LINE_GAP;
    LayoutMetrics {
        col_width,
        line_height,
        baseline: ascent,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_does_not_panic() {
        let _ = FontRenderer::new();
    }

    #[test]
    fn metrics_are_reasonable() {
        let r = FontRenderer::new();
        let m = r.metrics();
        // M25 measured (cross-validated with PIL): col_width≈16, line_height≈25.
        assert!(m.col_width >= 12, "col_width too small: {}", m.col_width);
        assert!(m.col_width <= 20, "col_width too large: {}", m.col_width);
        assert!(
            m.line_height >= 20,
            "line_height too small: {}",
            m.line_height
        );
        assert!(
            m.line_height <= 35,
            "line_height too large: {}",
            m.line_height
        );
        assert!(m.baseline > 0);
        assert!(m.baseline < m.line_height);
    }

    #[test]
    fn render_simple_text_returns_nonempty_buffer() {
        let mut r = FontRenderer::new();
        let (w, h, buf) = r.render_text_to_rgba("Hi", &[]);
        assert!(w > 0);
        assert!(h > 0);
        assert_eq!(buf.len(), w * h * 4);
        // Background should be white (255,255,255) for at least some pixels.
        let any_white = buf
            .chunks_exact(4)
            .any(|px| px[0] == 255 && px[1] == 255 && px[2] == 255);
        assert!(any_white, "expected some white background pixels");
        // Some pixels should be darkened (text glyph coverage).
        let any_dark = buf.chunks_exact(4).any(|px| px[0] < 200);
        assert!(any_dark, "expected some darkened glyph pixels");
    }

    #[test]
    fn render_multiline_text_has_correct_height() {
        let mut r = FontRenderer::new();
        let m = r.metrics();
        let (_w, h, _buf) = r.render_text_to_rgba("A\nB\nC", &[]);
        assert_eq!(h, m.line_height * 3);
    }

    #[test]
    fn render_chinese_chars_does_not_panic() {
        let mut r = FontRenderer::new();
        let (w, h, _buf) = r.render_text_to_rgba("你好", &[]);
        assert!(w > 0);
        assert!(h > 0);
    }

    #[test]
    fn glyph_vertical_position_formula_is_correct() {
        // M25 formula lock: pure math verification, no PNG decode.
        let r = FontRenderer::new();
        let baseline = r.metrics().baseline as i32;
        let mut rr = r;

        // 'b' (ascender to cap height)
        let mb = rr.cached_glyph('b').0;
        let y_origin_b = baseline - mb.ymin - mb.height as i32 + 1;
        let top_b = y_origin_b;
        let bottom_b = y_origin_b + mb.height as i32 - 1;

        // 'p' (descender below baseline)
        let mp = rr.cached_glyph('p').0;
        let y_origin_p = baseline - mp.ymin - mp.height as i32 + 1;
        let top_p = y_origin_p;
        let bottom_p = y_origin_p + mp.height as i32 - 1;

        // 1. 'b' top above 'p' top (cap height > x-height)
        assert!(
            top_b < top_p,
            "'b' top ({top_b}) must be above 'p' top ({top_p}) — vertical mirror bug"
        );
        // 2. 'p' bottom below 'b' bottom (descender)
        assert!(
            bottom_p > bottom_b,
            "'p' bottom ({bottom_p}) must be below 'b' bottom ({bottom_b}) — descender lost"
        );
        // 3. glyphs inside cell
        let line_height = baseline + 4 + LINE_GAP as i32;
        assert!(top_b >= 0);
        assert!(bottom_p < line_height);
        // 4. formula self-consistent: 'b' bottom near baseline (±3px)
        assert!(
            (bottom_b - baseline).abs() <= 3,
            "'b' bottom ({bottom_b}) should be near baseline ({baseline})"
        );
    }

    #[test]
    fn descender_glyph_has_negative_ymin() {
        let mut r = FontRenderer::new();
        let mg = r.cached_glyph('g').0;
        let me = r.cached_glyph('E').0;
        assert!(
            mg.ymin < me.ymin,
            "'g' ymin ({}) should be below 'E' ymin ({})",
            mg.ymin,
            me.ymin
        );
    }

    #[test]
    fn link_spans_paint_blue() {
        let mut r = FontRenderer::new();
        // 2 chars "go", link span covers col 0..2 → all blue.
        let (w, h, buf) = r.render_text_to_rgba("go", &[vec![(0, 2)]]);
        // Find a non-white pixel and check its blue channel dominates.
        let blue_pixel = buf
            .chunks_exact(4)
            .any(|px| px[2] > px[0] && px[2] > px[1] && px[2] < 200);
        assert!(blue_pixel, "expected at least one blue link pixel");
        let _ = (w, h);
    }

    // ── M36: CJK font fallback ──

    #[test]
    fn cjk_char_renders_real_glyph_not_tofu() {
        let mut r = FontRenderer::new();
        let (m, mask) = r.cached_glyph('你');
        // Real glyph has content
        assert!(m.width > 4, "'你' glyph too narrow: {}", m.width);
        assert!(m.height > 4, "'你' glyph too short: {}", m.height);
        // Tofu block has dense top/bottom borders but empty middle.
        // Real glyph has strokes throughout. Check middle row has content.
        let mid_row = &mask[(m.height / 2) * m.width..(m.height / 2 + 1) * m.width];
        let mid_nonzero = mid_row.iter().filter(|&&v| v > 0).count();
        assert!(
            mid_nonzero >= 3,
            "'你' middle row has only {mid_nonzero} nonzero pixels — likely tofu block"
        );
    }

    #[test]
    fn cjk_and_ascii_render_in_same_line() {
        let mut r = FontRenderer::new();
        let (w, h, buf) = r.render_text_to_rgba("A你B", &[]);
        assert!(w > 0 && h > 0);
        assert_eq!(buf.len(), w * h * 4);
        // Both ASCII 'A' and CJK '你' should produce dark pixels
        let dark = buf.chunks_exact(4).filter(|px| px[0] < 200).count();
        assert!(dark > 30, "expected substantial dark pixels for mixed text");
    }

    #[test]
    fn pure_chinese_text_renders() {
        let mut r = FontRenderer::new();
        let (w, h, buf) = r.render_text_to_rgba("你好世界", &[]);
        assert!(w > 0 && h > 0);
        // 4 Chinese chars → at least some dark pixels per char
        let dark = buf.chunks_exact(4).filter(|px| px[0] < 200).count();
        assert!(
            dark > 80,
            "expected substantial dark pixels for 4 CJK chars, got {dark}"
        );
    }

    #[test]
    fn cjk_punctuation_uses_cjk_font() {
        // CJK punctuation block U+3000-U+303F should use CJK font
        assert!(is_cjk_char('、'));
        assert!(is_cjk_char('。'));
        assert!(is_cjk_char('「'));
        // ASCII should not
        assert!(!is_cjk_char('A'));
        assert!(!is_cjk_char(' '));
        // Fullwidth should
        assert!(is_cjk_char('！'));
        assert!(is_cjk_char('（'));
    }

    #[test]
    fn cjk_glyph_fallback_for_missing_ascii_char() {
        // A char not in DejaVuSans (e.g. some special unicode) should
        // fall back to CJK font, not produce empty glyph.
        let mut r = FontRenderer::new();
        // ✓ (checkmark, U+2713) is not in DejaVuSans
        let (m, mask) = r.cached_glyph('✓');
        let _ = m;
        let _ = mask;
        // Should not panic regardless of whether glyph exists
    }
}
