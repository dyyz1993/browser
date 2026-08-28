//! Built-in default UA stylesheet (M72.1).
//!
//! Real browsers apply a default ("user agent") stylesheet when no author
//! CSS matches an element — this is why `<h1>` looks bigger than `<p>` even
//! on a page with no `<style>`. Without one, every element renders with
//! identical font metrics and margins: headings, paragraphs and lists all
//! collapse into a flat text stream (observed on example.com / svelte.dev
//! screenshots before M72.1).
//!
//! ## Cascade position
//!
//! UA rules are injected **before** the page's own stylesheet inside
//! [`crate::compute_styles`]. The engine's cascade is last-write-wins by
//! source order, so:
//!
//! ```text
//! UA rules  <  page <style> rules  <  inline style="..." attr
//! ```
//!
//! Author CSS therefore overrides every default here, exactly like a real
//! browser (`h1 { font-size: 1em }` in page CSS cancels the 2em default).
//!
//! ## How the values are consumed
//!
//! - `margin` declarations flow through the normal box-model path
//!   ([`crate::properties::parse_box_lengths`] → `layout` margins with
//!   collapsing). ASCII mode: 1em = 1 line.
//! - `font-size` is read by `layout::construct` as an em-ratio vs the 16px
//!   base; ratio >= 1.5 maps to UPPERCASE text (the ASCII proxy for larger
//!   glyphs — cap height is visibly taller in the rasterized screenshot).
//!   This also means author CSS like `.hero { font-size: 2em }` gets the
//!   same treatment, driven purely by computed values, not tags.
//! - `font-weight` / `font-style` / link `color` are declared for
//!   semantic completeness; the visible bold/italic emphasis comes from
//!   tag-driven `**…**` / `*…*` markers in `layout::construct`.
//! - Deliberately NOT here: `body { margin: 8px }` — 8 chars of indent on
//!   every page wastes precious ASCII viewport width (text browsers like
//!   lynx skip it too).

use std::sync::OnceLock;

use crate::ast::Stylesheet;
use crate::parser::parse;

/// The default UA stylesheet, in plain CSS. Kept as a string constant so it
/// is a single readable source of truth that goes through the exact same
/// parser as page CSS (no special-cased Rust rule structs). The parser is
/// whitespace/comment tolerant, so the indented source formatting is safe.
pub const UA_STYLESHEET: &str = r#"
    /* headings — descending size ladder (HTML spec defaults) */
    h1 { font-size: 2em; font-weight: bold; margin: 0.67em 0; }
    h2 { font-size: 1.5em; font-weight: bold; margin: 0.67em 0; }
    h3 { font-size: 1.17em; font-weight: bold; margin: 0.67em 0; }
    h4 { font-size: 1em; font-weight: bold; margin: 0.67em 0; }
    h5 { font-size: 0.83em; font-weight: bold; margin: 0.67em 0; }
    h6 { font-size: 0.67em; font-weight: bold; margin: 0.67em 0; }
    /* vertical rhythm for block content */
    p { margin: 1em 0; }
    blockquote { margin: 1em 0 1em 2em; }
    pre { margin: 1em 0; }
    figure { margin: 1em 0; }
    figcaption { margin: 0.5em 0; }
    hr { margin: 0.5em 0; }
    /* lists — browsers use padding-left: 40px; we model the indent with
       margin-left because layout consumes margins (padding is reserved) */
    ul, ol { margin: 1em 0; margin-left: 2em; }
    dd { margin-left: 2em; }
    /* inline emphasis + links */
    strong, b { font-weight: bold; }
    em, i, cite, var, dfn { font-style: italic; }
    a { color: #0000ee; }
"#;

static UA_SHEET: OnceLock<Stylesheet> = OnceLock::new();

/// Parse [`UA_STYLESHEET`] once and return the shared [`Stylesheet`].
#[must_use]
pub fn ua_stylesheet() -> &'static Stylesheet {
    UA_SHEET.get_or_init(|| parse(UA_STYLESHEET))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::computed::compute_styles;
    use browser_dom::{NodeData, Tree};

    #[test]
    fn ua_sheet_parses_into_expected_rule_count() {
        let sheet = ua_stylesheet();
        // One rule per line above minus comments: 6 headings + p +
        // blockquote + pre + figure + figcaption + hr + ul/ol + dd +
        // strong/b + em-family + a = 17 rules.
        assert_eq!(sheet.rules.len(), 17, "UA sheet rule count drifted");
    }

    #[test]
    fn ua_h1_has_2em_bold_and_margins() {
        let sheet = ua_stylesheet();
        let h1 = sheet
            .rules
            .iter()
            .find(|r| r.selectors == "h1")
            .expect("h1 rule");
        assert!(h1
            .declarations
            .iter()
            .any(|d| d.property == "font-size" && d.value == "2em"));
        assert!(h1
            .declarations
            .iter()
            .any(|d| d.property == "font-weight" && d.value == "bold"));
        assert!(h1
            .declarations
            .iter()
            .any(|d| d.property == "margin" && d.value == "0.67em 0"));
    }

    /// UA rules must lose to page rules: `p { margin: 0 }` in page CSS
    /// overrides the UA `p { margin: 1em 0 }`.
    #[test]
    fn page_css_overrides_ua_defaults() {
        let mut t = Tree::with_root(NodeData::Document);
        let root = t.root();
        let body = t.insert(
            Some(root),
            NodeData::Element {
                tag: "body".into(),
                attrs: vec![],
            },
        );
        let p = t.insert(
            Some(body),
            NodeData::Element {
                tag: "p".into(),
                attrs: vec![],
            },
        );
        let sheet = parse("p { margin: 0; }");
        let styles = compute_styles(&t, &sheet);
        let decls = styles.get(&p).expect("p must have computed decls");
        let margin = decls
            .iter()
            .rev()
            .find(|d| d.property == "margin")
            .expect("margin decl");
        assert_eq!(margin.value, "0", "page margin must override UA margin");
    }

    /// With an empty page stylesheet, elements still get UA defaults
    /// (this is the whole point of M72.1).
    #[test]
    fn ua_defaults_apply_with_empty_page_sheet() {
        let mut t = Tree::with_root(NodeData::Document);
        let root = t.root();
        let body = t.insert(
            Some(root),
            NodeData::Element {
                tag: "body".into(),
                attrs: vec![],
            },
        );
        let h1 = t.insert(
            Some(body),
            NodeData::Element {
                tag: "h1".into(),
                attrs: vec![],
            },
        );
        let sheet = parse("");
        let styles = compute_styles(&t, &sheet);
        let decls = styles.get(&h1).expect("h1 must inherit UA defaults");
        assert!(decls
            .iter()
            .any(|d| d.property == "font-size" && d.value == "2em"));
        assert!(decls
            .iter()
            .any(|d| d.property == "margin" && d.value == "0.67em 0"));
    }
}
