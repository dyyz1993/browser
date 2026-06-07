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

use crate::boxes::{FlexDirection, JustifyContent, LayoutBox};
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
fn layout_row(
    bx: &mut LayoutBox,
    base_x: f32,
    base_y: f32,
    containing_width: f32,
    gap: f32,
    em: f32,
) {
    let n = bx.children.len();
    // 1. Measure natural width of each child + track flex-grow.
    //    We compute natural widths up front (immutable borrow), then
    //    position children (mutable borrow) in a second pass.
    let natural_widths: Vec<f32> = bx
        .children
        .iter()
        .map(|c| measure_child_main_row(c, containing_width, em))
        .collect();
    let flex_grows: Vec<f32> = bx.children.iter().map(|c| c.flex_grow).collect();
    let total_gap = gap * (n.saturating_sub(1)) as f32;
    let total_natural: f32 = natural_widths.iter().sum();
    let free_space = containing_width - total_natural - total_gap;
    let total_grow: f32 = flex_grows.iter().sum();

    // 2. Compute final widths.
    let final_widths: Vec<f32> = if total_grow > 0.0 && free_space > 0.0 {
        // Distribute free space proportionally to flex-grow.
        natural_widths
            .iter()
            .zip(flex_grows.iter())
            .map(|(nat, &g)| nat + (free_space * g / total_grow))
            .collect()
    } else {
        natural_widths.clone()
    };
    let used_width: f32 = final_widths.iter().sum::<f32>() + total_gap;
    let leftover = (containing_width - used_width).max(0.0);

    // 3. Compute starting offset based on justify-content.
    let mut cursor_x = base_x;
    let mut between_gap = gap;
    match bx.flex.justify {
        JustifyContent::FlexStart => { /* cursor_x = base_x, gap unchanged */ }
        JustifyContent::Center => cursor_x += leftover / 2.0,
        JustifyContent::FlexEnd => cursor_x += leftover,
        JustifyContent::SpaceBetween => {
            if n > 1 {
                between_gap = gap + leftover / (n - 1) as f32;
            }
        }
    }

    // 4. Position + size each child. Measure height by recursing.
    let mut max_height: f32 = 0.0;
    for (i, child) in bx.children.iter_mut().enumerate() {
        let w = final_widths[i];
        layout_box_into(child, cursor_x, base_y, w, em);
        if child.dimensions.height > max_height {
            max_height = child.dimensions.height;
        }
        cursor_x += w + between_gap;
    }

    bx.dimensions.height = max_height;
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
    let total_gap = gap * (n.saturating_sub(1)) as f32;
    let natural_heights: Vec<f32> = bx
        .children
        .iter()
        .map(|c| measure_child_main_column(c, containing_width, em))
        .collect();
    let flex_grows: Vec<f32> = bx.children.iter().map(|c| c.flex_grow).collect();
    let total_natural: f32 = natural_heights.iter().sum();
    let free_space = containing_width - total_natural - total_gap; // placeholder
    let _ = free_space;
    let total_grow: f32 = flex_grows.iter().sum();

    // For column, we don't know container height ahead of time in
    // ASCII flow (height grows downward). So flex-grow for column is
    // treated as a no-op for now (height = natural). This is a known
    // limitation — column flex-grow needs a definite container height.
    let _ = total_grow;

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

/// Measure a child's natural main-axis size for `flex-direction: column`
/// (i.e., its natural height = 1 line for text, or recurse for blocks).
fn measure_child_main_column(child: &LayoutBox, containing_width: f32, em: f32) -> f32 {
    let margin_t = child.margin.top.resolve(containing_width, em);
    let margin_b = child.margin.bottom.resolve(containing_width, em);
    let h = natural_content_height(child);
    h + margin_t + margin_b
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

/// Natural content height in lines.
fn natural_content_height(bx: &LayoutBox) -> f32 {
    if let Some(text) = &bx.text {
        let lines = text.lines().count();
        return if lines == 0 { 0.0 } else { lines as f32 };
    }
    if bx.children.is_empty() {
        return 0.0;
    }
    bx.children.iter().map(natural_content_height).sum()
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

    // Block-like child: recurse via block layout for height.
    let _ = em;
    // Default: 1 line height per child block.
    bx.dimensions.height = 1.0;
}

#[cfg(test)]
mod tests {

    use crate::block::{layout, LayoutConfig};
    use crate::boxes::{BoxType, FlexDirection, FlexProps, JustifyContent, LayoutBox, LayoutTree};

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
}
