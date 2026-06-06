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
#[must_use]
pub fn compute_styles(tree: &Tree, sheet: &Stylesheet) -> HashMap<NodeId, Vec<Declaration>> {
    // Pre-parse every rule's selector list once.
    let parsed: Vec<(Selector, &[Declaration])> = sheet
        .rules
        .iter()
        .filter_map(|r| {
            Selector::parse(&r.selectors)
                .ok()
                .map(|s| (s, r.declarations.as_slice()))
        })
        .collect();

    let mut out: HashMap<NodeId, Vec<Declaration>> = HashMap::new();
    tree.traverse(tree.root(), |id, node| {
        if !matches!(node.data, NodeData::Element { .. }) {
            return true;
        }
        let mut decls: Vec<Declaration> = Vec::new();
        for (sel, ds) in &parsed {
            if sel.matches(tree, id) {
                decls.extend_from_slice(ds);
            }
        }
        if !decls.is_empty() {
            out.insert(id, decls);
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
        // No element is h1, so no styles at all.
        assert!(styles.is_empty());
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
}
