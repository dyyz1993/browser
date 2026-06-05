//! Pretty-print a [`Tree`] as an indented ASCII tree.
//!
//! Example output:
//! ```text
//! Document
//! ├─ Doctype(html)
//! └─ Element(html)
//!    ├─ Element(head)
//!    └─ Element(body)
//!       └─ Element(p)
//!          └─ Text("hello")
//! ```
//!
//! Designed for debugging and the CLI's `parse` / `get` subcommands.

use crate::tree::{NodeId, Tree};

/// Render `tree` as an indented ASCII string.
///
/// Starts at the root; visits every node pre-order. Children are
/// drawn under their parent with `├─` / `└─` connectors.
#[must_use]
pub fn pretty_print(tree: &Tree) -> String {
    let mut out = String::new();
    render_node(tree, tree.root(), &[], &mut out);
    if out.ends_with('\n') {
        out.pop();
    }
    out
}

/// Render one node: write its label, then recurse into its children.
///
/// `prefix` describes the connector prefix that should precede **this
/// node's children** when drawn. For the root call we pass `&[]`.
fn render_node(tree: &Tree, id: NodeId, prefix: &[bool], out: &mut String) {
    // The root itself has no branch line — just its label.
    out.push_str(&tree.get(id).data.to_string());
    out.push('\n');

    let children = tree.children_of(id);
    let n = children.len();
    if n == 0 {
        return;
    }
    for (i, &child) in children.iter().enumerate() {
        let is_last = i + 1 == n;
        // Write the connector line for this child.
        render_branch(prefix, is_last, out);
        // Recurse with prefix extended by one column.
        let mut next_prefix = prefix.to_vec();
        next_prefix.push(!is_last);
        render_node(tree, child, &next_prefix, out);
    }
}

/// Write the indent + branch connector (`├─` or `└─`) for one child row.
fn render_branch(prefix: &[bool], is_last: bool, out: &mut String) {
    for &has_more in prefix {
        if has_more {
            out.push_str("│  ");
        } else {
            out.push_str("   ");
        }
    }
    if is_last {
        out.push_str("└─ ");
    } else {
        out.push_str("├─ ");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::NodeData;
    use crate::tree::Tree;

    fn build_simple_tree() -> Tree {
        // Document
        //   └─ Element(html)
        //      ├─ Element(head)
        //      └─ Element(body)
        //         └─ Element(p)
        //            └─ Text("hello")
        let mut t = Tree::with_root(NodeData::Document);
        let root = t.root();
        let html = t.insert(
            Some(root),
            NodeData::Element {
                tag: "html".into(),
                attrs: vec![],
            },
        );
        let _head = t.insert(
            Some(html),
            NodeData::Element {
                tag: "head".into(),
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
        let p = t.insert(
            Some(body),
            NodeData::Element {
                tag: "p".into(),
                attrs: vec![],
            },
        );
        let _text = t.insert(Some(p), NodeData::Text("hello".into()));
        t
    }

    fn build_tree_with_attrs() -> Tree {
        // Document
        //   └─ Element(a, attrs=[("href","x"), ("class","c")])
        //      └─ Text("link")
        let mut t = Tree::with_root(NodeData::Document);
        let root = t.root();
        let a = t.insert(
            Some(root),
            NodeData::Element {
                tag: "a".into(),
                attrs: vec![("href".into(), "x".into()), ("class".into(), "c".into())],
            },
        );
        let _text = t.insert(Some(a), NodeData::Text("link".into()));
        t
    }

    #[test]
    fn test_pretty_print_simple_tree_contains_core_labels() {
        let tree = build_simple_tree();
        let out = pretty_print(&tree);
        assert!(out.contains("Document"), "out = {out}");
        assert!(out.contains("Element(html)"), "out = {out}");
        assert!(out.contains("Element(head)"), "out = {out}");
        assert!(out.contains("Element(body)"), "out = {out}");
        assert!(out.contains("Element(p)"), "out = {out}");
        assert!(out.contains("Text(\"hello\")"), "out = {out}");
    }

    #[test]
    fn test_pretty_print_uses_branch_connectors() {
        let tree = build_simple_tree();
        let out = pretty_print(&tree);
        // html has 2 children, so at least one ├─ and one └─ must appear.
        assert!(out.contains("├─ "), "expected ├─ in output:\n{out}");
        assert!(out.contains("└─ "), "expected └─ in output:\n{out}");
    }

    #[test]
    fn test_pretty_print_attrs_appear_in_label() {
        let tree = build_tree_with_attrs();
        let out = pretty_print(&tree);
        let expected = "Element(a, attrs=[(\"href\", \"x\"), (\"class\", \"c\")])";
        assert!(
            out.contains(expected),
            "expected `{expected}` in output:\n{out}"
        );
    }

    #[test]
    fn test_pretty_print_single_node_document() {
        let tree = Tree::with_root(NodeData::Document);
        let out = pretty_print(&tree);
        assert_eq!(out, "Document");
    }

    #[test]
    fn test_pretty_print_doctype_and_text() {
        let mut tree = Tree::with_root(NodeData::Document);
        let root = tree.root();
        let _doctype = tree.insert(
            Some(root),
            NodeData::Doctype {
                name: "html".into(),
            },
        );
        let out = pretty_print(&tree);
        assert!(out.contains("Document"), "out = {out}");
        assert!(out.contains("Doctype(html)"), "out = {out}");
    }

    /// Snapshot of the simple tree — guards against accidental
    /// whitespace regressions in the connector logic.
    #[test]
    fn test_pretty_print_simple_tree_snapshot() {
        let tree = build_simple_tree();
        let out = pretty_print(&tree);
        let expected = [
            "Document",
            "└─ Element(html)",
            "   ├─ Element(head)",
            "   └─ Element(body)",
            "      └─ Element(p)",
            "         └─ Text(\"hello\")",
        ]
        .join("\n");
        assert_eq!(out, expected, "\n--- got:\n{out}\n--- want:\n{expected}");
    }
}
