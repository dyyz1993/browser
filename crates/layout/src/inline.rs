//! Inline layout: word-wrap text within a containing width.
//!
//! M2.6+M2.9 scope: assign (x, y) and height to every inline box
//! (text leaves) so the renderer can place characters.
//!
//! Wrapping rule (greedy):
//! - Split text on whitespace into words.
//! - Pack words left to right on the current line. Each non-first
//!   word consumes 1 column for a leading space.
//! - If the next word + leading space doesn't fit, wrap to next line.
//! - A single word longer than the line still gets placed (overflowing).

use crate::boxes::{BoxType, LayoutBox};

/// Lay out a sequence of top-level boxes belonging to one anonymous block.
///
/// Mutates inline leaf boxes' `dimensions`. Returns total height
/// consumed (in lines).
pub fn layout_inline_run(
    boxes: &mut [LayoutBox],
    x_start: f32,
    y_start: f32,
    containing_width: f32,
) -> f32 {
    let mut state = LayoutState::new(x_start, y_start, containing_width);
    for bx in boxes.iter_mut() {
        layout_box_recursive(bx, &mut state);
    }
    if state.cursor_x == x_start && state.cursor_y == y_start {
        0.0
    } else {
        state.cursor_y - y_start + 1.0
    }
}

struct LayoutState {
    x_start: f32,
    cursor_x: f32,
    cursor_y: f32,
    width: f32,
}

impl LayoutState {
    fn new(x_start: f32, y_start: f32, width: f32) -> Self {
        Self {
            x_start,
            cursor_x: x_start,
            cursor_y: y_start,
            width,
        }
    }

    /// Place one word. Returns (start_x, start_y) of the word's first
    /// character — useful for the caller to record where its text
    /// actually begins.
    fn place_word(&mut self, word: &str) -> (f32, f32) {
        let word_cols = word.chars().count() as f32;
        let on_line_start = self.cursor_x == self.x_start;
        let needed = if on_line_start {
            word_cols
        } else {
            1.0 + word_cols
        };
        let available = self.x_start + self.width - self.cursor_x;
        if !on_line_start && needed > available {
            self.cursor_y += 1.0;
            self.cursor_x = self.x_start;
        }
        if self.cursor_x > self.x_start {
            self.cursor_x += 1.0; // leading space
        }
        let start_x = self.cursor_x;
        let start_y = self.cursor_y;
        self.cursor_x += word_cols;
        (start_x, start_y)
    }
}

fn layout_box_recursive(bx: &mut LayoutBox, state: &mut LayoutState) {
    if bx.box_type == BoxType::Anonymous {
        bx.dimensions.x = state.x_start;
        bx.dimensions.y = state.cursor_y;
        for child in bx.children.iter_mut() {
            layout_box_recursive(child, state);
        }
        bx.dimensions.height = (state.cursor_y - bx.dimensions.y).max(0.0) + 1.0;
        // M70.1: don't double-advance cursor_y. The height already accounts for
        // the line (the +1.0 above), and the parent block advances by
        // `child.dimensions.bottom()` (= y + height), so an extra `cursor_y += 1`
        // here produced a spurious blank line after each text run.
        state.cursor_x = state.x_start;
        return;
    }

    // Inline box (with or without direct text).
    bx.dimensions.x = state.cursor_x;
    bx.dimensions.y = state.cursor_y;

    if let Some(text) = bx.text.clone() {
        let words: Vec<&str> = text.split_whitespace().collect();
        if words.is_empty() {
            bx.dimensions.height = 0.0;
            bx.dimensions.width = 0.0;
            bx.words.clear();
            return;
        }
        bx.words.clear();
        bx.words.reserve(words.len());
        let start_y = state.cursor_y;
        for (i, word) in words.iter().enumerate() {
            let (sx, sy) = state.place_word(word);
            bx.words.push(((*word).to_string(), sx, sy));
            if i == 0 {
                bx.dimensions.x = sx;
                bx.dimensions.y = sy;
            }
        }
        bx.dimensions.height = (state.cursor_y - start_y).round() + 1.0;
        bx.dimensions.width = (state.cursor_x - bx.dimensions.x).max(0.0);
    } else {
        // Inline wrapper (e.g. <span>, <a>): recurse; its dimensions
        // span the bounding box of its children.
        for child in bx.children.iter_mut() {
            layout_box_recursive(child, state);
        }
        bx.dimensions.height = (state.cursor_y - bx.dimensions.y).max(0.0) + 1.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::boxes::{BoxType, LayoutBox};

    fn inline_text(s: &str) -> LayoutBox {
        LayoutBox::new(BoxType::Inline).with_text(s.into())
    }

    fn inline_wrap(children: Vec<LayoutBox>) -> LayoutBox {
        let mut b = LayoutBox::new(BoxType::Inline);
        b.children = children;
        b
    }

    #[test]
    fn short_text_fits_on_one_line() {
        let mut boxes = vec![inline_text("hello")];
        let h = layout_inline_run(&mut boxes, 0.0, 0.0, 80.0);
        assert!(h >= 1.0);
        assert_eq!(boxes[0].dimensions.x, 0.0);
        assert_eq!(boxes[0].dimensions.y, 0.0);
    }

    #[test]
    fn long_text_wraps_to_multiple_lines() {
        let long_text = "word ".repeat(50);
        let mut boxes = vec![inline_text(long_text.trim())];
        let _h = layout_inline_run(&mut boxes, 0.0, 0.0, 80.0);
        assert!(
            boxes[0].dimensions.height >= 3.0,
            "expected >=3 lines, got {}",
            boxes[0].dimensions.height
        );
    }

    #[test]
    fn empty_text_has_zero_height() {
        let mut boxes = vec![inline_text("")];
        let h = layout_inline_run(&mut boxes, 0.0, 0.0, 80.0);
        assert_eq!(h, 0.0);
        assert_eq!(boxes[0].dimensions.height, 0.0);
    }

    #[test]
    fn two_short_boxes_share_line_with_space_between() {
        let mut boxes = vec![inline_text("alpha"), inline_text("beta")];
        let _h = layout_inline_run(&mut boxes, 0.0, 0.0, 80.0);
        // alpha at (0, 0), beta at (6, 0) (alpha=5 chars + leading space).
        assert_eq!(boxes[0].dimensions.x, 0.0);
        assert_eq!(boxes[1].dimensions.x, 6.0);
        assert_eq!(boxes[0].dimensions.y, boxes[1].dimensions.y);
    }

    #[test]
    fn inline_wrapper_recurse_into_text_child() {
        // <span><a>text</a></span>
        let text = inline_text("inside wrapper");
        let a = inline_wrap(vec![text]);
        let span = inline_wrap(vec![a]);
        let mut boxes = vec![span];
        let h = layout_inline_run(&mut boxes, 0.0, 0.0, 80.0);
        assert!(h >= 1.0);
        let span_b = &boxes[0];
        let a_b = &span_b.children[0];
        let text_b = &a_b.children[0];
        assert_eq!(text_b.dimensions.x, 0.0);
        assert_eq!(text_b.dimensions.y, 0.0);
        assert!(text_b.dimensions.height >= 1.0);
    }

    #[test]
    fn mixed_text_and_wrapper_in_order() {
        // Inline run: [text "A"], [wrap > text "B"], [text "C"]
        let a = inline_text("A");
        let wrap = inline_wrap(vec![inline_text("B")]);
        let c = inline_text("C");
        let mut boxes = vec![a, wrap, c];
        let _h = layout_inline_run(&mut boxes, 0.0, 0.0, 80.0);
        // Order on line: A (x=0), B (x=2: A=1 + space), C (x=4: B=1 + space).
        assert_eq!(boxes[0].dimensions.x, 0.0); // A
        let wrap_b = &boxes[1];
        let inner_b = &wrap_b.children[0];
        assert_eq!(inner_b.dimensions.x, 2.0); // B
        assert_eq!(boxes[2].dimensions.x, 4.0); // C
        assert_eq!(boxes[0].dimensions.y, boxes[2].dimensions.y);
    }

    #[test]
    fn narrow_width_forces_wrap() {
        let mut boxes = vec![inline_text("aaa bbb ccc ddd")];
        let _h = layout_inline_run(&mut boxes, 0.0, 0.0, 10.0);
        assert_eq!(
            boxes[0].dimensions.height, 2.0,
            "expected 2 lines for 4 short words in 10 cols, got {}",
            boxes[0].dimensions.height
        );
    }

    #[test]
    fn whitespace_only_text_treated_as_empty() {
        let mut boxes = vec![inline_text("   \t  ")];
        let h = layout_inline_run(&mut boxes, 0.0, 0.0, 80.0);
        assert_eq!(h, 0.0);
    }
}
