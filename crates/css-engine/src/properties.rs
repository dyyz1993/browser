//! CSS property value parsing for box-model properties (M7.1.1+).
//!
//! Scope: parse the `margin-*` and `padding-*` declarations emitted by
//! [`crate::computed::compute_styles`] into structured values the
//! layout engine can consume directly.
//!
//! Out of scope (deferred to later M7.x): border, position, flex,
//! grid. We only model what M7.1 needs: margin + padding + the
//! CSS box-edge shorthand rules.
//!
//! ## Length units
//!
//! We support the units that make sense for our renderer:
//! - `0` (no unit, parsed as `Length::Zero`)
//! - `px`  → absolute; in ASCII mode, 1px ≈ 1 character (crude but
//!   consistent for M7.1's text-mode layout)
//! - `em`  → relative; 1em = 1 line height = 1 character cell
//! - `%`   → percent of the containing block's width (margin) or
//!   height; resolved by layout
//! - `auto` → only meaningful for margins (and width/height); M7.1
//!   treats it as `0` for simplicity
//!
//! Other units (`rem`, `vh`, `vw`, `pt`, `cm`...) are recognized
//! syntactically but fall back to `Length::Px(value)` so that
//! hand-written fixtures don't blow up.

use crate::ast::Declaration;

/// A length value resolved at parse time, deferred to layout for
/// relative units.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum Length {
    /// `0` or absent / invalid — zero contribution.
    #[default]
    Zero,
    /// Absolute pixel count. In ASCII mode treated as characters.
    Px(f32),
    /// `em` — relative to font size. In ASCII mode 1em = 1 line.
    Em(f32),
    /// `%` — percent of containing block dimension.
    Percent(f32),
    /// `auto` — for M7.1, equivalent to Zero in margin context.
    Auto,
}

impl Length {
    /// Resolve a length against a containing-block size and an em
    /// size, returning a plain `f32` in character units.
    ///
    /// - `Zero` / `Auto` → 0
    /// - `Px(v)` → `v` (character units in ASCII mode)
    /// - `Em(v)` → `v * em_size`
    /// - `Percent(v)` → `v / 100.0 * container_size`
    #[must_use]
    pub fn resolve(self, container_size: f32, em_size: f32) -> f32 {
        match self {
            Length::Zero | Length::Auto => 0.0,
            Length::Px(v) => v,
            Length::Em(v) => v * em_size,
            Length::Percent(v) => v / 100.0 * container_size,
        }
    }
}

/// Box-edge values: top / right / bottom / left, in that order to
/// match CSS conventions.
///
/// Provides a [`BoxEdges::all`] constructor and the standard
/// shorthand-expansion helpers used by [`parse_box_lengths`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BoxEdges<T: Copy> {
    pub top: T,
    pub right: T,
    pub bottom: T,
    pub left: T,
}

impl<T: Copy + Default> Default for BoxEdges<T> {
    fn default() -> Self {
        Self {
            top: T::default(),
            right: T::default(),
            bottom: T::default(),
            left: T::default(),
        }
    }
}

impl<T: Copy> BoxEdges<T> {
    /// All four edges set to the same value.
    #[must_use]
    pub const fn all(v: T) -> Self {
        Self {
            top: v,
            right: v,
            bottom: v,
            left: v,
        }
    }
}

/// Parse a single CSS length token like `"12px"`, `"1.5em"`,
/// `"50%"`, `"0"`, `"auto"`.
///
/// Returns `None` for empty or unrecognized input.
///
/// # Examples
///
/// ```
/// use browser_css_engine::properties::parse_length;
/// assert_eq!(parse_length("12px"), Some(parse_length("12px").unwrap()));
/// ```
#[must_use]
pub fn parse_length(s: &str) -> Option<Length> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let lower = s.to_ascii_lowercase();
    if lower == "auto" {
        return Some(Length::Auto);
    }
    // Strip trailing unit and parse the numeric prefix.
    // We accept "0" without unit (CSS shorthand).
    if let Ok(v) = lower.parse::<f32>() {
        return Some(if v == 0.0 {
            Length::Zero
        } else {
            // No unit + non-zero: technically invalid in CSS, but
            // we treat it as px for robustness.
            Length::Px(v)
        });
    }
    // Walk the string finding where digits end.
    let mut split = 0;
    for (i, c) in lower.char_indices() {
        if c.is_ascii_digit() || c == '.' || c == '-' || c == '+' {
            split = i + c.len_utf8();
        } else {
            break;
        }
    }
    if split == 0 {
        return None;
    }
    let (num_part, unit) = lower.split_at(split);
    let value: f32 = num_part.parse().ok()?;
    let unit = unit.trim();
    match unit {
        "" => Some(if value == 0.0 {
            Length::Zero
        } else {
            Length::Px(value)
        }),
        "px" => Some(if value == 0.0 {
            Length::Zero
        } else {
            Length::Px(value)
        }),
        "em" => Some(if value == 0.0 {
            Length::Zero
        } else {
            Length::Em(value)
        }),
        "%" => Some(if value == 0.0 {
            Length::Zero
        } else {
            Length::Percent(value)
        }),
        // Other units (rem, pt, vh, vw, cm, ...) — treat as px
        // for ASCII-mode robustness.
        _ => Some(if value == 0.0 {
            Length::Zero
        } else {
            Length::Px(value)
        }),
    }
}

/// Expand a 1-to-4-token CSS shorthand into 4 box edges, matching the
/// standard CSS box-edge shorthand rules:
///
/// - 1 token: all four edges
/// - 2 tokens: top/bottom, left/right
/// - 3 tokens: top, left/right, bottom
/// - 4 tokens: top, right, bottom, left
///
/// Returns `None` if any token fails to parse or the shorthand has
/// 0 or >4 tokens.
fn expand_box_shorthand(s: &str) -> Option<BoxEdges<Length>> {
    let parts: Vec<&str> = s.split_ascii_whitespace().collect();
    match parts.as_slice() {
        [] => None,
        [a] => {
            let v = parse_length(a)?;
            Some(BoxEdges::all(v))
        }
        [a, b] => {
            let v = parse_length(a)?;
            let h = parse_length(b)?;
            Some(BoxEdges {
                top: v,
                right: h,
                bottom: v,
                left: h,
            })
        }
        [a, b, c] => {
            let top = parse_length(a)?;
            let h = parse_length(b)?;
            let bottom = parse_length(c)?;
            Some(BoxEdges {
                top,
                right: h,
                left: h,
                bottom,
            })
        }
        [a, b, c, d] => {
            let top = parse_length(a)?;
            let right = parse_length(b)?;
            let bottom = parse_length(c)?;
            let left = parse_length(d)?;
            Some(BoxEdges {
                top,
                right,
                bottom,
                left,
            })
        }
        _ => None,
    }
}

/// Build a `BoxEdges<Length>` from a list of declarations matching a
/// property prefix (e.g. `"margin"`, `"padding"`).
///
/// Looks for the shorthand (`margin`, `padding`) first, then the
/// per-edge longhands (`margin-top`, etc.). Longhands override the
/// shorthand when both are present, matching CSS source order
/// (last-write-wins per M2 cascade rule).
///
/// # Examples
///
/// ```
/// use browser_css_engine::ast::Declaration;
/// use browser_css_engine::properties::{parse_box_lengths, Length};
///
/// let decls = vec![
///     Declaration { property: "margin".into(), value: "10px".into(), important: false },
/// ];
/// let m = parse_box_lengths(&decls, "margin");
/// assert_eq!(m.top, Length::Px(10.0));
/// ```
#[must_use]
pub fn parse_box_lengths(decls: &[Declaration], prefix: &str) -> BoxEdges<Length> {
    let mut edges: Option<BoxEdges<Length>> = None;
    let longhands = ["top", "right", "bottom", "left"];
    for d in decls {
        if d.property == prefix {
            if let Some(parsed) = expand_box_shorthand(&d.value) {
                edges = Some(parsed);
            }
        } else if let Some(side) = longhands
            .iter()
            .find(|&&side| d.property == format!("{prefix}-{side}"))
        {
            // Per-edge longhand overrides the corresponding edge.
            let parsed = parse_length(&d.value).unwrap_or(Length::Zero);
            let e = edges.get_or_insert(BoxEdges::all(Length::Zero));
            match *side {
                "top" => e.top = parsed,
                "right" => e.right = parsed,
                "bottom" => e.bottom = parsed,
                "left" => e.left = parsed,
                _ => {}
            }
        }
    }
    edges.unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::Declaration;

    fn decl(prop: &str, val: &str) -> Declaration {
        Declaration {
            property: prop.into(),
            value: val.into(),
            important: false,
        }
    }

    // ── parse_length ────────────────────────────────────────────

    #[test]
    fn parse_length_px() {
        assert_eq!(parse_length("12px"), Some(Length::Px(12.0)));
    }

    #[test]
    fn parse_length_em() {
        assert_eq!(parse_length("1.5em"), Some(Length::Em(1.5)));
    }

    #[test]
    fn parse_length_percent() {
        assert_eq!(parse_length("50%"), Some(Length::Percent(50.0)));
    }

    #[test]
    fn parse_length_zero() {
        assert_eq!(parse_length("0"), Some(Length::Zero));
        assert_eq!(parse_length("0px"), Some(Length::Zero));
        assert_eq!(parse_length("0em"), Some(Length::Zero));
    }

    #[test]
    fn parse_length_auto() {
        assert_eq!(parse_length("auto"), Some(Length::Auto));
    }

    #[test]
    fn parse_length_unknown_unit_falls_back_to_px() {
        // Robustness: don't blow up on rem/pt/vh — treat as px.
        assert_eq!(parse_length("2rem"), Some(Length::Px(2.0)));
        assert_eq!(parse_length("10pt"), Some(Length::Px(10.0)));
    }

    #[test]
    fn parse_length_empty_returns_none() {
        assert_eq!(parse_length(""), None);
        assert_eq!(parse_length("   "), None);
    }

    #[test]
    fn parse_length_negative() {
        // Negative margins exist (`margin: -10px`).
        assert_eq!(parse_length("-10px"), Some(Length::Px(-10.0)));
    }

    // ── Length::resolve ─────────────────────────────────────────

    #[test]
    fn length_resolve_zero() {
        assert_eq!(Length::Zero.resolve(100.0, 16.0), 0.0);
    }

    #[test]
    fn length_resolve_px() {
        assert_eq!(Length::Px(12.0).resolve(100.0, 16.0), 12.0);
    }

    #[test]
    fn length_resolve_em() {
        assert_eq!(Length::Em(2.0).resolve(100.0, 16.0), 32.0);
    }

    #[test]
    fn length_resolve_percent() {
        assert_eq!(Length::Percent(50.0).resolve(100.0, 16.0), 50.0);
    }

    #[test]
    fn length_resolve_auto_is_zero_in_m7() {
        assert_eq!(Length::Auto.resolve(100.0, 16.0), 0.0);
    }

    // ── expand_box_shorthand ───────────────────────────────────

    #[test]
    fn shorthand_one_value_applies_to_all() {
        let e = expand_box_shorthand("10px").unwrap();
        assert!(
            matches!(e, BoxEdges { top: Length::Px(10.0), .. } if e.right == Length::Px(10.0)
            && e.bottom == Length::Px(10.0) && e.left == Length::Px(10.0))
        );
    }

    #[test]
    fn shorthand_two_values() {
        // top/bottom = 10px, left/right = 20px
        let e = expand_box_shorthand("10px 20px").unwrap();
        assert_eq!(e.top, Length::Px(10.0));
        assert_eq!(e.right, Length::Px(20.0));
        assert_eq!(e.bottom, Length::Px(10.0));
        assert_eq!(e.left, Length::Px(20.0));
    }

    #[test]
    fn shorthand_three_values() {
        // top = 10, left/right = 20, bottom = 30
        let e = expand_box_shorthand("10px 20px 30px").unwrap();
        assert_eq!(e.top, Length::Px(10.0));
        assert_eq!(e.right, Length::Px(20.0));
        assert_eq!(e.bottom, Length::Px(30.0));
        assert_eq!(e.left, Length::Px(20.0));
    }

    #[test]
    fn shorthand_four_values() {
        let e = expand_box_shorthand("1px 2px 3px 4px").unwrap();
        assert_eq!(e.top, Length::Px(1.0));
        assert_eq!(e.right, Length::Px(2.0));
        assert_eq!(e.bottom, Length::Px(3.0));
        assert_eq!(e.left, Length::Px(4.0));
    }

    #[test]
    fn shorthand_invalid_returns_none() {
        assert!(expand_box_shorthand("").is_none());
        assert!(expand_box_shorthand("garbage").is_none());
    }

    // ── parse_box_lengths ──────────────────────────────────────

    #[test]
    fn parse_box_lengths_shorthand_only() {
        let decls = vec![decl("margin", "10px")];
        let m = parse_box_lengths(&decls, "margin");
        assert_eq!(m.top, Length::Px(10.0));
        assert_eq!(m.right, Length::Px(10.0));
        assert_eq!(m.bottom, Length::Px(10.0));
        assert_eq!(m.left, Length::Px(10.0));
    }

    #[test]
    fn parse_box_lengths_longhand_only() {
        let decls = vec![decl("margin-top", "5px"), decl("margin-left", "10px")];
        let m = parse_box_lengths(&decls, "margin");
        assert_eq!(m.top, Length::Px(5.0));
        assert_eq!(m.left, Length::Px(10.0));
        assert_eq!(m.right, Length::Zero);
        assert_eq!(m.bottom, Length::Zero);
    }

    #[test]
    fn parse_box_lengths_longhand_overrides_shorthand() {
        let decls = vec![
            decl("margin", "10px"),
            decl("margin-top", "99px"), // overrides top
        ];
        let m = parse_box_lengths(&decls, "margin");
        assert_eq!(m.top, Length::Px(99.0));
        assert_eq!(m.right, Length::Px(10.0)); // shorthand value
        assert_eq!(m.bottom, Length::Px(10.0));
        assert_eq!(m.left, Length::Px(10.0));
    }

    #[test]
    fn parse_box_lengths_no_decls_returns_default() {
        let decls: Vec<Declaration> = vec![];
        let m = parse_box_lengths(&decls, "margin");
        assert_eq!(m.top, Length::Zero);
        assert_eq!(m.right, Length::Zero);
        assert_eq!(m.bottom, Length::Zero);
        assert_eq!(m.left, Length::Zero);
    }

    #[test]
    fn parse_box_lengths_padding_works_too() {
        let decls = vec![decl("padding", "1em 2em")];
        let p = parse_box_lengths(&decls, "padding");
        assert_eq!(p.top, Length::Em(1.0));
        assert_eq!(p.right, Length::Em(2.0));
        assert_eq!(p.bottom, Length::Em(1.0));
        assert_eq!(p.left, Length::Em(2.0));
    }

    #[test]
    fn parse_box_lengths_ignores_other_properties() {
        let decls = vec![
            decl("color", "red"),
            decl("margin", "5px"),
            decl("font-size", "16px"),
        ];
        let m = parse_box_lengths(&decls, "margin");
        assert_eq!(m.top, Length::Px(5.0));
    }
}
