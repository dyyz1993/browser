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

/// M39: per-line background span = `(start_col, end_col, (r, g, b))` half-open range.
pub type BgSpan = (usize, usize, (u8, u8, u8));
/// M39: per-line background spans.
pub type BgSpans = Vec<BgSpan>;
/// M70: per-line foreground (CSS color) span = `(start_col, end_col, (r, g, b))` half-open range.
pub type FgSpan = (usize, usize, (u8, u8, u8));
/// M70: per-line foreground spans.
pub type FgSpans = Vec<FgSpan>;

// M80: pub(crate) — pixel.rs (近似像素渲染器) 复用同一对内嵌字体，
// 仅放开可见性，ASCII 路径行为零变化。
pub(crate) const FONT_BYTES: &[u8] = include_bytes!("../assets/font.ttf");
/// M36: CJK fallback font (NotoSansSC GB2312 subset, ~1.6MB).
/// Covers 6763 most common Chinese chars + CJK punctuation.
pub(crate) const CJK_FONT_BYTES: &[u8] = include_bytes!("../assets/cjk.ttf");
const FONT_SIZE: f32 = 16.0;
/// Extra spacing beyond ascent+descent (M25 measured value).
pub(crate) const LINE_GAP: usize = 6;

/// M72: 默认背景色（白，与 RGBA 缓冲区初始化一致）。
const WHITE: (u8, u8, u8) = (255, 255, 255);
/// M30: W3C link 蓝 #0000EE。
const LINK_BLUE: (u8, u8, u8) = (0, 0, 238);
/// M72: 对比度门槛（WCAG AA 大字号标准 3.0:1）。低于此值自动翻转文字色。
const MIN_CONTRAST: f64 = 3.0;

/// M72: sRGB 单通道线性化（WCAG 2.x 相对亮度用）。
fn srgb_channel(c: u8) -> f64 {
    let c = f64::from(c) / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// M72: WCAG 2.x 相对亮度 `L = 0.2126R + 0.7152G + 0.0722B`（线性化后）。
fn relative_luminance(rgb: (u8, u8, u8)) -> f64 {
    0.2126 * srgb_channel(rgb.0) + 0.7152 * srgb_channel(rgb.1) + 0.0722 * srgb_channel(rgb.2)
}

/// M72: WCAG 对比度 `(L1+0.05)/(L2+0.05)`，值域 [1, 21]。
fn contrast_ratio(l1: f64, l2: f64) -> f64 {
    let (hi, lo) = if l1 >= l2 { (l1, l2) } else { (l2, l1) };
    (hi + 0.05) / (lo + 0.05)
}

/// M72: 对比度保障（暗色主题截图可读性的核心）。
///
/// 墨色与背景的 WCAG 对比度 ≥ [`MIN_CONTRAST`] 时保留原色（尊重站点配色）；
/// 不足时**保背景色块不动**，把文字翻成黑/白中与背景对比更高的那个：
/// 暗底上的黑字/蓝字 → 白，白底上的浅灰字 → 黑。
#[must_use]
pub(crate) fn ensure_contrast(ink: (u8, u8, u8), bg: (u8, u8, u8)) -> (u8, u8, u8) {
    let l_bg = relative_luminance(bg);
    if contrast_ratio(relative_luminance(ink), l_bg) >= MIN_CONTRAST {
        return ink;
    }
    let black = contrast_ratio(0.0, l_bg);
    let white = contrast_ratio(1.0, l_bg);
    if white >= black {
        (255, 255, 255)
    } else {
        (0, 0, 0)
    }
}

/// M72: 查找该列所在格子的背景色（span 按列的半开区间匹配）。
fn bg_at(spans: &[(usize, usize, (u8, u8, u8))], col: usize) -> Option<(u8, u8, u8)> {
    spans
        .iter()
        .find(|&&(s, e, _)| col >= s && col < e)
        .map(|&(_, _, c)| c)
}

/// M70: 查找该列的 CSS 前景色。
fn fg_color_of(spans: &[(usize, usize, (u8, u8, u8))], col: usize) -> Option<(u8, u8, u8)> {
    spans
        .iter()
        .find(|&&(s, e, _)| col >= s && col < e)
        .map(|&(_, _, c)| c)
}

/// M72: 单通道 alpha 合成 `out = ink*a + bg*(1-a)`（`a`: 0..=255 coverage）。
fn blend_channel(ink: u8, bg: u8, a: u32) -> u8 {
    ((u32::from(ink) * a + u32::from(bg) * (255 - a)) / 255) as u8
}

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

    /// Rasterize `text` into a freshly-allocated RGBA buffer (default
    /// white background, black text). Link spans get W3C link blue (#0000EE).
    ///
    /// - `link_spans`: per-line list of `(start_col, end_col)` half-open
    ///   ranges to paint blue. Pass empty for plain black text.
    /// - `bg_spans`: M39 per-line list of `(start_col, end_col, (r,g,b))`
    ///   half-open ranges to fill background color.
    /// - `fg_spans`: M70 per-line list of `(start_col, end_col, (r,g,b))`
    ///   half-open ranges to paint text in CSS color (overrides default black).
    ///   Priority: link > fg > default black.
    ///
    /// M72 (dark-theme readability): glyph pixels are alpha-composited
    /// against the actual background color under each cell
    /// (`out = ink*a + bg*(1-a)`), and when the ink-vs-background WCAG
    /// contrast ratio is below [`MIN_CONTRAST`] the ink is flipped to
    /// black/white (whichever contrasts more) — background blocks are kept.
    /// This keeps light text visible on dark backgrounds and vice versa.
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
        bg_spans_per_line: &[BgSpans],
        fg_spans_per_line: &[FgSpans],
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

        // M39: 先填充背景色（在画文字之前，文字会覆盖在背景之上）。
        for (row, _line) in lines.iter().enumerate() {
            let bgs = bg_spans_per_line.get(row).map(Vec::as_slice).unwrap_or(&[]);
            let row_top = row * line_height;
            for &(start, end, (br, bg, bb)) in bgs {
                let x0 = start * col_width;
                let x1 = end * col_width;
                for py in row_top..row_top + line_height {
                    for px in x0..x1 {
                        if px >= img_w || py >= img_h {
                            continue;
                        }
                        let idx = (py * img_w + px) * 4;
                        buf[idx] = br;
                        buf[idx + 1] = bg;
                        buf[idx + 2] = bb;
                        buf[idx + 3] = 255;
                    }
                }
            }
        }

        for (row, raw_line) in lines.iter().enumerate() {
            let spans = link_spans_per_line
                .get(row)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            let fg_spans = fg_spans_per_line.get(row).map(Vec::as_slice).unwrap_or(&[]);
            let bgs = bg_spans_per_line.get(row).map(Vec::as_slice).unwrap_or(&[]);
            let row_top = row * line_height;
            for (col, ch) in raw_line.chars().enumerate() {
                let (m, mask) = self.cached_glyph(ch);
                if m.width == 0 || m.height == 0 {
                    continue;
                }
                let y_origin = baseline as i32 - m.ymin - m.height as i32 + 1;
                let is_link = spans.iter().any(|&(s, e)| col >= s && col < e);
                // M70: 查找该列的 CSS 前景色（仅非 link 时生效）。
                // M72: 墨色优先级 link(蓝) > CSS fg > 默认黑。
                let ink = if is_link {
                    LINK_BLUE
                } else {
                    fg_color_of(fg_spans, col).unwrap_or((0, 0, 0))
                };
                // M72: 按 cell 记忆（bg 查找 + WCAG 对比度计算），避免逐像素重复算。
                let mut memo_cell = usize::MAX;
                let mut memo = (WHITE, (0, 0, 0)); // (bg, adjusted ink)
                for dy in 0..m.height {
                    for dx in 0..m.width {
                        let alpha = u32::from(mask[dy * m.width + dx]); // coverage, 255 = full ink
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
                        // M72: 逐 cell 查背景（bg span 覆盖整格，格子序号 = px / col_width；
                        // 无 span 视为白底，与缓冲区初始化一致）。
                        let cell = pxu / col_width;
                        if cell != memo_cell {
                            let bg = bg_at(bgs, cell).unwrap_or(WHITE);
                            memo = (bg, ensure_contrast(ink, bg));
                            memo_cell = cell;
                        }
                        let (bg, ink_adj) = memo;
                        let idx = (pyu * img_w + pxu) * 4;
                        // M72: 正确的 alpha 合成 out = ink*a + bg*(1-a)。
                        // 旧实现 `v = 255 - alpha` 再乘墨色，墨色强度与 coverage **反向**：
                        // 彩色/浅色文字笔画核心（coverage=255）被画成黑色 —— 白底上看不出
                        // （近似深色文字），暗底上核心隐形（qwik.dev 截图根因）。
                        // 新公式在默认黑/白底下与旧结果完全一致（255-a），无回归。
                        buf[idx] = blend_channel(ink_adj.0, bg.0, alpha);
                        buf[idx + 1] = blend_channel(ink_adj.1, bg.1, alpha);
                        buf[idx + 2] = blend_channel(ink_adj.2, bg.2, alpha);
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
        let (w, h, buf) = r.render_text_to_rgba("Hi", &[], &[], &[]);
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
        let (_w, h, _buf) = r.render_text_to_rgba("A\nB\nC", &[], &[], &[]);
        assert_eq!(h, m.line_height * 3);
    }

    #[test]
    fn render_chinese_chars_does_not_panic() {
        let mut r = FontRenderer::new();
        let (w, h, _buf) = r.render_text_to_rgba("你好", &[], &[], &[]);
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
        let (w, h, buf) = r.render_text_to_rgba("go", &[vec![(0, 2)]], &[], &[]);
        // M72 fix: link core pixel is now saturated #0000EE (proper alpha
        // compositing). Old inverted formula made the core BLACK with only
        // blue fringe pixels (b<200) — update expectation accordingly.
        let blue_pixel = buf
            .chunks_exact(4)
            .any(|px| px[2] > px[0] && px[2] > px[1] && px[2] >= 200);
        assert!(
            blue_pixel,
            "expected at least one saturated blue link pixel"
        );
        let _ = (w, h);
    }

    #[test]
    fn fg_spans_paint_color() {
        // M70: CSS color via fg_spans → pixels take that color (not gray, not blue).
        let mut r = FontRenderer::new();
        // Pure red (255,0,0) over "go" cols 0..2.
        let (_w, _h, buf) = r.render_text_to_rgba("go", &[], &[], &[vec![(0, 2, (255, 0, 0))]]);
        // Expect a pixel with red dominant (red channel high, green/blue low).
        let red_pixel = buf
            .chunks_exact(4)
            .any(|px| px[0] > 100 && px[1] < px[0] && px[2] < px[0]);
        assert!(red_pixel, "expected at least one red fg pixel");
    }

    #[test]
    fn fg_spans_override_default_black() {
        // M70: without fg span text is black (gray). With a fg span it's colored.
        // Compare: default black text has equal RGB channels; colored does not.
        let mut r = FontRenderer::new();
        let (_, _, buf_default) = r.render_text_to_rgba("Hi", &[], &[], &[]);
        // green: some ink pixel with g > r. M72: uses WCAG-passing green
        // (0,150,0) = 3.9:1 on white; (0,200,0) would be 2.3:1 and correctly
        // flipped to black by the contrast guard.
        let (_, _, buf_green) = r.render_text_to_rgba("Hi", &[], &[], &[vec![(0, 2, (0, 150, 0))]]);
        // default: some ink pixel with r==g==b (gray)
        let has_gray_ink = buf_default
            .chunks_exact(4)
            .any(|px| px[0] == px[1] && px[1] == px[2] && px[0] < 200);
        // green: some ink pixel with g > r
        let has_green_ink = buf_green
            .chunks_exact(4)
            .any(|px| px[1] > px[0] && px[1] > 50);
        assert!(has_gray_ink, "default text should have gray ink");
        assert!(has_green_ink, "fg-spans text should have green ink");
    }

    #[test]
    fn link_overrides_fg_spans() {
        // M70: priority link > fg. A cell that is both link AND fg should render blue.
        let mut r = FontRenderer::new();
        let (_, _, buf) = r.render_text_to_rgba(
            "go",
            &[vec![(0, 2)]], // link
            &[],
            &[vec![(0, 2, (255, 0, 0))]], // fg = red (should be overridden by link blue)
        );
        // Expect a blue-ish pixel (b > r), NOT a red pixel.
        let blue_pixel = buf.chunks_exact(4).any(|px| px[2] > px[0] && px[2] > px[1]);
        let red_pixel = buf
            .chunks_exact(4)
            .any(|px| px[0] > 100 && px[1] < px[0] && px[2] < px[0]);
        assert!(blue_pixel, "link should override fg → blue");
        assert!(!red_pixel, "fg red should NOT win over link blue");
    }

    // ── M72: dark-theme contrast guard + proper alpha compositing ──

    #[test]
    fn wcag_contrast_ratio_values_are_correct() {
        // Locked reference values: black/white = 21:1, equal colors = 1:1.
        assert!((contrast_ratio(0.0, 1.0) - 21.0).abs() < 1e-9);
        assert!((contrast_ratio(0.5, 0.5) - 1.0).abs() < 1e-9);
        // #777 vs white ≈ 4.48 (well-known WCAG reference).
        let l777 = relative_luminance((119, 119, 119));
        assert!((contrast_ratio(l777, 1.0) - 4.48).abs() < 0.05);
    }

    #[test]
    fn dark_bg_flips_default_black_text_to_white() {
        // qwik.dev symptom: dark bg + default (black) ink → invisible.
        // M72: ink must flip to white; the dark background block stays.
        let mut r = FontRenderer::new();
        let cols = 2;
        let (_, _, buf) = r.render_text_to_rgba("Hi", &[], &[vec![(0, cols, (26, 26, 46))]], &[]);
        // Some glyph pixel must be bright (white-ish ink on dark bg).
        let bright_ink = buf
            .chunks_exact(4)
            .any(|px| px[0] > 200 && px[1] > 200 && px[2] > 200);
        assert!(bright_ink, "black ink on dark bg must flip to white");
        // Background block must remain dark (kept, not inverted).
        let dark_bg = buf
            .chunks_exact(4)
            .any(|px| px[0] == 26 && px[1] == 26 && px[2] == 46);
        assert!(dark_bg, "dark background block must be preserved");
        // No black-core text: darkest pixel sum must stay well above pure
        // black (old inverted formula gave sum=0 cores). The bg itself
        // (26,26,46) sums to 98, so anything < 60 means a black text core.
        let darkest = buf
            .chunks_exact(4)
            .map(|px| u32::from(px[0]) + u32::from(px[1]) + u32::from(px[2]))
            .min()
            .unwrap_or(765);
        assert!(
            darkest >= 60,
            "no near-black ink pixels allowed, got {darkest}"
        );
    }

    #[test]
    fn dark_bg_keeps_light_fg_text_light() {
        // Author sets light text (#eaeaea) on dark bg — contrast is fine,
        // M72 must KEEP the author color (proper compositing makes core light).
        let mut r = FontRenderer::new();
        let cols = 2;
        let (_, _, buf) = r.render_text_to_rgba(
            "Hi",
            &[],
            &[vec![(0, cols, (26, 26, 46))]],
            &[vec![(0, cols, (234, 234, 234))]],
        );
        let light_ink = buf
            .chunks_exact(4)
            .any(|px| px[0] > 180 && px[1] > 180 && px[2] > 180);
        assert!(light_ink, "light ink on dark bg must stay light");
    }

    #[test]
    fn link_blue_on_dark_bg_flips_to_white() {
        // Link blue #0000EE on dark navy: WCAG ratio ≈ 1.8 < 3 → flip to white.
        let mut r = FontRenderer::new();
        let cols = 2;
        let (_, _, buf) = r.render_text_to_rgba(
            "go",
            &[vec![(0, cols)]],
            &[vec![(0, cols, (26, 26, 46))]],
            &[],
        );
        let bright = buf
            .chunks_exact(4)
            .any(|px| px[0] > 200 && px[1] > 200 && px[2] > 200);
        assert!(bright, "link blue on dark bg must flip to readable white");
    }

    #[test]
    fn light_fg_on_implicit_white_bg_flips_to_black() {
        // Inverse case: author sets near-white text but page bg is white
        // (no bg span → implicit white) → flip to black for readability.
        let mut r = FontRenderer::new();
        let (_, _, buf) = r.render_text_to_rgba("Hi", &[], &[], &[vec![(0, 2, (234, 234, 234))]]);
        let dark_ink = buf
            .chunks_exact(4)
            .any(|px| px[0] < 100 && px[1] < 100 && px[2] < 100);
        assert!(dark_ink, "near-white ink on white bg must flip to black");
    }

    #[test]
    fn colored_fg_core_pixel_matches_exact_color_on_white() {
        // Lock the M72 compositing fix: red ink at full coverage must be
        // exactly (255,0,0). The old inverted formula produced (0,0,0).
        let mut r = FontRenderer::new();
        let (_, _, buf) = r.render_text_to_rgba("HH", &[], &[], &[vec![(0, 2, (255, 0, 0))]]);
        let core = buf
            .chunks_exact(4)
            .any(|px| px[0] == 255 && px[1] == 0 && px[2] == 0);
        assert!(core, "red ink core must be exactly (255,0,0)");
    }

    #[test]
    fn default_black_on_white_blend_unchanged() {
        // No bg/fg spans → black ink on white: M72 blend reduces to the old
        // `v = 255 - alpha` formula (byte-identical), so plain pages don't move.
        let mut r = FontRenderer::new();
        let (_, _, buf) = r.render_text_to_rgba("Hi", &[], &[], &[]);
        let black_core = buf
            .chunks_exact(4)
            .any(|px| px[0] == 0 && px[1] == 0 && px[2] == 0);
        let gray_edge = buf
            .chunks_exact(4)
            .any(|px| px[0] == px[1] && px[1] == px[2] && px[0] > 0 && px[0] < 255);
        assert!(black_core, "black core expected on white");
        assert!(gray_edge, "grayscale AA edge expected on white");
    }

    #[test]
    fn ensure_contrast_keeps_high_contrast_author_colors() {
        // Black on light gray keeps black (21-ish margin, well above 3).
        assert_eq!(ensure_contrast((0, 0, 0), (240, 240, 240)), (0, 0, 0));
        // Red ink on white: 4.0:1 ≥ 3 → keep author color.
        assert_eq!(ensure_contrast((255, 0, 0), (255, 255, 255)), (255, 0, 0));
        // White ink on dark navy keeps white.
        assert_eq!(
            ensure_contrast((255, 255, 255), (26, 26, 46)),
            (255, 255, 255)
        );
        // Black on dark navy flips to white.
        assert_eq!(ensure_contrast((0, 0, 0), (26, 26, 46)), (255, 255, 255));
        // Near-white on white flips to black.
        assert_eq!(ensure_contrast((234, 234, 234), (255, 255, 255)), (0, 0, 0));
        // White on Material green #4CAF50 is only 2.78:1 → correctly flips
        // to black (7.6:1), even though many sites pair them.
        assert_eq!(ensure_contrast((255, 255, 255), (76, 175, 80)), (0, 0, 0));
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
        let (w, h, buf) = r.render_text_to_rgba("A你B", &[], &[], &[]);
        assert!(w > 0 && h > 0);
        assert_eq!(buf.len(), w * h * 4);
        // Both ASCII 'A' and CJK '你' should produce dark pixels
        let dark = buf.chunks_exact(4).filter(|px| px[0] < 200).count();
        assert!(dark > 30, "expected substantial dark pixels for mixed text");
    }

    #[test]
    fn pure_chinese_text_renders() {
        let mut r = FontRenderer::new();
        let (w, h, buf) = r.render_text_to_rgba("你好世界", &[], &[], &[]);
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
