//! M33: CSS Grid layout (minimal useful subset).
//!
//! Supports the most common grid patterns found in real-world SPAs:
//! - `display: grid`
//! - `grid-template-columns: 1fr 1fr | 100px 200px | auto`
//! - `gap: Npx`
//! - M35.3: `grid-column` / `grid-row` explicit placement
//!
//! ## Algorithm (simplified CSS Grid)
//!
//! 1. Parse `grid-template-columns` into track sizes (Fr/Px/Auto).
//! 2. Resolve track widths (Px fixed, Auto=content, Fr=proportional).
//! 3. Compute placements: explicit grid_placement items first, then
//!    auto-fill remaining items left-to-right, skipping occupied cells.
//! 4. Layout each child into its cell. Row height = max item height.

use crate::boxes::{GridTrack, LayoutBox};

/// Entry point: lay out children of a `display:grid` container.
pub fn layout_grid_children(bx: &mut LayoutBox, containing_width: f32) {
    let base_x = bx.dimensions.x;
    let base_y = bx.dimensions.y;
    let gap = bx.grid.gap;
    let columns = if bx.grid.columns.is_empty() {
        vec![GridTrack::Auto]
    } else {
        bx.grid.columns.clone()
    };

    let n_cols = columns.len();
    if n_cols == 0 || bx.children.is_empty() {
        bx.dimensions.height = 0.0;
        return;
    }

    let col_widths = resolve_column_widths(&columns, &bx.children, containing_width, gap);
    let placements = compute_placements(&bx.children, n_cols);
    let n_rows = placements.iter().map(|p| p.0).max().map_or(0, |r| r + 1);

    // Pass 1: layout each child (x, width, y=base_y) to measure heights.
    // Grid items are blockified (CSS spec): force width to cell width
    // even for inline-level items.
    for (i, &(_row, col, span)) in placements.iter().enumerate() {
        let child = &mut bx.children[i];
        let x = base_x + col_x_offset(&col_widths, col, gap);
        let w = spanned_width(&col_widths, col, span, gap);
        crate::block::layout_box_pub(child, x, base_y, w);
        // Force blockify: override width regardless of item's box_type
        // (inline items would otherwise shrink to content width).
        child.dimensions.width = w;
    }

    // Compute row heights from measured children.
    let mut row_heights = vec![0.0_f32; n_rows];
    for (i, &(row, _, _)) in placements.iter().enumerate() {
        let h = bx.children[i].dimensions.height;
        if h > row_heights[row] {
            row_heights[row] = h;
        }
    }

    // Compute cumulative row y-offsets.
    let mut row_y = vec![0.0_f32; n_rows];
    let mut cy = base_y;
    for r in 0..n_rows {
        row_y[r] = cy;
        cy += row_heights[r];
        if r + 1 < n_rows {
            cy += gap;
        }
    }

    // Pass 2: re-layout with correct y per row.
    for (i, &(row, col, span)) in placements.iter().enumerate() {
        let child = &mut bx.children[i];
        let x = base_x + col_x_offset(&col_widths, col, gap);
        let w = spanned_width(&col_widths, col, span, gap);
        crate::block::layout_box_pub(child, x, row_y[row], w);
        // Force blockify: override width.
        child.dimensions.width = w;
    }

    bx.dimensions.height = if n_rows == 0 {
        0.0
    } else {
        row_heights.iter().sum::<f32>() + gap * (n_rows.saturating_sub(1)) as f32
    };
}

/// x-offset of column `col` (sum of prior widths + gaps).
fn col_x_offset(col_widths: &[f32], col: usize, gap: f32) -> f32 {
    col_widths[..col].iter().map(|&w| w + gap).sum()
}

/// Width spanning `span` columns starting at `col` (including internal gaps).
fn spanned_width(col_widths: &[f32], col: usize, span: usize, gap: f32) -> f32 {
    let end = (col + span).min(col_widths.len());
    col_widths[col..end].iter().sum::<f32>() + gap * span.saturating_sub(1) as f32
}

/// M35.3: Compute (row, col, col_span) for each child.
/// Explicit `grid_placement` items get fixed positions; others auto-fill.
fn compute_placements(children: &[LayoutBox], n_cols: usize) -> Vec<(usize, usize, usize)> {
    let n = children.len();
    let mut result = vec![(0usize, 0usize, 1usize); n];
    let mut occupied: std::collections::HashSet<(usize, usize)> = std::collections::HashSet::new();

    // Phase 1: explicit items — only those with explicit start position.
    for (i, child) in children.iter().enumerate() {
        if let Some(p) = &child.grid_placement {
            if p.col_start.is_some() || p.row_start.is_some() {
                let col = p.col_start.unwrap_or(1).saturating_sub(1);
                let row = p.row_start.unwrap_or(1).saturating_sub(1);
                let span = p.col_span.max(1);
                result[i] = (row, col, span);
                for s in 0..span {
                    occupied.insert((row, col + s));
                }
            }
        }
    }

    // Phase 2: auto-fill remaining items (including span-only ones).
    // Respect col_span: find span consecutive free cells.
    let mut auto_row = 0usize;
    let mut auto_col = 0usize;
    for (i, child) in children.iter().enumerate() {
        // Skip items already explicitly placed in Phase 1.
        let explicit = child
            .grid_placement
            .as_ref()
            .map(|p| p.col_start.is_some() || p.row_start.is_some())
            .unwrap_or(false);
        if explicit {
            continue;
        }
        let span = child
            .grid_placement
            .as_ref()
            .map(|p| p.col_span.max(1))
            .unwrap_or(1);
        loop {
            if auto_col + span > n_cols {
                auto_col = 0;
                auto_row += 1;
            }
            let fits = (0..span).all(|s| !occupied.contains(&(auto_row, auto_col + s)));
            if fits {
                break;
            }
            auto_col += 1;
        }
        result[i] = (auto_row, auto_col, span);
        for s in 0..span {
            occupied.insert((auto_row, auto_col + s));
        }
        auto_col += span;
    }

    result
}

/// Resolve track widths: Px fixed, Auto = max content width, Fr proportional.
fn resolve_column_widths(
    tracks: &[GridTrack],
    children: &[LayoutBox],
    containing_width: f32,
    gap: f32,
) -> Vec<f32> {
    let n = tracks.len();
    let total_gap = gap * (n.saturating_sub(1)) as f32;
    let available = (containing_width - total_gap).max(0.0);

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
            tracks.push(GridTrack::Px(v));
        }
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

/// M35.3: Parse `grid-column` / `grid-row` value into placement.
/// Supports:
/// - `N` (start line, span 1)
/// - `span N` (auto-start, span N)
/// - `N / M` (start line / end line → span = M - N)
/// - `N / span K` (start line, span K)
#[must_use]
pub fn parse_grid_placement(value: &str) -> (Option<usize>, usize) {
    let trimmed = value.trim();
    let parts: Vec<&str> = trimmed.split('/').map(str::trim).collect();

    if parts.len() == 2 {
        let start = parts[0].parse::<usize>().ok().map(|v| v.max(1));
        let end_part = parts[1].trim().to_ascii_lowercase();
        if let Some(rest) = end_part.strip_prefix("span ") {
            let span = rest.parse::<usize>().unwrap_or(1).max(1);
            return (start, span);
        }
        if let (Some(s), Ok(e)) = (start, parts[1].parse::<usize>()) {
            if e > s {
                return (Some(s), e - s);
            }
        }
        return (start, 1);
    }

    // Single value: "span N" or "N"
    let single = parts[0].trim().to_ascii_lowercase();
    if let Some(rest) = single.strip_prefix("span ") {
        let span = rest.parse::<usize>().unwrap_or(1).max(1);
        return (None, span);
    }
    let start = parts[0].parse::<usize>().ok().map(|v| v.max(1));
    (start, 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::{layout, LayoutConfig};
    use crate::boxes::{BoxType, GridItemPlacement, GridProps, GridTrack, LayoutBox, LayoutTree};

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

    // ── parse_grid_template_columns ──

    #[test]
    fn parse_fr_tracks() {
        let t = parse_grid_template_columns("1fr 2fr 1fr");
        assert_eq!(
            t,
            vec![GridTrack::Fr(1.0), GridTrack::Fr(2.0), GridTrack::Fr(1.0)]
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

    // ── parse_grid_placement (M35.3) ──

    #[test]
    fn placement_single_number() {
        let (s, span) = parse_grid_placement("3");
        assert_eq!(s, Some(3));
        assert_eq!(span, 1);
    }

    #[test]
    fn placement_span() {
        let (s, span) = parse_grid_placement("span 2");
        assert_eq!(s, None);
        assert_eq!(span, 2);
    }

    #[test]
    fn placement_start_end() {
        let (s, span) = parse_grid_placement("1 / 3");
        assert_eq!(s, Some(1));
        assert_eq!(span, 2); // 3 - 1 = 2 columns
    }

    #[test]
    fn placement_start_span() {
        let (s, span) = parse_grid_placement("2 / span 3");
        assert_eq!(s, Some(2));
        assert_eq!(span, 3);
    }

    // ── layout ──

    #[test]
    fn two_column_grid_places_items_horizontally() {
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
        assert!(g.children[1].dimensions.x > g.children[0].dimensions.x);
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

    // ── M35.3: explicit placement ──

    #[test]
    fn explicit_col_start_places_item_in_column() {
        let mut item = text_item("X");
        item.grid_placement = Some(GridItemPlacement {
            col_start: Some(2), // 1-based → 0-based col 1
            col_span: 1,
            row_start: None,
            row_span: 1,
        });
        let g = grid_box(
            GridProps {
                columns: vec![GridTrack::Fr(1.0), GridTrack::Fr(1.0)],
                gap: 0.0,
            },
            vec![item],
        );
        let mut tree = LayoutTree { root: g };
        layout(
            &mut tree,
            LayoutConfig {
                viewport_width: 80.0,
            },
        );
        let g = &tree.root;
        // col_start=2 → 0-based col 1 → x ≈ 40 (half of 80)
        assert!(
            g.children[0].dimensions.x >= 30.0,
            "expected x >= 30, got {}",
            g.children[0].dimensions.x
        );
    }

    #[test]
    fn col_span_makes_item_wider() {
        let mut spanning = text_item("Wide");
        spanning.grid_placement = Some(GridItemPlacement {
            col_start: None,
            col_span: 2, // spans 2 columns
            row_start: None,
            row_span: 1,
        });
        let g = grid_box(
            GridProps {
                columns: vec![GridTrack::Fr(1.0), GridTrack::Fr(1.0)],
                gap: 0.0,
            },
            vec![spanning],
        );
        let mut tree = LayoutTree { root: g };
        layout(
            &mut tree,
            LayoutConfig {
                viewport_width: 80.0,
            },
        );
        let g = &tree.root;
        // Spanning 2 cols of width 40 each → ~80
        assert!(
            g.children[0].dimensions.width >= 70.0,
            "expected width >= 70, got {}",
            g.children[0].dimensions.width
        );
    }

    #[test]
    fn auto_placement_skips_occupied_cells() {
        // 3-col grid, item 0 explicitly at col 2 (0-based 1).
        // Items 1, 2 auto-fill: item 1 → col 0, item 2 → col 2.
        let mut explicit = text_item("E");
        explicit.grid_placement = Some(GridItemPlacement {
            col_start: Some(2),
            col_span: 1,
            row_start: Some(1),
            row_span: 1,
        });
        let g = grid_box(
            GridProps {
                columns: vec![GridTrack::Fr(1.0), GridTrack::Fr(1.0), GridTrack::Fr(1.0)],
                gap: 0.0,
            },
            vec![explicit, text_item("A"), text_item("B")],
        );
        let mut tree = LayoutTree { root: g };
        layout(
            &mut tree,
            LayoutConfig {
                viewport_width: 90.0,
            },
        );
        let g = &tree.root;
        // Explicit item at col 1 (x ≈ 30)
        assert!((g.children[0].dimensions.x - 30.0).abs() < 5.0);
        // Auto items at col 0 (x ≈ 0) and col 2 (x ≈ 60)
        assert!(
            g.children[1].dimensions.x < 10.0,
            "auto item 1 should be at col 0"
        );
        assert!(
            g.children[2].dimensions.x >= 50.0,
            "auto item 2 should be at col 2"
        );
    }
}
