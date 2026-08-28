//! Compute the set of declarations that apply to each element in a tree.
//!
//! Output: a map from element NodeId to its matching [`Declaration`]s,
//! in the order they appear in the stylesheet. (Cascade priority and
//! specificity are not implemented in M2 — every matching declaration
//! contributes, last-write-wins by source order.)

use std::collections::HashMap;

use browser_dom::{NodeData, NodeId, Tree};

use crate::ast::{Declaration, Stylesheet};
use crate::selector::Selector;

/// For every Element in `tree`, collect the declarations whose selector
/// list matches that element. Text / Comment / Doctype nodes are skipped.
///
/// M2 cascade rule: later rules in the stylesheet override earlier ones
/// (we don't implement specificity or `!important` priority yet).
///
/// M70.1: also folds each Element's inline `style="..."` attribute into the
/// computed map. Inline styles are appended last (last-write-wins), giving
/// them the highest priority — matching browser behavior where inline styles
/// beat stylesheet rules (without `!important`).
///
/// M72.1: the built-in UA stylesheet ([`crate::ua::ua_stylesheet`]) is
/// prepended before the page's rules, so author CSS overrides browser
/// defaults while unstyled elements still get them (h1 sizing, p margins,
/// list indents...). Full order, lowest → highest priority:
/// UA sheet → page sheet → inline `style="..."`.
#[must_use]
pub fn compute_styles(tree: &Tree, sheet: &Stylesheet) -> HashMap<NodeId, Vec<Declaration>> {
    // Pre-parse every rule's selector list once. UA rules first (lowest
    // priority), page rules after.
    let ua = crate::ua::ua_stylesheet();
    let mut parsed: Vec<(Selector, &[Declaration])> =
        Vec::with_capacity(ua.rules.len() + sheet.rules.len());
    for r in ua.rules.iter().chain(sheet.rules.iter()) {
        if let Ok(s) = Selector::parse(&r.selectors) {
            parsed.push((s, r.declarations.as_slice()));
        }
    }

    let mut out: HashMap<NodeId, Vec<Declaration>> = HashMap::new();
    tree.traverse(tree.root(), |id, node| {
        if let NodeData::Element { tag: _, attrs } = &node.data {
            let mut decls: Vec<Declaration> = Vec::new();
            // Stylesheet rules (selector-matched).
            for (sel, ds) in &parsed {
                if sel.matches(tree, id) {
                    decls.extend_from_slice(ds);
                }
            }
            // M70.1: inline `style="..."` attribute — appended last so it wins.
            if let Some(style_val) = attrs
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("style"))
                .map(|(_, v)| v.as_str())
            {
                decls.extend(crate::parser::parse_declaration_list(style_val));
            }
            if !decls.is_empty() {
                out.insert(id, decls);
            }
        }
        true
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse as parse_css;
    use browser_dom::{NodeData, Tree};

    /// Tree: html > body > p
    fn fixture() -> Tree {
        let mut t = Tree::with_root(NodeData::Document);
        let root = t.root();
        let html = t.insert(
            Some(root),
            NodeData::Element {
                tag: "html".into(),
                attrs: vec![],
            },
        );
        let body = t.insert(
            Some(html),
            NodeData::Element {
                tag: "body".into(),
                attrs: vec![],
            },
        );
        let _p = t.insert(
            Some(body),
            NodeData::Element {
                tag: "p".into(),
                attrs: vec![("class".into(), "text".into())],
            },
        );
        t
    }

    fn get(out: &HashMap<NodeId, Vec<Declaration>>, id: NodeId, prop: &str) -> Option<String> {
        out.get(&id)
            .and_then(|decls| decls.iter().rev().find(|d| d.property == prop))
            .map(|d| d.value.clone())
    }

    #[test]
    fn compute_styles_applies_matching_rule() {
        let tree = fixture();
        let sheet = parse_css("p { color: red; }");
        let styles = compute_styles(&tree, &sheet);
        // <p> is at id=4 (root=0, html=1, body=2, p=3). Wait — 3 children
        // inserted under root? Let me check: root, html, body, p = ids 0..3.
        assert_eq!(get(&styles, 3, "color").as_deref(), Some("red"));
    }

    #[test]
    fn compute_styles_skips_non_matching() {
        let tree = fixture();
        let sheet = parse_css("h1 { color: red; }");
        let styles = compute_styles(&tree, &sheet);
        // No element is h1, so the page rule matches nothing. M72.1: the
        // <p> still receives UA-default declarations (e.g. margin), but
        // never the page rule's color.
        assert_eq!(get(&styles, 3, "color"), None);
        assert!(get(&styles, 3, "margin").is_some(), "UA p margin expected");
    }

    #[test]
    fn compute_styles_later_rule_overrides() {
        let tree = fixture();
        let sheet = parse_css("p { color: red; } p { color: blue; }");
        let styles = compute_styles(&tree, &sheet);
        // Last-write-wins → blue.
        assert_eq!(get(&styles, 3, "color").as_deref(), Some("blue"));
    }

    #[test]
    fn compute_styles_class_selector() {
        let tree = fixture();
        let sheet = parse_css(".text { font-size: 14px; }");
        let styles = compute_styles(&tree, &sheet);
        assert_eq!(get(&styles, 3, "font-size").as_deref(), Some("14px"));
    }

    #[test]
    fn compute_styles_descendant_selector() {
        let tree = fixture();
        let sheet = parse_css("body p { color: green; }");
        let styles = compute_styles(&tree, &sheet);
        assert_eq!(get(&styles, 3, "color").as_deref(), Some("green"));
        // <body> alone doesn't match.
        assert!(get(&styles, 2, "color").is_none());
    }

    #[test]
    fn compute_styles_text_nodes_not_in_output() {
        let tree = fixture();
        let sheet = parse_css("* { color: red; }");
        let styles = compute_styles(&tree, &sheet);
        // Only Element node ids appear in the output.
        for id in styles.keys() {
            assert!(
                matches!(tree.data(*id), NodeData::Element { .. }),
                "non-Element node {id} in styles map"
            );
        }
    }

    // ---- M70.1: inline `style="..."` attribute parsing ----

    /// Build a tree where <p> has an inline style attribute.
    fn fixture_with_inline_style(style_val: &str) -> Tree {
        let mut t = Tree::with_root(NodeData::Document);
        let root = t.root();
        let html = t.insert(
            Some(root),
            NodeData::Element {
                tag: "html".into(),
                attrs: vec![],
            },
        );
        let body = t.insert(
            Some(html),
            NodeData::Element {
                tag: "body".into(),
                attrs: vec![],
            },
        );
        let _p = t.insert(
            Some(body),
            NodeData::Element {
                tag: "p".into(),
                attrs: vec![("style".into(), style_val.into())],
            },
        );
        t
    }

    #[test]
    fn inline_style_attribute_is_parsed() {
        // <p style="display: grid; color: red">
        let tree = fixture_with_inline_style("display: grid; color: red");
        let sheet = parse_css(""); // empty stylesheet
        let styles = compute_styles(&tree, &sheet);
        // p is at id 3 (root=0, html=1, body=2, p=3)
        assert_eq!(get(&styles, 3, "display").as_deref(), Some("grid"));
        assert_eq!(get(&styles, 3, "color").as_deref(), Some("red"));
    }

    #[test]
    fn inline_style_overrides_stylesheet() {
        // Inline style should win over stylesheet rule (last-write-wins).
        let tree = fixture_with_inline_style("color: blue");
        let sheet = parse_css("p { color: red; }");
        let styles = compute_styles(&tree, &sheet);
        // blue (inline) beats red (stylesheet)
        assert_eq!(get(&styles, 3, "color").as_deref(), Some("blue"));
    }

    #[test]
    fn inline_style_missing_is_noop() {
        // Element without style attribute → no inline decls. M72.1: UA
        // defaults still show up (p margin), but the UA sheet declares no
        // `color` for p, so a missing inline style means no color either.
        let tree = fixture(); // fixture's <p> has class but no style
        let sheet = parse_css("");
        let styles = compute_styles(&tree, &sheet);
        assert_eq!(get(&styles, 3, "color"), None);
    }
}
