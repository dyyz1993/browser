//! Bridge `html5ever` → `browser_dom::Tree`.
//!
//! Implements `markup5ever::interface::tree_builder::TreeSink` so the
//! HTML5 parser can drive construction of our arena-backed DOM tree.

use std::borrow::Cow;
use std::collections::HashMap;

use browser_dom::{NodeData, NodeId, Tree};
use html5ever::tree_builder::{ElementFlags, NodeOrText, QuirksMode, TreeSink};
use html5ever::{parse_document, Attribute, ExpandedName, QualName};
use html5ever::{tendril::StrTendril, tendril::TendrilSink};

/// Parse an HTML string into a [`browser_dom::Tree`].
///
/// Always produces a tree with a `Document` root. The parser may
/// inject `html` / `head` / `body` elements that are missing from
/// the source.
///
/// # Example
/// ```
/// use browser_html_parser::parse;
///
/// let tree = parse("<p>hi</p>");
/// assert!(tree.len() > 1);
/// ```
#[must_use]
pub fn parse(html: &str) -> Tree {
    let sink = Sink::new();
    parse_document(sink, html5ever::ParseOpts::default())
        .from_utf8()
        .one(html.as_bytes())
}

/// Parse an HTML **fragment** (as set via `innerHTML` / `outerHTML`) into a
/// [`browser_dom::Tree`].
///
/// Unlike [`parse`], this uses the HTML5 *fragment parsing algorithm* with a
/// `body` context element, so leading `<script>` / `<style>` / `<template>`
/// content lands as direct children of the context element instead of being
/// relocated into `<head>` by the document tree-construction modes.
///
/// The returned tree always has the shape `Document → body → [fragment
/// nodes]`, so callers can collect the parsed nodes via the `body` element.
///
/// # Example
/// ```
/// use browser_html_parser::parse_fragment;
///
/// // A fragment starting with <script> must keep the script as a node
/// // (document parsing would move it into <head>).
/// let tree = parse_fragment("<script>var a = 1;</script>");
/// assert!(tree.len() > 1);
/// ```
#[must_use]
pub fn parse_fragment(source: &str) -> Tree {
    let sink = Sink::new();
    let mut tree = html5ever::parse_fragment(
        sink,
        html5ever::ParseOpts::default(),
        // 片段上下文 = body：开头是 <script>/<style> 的节点不会被挪进 head
        QualName::new(
            None,
            markup5ever::Namespace::from("http://www.w3.org/1999/xhtml"),
            markup5ever::LocalName::from("body"),
        ),
        Vec::new(),
    )
    .from_utf8()
    .one(source.as_bytes());

    // html5ever fragment 语义（HTML5 spec「parse a fragment」步骤 5–7）：
    // 解析期间会新建一个 root `html` 元素承接片段节点；context `body` 只参与
    // tokenizer 状态与插入模式判定，不承载结果。这里把 root html 的子节点搬到
    // context body 下，使产物结构恒为 `Document → body → [fragment nodes]`，
    // 调用方（js-runtime bridge 的 innerHTML setter）从 body 收集节点即可。
    let root = tree.root();
    let root_children = tree.children_of(root).to_vec();
    let mut body_id = None;
    let mut html_id = None;
    for &child in &root_children {
        if let NodeData::Element { tag, .. } = tree.data(child) {
            match tag.as_str() {
                "body" => body_id = Some(child),
                "html" => html_id = Some(child),
                _ => {}
            }
        }
    }
    if let (Some(body), Some(html)) = (body_id, html_id) {
        let fragment_nodes = tree.children_of(html).to_vec();
        for &node in &fragment_nodes {
            tree.get_mut(node).parent = Some(body);
            tree.get_mut(body).children.push(node);
        }
        tree.get_mut(html).children.clear();
        // 从 Document 摘掉空的 root html；arena 中的孤儿节点保持不可达
        tree.get_mut(root).children.retain(|&c| c != html);
    }
    tree
}

/// html5ever `TreeSink` implementation that builds a `browser_dom::Tree`.
///
/// We keep a `QualName` cache per element node id so that
/// [`TreeSink::elem_name`] can return a borrowed `ExpandedName`
/// via `QualName::expanded()` (which yields `'static` lifetime).
struct Sink {
    tree: Tree,
    /// Cached `QualName` per Element node, so elem_name can return a
    /// borrowed `ExpandedName<'static>` cheaply.
    elem_names: HashMap<NodeId, QualName>,
}

impl Sink {
    fn new() -> Self {
        Self {
            tree: Tree::with_root(NodeData::Document),
            elem_names: HashMap::new(),
        }
    }
}

impl TreeSink for Sink {
    type Handle = NodeId;
    type Output = Tree;

    fn finish(self) -> Self::Output {
        self.tree
    }

    fn parse_error(&mut self, _msg: Cow<'static, str>) {}

    fn get_document(&mut self) -> Self::Handle {
        self.tree.root()
    }

    fn elem_name(&self, target: &Self::Handle) -> ExpandedName<'_> {
        self.elem_names
            .get(target)
            .expect("elem_name: unknown element id")
            .expanded()
    }

    fn create_element(
        &mut self,
        name: QualName,
        attrs: Vec<Attribute>,
        _flags: ElementFlags,
    ) -> Self::Handle {
        let tag = name.local.as_ref().to_string();
        let attrs: Vec<(String, String)> = attrs
            .into_iter()
            .map(|a| (a.name.local.as_ref().to_string(), a.value.to_string()))
            .collect();
        let parent = self.tree.root();
        let id = self
            .tree
            .insert(Some(parent), NodeData::Element { tag, attrs });
        self.elem_names.insert(id, name);
        id
    }

    fn create_comment(&mut self, text: StrTendril) -> Self::Handle {
        let parent = self.tree.root();
        self.tree
            .insert(Some(parent), NodeData::Comment(text.to_string()))
    }

    fn create_pi(&mut self, _target: StrTendril, _data: StrTendril) -> Self::Handle {
        let parent = self.tree.root();
        self.tree
            .insert(Some(parent), NodeData::Comment(String::new()))
    }

    fn append(&mut self, parent: &Self::Handle, child: NodeOrText<Self::Handle>) {
        match child {
            NodeOrText::AppendText(text) => {
                let last = self.tree.children_of(*parent).last().copied();
                if let Some(last) = last {
                    if matches!(self.tree.data(last), NodeData::Text(_)) {
                        if let NodeData::Text(s) = &mut self.tree.get_mut(last).data {
                            s.push_str(&text);
                            return;
                        }
                    }
                }
                self.tree
                    .insert(Some(*parent), NodeData::Text(text.to_string()));
            }
            NodeOrText::AppendNode(node) => {
                if let Some(old) = self.tree.get(node).parent {
                    self.tree.get_mut(old).children.retain(|c| *c != node);
                }
                self.tree.get_mut(node).parent = Some(*parent);
                self.tree.get_mut(*parent).children.push(node);
            }
        }
    }

    fn append_based_on_parent_node(
        &mut self,
        element: &Self::Handle,
        prev_element: &Self::Handle,
        child: NodeOrText<Self::Handle>,
    ) {
        if self.tree.get(*element).parent.is_some() {
            self.append_before_sibling(element, child);
        } else {
            self.append(prev_element, child);
        }
    }

    fn append_before_sibling(
        &mut self,
        sibling: &Self::Handle,
        new_node: NodeOrText<Self::Handle>,
    ) {
        let parent = self
            .tree
            .get(*sibling)
            .parent
            .expect("append_before_sibling: sibling has no parent");
        let idx = self
            .tree
            .get(parent)
            .children
            .iter()
            .position(|c| *c == *sibling)
            .expect("sibling not in parent's children");
        match new_node {
            NodeOrText::AppendText(text) => {
                let prev_is_text = idx > 0
                    && matches!(
                        self.tree.data(self.tree.get(parent).children[idx - 1]),
                        NodeData::Text(_)
                    );
                if prev_is_text {
                    let prev = self.tree.get(parent).children[idx - 1];
                    if let NodeData::Text(s) = &mut self.tree.get_mut(prev).data {
                        s.push_str(&text);
                        return;
                    }
                }
                let id = self
                    .tree
                    .insert(Some(parent), NodeData::Text(text.to_string()));
                let len = self.tree.get(parent).children.len();
                let node = self.tree.get_mut(parent).children.remove(len - 1);
                self.tree.get_mut(parent).children.insert(idx, node);
                self.tree.get_mut(id).parent = Some(parent);
            }
            NodeOrText::AppendNode(node) => {
                if let Some(old) = self.tree.get(node).parent {
                    self.tree.get_mut(old).children.retain(|c| *c != node);
                }
                self.tree.get_mut(node).parent = Some(parent);
                self.tree.get_mut(parent).children.insert(idx, node);
            }
        }
    }

    fn append_doctype_to_document(
        &mut self,
        name: StrTendril,
        _public_id: StrTendril,
        _system_id: StrTendril,
    ) {
        let root = self.tree.root();
        let id = self.tree.insert(
            Some(root),
            NodeData::Doctype {
                name: name.to_string(),
            },
        );
        let children = &mut self.tree.get_mut(root).children;
        let pos = children
            .iter()
            .position(|c| *c == id)
            .expect("just inserted");
        let node = children.remove(pos);
        children.insert(0, node);
    }

    fn mark_script_already_started(&mut self, _node: &Self::Handle) {}

    fn get_template_contents(&mut self, target: &Self::Handle) -> Self::Handle {
        *target
    }

    fn same_node(&self, x: &Self::Handle, y: &Self::Handle) -> bool {
        x == y
    }

    fn set_quirks_mode(&mut self, _mode: QuirksMode) {}

    fn add_attrs_if_missing(&mut self, target: &Self::Handle, attrs: Vec<Attribute>) {
        let node = self.tree.get_mut(*target);
        if let NodeData::Element {
            attrs: existing, ..
        } = &mut node.data
        {
            for attr in attrs {
                let name = attr.name.local.as_ref().to_string();
                if !existing.iter().any(|(n, _)| *n == name) {
                    existing.push((name, attr.value.to_string()));
                }
            }
        }
    }

    fn remove_from_parent(&mut self, target: &Self::Handle) {
        if let Some(parent) = self.tree.get(*target).parent {
            self.tree.get_mut(parent).children.retain(|c| c != target);
            self.tree.get_mut(*target).parent = None;
        }
    }

    fn reparent_children(&mut self, node: &Self::Handle, new_parent: &Self::Handle) {
        let children: Vec<NodeId> = self.tree.get(*node).children.clone();
        self.tree.get_mut(*node).children.clear();
        for child in children {
            self.tree.get_mut(child).parent = Some(*new_parent);
            self.tree.get_mut(*new_parent).children.push(child);
        }
    }
}
