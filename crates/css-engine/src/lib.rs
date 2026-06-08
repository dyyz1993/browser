//! `browser-css-engine` — CSS parser + selector engine + computed styles.
//!
//! M2.1 scope: parsing.
//! M2.2 scope: selectors.
//! M2.3 scope: computed styles.
//! - [`parse`] — CSS text → [`Stylesheet`]
//! - [`Stylesheet`] / [`Rule`] / [`Declaration`] — AST
//! - [`Selector`] / [`CompoundSelector`] — selectors with matching
//! - [`compute_styles`] — apply [`Stylesheet`] to a DOM [`Tree`]

#![forbid(unsafe_code)]

pub mod ast;
pub mod computed;
pub mod parser;
pub mod properties;
pub mod selector;

pub use ast::{Declaration, Rule, Stylesheet};
pub use computed::compute_styles;
pub use parser::parse;
pub use properties::{parse_box_lengths, parse_color, parse_length, BoxEdges, Length};
pub use selector::{CompoundSelector, Selector, SelectorChain};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ping() {
        assert_eq!(env!("CARGO_PKG_NAME"), "browser-css-engine");
    }

    #[test]
    fn parse_single_rule_one_decl() {
        let sheet = parse("h1 { color: red; }");
        assert_eq!(sheet.rules.len(), 1);
        assert_eq!(sheet.rules[0].selectors, "h1");
        assert_eq!(sheet.rules[0].declarations.len(), 1);
        assert_eq!(sheet.rules[0].declarations[0].property, "color");
        assert_eq!(sheet.rules[0].declarations[0].value, "red");
        assert!(!sheet.rules[0].declarations[0].important);
    }

    #[test]
    fn parse_multiple_declarations() {
        let sheet = parse("p { color: red; font-size: 14px; }");
        assert_eq!(sheet.rules.len(), 1);
        let decls = &sheet.rules[0].declarations;
        assert_eq!(decls.len(), 2);
        assert_eq!(decls[0].property, "color");
        assert_eq!(decls[1].property, "font-size");
        assert_eq!(decls[1].value, "14px");
    }

    #[test]
    fn parse_important_flag() {
        let sheet = parse("a { color: red !important; }");
        assert!(sheet.rules[0].declarations[0].important);
    }

    #[test]
    fn parse_multiple_rules() {
        let sheet = parse("h1 { color: red; } p { color: blue; }");
        assert_eq!(sheet.rules.len(), 2);
        assert_eq!(sheet.rules[0].selectors, "h1");
        assert_eq!(sheet.rules[1].selectors, "p");
    }

    #[test]
    fn parse_empty_input() {
        let sheet = parse("");
        assert!(sheet.rules.is_empty());
    }

    #[test]
    fn parse_multiple_selectors_kept_as_string() {
        let sheet = parse("h1, h2, h3 { color: red; }");
        assert_eq!(sheet.rules[0].selectors, "h1, h2, h3");
    }
}
