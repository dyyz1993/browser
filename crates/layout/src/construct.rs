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

use browser_css_engine::{parse_box_lengths, BoxEdges, Declaration, Length};
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
    styles: &HashMap<NodeId, Vec<Declaration>>,
) -> LayoutTree {
    // The DOM root is Document; we model the layout root as an
    // anonymous block that contains whatever Document's children produce.
    let mut root = LayoutBox::new(BoxType::Anonymous);
    root.element_id = Some(tree.root());
    for &child in tree.children_of(tree.root()) {
        build_box(tree, child, styles, &mut root.children);
    }
    LayoutTree { root }
}

fn build_box(
    tree: &Tree,
    id: NodeId,
    styles: &HashMap<NodeId, Vec<Declaration>>,
    out: &mut Vec<LayoutBox>,
) {
    match tree.data(id) {
        NodeData::Element { tag, .. } => {
            if is_non_rendered_tag(tag) {
                return;
            }
            let bt = box_type_for_element(tag);
            let mut bx = LayoutBox::new(bt).with_element(id);
            bx.children = build_children(tree, id, bt, styles);
            // M7.1.3: fill margin/padding from CSS + UA defaults.
            apply_box_model(tag, id, styles, &mut bx);
            // M6.0c: <li> bullet prefix (CSS ::marker placeholder).
            if tag.eq_ignore_ascii_case("li") {
                inject_li_bullet(&mut bx);
            }
            out.push(bx);
        }
        NodeData::Text(s) => {
            let bx = LayoutBox::new(BoxType::Inline)
                .with_element(id)
                .with_text(s.clone());
            out.push(bx);
        }
        NodeData::Comment(_) | NodeData::Doctype { .. } | NodeData::Document => {
            // Skip.
        }
    }
}

/// Apply CSS margin/padding + UA defaults to a freshly-built box.
/// CSS overrides non-Zero UA edges. Longhands override shorthand
/// via parse_box_lengths.
fn apply_box_model(
    tag: &str,
    id: NodeId,
    styles: &HashMap<NodeId, Vec<Declaration>>,
    bx: &mut LayoutBox,
) {
    bx.margin = ua_default_margins(tag);
    bx.padding = BoxEdges::default();
    if let Some(decls) = styles.get(&id) {
        let css_margin = parse_box_lengths(decls, "margin");
        let css_padding = parse_box_lengths(decls, "padding");
        // M7.1.6 fix: explicit `0` in CSS must override UA defaults.
        // If the user declared *any* margin property (shorthand or
        // longhand), replace the whole BoxEdges — parse_box_lengths
        // fills missing longhand edges with Zero, which is the right
        // behavior for "user reset to zero".
        let any_margin_decl = decls
            .iter()
            .any(|d| d.property == "margin" || d.property.starts_with("margin-"));
        let any_padding_decl = decls
            .iter()
            .any(|d| d.property == "padding" || d.property.starts_with("padding-"));
        if any_margin_decl {
            bx.margin = css_margin;
        }
        if any_padding_decl {
            bx.padding = css_padding;
        }
    }
}

/// UA default margins for block-level elements. ASCII mode: 1em = 1 line.
#[must_use]
fn ua_default_margins(tag: &str) -> browser_css_engine::BoxEdges<Length> {
    let lower = tag.to_ascii_lowercase();
    match lower.as_str() {
        "p" | "div" => browser_css_engine::BoxEdges {
            top: Length::Em(1.0),
            right: Length::Zero,
            bottom: Length::Em(1.0),
            left: Length::Zero,
        },
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => browser_css_engine::BoxEdges {
            top: Length::Em(0.67),
            right: Length::Zero,
            bottom: Length::Em(0.67),
            left: Length::Zero,
        },
        "ul" | "ol" => browser_css_engine::BoxEdges {
            top: Length::Em(1.0),
            right: Length::Zero,
            bottom: Length::Em(1.0),
            left: Length::Zero,
        },
        "hr" => browser_css_engine::BoxEdges {
            top: Length::Em(0.5),
            right: Length::Zero,
            bottom: Length::Em(0.5),
            left: Length::Zero,
        },
        _ => browser_css_engine::BoxEdges::default(),
    }
}

/// Tags whose subtrees produce no visual output. Browsers suppress
/// these completely during layout.
///
/// - `head` and `meta`/`link`/`title`: contain document metadata,
///   not rendered body content. Skipping the whole `<head>` subtree
///   is the cleanest fix — `<title>` text won't leak into output.
/// - `script` / `style` / `noscript` / `template`: already established
///   in M4.1.
fn is_non_rendered_tag(tag: &str) -> bool {
    matches!(
        tag.to_ascii_lowercase().as_str(),
        "head" | "meta" | "link" | "title" | "script" | "style" | "noscript" | "template" // M9.1: img 需要（渲染为 [IMG: src] 占位符），不列入黑名单
    )
}

/// Build the children of an Element, inserting Anonymous block wrappers
/// whenever a Block parent has Inline children mixed with Block children.
fn build_children(
    tree: &Tree,
    parent_id: NodeId,
    parent_box: BoxType,
    styles: &HashMap<NodeId, Vec<Declaration>>,
) -> Vec<LayoutBox> {
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
                build_box(tree, child_id, styles, &mut tmp);
                inline_buf.extend(tmp);
            } else {
                flush_inline_buf(&mut inline_buf, &mut result);
                build_box(tree, child_id, styles, &mut result);
            }
        }
        flush_inline_buf(&mut inline_buf, &mut result);
        result
    } else {
        // Inline parent → just collect children inline (no anonymous wrappers).
        let mut result = Vec::new();
        for &child_id in dom_children {
            build_box(tree, child_id, styles, &mut result);
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

/// Prepend a "• " bullet to the first text-bearing descendant of an
/// `<li>` layout box. The bullet sits at the same (x, y) as the text
/// would have started, then the text follows after 2 chars. We
/// implement this by mutating the first inline text leaf's `text`.
fn inject_li_bullet(bx: &mut LayoutBox) {
    if let Some(leaf) = find_first_text_leaf_mut(bx) {
        if let Some(text) = &mut leaf.text {
            if !text.starts_with("• ") {
                let mut new_text = String::with_capacity(text.len() + 2);
                new_text.push_str("• ");
                new_text.push_str(text);
                *text = new_text;
            }
        }
    }
}

/// Recursive mutable search for the first inline leaf with non-empty text.
fn find_first_text_leaf_mut(bx: &mut LayoutBox) -> Option<&mut LayoutBox> {
    if bx.box_type == BoxType::Inline && bx.text.as_ref().is_some_and(|t| !t.is_empty()) {
        return Some(bx);
    }
    for child in bx.children.iter_mut() {
        if let Some(found) = find_first_text_leaf_mut(child) {
            return Some(found);
        }
    }
    None
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
    fn construct_skips_head_subtree_including_title() {
        // Document > html > [head > title("page title"), body > p("visible")]
        // Only <p> should produce layout output; <title>'s text must
        // NOT leak through.
        let mut t = Tree::with_root(NodeData::Document);
        let root = t.root();
        let html = t.insert(
            Some(root),
            NodeData::Element {
                tag: "html".into(),
                attrs: vec![],
            },
        );
        let head = t.insert(
            Some(html),
            NodeData::Element {
                tag: "head".into(),
                attrs: vec![],
            },
        );
        let title = t.insert(
            Some(head),
            NodeData::Element {
                tag: "title".into(),
                attrs: vec![],
            },
        );
        let _ = t.insert(Some(title), NodeData::Text("page title".into()));
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
        let _ = t.insert(Some(p), NodeData::Text("visible".into()));

        let layout = construct_layout_tree(&t, &HashMap::new());
        let html_box = &layout.root.children[0];
        // html's children should be only <body> now (head subtree dropped).
        assert_eq!(html_box.children.len(), 1);
        // Sanity: text "page title" must NOT appear anywhere in the tree.
        let mut found_leak = false;
        fn walk(b: &LayoutBox, found: &mut bool) {
            if let Some(t) = &b.text {
                if t.contains("page title") {
                    *found = true;
                }
            }
            for c in &b.children {
                walk(c, found);
            }
        }
        walk(&layout.root, &mut found_leak);
        assert!(!found_leak, "<title> text leaked into layout tree");
    }

    #[test]
    fn construct_skips_script_and_style_content() {
        // body > [script("..."), p("visible"), style("...")]
        // Only the <p> should produce a layout box.
        let mut t = Tree::with_root(NodeData::Document);
        let root = t.root();
        let body = t.insert(
            Some(root),
            NodeData::Element {
                tag: "body".into(),
                attrs: vec![],
            },
        );
        let script = t.insert(
            Some(body),
            NodeData::Element {
                tag: "script".into(),
                attrs: vec![],
            },
        );
        let _ = t.insert(Some(script), NodeData::Text("__setBody('x')".into()));
        let p = t.insert(
            Some(body),
            NodeData::Element {
                tag: "p".into(),
                attrs: vec![],
            },
        );
        let _ = t.insert(Some(p), NodeData::Text("visible".into()));
        let style = t.insert(
            Some(body),
            NodeData::Element {
                tag: "style".into(),
                attrs: vec![],
            },
        );
        let _ = t.insert(Some(style), NodeData::Text("body{color:red}".into()));

        let layout = construct_layout_tree(&t, &HashMap::new());
        let body_box = &layout.root.children[0];
        // Only <p> should survive — script and style subtrees dropped.
        assert_eq!(body_box.children.len(), 1);
        assert_eq!(body_box.children[0].element_id, Some(p));
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
