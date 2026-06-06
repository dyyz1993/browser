//! Terminal ASCII renderer.
//!
//! Walks a laid-out [`LayoutTree`] and writes characters into a 2D
//! character buffer, then collapses the buffer into a `String` for
//! printing. Coordinate system matches layout: y grows downward.

use std::fmt::Write;

use browser_layout::{BoxType, LayoutBox, LayoutTree};

/// Render a laid-out tree into an ASCII string.
///
/// `viewport_width` is the number of columns to allocate. Lines that
/// would exceed the buffer are clipped at the right edge; lines past
/// the bottom of the content are not emitted.
#[must_use]
pub fn render_ascii(tree: &LayoutTree, viewport_width: usize) -> String {
    // First pass: compute the maximum y (line index) any text reaches.
    let mut max_y = 0usize;
    collect_text_extent(&tree.root, &mut max_y);
    if max_y == 0 {
        return String::new();
    }

    let mut buf = CharBuffer::new(viewport_width, max_y);
    paint(&tree.root, &mut buf);

    buf.to_string()
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
        if let Some(text) = &bx.text {
            let mut x = bx.dimensions.x.round() as usize;
            let y = bx.dimensions.y.round() as usize;
            for ch in text.chars() {
                if ch == '\n' || ch == '\r' {
                    continue;
                }
                if ch.is_whitespace() {
                    // Use a space char so adjacent words stay separated
                    // in the rendered output.
                    buf.put(y, x, ' ');
                    x += 1;
                } else {
                    buf.put(y, x, ch);
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
}

impl std::fmt::Display for CharBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for row in &self.rows {
            let s: String = row.iter().collect();
            let trimmed = s.trim_end();
            f.write_str(trimmed)?;
            f.write_char('\n')?;
        }
        Ok(())
    }
}

impl CharBuffer {
    fn new(width: usize, height: usize) -> Self {
        let rows = (0..height).map(|_| vec![' '; width]).collect();
        Self { width, rows }
    }

    fn put(&mut self, y: usize, x: usize, c: char) {
        if y < self.rows.len() && x < self.width {
            self.rows[y][x] = c;
        }
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
}
