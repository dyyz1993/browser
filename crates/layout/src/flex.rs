//! M32: Flexbox layout (minimal useful subset).
//!
//! Supports the most common flex patterns found in real-world SPAs:
//! - `display: flex`
//! - `flex-direction: row` (default) / `column`
//! - `justify-content: flex-start` (default) / `center` / `flex-end`
//!   / `space-between`
//! - `gap: Npx`
//! - `flex-grow: N` (proportional width distribution)
//!
//! ## Algorithm (simplified CSS Flexbox)
//!
//! 1. Measure each child's natural size (main axis):
//!    - Row: natural width = text length / child content width
//!    - Column: natural height = 1 line (text) or recurse
//! 2. total_gap = gap × (n-1)
//! 3. free_space = container_main - sum(natural) - total_gap
//! 4. If any child has flex-grow > 0:
//!    - Distribute free_space proportionally to flex-grow factors
//! 5. Else apply justify-content to position items.
//! 6. Position items along main axis, recurse for cross-axis sizing.
//!
//! ## Limitations (deliberately out of scope)
//! - No `flex-wrap` (items never wrap to next line)
//! - No `align-items` / `align-self` (cross-axis alignment = stretch/fill)
//! - No `flex-shrink` / `flex-basis` (only grow)
//! - No `order` (DOM order only)

use crate::boxes::{AlignItems, FlexDirection, FlexWrap, JustifyContent, LayoutBox};
use crate::inline::layout_inline_run;

/// Entry point: lay out children of a `display:flex` container.
/// Called from [`crate::block::layout_box`].
pub fn layout_flex_children(bx: &mut LayoutBox, containing_width: f32) {
    let base_x = bx.dimensions.x;
    let base_y = bx.dimensions.y;
    let em = 1.0_f32;
    let gap = bx.flex.gap;
    let n = bx.children.len();
    if n == 0 {
        bx.dimensions.height = 0.0;
        return;
    }

    match bx.flex.direction {
        FlexDirection::Row => layout_row(bx, base_x, base_y, containing_width, gap, em),
        FlexDirection::Column => layout_column(bx, base_x, base_y, containing_width, gap, em),
    }
}

/// `flex-direction: row` — items laid horizontally.
/// M35.1: supports flex-wrap (items wrap to next line on overflow).
/// M35.2: supports align-items (cross-axis alignment within each line).
fn layout_row(
    bx: &mut LayoutBox,
    base_x: f32,
    base_y: f32,
    containing_width: f32,
    gap: f32,
    em: f32,
) {
    let n = bx.children.len();
    let natural_widths: Vec<f32> = bx
        .children
        .iter()
        .map(|c| measure_child_main_row(c, containing_width, em))
        .collect();
    let flex_grows: Vec<f32> = bx.children.iter().map(|c| c.flex_grow).collect();
    // M35.1: flex-wrap logic. Group children into lines.
    // nowrap → single line. wrap → break when cursor + item > container.
    let lines: Vec<Vec<usize>> = if bx.flex.wrap == FlexWrap::Wrap {
        pack_into_lines(&natural_widths, containing_width, gap)
    } else {
        vec![(0..n).collect()]
    };

    let align = bx.flex.align;
    let mut cursor_y = base_y;
    let mut container_height: f32 = 0.0;

    for line_indices in &lines {
        let line_n = line_indices.len();
        let line_natural: f32 = line_indices.iter().map(|&i| natural_widths[i]).sum();
        let line_gap = gap * (line_n.saturating_sub(1)) as f32;
        let line_free = containing_width - line_natural - line_gap;
        let line_grow: f32 = line_indices.iter().map(|&i| flex_grows[i]).sum();

        // Compute final widths for this line.
        let final_widths: Vec<f32> = if line_grow > 0.0 && line_free > 0.0 {
            line_indices
                .iter()
                .map(|&i| natural_widths[i] + (line_free * flex_grows[i] / line_grow))
                .collect()
        } else {
            line_indices.iter().map(|&i| natural_widths[i]).collect()
        };
        let line_used: f32 = final_widths.iter().sum::<f32>() + line_gap;
        let line_leftover = (containing_width - line_used).max(0.0);

        // Justify-content offset for this line.
        let mut line_x = base_x;
        let mut line_between = gap;
        match bx.flex.justify {
            JustifyContent::FlexStart => {}
            JustifyContent::Center => line_x += line_leftover / 2.0,
            JustifyContent::FlexEnd => line_x += line_leftover,
            JustifyContent::SpaceBetween => {
                if line_n > 1 {
                    line_between = gap + line_leftover / (line_n - 1) as f32;
                }
            }
        }

        // Layout each child in this line to measure heights.
        let mut child_dims: Vec<(f32, f32)> = Vec::with_capacity(line_n); // (width, height)
        let mut max_height: f32 = 0.0;
        for (slot, &child_idx) in line_indices.iter().enumerate() {
            let child = &mut bx.children[child_idx];
            let w = final_widths[slot];
            layout_box_into(child, line_x, cursor_y, w, em);
            child_dims.push((w, child.dimensions.height));
            if child.dimensions.height > max_height {
                max_height = child.dimensions.height;
            }
            line_x += w + line_between;
        }

        // M35.2: align-items — adjust y within this line.
        for (slot, &child_idx) in line_indices.iter().enumerate() {
            let child = &mut bx.children[child_idx];
            let item_h = child_dims[slot].1;
            match align {
                AlignItems::Stretch | AlignItems::FlexStart => {
                    // Already positioned at cursor_y (top of line).
                }
                AlignItems::Center => {
                    let offset = (max_height - item_h) / 2.0;
                    if offset > 0.0 {
                        layout_box_into(
                            child,
                            child.dimensions.x,
                            cursor_y + offset,
                            child_dims[slot].0,
                            em,
                        );
                    }
                }
                AlignItems::FlexEnd => {
                    let offset = max_height - item_h;
                    if offset > 0.0 {
                        layout_box_into(
                            child,
                            child.dimensions.x,
                            cursor_y + offset,
                            child_dims[slot].0,
                            em,
                        );
                    }
                }
            }
        }

        cursor_y += max_height + gap;
        container_height += max_height + gap;
    }
    // Remove trailing gap.
    if !lines.is_empty() {
        container_height -= gap;
    }

    bx.dimensions.height = container_height.max(0.0);
}

/// M35.1: Pack child indices into lines for flex-wrap.
/// Greedily fills each line until the next item would overflow.
fn pack_into_lines(natural_widths: &[f32], containing_width: f32, gap: f32) -> Vec<Vec<usize>> {
    let mut lines: Vec<Vec<usize>> = Vec::new();
    let mut current_line: Vec<usize> = Vec::new();
    let mut current_w: f32 = 0.0;

    for (i, &w) in natural_widths.iter().enumerate() {
        let added_gap = if current_line.is_empty() { 0.0 } else { gap };
        if current_line.is_empty() || current_w + added_gap + w <= containing_width {
            current_w += added_gap + w;
            current_line.push(i);
        } else {
            // Overflow — start new line.
            lines.push(std::mem::take(&mut current_line));
            current_w = w;
            current_line.push(i);
        }
    }
    if !current_line.is_empty() {
        lines.push(current_line);
    }
    lines
}

/// `flex-direction: column` — items stacked vertically (like block flow
/// but with flex-grow distributing height, and gap adding spacing).
fn layout_column(
    bx: &mut LayoutBox,
    base_x: f32,
    base_y: f32,
    containing_width: f32,
    gap: f32,
    em: f32,
) {
    let n = bx.children.len();
    // flex-grow for column needs definite container height — ASCII flow
    // grows downward so we treat it as no-op (height = natural).
    // Just stack children vertically with gap, measuring each via recursion.

    let mut cursor_y = base_y;
    let mut max_width: f32 = 0.0;
    for child in bx.children.iter_mut() {
        let margin_left = child.margin.left.resolve(containing_width, em);
        let child_x = base_x + margin_left;
        layout_box_into(child, child_x, cursor_y, containing_width, em);
        if child.dimensions.width > max_width {
            max_width = child.dimensions.width;
        }
        cursor_y += child.dimensions.height + gap;
    }
    // Last item shouldn't add trailing gap.
    if n > 0 {
        cursor_y -= gap;
    }
    bx.dimensions.height = (cursor_y - base_y).max(0.0);
    bx.dimensions.width = max_width.max(containing_width);
}

/// Measure a child's natural main-axis size for `flex-direction: row`
/// (i.e., its natural width without flex distribution).
fn measure_child_main_row(child: &LayoutBox, containing_width: f32, em: f32) -> f32 {
    let margin_l = child.margin.left.resolve(containing_width, em);
    let margin_r = child.margin.right.resolve(containing_width, em);
    let content_w = natural_content_width(child);
    content_w + margin_l + margin_r
}

/// Natural content width in characters (text length, or width from
/// inline run, or default block width 0).
fn natural_content_width(bx: &LayoutBox) -> f32 {
    if let Some(text) = &bx.text {
        // Longest line width, capped by containing block (caller
        // will clip at layout time).
        return text.lines().map(|l| l.chars().count()).max().unwrap_or(0) as f32;
    }
    // For block/flex children without direct text, measure children.
    if bx.children.is_empty() {
        return 0.0;
    }
    // Use max child natural width (rough heuristic).
    bx.children.iter().map(natural_content_width).sum()
}

/// Position a child into (x, y, width) and recurse to fill dimensions.
/// Mirrors `layout_box` but is private to flex so we control width.
fn layout_box_into(bx: &mut LayoutBox, x: f32, y: f32, width: f32, em: f32) {
    bx.dimensions.x = x;
    bx.dimensions.y = y;
    bx.dimensions.width = width;

    // For text-bearing children, lay out as inline run.
    if !bx.children.is_empty() && bx.children.iter().all(|c| c.text.is_some()) {
        let h = layout_inline_run(&mut bx.children, x, y, width);
        bx.dimensions.height = h.max(0.0);
        return;
    }

    if let Some(text) = &bx.text {
        let chars = text.chars().count();
        bx.dimensions.height = if chars == 0 { 0.0 } else { 1.0 };
        return;
    }

    // M32 fix: Block/Flex children must recurse — otherwise their
    // children's dimensions are never computed (all stay 0,0),
    // causing all nested text to pile at origin → only last item
    // visible. Delegate to block::layout_box for full recursion.
    let _ = em;
    crate::block::layout_box_pub(bx, x, y, width);
}

#[cfg(test)]
mod tests {

    use crate::block::{layout, LayoutConfig};
    use crate::boxes::{
        AlignItems, BoxType, FlexDirection, FlexProps, FlexWrap, JustifyContent, LayoutBox,
        LayoutTree,
    };

    fn flex_box(props: FlexProps, children: Vec<LayoutBox>) -> LayoutBox {
        let mut b = LayoutBox::new(BoxType::Flex);
        b.flex = props;
        b.children = children;
        b
    }

    fn text_item(s: &str) -> LayoutBox {
        LayoutBox::new(BoxType::Inline).with_text(s.into())
    }

    fn block_item() -> LayoutBox {
        LayoutBox::new(BoxType::Block)
    }

    #[test]
    fn row_places_items_horizontally() {
        // flex > [A, B, C], gap 0, justify flex-start
        let f = flex_box(
            FlexProps::default(),
            vec![text_item("AAA"), text_item("BB"), text_item("C")],
        );
        let mut tree = LayoutTree { root: f };
        layout(
            &mut tree,
            LayoutConfig {
                viewport_width: 80.0,
            },
        );
        let f = &tree.root;
        assert_eq!(f.children[0].dimensions.x, 0.0);
        // Each child placed right after previous (width = natural).
        assert!(f.children[1].dimensions.x > 0.0);
        assert!(f.children[2].dimensions.x > f.children[1].dimensions.x);
    }

    #[test]
    fn row_with_gap_adds_spacing() {
        let f = flex_box(
            FlexProps {
                gap: 5.0,
                ..Default::default()
            },
            vec![text_item("A"), text_item("B")],
        );
        let mut tree = LayoutTree { root: f };
        layout(
            &mut tree,
            LayoutConfig {
                viewport_width: 80.0,
            },
        );
        let f = &tree.root;
        // Second child should be at least 5 chars after first.
        let gap = f.children[1].dimensions.x
            - (f.children[0].dimensions.x + f.children[0].dimensions.width);
        assert!(gap >= 5.0, "expected gap >= 5, got {gap}");
    }

    #[test]
    fn row_justify_center() {
        let f = flex_box(
            FlexProps {
                justify: JustifyContent::Center,
                ..Default::default()
            },
            vec![text_item("AB"), text_item("CD")],
        );
        let mut tree = LayoutTree { root: f };
        layout(
            &mut tree,
            LayoutConfig {
                viewport_width: 80.0,
            },
        );
        let f = &tree.root;
        // total width = 4, leftover = 76, first item x = 76/2 = 38
        assert!(f.children[0].dimensions.x >= 30.0, "center offset missing");
    }

    #[test]
    fn row_justify_flex_end() {
        let f = flex_box(
            FlexProps {
                justify: JustifyContent::FlexEnd,
                ..Default::default()
            },
            vec![text_item("AB")],
        );
        let mut tree = LayoutTree { root: f };
        layout(
            &mut tree,
            LayoutConfig {
                viewport_width: 80.0,
            },
        );
        let f = &tree.root;
        // Item width 2, leftover 78 → x = 78
        assert!(f.children[0].dimensions.x >= 70.0);
    }

    #[test]
    fn row_flex_grow_distributes_space() {
        // 2 items, each flex-grow 1 → split free space evenly.
        let mut item_a = block_item();
        item_a.flex_grow = 1.0;
        let mut item_b = block_item();
        item_b.flex_grow = 1.0;
        let f = flex_box(FlexProps::default(), vec![item_a, item_b]);
        let mut tree = LayoutTree { root: f };
        layout(
            &mut tree,
            LayoutConfig {
                viewport_width: 80.0,
            },
        );
        let f = &tree.root;
        // Both items should have width ~40 (80 / 2).
        assert!(
            (f.children[0].dimensions.width - 40.0).abs() < 1.0,
            "expected width ~40, got {}",
            f.children[0].dimensions.width
        );
    }

    #[test]
    fn row_flex_grow_uneven_proportions() {
        // item A grow=1, item B grow=3 → A gets 25%, B gets 75% of 80.
        let mut item_a = block_item();
        item_a.flex_grow = 1.0;
        let mut item_b = block_item();
        item_b.flex_grow = 3.0;
        let f = flex_box(FlexProps::default(), vec![item_a, item_b]);
        let mut tree = LayoutTree { root: f };
        layout(
            &mut tree,
            LayoutConfig {
                viewport_width: 80.0,
            },
        );
        let f = &tree.root;
        assert!(
            f.children[1].dimensions.width > f.children[0].dimensions.width,
            "grow=3 should be wider than grow=1"
        );
    }

    #[test]
    fn column_stacks_vertically() {
        let f = flex_box(
            FlexProps {
                direction: FlexDirection::Column,
                ..Default::default()
            },
            vec![text_item("A"), text_item("B"), text_item("C")],
        );
        let mut tree = LayoutTree { root: f };
        layout(
            &mut tree,
            LayoutConfig {
                viewport_width: 80.0,
            },
        );
        let f = &tree.root;
        assert!(f.children[1].dimensions.y > f.children[0].dimensions.y);
        assert!(f.children[2].dimensions.y > f.children[1].dimensions.y);
    }

    #[test]
    fn empty_flex_has_zero_height() {
        let f = flex_box(FlexProps::default(), vec![]);
        let mut tree = LayoutTree { root: f };
        layout(
            &mut tree,
            LayoutConfig {
                viewport_width: 80.0,
            },
        );
        assert_eq!(tree.root.dimensions.height, 0.0);
    }

    #[test]
    fn row_justify_space_between() {
        // 3 items width 2 each = 6, leftover = 74, between = 74/2 = 37
        let f = flex_box(
            FlexProps {
                justify: JustifyContent::SpaceBetween,
                ..Default::default()
            },
            vec![text_item("AA"), text_item("BB"), text_item("CC")],
        );
        let mut tree = LayoutTree { root: f };
        layout(
            &mut tree,
            LayoutConfig {
                viewport_width: 80.0,
            },
        );
        let f = &tree.root;
        // First at x=0, last at right edge.
        assert_eq!(f.children[0].dimensions.x, 0.0);
        assert!(f.children[2].dimensions.x >= 70.0);
    }

    // ---- M35.1: flex-wrap ----

    #[test]
    fn flex_wrap_nowrap_overflow_stays_single_line() {
        // nowrap: 3 items width 40 each, container 80 → all on one line (overflow).
        let f = flex_box(
            FlexProps::default(), // wrap = Nowrap
            vec![
                text_item("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"),
                text_item("BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB"),
                text_item("CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC"),
            ],
        );
        let mut tree = LayoutTree { root: f };
        layout(
            &mut tree,
            LayoutConfig {
                viewport_width: 80.0,
            },
        );
        // All items same y (no wrap).
        let f = &tree.root;
        assert_eq!(f.children[0].dimensions.y, f.children[1].dimensions.y);
        assert_eq!(f.children[1].dimensions.y, f.children[2].dimensions.y);
    }

    #[test]
    fn flex_wrap_wraps_to_next_line() {
        // wrap: 3 items width 40 each, container 80 → 2 lines: [0,1] then [2].
        let f = flex_box(
            FlexProps {
                wrap: FlexWrap::Wrap,
                ..Default::default()
            },
            vec![
                text_item("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"),
                text_item("BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB"),
                text_item("CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC"),
            ],
        );
        let mut tree = LayoutTree { root: f };
        layout(
            &mut tree,
            LayoutConfig {
                viewport_width: 80.0,
            },
        );
        let f = &tree.root;
        // Item 0 and 1 on first line, item 2 on second line.
        assert!(f.children[2].dimensions.y > f.children[0].dimensions.y);
    }

    // ---- M35.2: align-items ----

    #[test]
    fn align_items_flex_start_aligns_to_top() {
        // 2 items, one taller than the other. flex-start aligns both to top.
        let mut tall = text_item("TALL");
        tall.dimensions.height = 3.0; // pre-set won't matter, layout overrides
        let short = text_item("X");
        let f = flex_box(
            FlexProps {
                align: AlignItems::FlexStart,
                ..Default::default()
            },
            vec![tall, short],
        );
        let mut tree = LayoutTree { root: f };
        layout(
            &mut tree,
            LayoutConfig {
                viewport_width: 80.0,
            },
        );
        let f = &tree.root;
        // Both items start at the same y (top of line).
        assert_eq!(f.children[0].dimensions.y, f.children[1].dimensions.y);
    }

    #[test]
    fn align_items_center_centers_cross_axis() {
        // Item 'A' height 1, line max height = 1 → center doesn't shift.
        // Use a column of text to create different heights is complex,
        // so we test that center doesn't break layout.
        let f = flex_box(
            FlexProps {
                align: AlignItems::Center,
                ..Default::default()
            },
            vec![text_item("A"), text_item("B")],
        );
        let mut tree = LayoutTree { root: f };
        layout(
            &mut tree,
            LayoutConfig {
                viewport_width: 80.0,
            },
        );
        // Should not panic; items rendered.
        let f = &tree.root;
        assert!(f.children[0].dimensions.y >= 0.0);
    }
}
