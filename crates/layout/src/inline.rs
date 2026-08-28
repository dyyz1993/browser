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

    // M78.142: Block-level boxes (Block / Flex / Grid) inside an inline run
    // are atomic per CSS — `display:flex` on an inline tag (`<a>`, `<span>`)
    // still creates a block-level box that breaks the line. Lay them out
    // with their real layout algorithm at the current position, then move
    // any following inline content to a fresh line. Previously these fell
    // through to the inline-wrapper recursion below: flex/grid layout never
    // ran, children stacked at overlapping x/y, and their painted words
    // interleaved into glued garbage ("Addcomponentswithout...").
    if matches!(bx.box_type, BoxType::Block | BoxType::Flex | BoxType::Grid) {
        let em = 1.0_f32;
        let margin_left = bx.margin.left.resolve(state.width, em);
        let margin_top = bx.margin.top.resolve(state.width, em);
        let margin_bottom = bx.margin.bottom.resolve(state.width, em);
        // Inline content already on this line → the block starts on a new
        // line (CSS line-break semantics around block-level boxes).
        if state.cursor_x > state.x_start {
            state.cursor_y += 1.0;
            state.cursor_x = state.x_start;
        }
        state.cursor_y += margin_top;
        crate::block::layout_box_pub(bx, state.x_start + margin_left, state.cursor_y, state.width);
        state.cursor_y = bx.dimensions.bottom() + margin_bottom;
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

    // ---- M78.142: block-level boxes inside an inline run ----

    #[test]
    fn flex_box_inside_inline_run_runs_flex_layout() {
        // <a style="display:flex;flex-direction:column"> inside an anonymous
        // inline wrapper: the flex container must lay out its children with
        // the real flex algorithm (column stacking), not inline recursion.
        let mut inner = LayoutBox::new(BoxType::Flex);
        inner.flex.direction = crate::boxes::FlexDirection::Column;
        inner.children = vec![inline_text("AA"), inline_text("BB")];
        let mut boxes = vec![inline_text("hi "), inner, inline_text(" tail")];
        let _h = layout_inline_run(&mut boxes, 0.0, 0.0, 80.0);
        let flex = &boxes[1];
        let aa = &flex.children[0];
        let bb = &flex.children[1];
        // Column flex: BB must be on a line BELOW AA (previously both were
        // inline-recursed onto the same cursor line).
        assert!(
            bb.dimensions.y > aa.dimensions.y,
            "column flex items must stack, got aa.y={} bb.y={}",
            aa.dimensions.y,
            bb.dimensions.y
        );
        // Following inline text must start on a fresh line below the flex box.
        let tail = &boxes[2];
        assert!(
            tail.dimensions.y >= bb.dimensions.y,
            "inline tail must not overlap flex content, tail.y={} bb.y={}",
            tail.dimensions.y,
            bb.dimensions.y
        );
    }

    #[test]
    fn nested_flex_row_in_flex_item_keeps_gap() {
        // flex item that is itself a flex row with gap: the inner gap must
        // produce visible spacing between spans (M78.142 dispatch in
        // flex::layout_box_into).
        let mut inner = LayoutBox::new(BoxType::Flex);
        inner.flex.gap = 5.0;
        inner.children = vec![inline_text("AAA"), inline_text("BB")];
        let mut outer = LayoutBox::new(BoxType::Flex);
        outer.flex.direction = crate::boxes::FlexDirection::Column;
        outer.children = vec![inner];
        let mut tree = crate::boxes::LayoutTree { root: outer };
        crate::block::layout(
            &mut tree,
            crate::block::LayoutConfig {
                viewport_width: 80.0,
            },
        );
        let inner = &tree.root.children[0];
        let a = &inner.children[0];
        let b = &inner.children[1];
        let gap = b.dimensions.x - (a.dimensions.x + a.dimensions.width);
        assert!(
            (gap - 5.0).abs() < 0.5,
            "expected inner gap 5 between spans, got {gap}"
        );
    }
}
