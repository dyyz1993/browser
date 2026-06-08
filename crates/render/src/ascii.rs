//! Terminal ASCII renderer.
//!
//! Walks a laid-out [`LayoutTree`] and writes characters into a 2D
//! character buffer, then collapses the buffer into a `String` for
//! printing. Coordinate system matches layout: y grows downward.

use browser_layout::{BoxType, LayoutBox, LayoutTree};

/// Render a laid-out tree into an ASCII string.
///
/// `viewport_width` is the number of columns to allocate. Lines that
/// would exceed the buffer are clipped at the right edge; lines past
/// the bottom of the content are not emitted.
///
/// `colored`: when `true`, hyperlink text is wrapped in ANSI escape
/// sequences (underline+blue). Used for screenshots (PNG renderer
/// parses ANSI codes). When `false`, plain ASCII (safe for crawlers).
#[must_use]
pub fn render_ascii(tree: &LayoutTree, viewport_width: usize) -> String {
    render_ascii_inner(tree, viewport_width, false)
}

/// M30: colored variant — hyperlink text gets ANSI underline+blue.
#[must_use]
pub fn render_ascii_colored(tree: &LayoutTree, viewport_width: usize) -> String {
    render_ascii_inner(tree, viewport_width, true)
}

fn render_ascii_inner(tree: &LayoutTree, viewport_width: usize, colored: bool) -> String {
    // First pass: compute the maximum y (line index) any text reaches.
    let mut max_y = 0usize;
    collect_text_extent(&tree.root, &mut max_y);
    if max_y == 0 {
        return String::new();
    }

    let mut buf = CharBuffer::new(viewport_width, max_y);
    paint(&tree.root, &mut buf);

    buf.to_string(colored)
}

fn collect_text_extent(bx: &LayoutBox, max_y: &mut usize) {
    if bx.text.as_ref().is_some_and(|t: &String| !t.is_empty()) {
        let bottom = (bx.dimensions.y + bx.dimensions.height).ceil() as usize;
        if bottom > *max_y {
            *max_y = bottom;
        }
    }
    for child in &bx.children {
        collect_text_extent(child, max_y);
    }
}

fn paint(bx: &LayoutBox, buf: &mut CharBuffer) {
    // M39: 先画 background-color（填充矩形）+ border（box-drawing），
    // 再画文字（文字会覆盖 border/background 的空格位置）。
    // 注意：background/border 只画有明确尺寸的 box（width>0 && height>0）。
    if bx.dimensions.width > 0.0 && bx.dimensions.height > 0.0 {
        let x0 = bx.dimensions.x.round() as usize;
        let y0 = bx.dimensions.y.round() as usize;
        let w = bx.dimensions.width.round() as usize;
        let h = bx.dimensions.height.round() as usize;
        // background-color（填充矩形背景）
        if let Some(bg_color) = &bx.style.background {
            buf.fill_background(x0, y0, w, h, (bg_color.r, bg_color.g, bg_color.b));
        }
        // border（box-drawing 字符）
        let b = &bx.style.border;
        if b.top || b.bottom || b.left || b.right {
            buf.draw_box_border(x0, y0, w, h, b);
        }
    }
    // Only Inline boxes with text actually emit characters. Block /
    // anonymous boxes are positioning containers; recurse into their
    // children.
    if bx.box_type == BoxType::Inline {
        // M30: `bx.link` is set for `<a>` boxes. The mask is always
        // recorded; whether ANSI codes are emitted is decided later
        // by `to_string(colored)`.
        let is_link = bx.link;
        // Prefer laid-out `words` (M6.0a fix) when present — this is
        // what the inline word-wrap pass produced and accurately
        // reflects where each wrapped word starts. The old code
        // painted characters from `bx.dimensions.x` linearly, which
        // ignored line wraps and produced truncation artifacts.
        if !bx.words.is_empty() {
            for (word, sx, sy) in &bx.words {
                let mut x = sx.round() as usize;
                let y = sy.round() as usize;
                for ch in word.chars() {
                    if ch == '\n' || ch == '\r' {
                        continue;
                    }
                    buf.put(y, x, ch, is_link);
                    x += 1;
                }
            }
        } else if let Some(text) = &bx.text {
            // Fallback for inline boxes whose layout didn't go through
            // word-wrap (e.g. directly constructed LayoutBoxes in
            // tests). Same behavior as before M6.0a.
            let mut x = bx.dimensions.x.round() as usize;
            let y = bx.dimensions.y.round() as usize;
            for ch in text.chars() {
                if ch == '\n' || ch == '\r' {
                    continue;
                }
                if ch.is_whitespace() {
                    buf.put(y, x, ' ', is_link);
                    x += 1;
                } else {
                    buf.put(y, x, ch, is_link);
                    x += 1;
                }
            }
        }
    }
    for child in &bx.children {
        paint(child, buf);
    }
}

/// 2D character buffer with explicit spaces.
struct CharBuffer {
    width: usize,
    rows: Vec<Vec<char>>,
    /// M30: parallel grid marking link cells (for ANSI blue/underline).
    links: Vec<Vec<bool>>,
    /// M39: parallel grid marking background color per cell.
    bg: Vec<Vec<Option<(u8, u8, u8)>>>,
}

impl CharBuffer {
    fn new(width: usize, height: usize) -> Self {
        let rows = (0..height).map(|_| vec![' '; width]).collect();
        let links = (0..height).map(|_| vec![false; width]).collect();
        let bg = (0..height).map(|_| vec![None; width]).collect();
        Self {
            width,
            rows,
            links,
            bg,
        }
    }

    fn put(&mut self, y: usize, x: usize, c: char, link: bool) {
        if y < self.rows.len() && x < self.width {
            self.rows[y][x] = c;
            if link {
                self.links[y][x] = true;
            }
        }
    }

    /// M39: 填充矩形区域的背景色（不会覆盖已有字符，只设 bg 网格）。
    fn fill_background(&mut self, x0: usize, y0: usize, w: usize, h: usize, color: (u8, u8, u8)) {
        let x_end = (x0 + w).min(self.width);
        let y_end = (y0 + h).min(self.rows.len());
        for y in y0..y_end {
            for x in x0..x_end {
                self.bg[y][x] = Some(color);
            }
        }
    }

    /// M39: 画一个字符到指定位置，尊重已有背景（不覆盖 bg）。
    fn put_border(&mut self, y: usize, x: usize, c: char) {
        if y < self.rows.len() && x < self.width {
            self.rows[y][x] = c;
            // border 字符保留已有背景色
        }
    }

    /// M39: 画矩形的四条边（box-drawing 字符）。
    /// 角落用 ┌┐└┘，水平用 ─，垂直用 │。
    fn draw_box_border(
        &mut self,
        x0: usize,
        y0: usize,
        w: usize,
        h: usize,
        border: &browser_css_engine::BoxEdges<bool>,
    ) {
        if w < 2 || h < 2 {
            return;
        }
        let x1 = x0 + w - 1;
        let y1 = y0 + h - 1;
        // 水平线（top/bottom）
        if border.top {
            for x in x0 + 1..x1 {
                self.put_border(y0, x, '─');
            }
        }
        if border.bottom {
            for x in x0 + 1..x1 {
                self.put_border(y1, x, '─');
            }
        }
        // 垂直线（left/right）
        if border.left {
            for y in y0 + 1..y1 {
                self.put_border(y, x0, '│');
            }
        }
        if border.right {
            for y in y0 + 1..y1 {
                self.put_border(y, x1, '│');
            }
        }
        // 角落（仅当相邻两边都有 border）
        if border.top && border.left {
            self.put_border(y0, x0, '┌');
        }
        if border.top && border.right {
            self.put_border(y0, x1, '┐');
        }
        if border.bottom && border.left {
            self.put_border(y1, x0, '└');
        }
        if border.bottom && border.right {
            self.put_border(y1, x1, '┘');
        }
    }

    /// M30/M39: Render to string.
    /// - `colored=true`: link cells → ANSI underline+blue foreground;
    ///   background cells → ANSI truecolor background (`\x1b[48;2;R;G;Bm`).
    ///   Link + background 叠加时合并为一个 SGR 序列。
    /// - Trailing whitespace is trimmed, BUT background cells extend the
    ///   trim boundary (so colored background blocks aren't truncated).
    fn to_string(&self, colored: bool) -> String {
        let mut out = String::new();
        for y in 0..self.rows.len() {
            let row = &self.rows[y];
            // Find last significant column: non-space char OR cell with background.
            let mut last = 0usize;
            for x in (0..self.width).rev() {
                if row[x] != ' ' || self.bg[y][x].is_some() {
                    last = x + 1;
                    break;
                }
            }
            let mut x = 0;
            while x < last {
                let is_link = self.links[y][x];
                let bg = self.bg[y][x];

                if !colored || (!is_link && bg.is_none()) {
                    // Plain cell — no ANSI.
                    out.push(row[x]);
                    x += 1;
                    continue;
                }

                // Build the SGR prefix for this cell's style.
                let prefix = build_sgr_prefix(is_link, bg);
                out.push_str(&prefix);

                // Consume run of cells with identical (link, bg) style.
                while x < last && self.links[y][x] == is_link && self.bg[y][x] == bg {
                    out.push(row[x]);
                    x += 1;
                }
                out.push_str("\x1b[0m");
            }
            out.push('\n');
        }
        out
    }
}

/// M39: Build ANSI SGR prefix for a cell style.
///
/// - link: underline + W3C link blue (#0000EE) foreground
/// - background: truecolor background (`48;2;R;G;B`)
///
/// Both can combine in a single SGR sequence.
fn build_sgr_prefix(is_link: bool, bg: Option<(u8, u8, u8)>) -> String {
    // M30 link color: #0000EE = (0, 0, 238) + underline (4)
    let mut codes: Vec<String> = Vec::new();
    if is_link {
        codes.push("4".into()); // underline
        codes.push("38;2;0;0;238".into()); // link blue foreground
    }
    if let Some((r, g, b)) = bg {
        codes.push(format!("48;2;{r};{g};{b}"));
    }
    if codes.is_empty() {
        return String::new();
    }
    format!("\x1b[{}m", codes.join(";"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use browser_layout::Dimensions;

    fn text_box(text: &str, x: f32, y: f32) -> LayoutBox {
        let mut b = LayoutBox::new(BoxType::Inline).with_text(text.into());
        b.dimensions = Dimensions::new(x, y, text.chars().count() as f32, 1.0);
        b
    }

    fn block_with(children: Vec<LayoutBox>) -> LayoutBox {
        let mut b = LayoutBox::new(BoxType::Block);
        let height: f32 = children.iter().map(|c| c.dimensions.height).sum();
        b.dimensions = Dimensions::new(0.0, 0.0, 80.0, height);
        b.children = children;
        b
    }

    #[test]
    fn render_empty_returns_empty_string() {
        let tree = LayoutTree {
            root: LayoutBox::new(BoxType::Block),
        };
        assert_eq!(render_ascii(&tree, 80), "");
    }

    #[test]
    fn render_single_text_box() {
        // block > text("hello") at (0, 0)
        let mut root = LayoutBox::new(BoxType::Block);
        root.children.push(text_box("hello", 0.0, 0.0));
        root.dimensions = Dimensions::new(0.0, 0.0, 80.0, 1.0);
        let tree = LayoutTree { root };
        let out = render_ascii(&tree, 80);
        assert_eq!(out, "hello\n");
    }

    #[test]
    fn render_two_lines() {
        let mut root = LayoutBox::new(BoxType::Block);
        root.children.push(text_box("foo", 0.0, 0.0));
        root.children.push(text_box("bar", 0.0, 1.0));
        root.dimensions = Dimensions::new(0.0, 0.0, 80.0, 2.0);
        let tree = LayoutTree { root };
        let out = render_ascii(&tree, 80);
        assert_eq!(out, "foo\nbar\n");
    }

    #[test]
    fn render_preserves_leading_spaces_via_x_offset() {
        let mut root = LayoutBox::new(BoxType::Block);
        // A text at x=4 → output has 4 leading spaces.
        root.children.push(text_box("hi", 4.0, 0.0));
        root.dimensions = Dimensions::new(0.0, 0.0, 80.0, 1.0);
        let tree = LayoutTree { root };
        let out = render_ascii(&tree, 80);
        assert_eq!(out, "    hi\n");
    }

    #[test]
    fn render_skips_text_on_block_box() {
        // A Block box with text in its `text` field should NOT emit
        // (only Inline does). Put it as a child of root and verify.
        let mut root = LayoutBox::new(BoxType::Block);
        let mut block_with_text = LayoutBox::new(BoxType::Block).with_text("ignored".into());
        block_with_text.dimensions = Dimensions::new(0.0, 0.0, 80.0, 0.0);
        root.children.push(block_with_text);
        let tree = LayoutTree { root };
        let out = render_ascii(&tree, 80);
        assert_eq!(out, "");
    }

    #[test]
    fn render_nested_block_text() {
        // root > block > text("world")
        let mut inner = LayoutBox::new(BoxType::Block);
        inner.children.push(text_box("world", 0.0, 0.0));
        inner.dimensions = Dimensions::new(0.0, 0.0, 80.0, 1.0);
        let mut root = LayoutBox::new(BoxType::Block);
        root.children.push(inner);
        root.dimensions = Dimensions::new(0.0, 0.0, 80.0, 1.0);
        let tree = LayoutTree { root };
        assert_eq!(render_ascii(&tree, 80), "world\n");
    }

    #[test]
    fn render_clips_text_beyond_viewport() {
        let mut root = LayoutBox::new(BoxType::Block);
        // Text at x=78 + 5 chars = would go past 80-col viewport.
        root.children.push(text_box("hello", 78.0, 0.0));
        root.dimensions = Dimensions::new(0.0, 0.0, 80.0, 1.0);
        let tree = LayoutTree { root };
        let out = render_ascii(&tree, 80);
        // 78 leading spaces (cols 0..77 empty) + "he" (cols 78..79) + newline.
        let expected = format!("{}he\n", " ".repeat(78));
        assert_eq!(out, expected);
    }

    #[test]
    fn render_trims_trailing_whitespace() {
        let mut root = LayoutBox::new(BoxType::Block);
        root.children.push(text_box("a b", 0.0, 0.0));
        root.dimensions = Dimensions::new(0.0, 0.0, 80.0, 1.0);
        let tree = LayoutTree { root };
        let out = render_ascii(&tree, 80);
        // Should be "a b" + newline (no 76 trailing spaces).
        assert_eq!(out, "a b\n");
        assert!(out.len() < 10);
    }

    // silence unused helper
    #[test]
    fn block_with_helper_compiles() {
        let _ = block_with(vec![]);
    }

    // ---- M30: colored link rendering ----

    #[test]
    fn plain_render_no_ansi_for_link() {
        // `<a>` link text rendered in plain mode (default `render_ascii`)
        // must NOT contain ANSI escape codes — crawlers need clean ASCII.
        let mut link_box = LayoutBox::new(BoxType::Inline)
            .with_text("click".into())
            .with_link();
        link_box.dimensions = Dimensions::new(0.0, 0.0, 5.0, 1.0);
        let mut root = LayoutBox::new(BoxType::Block);
        root.children.push(link_box);
        root.dimensions = Dimensions::new(0.0, 0.0, 80.0, 1.0);
        let tree = LayoutTree { root };
        let out = render_ascii(&tree, 80);
        assert_eq!(out, "click\n");
        assert!(!out.contains("\x1b["));
    }

    #[test]
    fn colored_render_wraps_link_in_ansi() {
        let mut link_box = LayoutBox::new(BoxType::Inline)
            .with_text("go".into())
            .with_link();
        link_box.dimensions = Dimensions::new(0.0, 0.0, 2.0, 1.0);
        let mut root = LayoutBox::new(BoxType::Block);
        root.children.push(link_box);
        root.dimensions = Dimensions::new(0.0, 0.0, 80.0, 1.0);
        let tree = LayoutTree { root };
        let out = render_ascii_colored(&tree, 80);
        // Expected: underline+blue ANSI + "go" + reset + newline.
        assert_eq!(out, "\x1b[4;38;2;0;0;238mgo\x1b[0m\n");
    }

    #[test]
    fn colored_render_link_surrounded_by_plain() {
        // "pre" then link "MID" then plain "post" on same line.
        let mut pre = text_box("pre ", 0.0, 0.0);
        pre.dimensions = Dimensions::new(0.0, 0.0, 4.0, 1.0);
        let mut mid = LayoutBox::new(BoxType::Inline)
            .with_text("MID".into())
            .with_link();
        mid.dimensions = Dimensions::new(4.0, 0.0, 3.0, 1.0);
        let mut post = text_box(" post", 7.0, 0.0);
        post.dimensions = Dimensions::new(7.0, 0.0, 5.0, 1.0);
        let mut root = LayoutBox::new(BoxType::Block);
        root.children = vec![pre, mid, post];
        root.dimensions = Dimensions::new(0.0, 0.0, 80.0, 1.0);
        let tree = LayoutTree { root };
        let plain = render_ascii(&tree, 80);
        assert_eq!(plain, "pre MID post\n");
        let colored = render_ascii_colored(&tree, 80);
        assert_eq!(colored, "pre \x1b[4;38;2;0;0;238mMID\x1b[0m post\n");
    }
}
