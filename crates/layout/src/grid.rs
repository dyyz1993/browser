//! M33: CSS Grid layout (minimal useful subset).
//!
//! Supports the most common grid patterns found in real-world SPAs:
//! - `display: grid`
//! - `grid-template-columns: 1fr 1fr | 100px 200px | auto`
//! - `gap: Npx`
//!
//! ## Algorithm (simplified CSS Grid)
//!
//! 1. Parse `grid-template-columns` into track sizes (Fr/Px/Auto).
//! 2. Resolve track widths:
//!    - Px → fixed width
//!    - Auto → natural content width (max of items in that column)
//!    - Fr → proportional share of leftover space
//! 3. Place children left-to-right, top-to-bottom (auto-placement).
//!    Each child goes into the next available cell.
//! 4. Row height = max content height of items in that row.
//!
//! ## Limitations (deliberately out of scope)
//! - No explicit `grid-column` / `grid-row` placement (auto only)
//! - No `grid-template-rows` (rows auto-sized to content)
//! - No `align-items` / `justify-items`
//! - No named grid areas

use crate::boxes::{GridTrack, LayoutBox};

/// Entry point: lay out children of a `display:grid` container.
/// Called from [`crate::block::layout_box`].
pub fn layout_grid_children(bx: &mut LayoutBox, containing_width: f32) {
    let base_x = bx.dimensions.x;
    let base_y = bx.dimensions.y;
    let gap = bx.grid.gap;
    let columns = if bx.grid.columns.is_empty() {
        // No template → single auto column.
        vec![GridTrack::Auto]
    } else {
        bx.grid.columns.clone()
    };

    let n_cols = columns.len();
    if n_cols == 0 || bx.children.is_empty() {
        bx.dimensions.height = 0.0;
        return;
    }

    // 1. Resolve column widths.
    let col_widths = resolve_column_widths(&columns, &bx.children, containing_width, gap);

    // 2. Auto-placement: assign each child to (row, col).
    let n_children = bx.children.len();
    let n_rows = n_children.div_ceil(n_cols);

    // 3. Layout each child into its cell.
    let total_gap_w = gap * (n_cols - 1) as f32;
    let _ = total_gap_w; // already subtracted in resolve
    let mut cursor_y = base_y;
    let mut row_heights: Vec<f32> = Vec::new();

    for row in 0..n_rows {
        let row_start = row * n_cols;
        let row_end = n_children.min(row_start + n_cols);
        let mut max_h: f32 = 0.0;
        for (col, child_idx) in (row_start..row_end).enumerate() {
            let child = &mut bx.children[child_idx];
            let x = base_x + col_x_offset(&col_widths, col, gap);
            let w = col_widths[col];
            crate::block::layout_box_pub(child, x, cursor_y, w);
            if child.dimensions.height > max_h {
                max_h = child.dimensions.height;
            }
        }
        row_heights.push(max_h);
        cursor_y += max_h + gap;
    }
    // Remove trailing gap.
    if !row_heights.is_empty() {
        cursor_y -= gap;
    }

    bx.dimensions.height = (cursor_y - base_y).max(0.0);
}

/// Compute the x-offset of column `col` (sum of prior widths + gaps).
fn col_x_offset(col_widths: &[f32], col: usize, gap: f32) -> f32 {
    col_widths[..col].iter().map(|&w| w + gap).sum()
}

/// Resolve track widths: Px fixed, Auto = max content width, Fr =
/// proportional share of leftover space.
fn resolve_column_widths(
    tracks: &[GridTrack],
    children: &[LayoutBox],
    containing_width: f32,
    gap: f32,
) -> Vec<f32> {
    let n = tracks.len();
    let total_gap = gap * (n.saturating_sub(1)) as f32;
    let available = (containing_width - total_gap).max(0.0);

    // First pass: resolve Px and Auto, collect Fr factors.
    let mut widths = vec![0.0_f32; n];
    let mut total_fr: f32 = 0.0;
    let mut used: f32 = 0.0;

    for (i, track) in tracks.iter().enumerate() {
        match track {
            GridTrack::Px(v) => {
                widths[i] = *v;
                used += v;
            }
            GridTrack::Auto => {
                // Max natural content width of children in this column.
                let col_content = children
                    .iter()
                    .step_by(n)
                    .skip(i)
                    .map(natural_content_width)
                    .max_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
                    .unwrap_or(1.0);
                widths[i] = col_content;
                used += col_content;
            }
            GridTrack::Fr(f) => {
                total_fr += f;
            }
        }
    }

    // Second pass: distribute leftover to Fr tracks.
    if total_fr > 0.0 {
        let leftover = (available - used).max(0.0);
        for (i, track) in tracks.iter().enumerate() {
            if let GridTrack::Fr(f) = track {
                widths[i] = leftover * f / total_fr;
            }
        }
    }

    widths
}

/// Natural content width (text length in characters).
fn natural_content_width(bx: &LayoutBox) -> f32 {
    if let Some(text) = &bx.text {
        return text.lines().map(|l| l.chars().count()).max().unwrap_or(0) as f32;
    }
    if bx.children.is_empty() {
        return 1.0;
    }
    bx.children
        .iter()
        .map(natural_content_width)
        .sum::<f32>()
        .max(1.0)
}

/// M33: Parse `grid-template-columns` value into track sizes.
/// Handles space-separated list of `Nfr` / `Npx` / `auto`.
/// Returns empty Vec on parse failure (caller falls back to single auto column).
#[must_use]
pub fn parse_grid_template_columns(value: &str) -> Vec<GridTrack> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    let mut tracks = Vec::new();
    for token in trimmed.split_whitespace() {
        let lower = token.to_ascii_lowercase();
        if lower == "auto" {
            tracks.push(GridTrack::Auto);
        } else if lower.ends_with("fr") {
            if let Ok(v) = lower.trim_end_matches("fr").parse::<f32>() {
                tracks.push(GridTrack::Fr(v));
            }
        } else if let Some(px_val) = strip_px(&lower) {
            tracks.push(GridTrack::Px(px_val));
        } else if let Ok(v) = lower.parse::<f32>() {
            // Bare number = px (lenient).
            tracks.push(GridTrack::Px(v));
        }
        // Unrecognized tokens are silently skipped.
    }
    tracks
}

/// Strip trailing `px` suffix and parse the number.
fn strip_px(s: &str) -> Option<f32> {
    if s.ends_with("px") {
        s.trim_end_matches("px").parse::<f32>().ok()
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::{layout, LayoutConfig};
    use crate::boxes::{BoxType, GridProps, GridTrack, LayoutBox, LayoutTree};

    fn grid_box(props: GridProps, children: Vec<LayoutBox>) -> LayoutBox {
        let mut b = LayoutBox::new(BoxType::Grid);
        b.grid = props;
        b.children = children;
        b
    }

    fn text_item(s: &str) -> LayoutBox {
        LayoutBox::new(BoxType::Inline).with_text(s.into())
    }

    fn block_item() -> LayoutBox {
        LayoutBox::new(BoxType::Block)
    }

    // ── parse_grid_template_columns ────────────────────────────

    #[test]
    fn parse_fr_tracks() {
        let t = parse_grid_template_columns("1fr 2fr 1fr");
        assert_eq!(
            t,
            vec![GridTrack::Fr(1.0), GridTrack::Fr(2.0), GridTrack::Fr(1.0),]
        );
    }

    #[test]
    fn parse_px_tracks() {
        let t = parse_grid_template_columns("100px 200px");
        assert_eq!(t, vec![GridTrack::Px(100.0), GridTrack::Px(200.0)]);
    }

    #[test]
    fn parse_auto_track() {
        let t = parse_grid_template_columns("auto auto");
        assert_eq!(t, vec![GridTrack::Auto, GridTrack::Auto]);
    }

    #[test]
    fn parse_mixed_tracks() {
        let t = parse_grid_template_columns("100px 1fr auto");
        assert_eq!(t.len(), 3);
        assert_eq!(t[0], GridTrack::Px(100.0));
        assert_eq!(t[1], GridTrack::Fr(1.0));
        assert_eq!(t[2], GridTrack::Auto);
    }

    #[test]
    fn parse_empty_returns_empty() {
        assert!(parse_grid_template_columns("").is_empty());
    }

    // ── layout ─────────────────────────────────────────────────

    #[test]
    fn two_column_grid_places_items_horizontally() {
        // grid > [A, B, C, D], 2 cols → 2 rows × 2 cols
        let g = grid_box(
            GridProps {
                columns: vec![GridTrack::Fr(1.0), GridTrack::Fr(1.0)],
                gap: 0.0,
            },
            vec![
                text_item("A"),
                text_item("B"),
                text_item("C"),
                text_item("D"),
            ],
        );
        let mut tree = LayoutTree { root: g };
        layout(
            &mut tree,
            LayoutConfig {
                viewport_width: 80.0,
            },
        );
        let g = &tree.root;
        // Row 0: A at x≈0, B at x≈40
        assert!(g.children[1].dimensions.x > g.children[0].dimensions.x);
        // Row 1: C below A, D below B
        assert!(g.children[2].dimensions.y > g.children[0].dimensions.y);
    }

    #[test]
    fn grid_gap_adds_spacing() {
        let g = grid_box(
            GridProps {
                columns: vec![GridTrack::Px(10.0), GridTrack::Px(10.0)],
                gap: 5.0,
            },
            vec![text_item("A"), text_item("B")],
        );
        let mut tree = LayoutTree { root: g };
        layout(
            &mut tree,
            LayoutConfig {
                viewport_width: 80.0,
            },
        );
        let g = &tree.root;
        // Second column starts at x = 10 + 5 = 15
        assert!((g.children[1].dimensions.x - 15.0).abs() < 0.5);
    }

    #[test]
    fn grid_fr_splits_evenly() {
        let g = grid_box(
            GridProps {
                columns: vec![GridTrack::Fr(1.0), GridTrack::Fr(1.0), GridTrack::Fr(1.0)],
                gap: 0.0,
            },
            vec![block_item(), block_item(), block_item()],
        );
        let mut tree = LayoutTree { root: g };
        layout(
            &mut tree,
            LayoutConfig {
                viewport_width: 90.0,
            },
        );
        let g = &tree.root;
        // Each column should be ~30 (90/3)
        assert!(
            (g.children[0].dimensions.width - 30.0).abs() < 1.0,
            "expected width ~30, got {}",
            g.children[0].dimensions.width
        );
    }

    #[test]
    fn empty_grid_has_zero_height() {
        let g = grid_box(
            GridProps {
                columns: vec![GridTrack::Fr(1.0)],
                gap: 0.0,
            },
            vec![],
        );
        let mut tree = LayoutTree { root: g };
        layout(
            &mut tree,
            LayoutConfig {
                viewport_width: 80.0,
            },
        );
        assert_eq!(tree.root.dimensions.height, 0.0);
    }

    #[test]
    fn no_template_defaults_single_column() {
        // Empty columns → single auto column → items stack vertically.
        let g = grid_box(
            GridProps {
                columns: vec![],
                gap: 0.0,
            },
            vec![text_item("A"), text_item("B")],
        );
        let mut tree = LayoutTree { root: g };
        layout(
            &mut tree,
            LayoutConfig {
                viewport_width: 80.0,
            },
        );
        let g = &tree.root;
        assert!(g.children[1].dimensions.y > g.children[0].dimensions.y);
    }
}
