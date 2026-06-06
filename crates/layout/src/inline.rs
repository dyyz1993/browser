//! Inline layout: word-wrap text within a containing width.
//!
//! M2.6 scope: given an anonymous block's inline children, assign
//! each text box a starting (x, y) and a height based on how many
//! wrapped lines the text occupies. The actual character placement
//! happens later in the renderer (`render::ascii`).
//!
//! Wrapping rule (greedy, per-box):
//! - Split text on whitespace into words.
//! - Walk words left to right; emit a space before each word except
//!   the first one on a line. If the next word + its leading space
//!   does not fit, push it to a new line.
//! - A single word longer than the line still gets placed (overflowing);
//!   we don't break inside words in M2.

use crate::boxes::{BoxType, LayoutBox};

/// Lay out a sequence of inline boxes belonging to one anonymous block.
///
/// Mutates each box's `dimensions`:
/// - `x`, `y`: position of the first character of the box's text
/// - `width`: `containing_width` (the box's allocated row width)
/// - `height`: number of lines this box's text occupies (>= 1 if non-empty)
///
/// Returns the total number of lines consumed by the run.
pub fn layout_inline_run(
    boxes: &mut [LayoutBox],
    x_start: f32,
    y_start: f32,
    containing_width: f32,
) -> f32 {
    let mut cursor_x = x_start;
    let mut cursor_y = y_start;

    for bx in boxes.iter_mut() {
        // Anonymous wrappers inside an inline run (rare in M2 but
        // possible) — recurse.
        if bx.box_type == BoxType::Anonymous {
            let h = layout_inline_run(&mut bx.children, cursor_x, cursor_y, containing_width);
            bx.dimensions.x = x_start;
            bx.dimensions.y = cursor_y;
            bx.dimensions.width = containing_width;
            bx.dimensions.height = h;
            cursor_y += h;
            cursor_x = x_start;
            continue;
        }

        let Some(text) = bx.text.clone() else {
            // Inline element without direct text (e.g. <span><b>…</b></span>).
            // Recurse into its children at the current cursor.
            let h = layout_inline_run(&mut bx.children, cursor_x, cursor_y, containing_width);
            bx.dimensions.x = cursor_x;
            bx.dimensions.y = cursor_y;
            bx.dimensions.width = containing_width - (cursor_x - x_start);
            bx.dimensions.height = h;
            cursor_y += h;
            cursor_x = x_start;
            continue;
        };

        let words: Vec<&str> = text.split_whitespace().collect();
        if words.is_empty() {
            bx.dimensions.x = cursor_x;
            bx.dimensions.y = cursor_y;
            bx.dimensions.width = 0.0;
            bx.dimensions.height = 0.0;
            continue;
        }

        let box_start_y = cursor_y;
        let mut line_start_x = cursor_x;

        for (i, word) in words.iter().enumerate() {
            let word_cols = word.chars().count() as f32;
            // First word on a fresh line: no leading space. Otherwise: 1 col.
            let needed = if cursor_x == line_start_x {
                word_cols
            } else {
                1.0 + word_cols
            };
            let available = (x_start + containing_width) - cursor_x;
            if needed > available && cursor_x > line_start_x {
                // Wrap.
                cursor_y += 1.0;
                cursor_x = x_start;
                line_start_x = x_start;
            }
            if cursor_x > line_start_x {
                cursor_x += 1.0; // leading space
            }
            // If this is the FIRST word of the box, freeze the box's (x, y).
            if i == 0 {
                bx.dimensions.x = cursor_x;
                bx.dimensions.y = cursor_y;
            }
            cursor_x += word_cols;
        }

        // Box height = number of lines it spanned (>= 1).
        let lines_used = (cursor_y - box_start_y).round() as i32 + 1;
        bx.dimensions.height = lines_used.max(1) as f32;
        bx.dimensions.width = containing_width;
        // Advance to next line for the next sibling box.
        cursor_y += 1.0;
        cursor_x = x_start;
    }

    (cursor_y - y_start).max(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::boxes::{BoxType, LayoutBox};

    fn inline_text(s: &str) -> LayoutBox {
        LayoutBox::new(BoxType::Inline).with_text(s.into())
    }

    #[test]
    fn short_text_fits_on_one_line() {
        let mut boxes = vec![inline_text("hello")];
        let h = layout_inline_run(&mut boxes, 0.0, 0.0, 80.0);
        // Single non-empty box consumes 1 line for itself + 1 line advance
        // for sibling separation = 1.0 in this implementation.
        assert!(h >= 1.0, "h={h}");
        assert_eq!(boxes[0].dimensions.x, 0.0);
        assert_eq!(boxes[0].dimensions.y, 0.0);
        assert_eq!(boxes[0].dimensions.height, 1.0);
    }

    #[test]
    fn long_text_wraps_to_multiple_lines() {
        // 80-col width; 200 chars should wrap to >= 3 lines.
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
        let _h = layout_inline_run(&mut boxes, 0.0, 0.0, 80.0);
        assert_eq!(boxes[0].dimensions.height, 0.0);
    }

    #[test]
    fn whitespace_only_text_treated_as_empty() {
        let mut boxes = vec![inline_text("   \t\n  ")];
        let _h = layout_inline_run(&mut boxes, 0.0, 0.0, 80.0);
        assert_eq!(boxes[0].dimensions.height, 0.0);
    }

    #[test]
    fn two_short_boxes_stack_vertically() {
        let mut boxes = vec![inline_text("a"), inline_text("b")];
        let _h = layout_inline_run(&mut boxes, 0.0, 0.0, 80.0);
        assert_eq!(boxes[0].dimensions.y, 0.0);
        assert_eq!(boxes[1].dimensions.y, 1.0);
    }

    #[test]
    fn narrow_width_forces_wrap_on_long_word_no_break() {
        // A single long word "supercalifragilistic" (20 chars) in a
        // 5-col line: we don't break inside words, so it occupies 1
        // line and overflows. Height = 1.
        let mut boxes = vec![inline_text("supercalifragilistic")];
        let _h = layout_inline_run(&mut boxes, 0.0, 0.0, 5.0);
        assert_eq!(boxes[0].dimensions.height, 1.0);
        assert_eq!(boxes[0].dimensions.x, 0.0);
    }

    #[test]
    fn multiple_words_split_across_lines() {
        // Width 10, text = "aaa bbb ccc ddd" (each word 3 chars + 1 space).
        // Expected layout:
        //   line 0: "aaa bbb ccc" (3+1+3+1+3 = 11... actually 11 > 10)
        // We're greedy: "aaa bbb ccc" needs 11 chars, so "ccc" wraps.
        // Line 0: "aaa bbb" (7 chars)
        // Line 1: "ccc ddd" (7 chars)
        // → 2 lines.
        let mut boxes = vec![inline_text("aaa bbb ccc ddd")];
        let _h = layout_inline_run(&mut boxes, 0.0, 0.0, 10.0);
        assert_eq!(
            boxes[0].dimensions.height, 2.0,
            "expected 2 lines for 4 short words in 10 cols, got {}",
            boxes[0].dimensions.height
        );
    }
}
