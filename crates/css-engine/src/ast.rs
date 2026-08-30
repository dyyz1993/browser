//! CSS data model: a minimal subset of CSSOM suitable for M2.
//!
//! We support only the bits needed for text-mode rendering:
//! - Stylesheets = list of rules
//! - Rules = list of simple selectors + list of declarations
//! - Declarations = property:value pairs (with `!important` flag)
//!
//! At-rule support: M81 adds `@media` — the block's condition is parsed
//! into a [`MediaQuery`] and attached to every inner [`Rule`] (see the
//! `media` field); other at-rules remain unsupported and are skipped.

use std::fmt;

/// A parsed stylesheet.
#[derive(Debug, Default, Clone)]
pub struct Stylesheet {
    pub rules: Vec<Rule>,
}

/// A single CSS rule: `selector-list { declarations }`.
#[derive(Debug, Clone)]
pub struct Rule {
    /// Raw selector list as authored in CSS (e.g. `"h1, .title"`).
    /// M2.2 will parse this into structured `Selector`s; for now we
    /// keep the source string to keep M2.1 self-contained.
    pub selectors: String,
    pub declarations: Vec<Declaration>,
    /// M81: enclosing `@media` condition, if any. `None` for unconditional
    /// rules (always apply). Inner rules of one `@media` block each carry a
    /// clone of the same condition.
    pub media: Option<MediaQuery>,
}

/// M81: media query condition from an `@media` prelude.
///
/// Minimal-usable subset for responsive SPA rendering: media types
/// (`screen` / `print`), width breakpoints (`(max-width: Npx)` /
/// `(min-width: Npx)`), and `and` combinations. `not` / `,` (or) /
/// non-px units are out of scope; such conditions parse to `None` and the
/// whole block is dropped (same as pre-M81 behavior).
#[derive(Debug, Clone, PartialEq)]
pub enum MediaQuery {
    /// `screen` — matches (this project always renders on screen).
    Screen,
    /// `print` — never matches (no print rendering in a crawler).
    Print,
    /// `(max-width: Npx)` — matches when the viewport width ≤ N.
    MaxWidth(u32),
    /// `(min-width: Npx)` — matches when the viewport width ≥ N.
    MinWidth(u32),
    /// `A and B and ...` — every part must match.
    All(Vec<MediaQuery>),
}

/// A `property: value;` declaration, optionally flagged `!important`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Declaration {
    pub property: String,
    pub value: String,
    pub important: bool,
}

impl fmt::Display for Declaration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.important {
            write!(f, "{}: {} !important", self.property, self.value)
        } else {
            write!(f, "{}: {}", self.property, self.value)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declaration_display_no_important() {
        let d = Declaration {
            property: "color".into(),
            value: "red".into(),
            important: false,
        };
        assert_eq!(format!("{d}"), "color: red");
    }

    #[test]
    fn declaration_display_with_important() {
        let d = Declaration {
            property: "color".into(),
            value: "red".into(),
            important: true,
        };
        assert_eq!(format!("{d}"), "color: red !important");
    }
}
