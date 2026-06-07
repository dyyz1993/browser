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
}

impl CharBuffer {
    fn new(width: usize, height: usize) -> Self {
        let rows = (0..height).map(|_| vec![' '; width]).collect();
        let links = (0..height).map(|_| vec![false; width]).collect();
        Self { width, rows, links }
    }

    fn put(&mut self, y: usize, x: usize, c: char, link: bool) {
        if y < self.rows.len() && x < self.width {
            self.rows[y][x] = c;
            if link {
                self.links[y][x] = true;
            }
        }
    }

    /// M30: Render to string. When `colored=true`, runs of link cells
    /// are wrapped in ANSI underline+blue escape sequences. Trailing
    /// whitespace on each line is trimmed (links are never whitespace).
    fn to_string(&self, colored: bool) -> String {
        let mut out = String::new();
        for y in 0..self.rows.len() {
            let row = &self.rows[y];
            // Find last non-space column (trim_end).
            let mut last = 0usize;
            for x in (0..self.width).rev() {
                if row[x] != ' ' {
                    last = x + 1;
                    break;
                }
            }
            let mut x = 0;
            while x < last {
                if colored && self.links[y][x] {
                    out.push_str("\x1b[4;34m");
                    while x < last && self.links[y][x] {
                        out.push(row[x]);
                        x += 1;
                    }
                    out.push_str("\x1b[0m");
                } else {
                    out.push(row[x]);
                    x += 1;
                }
            }
            out.push('\n');
        }
        out
    }
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
        assert_eq!(out, "\x1b[4;34mgo\x1b[0m\n");
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
        assert_eq!(colored, "pre \x1b[4;34mMID\x1b[0m post\n");
    }
}
