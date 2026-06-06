//! Build a [`LayoutTree`] from a DOM [`Tree`] + computed styles.
//!
//! Strategy (the "formatting context" decision):
//! - Block-level tags (html, body, div, p, h1-h6, ul, li, header,
//!   footer, section, article, main, nav, aside, blockquote, pre,
//!   hr) → [`BoxType::Block`]
//! - Everything else (a, span, em, strong, ...) → [`BoxType::Inline`]
//! - Text nodes become anonymous inline boxes carrying their text
//! - Anonymous block wrappers wrap mixed inline content inside a
//!   block parent (per CSS spec)

use std::collections::HashMap;

use browser_css_engine::Declaration;
use browser_dom::{NodeData, NodeId, Tree};

use crate::boxes::{BoxType, LayoutBox, LayoutTree};

/// Default block-level tag set. Conservative; can grow as fixtures demand.
const BLOCK_TAGS: &[&str] = &[
    "html",
    "body",
    "div",
    "p",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "ul",
    "ol",
    "li",
    "header",
    "footer",
    "section",
    "article",
    "main",
    "nav",
    "aside",
    "blockquote",
    "pre",
    "hr",
    "br",
    "table",
    "form",
    "figure",
    "figcaption",
];

fn is_block_tag(tag: &str) -> bool {
    BLOCK_TAGS.contains(&tag)
}

fn box_type_for_element(tag: &str) -> BoxType {
    if is_block_tag(tag) {
        BoxType::Block
    } else {
        BoxType::Inline
    }
}

/// Build a [`LayoutTree`] from a DOM tree and its computed styles.
///
/// `_styles` is currently consulted only for the eventual cascade
/// decision (display:block / display:inline). M2.3 only computes
/// declarations; explicit `display` override is left to a later
/// milestone — tag-based heuristics decide for now.
#[must_use]
pub fn construct_layout_tree(
    tree: &Tree,
    _styles: &HashMap<NodeId, Vec<Declaration>>,
) -> LayoutTree {
    // The DOM root is Document; we model the layout root as an
    // anonymous block that contains whatever Document's children produce.
    let mut root = LayoutBox::new(BoxType::Anonymous);
    root.element_id = Some(tree.root());
    for &child in tree.children_of(tree.root()) {
        build_box(tree, child, &mut root.children);
    }
    LayoutTree { root }
}

fn build_box(tree: &Tree, id: NodeId, out: &mut Vec<LayoutBox>) {
    match tree.data(id) {
        NodeData::Element { tag, .. } => {
            let bt = box_type_for_element(tag);
            let mut bx = LayoutBox::new(bt).with_element(id);
            bx.children = build_children(tree, id, bt);
            out.push(bx);
        }
        NodeData::Text(s) => {
            // Wrap text in an anonymous inline box.
            let bx = LayoutBox::new(BoxType::Inline)
                .with_element(id)
                .with_text(s.clone());
            out.push(bx);
        }
        NodeData::Comment(_) | NodeData::Doctype { .. } | NodeData::Document => {
            // Skip — comments / doctype don't render.
        }
    }
}

/// Build the children of an Element, inserting Anonymous block wrappers
/// whenever a Block parent has Inline children mixed with Block children.
fn build_children(tree: &Tree, parent_id: NodeId, parent_box: BoxType) -> Vec<LayoutBox> {
    let dom_children = tree.children_of(parent_id);
    if dom_children.is_empty() {
        return Vec::new();
    }

    if parent_box == BoxType::Block {
        // Group consecutive inline children into anonymous blocks.
        let mut result: Vec<LayoutBox> = Vec::new();
        let mut inline_buf: Vec<LayoutBox> = Vec::new();
        for &child_id in dom_children {
            let is_inline = is_inline_node(tree, child_id);
            if is_inline {
                let mut tmp = Vec::new();
                build_box(tree, child_id, &mut tmp);
                inline_buf.extend(tmp);
            } else {
                flush_inline_buf(&mut inline_buf, &mut result);
                build_box(tree, child_id, &mut result);
            }
        }
        flush_inline_buf(&mut inline_buf, &mut result);
        result
    } else {
        // Inline parent → just collect children inline (no anonymous wrappers).
        let mut result = Vec::new();
        for &child_id in dom_children {
            build_box(tree, child_id, &mut result);
        }
        result
    }
}

fn is_inline_node(tree: &Tree, id: NodeId) -> bool {
    match tree.data(id) {
        NodeData::Text(_) => true,
        NodeData::Element { tag, .. } => !is_block_tag(tag),
        _ => false,
    }
}

fn flush_inline_buf(buf: &mut Vec<LayoutBox>, out: &mut Vec<LayoutBox>) {
    if buf.is_empty() {
        return;
    }
    let drained: Vec<LayoutBox> = std::mem::take(buf);
    let mut anon = LayoutBox::new(BoxType::Anonymous);
    anon.children = drained;
    out.push(anon);
}

#[cfg(test)]
mod tests {
    use super::*;
    use browser_dom::Tree;

    /// Document > html > body > p(text)
    fn simple_tree() -> Tree {
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
        let p = t.insert(
            Some(body),
            NodeData::Element {
                tag: "p".into(),
                attrs: vec![],
            },
        );
        let _ = t.insert(Some(p), NodeData::Text("hello".into()));
        t
    }

    #[test]
    fn construct_produces_block_chain() {
        let tree = simple_tree();
        let styles = HashMap::new();
        let layout = construct_layout_tree(&tree, &styles);
        // Root is anonymous wrapping html.
        assert_eq!(layout.root.box_type, BoxType::Anonymous);
        let html = &layout.root.children[0];
        assert_eq!(html.box_type, BoxType::Block);
        assert_eq!(html.element_id, Some(1));
        let body = &html.children[0];
        assert_eq!(body.box_type, BoxType::Block);
        let p = &body.children[0];
        assert_eq!(p.box_type, BoxType::Block);
        // <p> wraps its text in an anonymous block (since <p> is Block
        // and its child is Inline).
        assert_eq!(p.children.len(), 1);
        let anon = &p.children[0];
        assert_eq!(anon.box_type, BoxType::Anonymous);
        let text_box = &anon.children[0];
        assert_eq!(text_box.box_type, BoxType::Inline);
        assert_eq!(text_box.text.as_deref(), Some("hello"));
    }

    /// Document > body > p(text) + a(text)
    /// <a> is inline, so the body's children produce two boxes:
    /// the <p> (block) and an anonymous block wrapping <a>.
    #[test]
    fn construct_inserts_anonymous_block_for_inline_sibling_of_block() {
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
        let _ = t.insert(Some(p), NodeData::Text("hello".into()));
        let a = t.insert(
            Some(body),
            NodeData::Element {
                tag: "a".into(),
                attrs: vec![],
            },
        );
        let _ = t.insert(Some(a), NodeData::Text("link".into()));

        let layout = construct_layout_tree(&t, &HashMap::new());
        let anon_root = &layout.root;
        let body_box = &anon_root.children[0];
        assert_eq!(body_box.box_type, BoxType::Block);
        // body's children: 1 block (p), 1 anonymous block wrapping <a>.
        assert_eq!(body_box.children.len(), 2);
        assert_eq!(body_box.children[0].box_type, BoxType::Block);
        assert_eq!(body_box.children[1].box_type, BoxType::Anonymous);
        // The anonymous block wraps <a>.
        assert_eq!(body_box.children[1].children.len(), 1);
        let a_wrap = &body_box.children[1].children[0];
        assert_eq!(a_wrap.box_type, BoxType::Inline);
    }

    #[test]
    fn construct_skips_comments_and_doctype() {
        let mut t = Tree::with_root(NodeData::Document);
        let root = t.root();
        let _ = t.insert(
            Some(root),
            NodeData::Doctype {
                name: "html".into(),
            },
        );
        let _ = t.insert(Some(root), NodeData::Comment("hi".into()));
        let p = t.insert(
            Some(root),
            NodeData::Element {
                tag: "p".into(),
                attrs: vec![],
            },
        );
        let _ = t.insert(Some(p), NodeData::Text("x".into()));

        let layout = construct_layout_tree(&t, &HashMap::new());
        // Only <p> should appear at top level.
        assert_eq!(layout.root.children.len(), 1);
        assert_eq!(layout.root.children[0].element_id, Some(p));
    }

    #[test]
    fn construct_text_only_body_wraps_in_anonymous() {
        let mut t = Tree::with_root(NodeData::Document);
        let root = t.root();
        let body = t.insert(
            Some(root),
            NodeData::Element {
                tag: "body".into(),
                attrs: vec![],
            },
        );
        let _ = t.insert(Some(body), NodeData::Text("bare".into()));
        let layout = construct_layout_tree(&t, &HashMap::new());
        let body_box = &layout.root.children[0];
        assert_eq!(body_box.box_type, BoxType::Block);
        // Text directly under body gets wrapped in anonymous block.
        assert_eq!(body_box.children.len(), 1);
        assert_eq!(body_box.children[0].box_type, BoxType::Anonymous);
    }
}
