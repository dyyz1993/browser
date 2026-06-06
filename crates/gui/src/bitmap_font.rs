//! Hand-written 5x7 bitmap font for ASCII characters.
//!
//! Each glyph is 5 columns × 7 rows of bits, packed into a `u32`
//! (low 35 bits used). This is enough to render visible ASCII
//! (0x20..=0x7E) for the GUI demo without pulling in a real font
//! rasterizer. M5.2 will swap in cosmic-text or fontdue.
//!
//! Bit order: column-major, top-down, left-to-right. So bit 0 is
//! (col=0, row=0), bit 1 is (col=0, row=1), …, bit 6 is (col=0, row=6),
//! bit 7 is (col=1, row=0), etc.

/// 5-pixel wide, 7-pixel tall glyph.
#[derive(Clone, Copy, Debug)]
pub struct BitmapGlyph {
    /// Column-major bits. Bit (col*7 + row) = 1 means that pixel is on.
    pub bits: u64,
    /// Advance width in pixels (always 5 for now).
    pub advance: u8,
}

impl BitmapGlyph {
    /// Pixel at (col, row). Returns `true` if the bit is on.
    #[must_use]
    pub fn pixel(&self, col: usize, row: usize) -> bool {
        if col >= 5 || row >= 7 {
            return false;
        }
        let bit = col * 7 + row;
        ((self.bits >> bit) & 1) == 1
    }
}

/// Bitmap font covering printable ASCII (0x20..=0x7E).
pub struct BitmapFont;

impl BitmapFont {
    /// Lookup a glyph by character. Returns `Some` for printable ASCII.
    #[must_use]
    pub fn glyph(ch: char) -> Option<BitmapGlyph> {
        let bits = glyph_bits(ch)?;
        Some(BitmapGlyph { bits, advance: 5 })
    }

    /// Character width in pixels (always 5).
    pub const CHAR_WIDTH: usize = 5;
    /// Character height in pixels (always 7).
    pub const CHAR_HEIGHT: usize = 7;
    /// Inter-character spacing in pixels.
    pub const CHAR_SPACING: usize = 1;
    /// Inter-line spacing in pixels (rows below the glyph).
    pub const LINE_SPACING: usize = 2;
}

/// Render a text into a 2D pixel buffer. Returns the buffer dimensions
/// (width, height). Caller can then blit it to tiny-skia or whatever.
///
/// `scale` is an integer multiplier (1 = 5x7, 2 = 10x14, 3 = 15x21, …).
/// Each glyph pixel becomes a `scale×scale` block in the output.
#[must_use]
pub fn measure_text(text: &str) -> (usize, usize) {
    if text.is_empty() {
        return (0, BitmapFont::CHAR_HEIGHT);
    }
    let cw = BitmapFont::CHAR_WIDTH;
    let sp = BitmapFont::CHAR_SPACING;
    let lines: Vec<&str> = text.split('\n').collect();
    let longest_line = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0);
    let width = longest_line * cw + longest_line.saturating_sub(1) * sp;
    let height = lines.len() * BitmapFont::CHAR_HEIGHT
        + lines.len().saturating_sub(1) * BitmapFont::LINE_SPACING;
    (width, height)
}

/// Paint a single character's glyph at (x, y) into a pixel buffer using
/// the given paint function. `scale` is the integer pixel-block size.
pub fn draw_glyph<F: FnMut(usize, usize)>(
    x: usize,
    y: usize,
    scale: usize,
    ch: char,
    mut paint: F,
) {
    let Some(g) = BitmapFont::glyph(ch) else {
        // Unknown char → draw a 5x7 box outline as fallback.
        for col in 0..5 {
            for row in 0..7 {
                let on = col == 0 || col == 4 || row == 0 || row == 6;
                if on {
                    paint_block(&mut paint, x, y, scale, col, row);
                }
            }
        }
        return;
    };
    for col in 0..5 {
        for row in 0..7 {
            if g.pixel(col, row) {
                paint_block(&mut paint, x, y, scale, col, row);
            }
        }
    }
}

/// Paint a string at (x, y) wrapping at newlines only (no word-wrap).
pub fn draw_text<F: FnMut(usize, usize)>(
    x: usize,
    y: usize,
    scale: usize,
    text: &str,
    mut paint: F,
) {
    let cw = BitmapFont::CHAR_WIDTH * scale;
    let sp = BitmapFont::CHAR_SPACING * scale;
    let lh = BitmapFont::CHAR_HEIGHT * scale + BitmapFont::LINE_SPACING * scale;
    for (row_idx, line) in text.split('\n').enumerate() {
        let line_y = y + row_idx * lh;
        for (col_idx, ch) in line.chars().enumerate() {
            let ch_x = x + col_idx * (cw + sp);
            draw_glyph(ch_x, line_y, scale, ch, &mut paint);
        }
    }
}

fn paint_block<F: FnMut(usize, usize)>(
    paint: &mut F,
    x: usize,
    y: usize,
    scale: usize,
    col: usize,
    row: usize,
) {
    let bx = x + col * scale;
    let by = y + row * scale;
    for dy in 0..scale {
        for dx in 0..scale {
            paint(bx + dx, by + dy);
        }
    }
}

// ---------------------------------------------------------------------------
// Glyph data
// ---------------------------------------------------------------------------

/// Return the column-major bits for a printable ASCII char, or `None`.
#[must_use]
fn glyph_bits(ch: char) -> Option<u64> {
    let idx = ch as u32;
    if !(0x20..=0x7E).contains(&idx) {
        return None;
    }
    Some(GLYPH_TABLE[(idx - 0x20) as usize])
}

/// Minimal 5x7 font table — only the glyphs we need for the demo.
/// Missing chars fall back to a box (drawn by draw_glyph).
///
/// Each entry is 35 bits: bit (col*7 + row) where col ∈ [0,5) and
/// row ∈ [0,7). To keep the table manageable, only the most useful
/// glyphs are populated; the rest stay 0 (will draw as a box).
const GLYPH_TABLE: [u64; 95] = build_table();

const fn build_table() -> [u64; 95] {
    // 95 slots for 0x20..=0x7E. Initialised to 0.
    let mut t = [0u64; 95];

    // Helper: bit at (col, row).
    // const fn, so manual indexing.
    let mut idx = 0;
    while idx < t.len() {
        let ch = (idx as u8) + 0x20;
        t[idx] = match ch {
            // ASCII char → bits
            b' ' => 0,
            b'!' => bits(&[
                [0, 1, 1, 1, 0],
                [0, 1, 1, 1, 0],
                [0, 1, 1, 1, 0],
                [0, 0, 0, 0, 0],
                [0, 1, 1, 1, 0],
                [0, 0, 0, 0, 0],
                [0, 0, 0, 0, 0],
            ]),
            b'"' => bits(&[
                [1, 1, 0, 1, 1],
                [1, 1, 0, 1, 1],
                [0, 0, 0, 0, 0],
                [0, 0, 0, 0, 0],
                [0, 0, 0, 0, 0],
                [0, 0, 0, 0, 0],
                [0, 0, 0, 0, 0],
            ]),
            b'\'' => bits(&[
                [0, 1, 1, 0, 0],
                [0, 1, 1, 0, 0],
                [0, 0, 0, 0, 0],
                [0, 0, 0, 0, 0],
                [0, 0, 0, 0, 0],
                [0, 0, 0, 0, 0],
                [0, 0, 0, 0, 0],
            ]),
            b'(' => bits(&[
                [0, 0, 1, 1, 0],
                [0, 1, 0, 0, 0],
                [0, 1, 0, 0, 0],
                [0, 1, 0, 0, 0],
                [0, 1, 0, 0, 0],
                [0, 1, 0, 0, 0],
                [0, 0, 1, 1, 0],
            ]),
            b')' => bits(&[
                [0, 1, 1, 0, 0],
                [0, 0, 0, 1, 0],
                [0, 0, 0, 1, 0],
                [0, 0, 0, 1, 0],
                [0, 0, 0, 1, 0],
                [0, 0, 0, 1, 0],
                [0, 1, 1, 0, 0],
            ]),
            b'+' => bits(&[
                [0, 0, 0, 0, 0],
                [0, 0, 1, 0, 0],
                [0, 0, 1, 0, 0],
                [1, 1, 1, 1, 1],
                [0, 0, 1, 0, 0],
                [0, 0, 1, 0, 0],
                [0, 0, 0, 0, 0],
            ]),
            b',' => bits(&[
                [0, 0, 0, 0, 0],
                [0, 0, 0, 0, 0],
                [0, 0, 0, 0, 0],
                [0, 0, 0, 0, 0],
                [0, 1, 1, 0, 0],
                [0, 1, 1, 0, 0],
                [1, 1, 0, 0, 0],
            ]),
            b'-' => bits(&[
                [0, 0, 0, 0, 0],
                [0, 0, 0, 0, 0],
                [0, 0, 0, 0, 0],
                [1, 1, 1, 1, 1],
                [0, 0, 0, 0, 0],
                [0, 0, 0, 0, 0],
                [0, 0, 0, 0, 0],
            ]),
            b'.' => bits(&[
                [0, 0, 0, 0, 0],
                [0, 0, 0, 0, 0],
                [0, 0, 0, 0, 0],
                [0, 0, 0, 0, 0],
                [0, 0, 0, 0, 0],
                [0, 1, 1, 0, 0],
                [0, 1, 1, 0, 0],
            ]),
            b'/' => bits(&[
                [0, 0, 0, 0, 1],
                [0, 0, 0, 1, 0],
                [0, 0, 0, 1, 0],
                [0, 0, 1, 0, 0],
                [0, 1, 0, 0, 0],
                [0, 1, 0, 0, 0],
                [1, 0, 0, 0, 0],
            ]),
            b'0' => bits(&[
                [0, 1, 1, 1, 0],
                [1, 0, 0, 0, 1],
                [1, 0, 0, 1, 1],
                [1, 0, 1, 0, 1],
                [1, 1, 0, 0, 1],
                [1, 0, 0, 0, 1],
                [0, 1, 1, 1, 0],
            ]),
            b'1' => bits(&[
                [0, 0, 1, 0, 0],
                [0, 1, 1, 0, 0],
                [0, 0, 1, 0, 0],
                [0, 0, 1, 0, 0],
                [0, 0, 1, 0, 0],
                [0, 0, 1, 0, 0],
                [0, 1, 1, 1, 0],
            ]),
            b'2' => bits(&[
                [0, 1, 1, 1, 0],
                [1, 0, 0, 0, 1],
                [0, 0, 0, 0, 1],
                [0, 0, 0, 1, 0],
                [0, 0, 1, 0, 0],
                [0, 1, 0, 0, 0],
                [1, 1, 1, 1, 1],
            ]),
            b'3' => bits(&[
                [0, 1, 1, 1, 0],
                [1, 0, 0, 0, 1],
                [0, 0, 0, 0, 1],
                [0, 0, 1, 1, 0],
                [0, 0, 0, 0, 1],
                [1, 0, 0, 0, 1],
                [0, 1, 1, 1, 0],
            ]),
            b'4' => bits(&[
                [0, 0, 0, 1, 0],
                [0, 0, 1, 1, 0],
                [0, 1, 0, 1, 0],
                [1, 0, 0, 1, 0],
                [1, 1, 1, 1, 1],
                [0, 0, 0, 1, 0],
                [0, 0, 0, 1, 0],
            ]),
            b'5' => bits(&[
                [1, 1, 1, 1, 1],
                [1, 0, 0, 0, 0],
                [1, 1, 1, 1, 0],
                [0, 0, 0, 0, 1],
                [0, 0, 0, 0, 1],
                [1, 0, 0, 0, 1],
                [0, 1, 1, 1, 0],
            ]),
            b'6' => bits(&[
                [0, 0, 1, 1, 0],
                [0, 1, 0, 0, 0],
                [1, 0, 0, 0, 0],
                [1, 1, 1, 1, 0],
                [1, 0, 0, 0, 1],
                [1, 0, 0, 0, 1],
                [0, 1, 1, 1, 0],
            ]),
            b'7' => bits(&[
                [1, 1, 1, 1, 1],
                [0, 0, 0, 0, 1],
                [0, 0, 0, 1, 0],
                [0, 0, 1, 0, 0],
                [0, 1, 0, 0, 0],
                [0, 1, 0, 0, 0],
                [0, 1, 0, 0, 0],
            ]),
            b'8' => bits(&[
                [0, 1, 1, 1, 0],
                [1, 0, 0, 0, 1],
                [1, 0, 0, 0, 1],
                [0, 1, 1, 1, 0],
                [1, 0, 0, 0, 1],
                [1, 0, 0, 0, 1],
                [0, 1, 1, 1, 0],
            ]),
            b'9' => bits(&[
                [0, 1, 1, 1, 0],
                [1, 0, 0, 0, 1],
                [1, 0, 0, 0, 1],
                [0, 1, 1, 1, 1],
                [0, 0, 0, 0, 1],
                [0, 0, 0, 1, 0],
                [0, 1, 1, 0, 0],
            ]),
            b':' => bits(&[
                [0, 0, 0, 0, 0],
                [0, 1, 1, 0, 0],
                [0, 1, 1, 0, 0],
                [0, 0, 0, 0, 0],
                [0, 1, 1, 0, 0],
                [0, 1, 1, 0, 0],
                [0, 0, 0, 0, 0],
            ]),
            b';' => bits(&[
                [0, 0, 0, 0, 0],
                [0, 1, 1, 0, 0],
                [0, 1, 1, 0, 0],
                [0, 0, 0, 0, 0],
                [0, 1, 1, 0, 0],
                [0, 1, 1, 0, 0],
                [1, 1, 0, 0, 0],
            ]),
            b'<' => bits(&[
                [0, 0, 0, 1, 0],
                [0, 0, 1, 0, 0],
                [0, 1, 0, 0, 0],
                [1, 0, 0, 0, 0],
                [0, 1, 0, 0, 0],
                [0, 0, 1, 0, 0],
                [0, 0, 0, 1, 0],
            ]),
            b'=' => bits(&[
                [0, 0, 0, 0, 0],
                [0, 0, 0, 0, 0],
                [1, 1, 1, 1, 1],
                [0, 0, 0, 0, 0],
                [1, 1, 1, 1, 1],
                [0, 0, 0, 0, 0],
                [0, 0, 0, 0, 0],
            ]),
            b'>' => bits(&[
                [0, 1, 0, 0, 0],
                [0, 0, 1, 0, 0],
                [0, 0, 0, 1, 0],
                [0, 0, 0, 0, 1],
                [0, 0, 0, 1, 0],
                [0, 0, 1, 0, 0],
                [0, 1, 0, 0, 0],
            ]),
            b'?' => bits(&[
                [0, 1, 1, 1, 0],
                [1, 0, 0, 0, 1],
                [0, 0, 0, 0, 1],
                [0, 0, 0, 1, 0],
                [0, 0, 1, 0, 0],
                [0, 0, 0, 0, 0],
                [0, 0, 1, 0, 0],
            ]),
            b'@' => bits(&[
                [0, 1, 1, 1, 0],
                [1, 0, 0, 0, 1],
                [1, 0, 1, 1, 1],
                [1, 0, 1, 0, 1],
                [1, 0, 1, 1, 1],
                [1, 0, 0, 0, 0],
                [0, 1, 1, 1, 0],
            ]),
            b'A' => bits(&[
                [0, 1, 1, 1, 0],
                [1, 0, 0, 0, 1],
                [1, 0, 0, 0, 1],
                [1, 1, 1, 1, 1],
                [1, 0, 0, 0, 1],
                [1, 0, 0, 0, 1],
                [1, 0, 0, 0, 1],
            ]),
            b'B' => bits(&[
                [1, 1, 1, 1, 0],
                [1, 0, 0, 0, 1],
                [1, 0, 0, 0, 1],
                [1, 1, 1, 1, 0],
                [1, 0, 0, 0, 1],
                [1, 0, 0, 0, 1],
                [1, 1, 1, 1, 0],
            ]),
            b'C' => bits(&[
                [0, 1, 1, 1, 0],
                [1, 0, 0, 0, 1],
                [1, 0, 0, 0, 0],
                [1, 0, 0, 0, 0],
                [1, 0, 0, 0, 0],
                [1, 0, 0, 0, 1],
                [0, 1, 1, 1, 0],
            ]),
            b'D' => bits(&[
                [1, 1, 1, 1, 0],
                [1, 0, 0, 0, 1],
                [1, 0, 0, 0, 1],
                [1, 0, 0, 0, 1],
                [1, 0, 0, 0, 1],
                [1, 0, 0, 0, 1],
                [1, 1, 1, 1, 0],
            ]),
            b'E' => bits(&[
                [1, 1, 1, 1, 1],
                [1, 0, 0, 0, 0],
                [1, 0, 0, 0, 0],
                [1, 1, 1, 1, 0],
                [1, 0, 0, 0, 0],
                [1, 0, 0, 0, 0],
                [1, 1, 1, 1, 1],
            ]),
            b'F' => bits(&[
                [1, 1, 1, 1, 1],
                [1, 0, 0, 0, 0],
                [1, 0, 0, 0, 0],
                [1, 1, 1, 1, 0],
                [1, 0, 0, 0, 0],
                [1, 0, 0, 0, 0],
                [1, 0, 0, 0, 0],
            ]),
            b'G' => bits(&[
                [0, 1, 1, 1, 0],
                [1, 0, 0, 0, 1],
                [1, 0, 0, 0, 0],
                [1, 0, 1, 1, 1],
                [1, 0, 0, 0, 1],
                [1, 0, 0, 0, 1],
                [0, 1, 1, 1, 0],
            ]),
            b'H' => bits(&[
                [1, 0, 0, 0, 1],
                [1, 0, 0, 0, 1],
                [1, 0, 0, 0, 1],
                [1, 1, 1, 1, 1],
                [1, 0, 0, 0, 1],
                [1, 0, 0, 0, 1],
                [1, 0, 0, 0, 1],
            ]),
            b'I' => bits(&[
                [0, 1, 1, 1, 0],
                [0, 0, 1, 0, 0],
                [0, 0, 1, 0, 0],
                [0, 0, 1, 0, 0],
                [0, 0, 1, 0, 0],
                [0, 0, 1, 0, 0],
                [0, 1, 1, 1, 0],
            ]),
            b'J' => bits(&[
                [0, 1, 1, 1, 1],
                [0, 0, 0, 0, 1],
                [0, 0, 0, 0, 1],
                [0, 0, 0, 0, 1],
                [0, 0, 0, 0, 1],
                [1, 0, 0, 0, 1],
                [0, 1, 1, 1, 0],
            ]),
            b'K' => bits(&[
                [1, 0, 0, 0, 1],
                [1, 0, 0, 1, 0],
                [1, 0, 1, 0, 0],
                [1, 1, 0, 0, 0],
                [1, 0, 1, 0, 0],
                [1, 0, 0, 1, 0],
                [1, 0, 0, 0, 1],
            ]),
            b'L' => bits(&[
                [1, 0, 0, 0, 0],
                [1, 0, 0, 0, 0],
                [1, 0, 0, 0, 0],
                [1, 0, 0, 0, 0],
                [1, 0, 0, 0, 0],
                [1, 0, 0, 0, 0],
                [1, 1, 1, 1, 1],
            ]),
            b'M' => bits(&[
                [1, 0, 0, 0, 1],
                [1, 1, 0, 1, 1],
                [1, 1, 0, 1, 1],
                [1, 0, 1, 0, 1],
                [1, 0, 0, 0, 1],
                [1, 0, 0, 0, 1],
                [1, 0, 0, 0, 1],
            ]),
            b'N' => bits(&[
                [1, 0, 0, 0, 1],
                [1, 1, 0, 0, 1],
                [1, 1, 0, 0, 1],
                [1, 0, 1, 0, 1],
                [1, 0, 0, 1, 1],
                [1, 0, 0, 1, 1],
                [1, 0, 0, 0, 1],
            ]),
            b'O' => bits(&[
                [0, 1, 1, 1, 0],
                [1, 0, 0, 0, 1],
                [1, 0, 0, 0, 1],
                [1, 0, 0, 0, 1],
                [1, 0, 0, 0, 1],
                [1, 0, 0, 0, 1],
                [0, 1, 1, 1, 0],
            ]),
            b'P' => bits(&[
                [1, 1, 1, 1, 0],
                [1, 0, 0, 0, 1],
                [1, 0, 0, 0, 1],
                [1, 1, 1, 1, 0],
                [1, 0, 0, 0, 0],
                [1, 0, 0, 0, 0],
                [1, 0, 0, 0, 0],
            ]),
            b'Q' => bits(&[
                [0, 1, 1, 1, 0],
                [1, 0, 0, 0, 1],
                [1, 0, 0, 0, 1],
                [1, 0, 0, 0, 1],
                [1, 0, 1, 0, 1],
                [1, 0, 0, 1, 0],
                [0, 1, 1, 0, 1],
            ]),
            b'R' => bits(&[
                [1, 1, 1, 1, 0],
                [1, 0, 0, 0, 1],
                [1, 0, 0, 0, 1],
                [1, 1, 1, 1, 0],
                [1, 0, 1, 0, 0],
                [1, 0, 0, 1, 0],
                [1, 0, 0, 0, 1],
            ]),
            b'S' => bits(&[
                [0, 1, 1, 1, 0],
                [1, 0, 0, 0, 1],
                [1, 0, 0, 0, 0],
                [0, 1, 1, 1, 0],
                [0, 0, 0, 0, 1],
                [1, 0, 0, 0, 1],
                [0, 1, 1, 1, 0],
            ]),
            b'T' => bits(&[
                [1, 1, 1, 1, 1],
                [0, 0, 1, 0, 0],
                [0, 0, 1, 0, 0],
                [0, 0, 1, 0, 0],
                [0, 0, 1, 0, 0],
                [0, 0, 1, 0, 0],
                [0, 0, 1, 0, 0],
            ]),
            b'U' => bits(&[
                [1, 0, 0, 0, 1],
                [1, 0, 0, 0, 1],
                [1, 0, 0, 0, 1],
                [1, 0, 0, 0, 1],
                [1, 0, 0, 0, 1],
                [1, 0, 0, 0, 1],
                [0, 1, 1, 1, 0],
            ]),
            b'V' => bits(&[
                [1, 0, 0, 0, 1],
                [1, 0, 0, 0, 1],
                [1, 0, 0, 0, 1],
                [1, 0, 0, 0, 1],
                [1, 0, 0, 0, 1],
                [0, 1, 0, 1, 0],
                [0, 0, 1, 0, 0],
            ]),
            b'W' => bits(&[
                [1, 0, 0, 0, 1],
                [1, 0, 0, 0, 1],
                [1, 0, 0, 0, 1],
                [1, 0, 1, 0, 1],
                [1, 1, 0, 1, 1],
                [1, 1, 0, 1, 1],
                [1, 0, 0, 0, 1],
            ]),
            b'X' => bits(&[
                [1, 0, 0, 0, 1],
                [1, 0, 0, 0, 1],
                [0, 1, 0, 1, 0],
                [0, 0, 1, 0, 0],
                [0, 1, 0, 1, 0],
                [1, 0, 0, 0, 1],
                [1, 0, 0, 0, 1],
            ]),
            b'Y' => bits(&[
                [1, 0, 0, 0, 1],
                [1, 0, 0, 0, 1],
                [0, 1, 0, 1, 0],
                [0, 0, 1, 0, 0],
                [0, 0, 1, 0, 0],
                [0, 0, 1, 0, 0],
                [0, 0, 1, 0, 0],
            ]),
            b'Z' => bits(&[
                [1, 1, 1, 1, 1],
                [0, 0, 0, 0, 1],
                [0, 0, 0, 1, 0],
                [0, 0, 1, 0, 0],
                [0, 1, 0, 0, 0],
                [1, 0, 0, 0, 0],
                [1, 1, 1, 1, 1],
            ]),
            b'[' => bits(&[
                [0, 1, 1, 1, 0],
                [0, 1, 0, 0, 0],
                [0, 1, 0, 0, 0],
                [0, 1, 0, 0, 0],
                [0, 1, 0, 0, 0],
                [0, 1, 0, 0, 0],
                [0, 1, 1, 1, 0],
            ]),
            b'\\' => bits(&[
                [1, 0, 0, 0, 0],
                [0, 1, 0, 0, 0],
                [0, 1, 0, 0, 0],
                [0, 0, 1, 0, 0],
                [0, 0, 0, 1, 0],
                [0, 0, 0, 1, 0],
                [0, 0, 0, 0, 1],
            ]),
            b']' => bits(&[
                [0, 1, 1, 1, 0],
                [0, 0, 0, 1, 0],
                [0, 0, 0, 1, 0],
                [0, 0, 0, 1, 0],
                [0, 0, 0, 1, 0],
                [0, 0, 0, 1, 0],
                [0, 1, 1, 1, 0],
            ]),
            b'_' => bits(&[
                [0, 0, 0, 0, 0],
                [0, 0, 0, 0, 0],
                [0, 0, 0, 0, 0],
                [0, 0, 0, 0, 0],
                [0, 0, 0, 0, 0],
                [0, 0, 0, 0, 0],
                [1, 1, 1, 1, 1],
            ]),
            b'a' => bits(&[
                [0, 0, 0, 0, 0],
                [0, 0, 0, 0, 0],
                [0, 1, 1, 1, 0],
                [0, 0, 0, 0, 1],
                [0, 1, 1, 1, 1],
                [1, 0, 0, 0, 1],
                [0, 1, 1, 1, 1],
            ]),
            b'b' => bits(&[
                [1, 0, 0, 0, 0],
                [1, 0, 0, 0, 0],
                [1, 1, 1, 1, 0],
                [1, 0, 0, 0, 1],
                [1, 0, 0, 0, 1],
                [1, 0, 0, 0, 1],
                [1, 1, 1, 1, 0],
            ]),
            b'c' => bits(&[
                [0, 0, 0, 0, 0],
                [0, 0, 0, 0, 0],
                [0, 1, 1, 1, 0],
                [1, 0, 0, 0, 1],
                [1, 0, 0, 0, 0],
                [1, 0, 0, 0, 1],
                [0, 1, 1, 1, 0],
            ]),
            b'd' => bits(&[
                [0, 0, 0, 0, 1],
                [0, 0, 0, 0, 1],
                [0, 1, 1, 1, 1],
                [1, 0, 0, 0, 1],
                [1, 0, 0, 0, 1],
                [1, 0, 0, 0, 1],
                [0, 1, 1, 1, 1],
            ]),
            b'e' => bits(&[
                [0, 0, 0, 0, 0],
                [0, 0, 0, 0, 0],
                [0, 1, 1, 1, 0],
                [1, 0, 0, 0, 1],
                [1, 1, 1, 1, 1],
                [1, 0, 0, 0, 0],
                [0, 1, 1, 1, 0],
            ]),
            b'f' => bits(&[
                [0, 0, 1, 1, 0],
                [0, 1, 0, 0, 1],
                [0, 1, 0, 0, 0],
                [1, 1, 1, 0, 0],
                [0, 1, 0, 0, 0],
                [0, 1, 0, 0, 0],
                [0, 1, 0, 0, 0],
            ]),
            b'g' => bits(&[
                [0, 0, 0, 0, 0],
                [0, 0, 0, 0, 0],
                [0, 1, 1, 1, 1],
                [1, 0, 0, 0, 1],
                [0, 1, 1, 1, 1],
                [0, 0, 0, 0, 1],
                [1, 1, 1, 1, 0],
            ]),
            b'h' => bits(&[
                [1, 0, 0, 0, 0],
                [1, 0, 0, 0, 0],
                [1, 1, 1, 1, 0],
                [1, 0, 0, 0, 1],
                [1, 0, 0, 0, 1],
                [1, 0, 0, 0, 1],
                [1, 0, 0, 0, 1],
            ]),
            b'i' => bits(&[
                [0, 0, 1, 0, 0],
                [0, 0, 0, 0, 0],
                [0, 1, 1, 0, 0],
                [0, 0, 1, 0, 0],
                [0, 0, 1, 0, 0],
                [0, 0, 1, 0, 0],
                [0, 1, 1, 1, 0],
            ]),
            b'j' => bits(&[
                [0, 0, 0, 1, 0],
                [0, 0, 0, 0, 0],
                [0, 0, 1, 1, 0],
                [0, 0, 0, 1, 0],
                [0, 0, 0, 1, 0],
                [1, 0, 0, 1, 0],
                [0, 1, 1, 0, 0],
            ]),
            b'k' => bits(&[
                [1, 0, 0, 0, 0],
                [1, 0, 0, 0, 0],
                [1, 0, 0, 1, 0],
                [1, 0, 1, 0, 0],
                [1, 1, 0, 0, 0],
                [1, 0, 1, 0, 0],
                [1, 0, 0, 1, 0],
            ]),
            b'l' => bits(&[
                [0, 1, 1, 0, 0],
                [0, 0, 1, 0, 0],
                [0, 0, 1, 0, 0],
                [0, 0, 1, 0, 0],
                [0, 0, 1, 0, 0],
                [0, 0, 1, 0, 0],
                [0, 1, 1, 1, 0],
            ]),
            b'm' => bits(&[
                [0, 0, 0, 0, 0],
                [0, 0, 0, 0, 0],
                [1, 1, 0, 1, 0],
                [1, 0, 1, 0, 1],
                [1, 0, 1, 0, 1],
                [1, 0, 0, 0, 1],
                [1, 0, 0, 0, 1],
            ]),
            b'n' => bits(&[
                [0, 0, 0, 0, 0],
                [0, 0, 0, 0, 0],
                [1, 1, 1, 1, 0],
                [1, 0, 0, 0, 1],
                [1, 0, 0, 0, 1],
                [1, 0, 0, 0, 1],
                [1, 0, 0, 0, 1],
            ]),
            b'o' => bits(&[
                [0, 0, 0, 0, 0],
                [0, 0, 0, 0, 0],
                [0, 1, 1, 1, 0],
                [1, 0, 0, 0, 1],
                [1, 0, 0, 0, 1],
                [1, 0, 0, 0, 1],
                [0, 1, 1, 1, 0],
            ]),
            b'p' => bits(&[
                [0, 0, 0, 0, 0],
                [0, 0, 0, 0, 0],
                [1, 1, 1, 1, 0],
                [1, 0, 0, 0, 1],
                [1, 1, 1, 1, 0],
                [1, 0, 0, 0, 0],
                [1, 0, 0, 0, 0],
            ]),
            b'q' => bits(&[
                [0, 0, 0, 0, 0],
                [0, 0, 0, 0, 0],
                [0, 1, 1, 1, 1],
                [1, 0, 0, 0, 1],
                [0, 1, 1, 1, 1],
                [0, 0, 0, 0, 1],
                [0, 0, 0, 0, 1],
            ]),
            b'r' => bits(&[
                [0, 0, 0, 0, 0],
                [0, 0, 0, 0, 0],
                [1, 0, 1, 1, 0],
                [1, 1, 0, 0, 1],
                [1, 0, 0, 0, 0],
                [1, 0, 0, 0, 0],
                [1, 0, 0, 0, 0],
            ]),
            b's' => bits(&[
                [0, 0, 0, 0, 0],
                [0, 0, 0, 0, 0],
                [0, 1, 1, 1, 1],
                [1, 0, 0, 0, 0],
                [0, 1, 1, 1, 0],
                [0, 0, 0, 0, 1],
                [1, 1, 1, 1, 0],
            ]),
            b't' => bits(&[
                [0, 1, 0, 0, 0],
                [0, 1, 0, 0, 0],
                [1, 1, 1, 0, 0],
                [0, 1, 0, 0, 0],
                [0, 1, 0, 0, 0],
                [0, 1, 0, 0, 1],
                [0, 0, 1, 1, 0],
            ]),
            b'u' => bits(&[
                [0, 0, 0, 0, 0],
                [0, 0, 0, 0, 0],
                [1, 0, 0, 0, 1],
                [1, 0, 0, 0, 1],
                [1, 0, 0, 0, 1],
                [1, 0, 0, 0, 1],
                [0, 1, 1, 1, 1],
            ]),
            b'v' => bits(&[
                [0, 0, 0, 0, 0],
                [0, 0, 0, 0, 0],
                [1, 0, 0, 0, 1],
                [1, 0, 0, 0, 1],
                [1, 0, 0, 0, 1],
                [0, 1, 0, 1, 0],
                [0, 0, 1, 0, 0],
            ]),
            b'w' => bits(&[
                [0, 0, 0, 0, 0],
                [0, 0, 0, 0, 0],
                [1, 0, 0, 0, 1],
                [1, 0, 0, 0, 1],
                [1, 0, 1, 0, 1],
                [1, 1, 0, 1, 1],
                [1, 0, 0, 0, 1],
            ]),
            b'x' => bits(&[
                [0, 0, 0, 0, 0],
                [0, 0, 0, 0, 0],
                [1, 0, 0, 0, 1],
                [0, 1, 0, 1, 0],
                [0, 0, 1, 0, 0],
                [0, 1, 0, 1, 0],
                [1, 0, 0, 0, 1],
            ]),
            b'y' => bits(&[
                [0, 0, 0, 0, 0],
                [0, 0, 0, 0, 0],
                [1, 0, 0, 0, 1],
                [1, 0, 0, 0, 1],
                [0, 1, 1, 1, 1],
                [0, 0, 0, 0, 1],
                [1, 1, 1, 1, 0],
            ]),
            b'z' => bits(&[
                [0, 0, 0, 0, 0],
                [0, 0, 0, 0, 0],
                [1, 1, 1, 1, 1],
                [0, 0, 0, 1, 0],
                [0, 0, 1, 0, 0],
                [0, 1, 0, 0, 0],
                [1, 1, 1, 1, 1],
            ]),
            b'|' => bits(&[
                [0, 0, 1, 0, 0],
                [0, 0, 1, 0, 0],
                [0, 0, 1, 0, 0],
                [0, 0, 1, 0, 0],
                [0, 0, 1, 0, 0],
                [0, 0, 1, 0, 0],
                [0, 0, 1, 0, 0],
            ]),
            _ => 0,
        };
        idx += 1;
    }
    t
}

/// Compile-time bit-packer. Takes a 7-row × 5-col pattern (row-major)
/// and returns column-major bits.
const fn bits(rows: &[[u8; 5]; 7]) -> u64 {
    let mut packed = 0u64;
    let mut col = 0;
    while col < 5 {
        let mut row = 0;
        while row < 7 {
            if rows[row][col] != 0 {
                let bit = col * 7 + row;
                packed |= 1u64 << bit;
            }
            row += 1;
        }
        col += 1;
    }
    packed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glyph_pixel_at_corner() {
        let g = BitmapFont::glyph('L').unwrap();
        for row in 0..7 {
            assert!(g.pixel(0, row), "L col=0 row={row} should be on");
        }
        assert!(!g.pixel(4, 0));
        assert!(g.pixel(4, 6));
    }

    #[test]
    fn glyph_lookup_for_printable_ascii() {
        for ch in ' '..='~' {
            let g = BitmapFont::glyph(ch);
            assert!(g.is_some(), "no glyph for {ch:?}");
        }
    }

    #[test]
    fn glyph_lookup_for_non_printable_returns_none() {
        assert!(BitmapFont::glyph('\t').is_none());
        assert!(BitmapFont::glyph('\x07').is_none());
        assert!(BitmapFont::glyph('é').is_none());
    }

    #[test]
    fn glyph_pixel_out_of_bounds_is_false() {
        let g = BitmapFont::glyph('A').unwrap();
        assert!(!g.pixel(5, 0));
        assert!(!g.pixel(0, 7));
        assert!(!g.pixel(99, 99));
    }

    #[test]
    fn measure_text_empty() {
        assert_eq!(measure_text(""), (0, BitmapFont::CHAR_HEIGHT));
    }

    #[test]
    fn measure_text_single_line() {
        let (w, h) = measure_text("hello");
        assert_eq!(w, 29);
        assert_eq!(h, BitmapFont::CHAR_HEIGHT);
    }

    #[test]
    fn measure_text_multiline() {
        let (w, h) = measure_text("hi\nhello");
        assert_eq!(w, 29);
        assert_eq!(h, 2 * BitmapFont::CHAR_HEIGHT + BitmapFont::LINE_SPACING);
    }

    fn collect_pixels(scale: usize, text: &str) -> Vec<(usize, usize)> {
        let mut p = Vec::new();
        draw_text(0, 0, scale, text, |x, y| p.push((x, y)));
        p
    }

    #[test]
    fn draw_text_paints_expected_pixels() {
        let pixels = collect_pixels(1, "A");
        assert!(!pixels.is_empty());
        assert!(!pixels.contains(&(0, 0)));
        assert!(!pixels.contains(&(4, 0)));
        for x in 1..=3 {
            assert!(pixels.contains(&(x, 0)), "missing top bar pixel ({x}, 0)");
        }
    }

    #[test]
    fn draw_text_with_scale_paints_blocks() {
        let pixels = collect_pixels(2, "A");
        assert!(pixels.contains(&(2, 0)));
        assert!(pixels.contains(&(3, 0)));
        assert!(pixels.contains(&(2, 1)));
        assert!(pixels.contains(&(3, 1)));
    }

    #[test]
    fn draw_text_unknown_char_falls_back_to_box() {
        let pixels = collect_pixels(1, "é");
        assert!(pixels.contains(&(0, 0)));
        assert!(pixels.contains(&(4, 0)));
        assert!(pixels.contains(&(0, 6)));
        assert!(pixels.contains(&(4, 6)));
    }

    #[test]
    fn draw_text_newline_moves_to_next_row() {
        let mut y_seen: Vec<usize> = Vec::new();
        draw_text(0, 0, 1, "A\nB", |_, y| y_seen.push(y));
        y_seen.sort();
        y_seen.dedup();
        assert!(y_seen.contains(&0));
        assert!(y_seen.contains(&9));
    }
}
