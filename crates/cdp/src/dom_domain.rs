//! M46: CDP `DOM` domain — getDocument + getOuterHTML + querySelector.
//!
//! Most useful domain for **scrapers**: lets clients query the DOM tree
//! **without executing JS** (sidestepping the boa engine limitation).
//!
//! ## Methods (M46 scope)
//!
//! - `DOM.getDocument` — return the full DOM as a CDP node tree (depth=-1).
//! - `DOM.getOuterHTML` — serialize a node (by nodeId) to HTML string.
//! - `DOM.querySelector` — find first element matching a simple selector.
//! - `DOM.querySelectorAll` — find all matching elements (returns nodeIds).
//!
//! ## Selector support (M46 subset)
//!
//! Supports tag (`div`), id (`#main`), class (`.item`), and compound
//! (`div.card`, `#id.cls`). No descendant combinators yet (M47+).

use std::collections::BTreeMap;

use browser_dom::{NodeData, NodeId, Tree};

use crate::jsonrpc::{CdpError, CdpMessage, Json};
use crate::page::PageState;

/// Build a CDP node object from a DOM tree node (recursive).
///
/// CDP `DOM.Node` shape:
/// ```json
/// {"nodeId":1,"parentId":0,"nodeType":1,"nodeName":"DIV",
///  "localName":"div","nodeValue":"","children":[...],"attributes":[...]}
/// ```
fn build_cdp_node(tree: &Tree, id: NodeId, parent: NodeId, next_id: &mut u32) -> (u32, Json) {
    let my_id = *next_id;
    *next_id += 1;
    let data = tree.data(id);
    let (node_type, node_name, local_name, node_value, attrs_flat, child_ids) = match data {
        NodeData::Document => (
            9i64,
            "#document".to_string(),
            String::new(),
            String::new(),
            Vec::new(),
            tree.children_of(id).to_vec(),
        ),
        NodeData::Doctype { name } => (
            10,
            name.clone(),
            String::new(),
            String::new(),
            Vec::new(),
            Vec::new(),
        ),
        NodeData::Element { tag, attrs } => {
            let mut flat = Vec::new();
            for (k, v) in attrs {
                flat.push(Json::String(k.clone()));
                flat.push(Json::String(v.clone()));
            }
            (
                1,
                tag.to_ascii_uppercase(),
                tag.to_ascii_lowercase(),
                String::new(),
                flat,
                tree.children_of(id).to_vec(),
            )
        }
        NodeData::Text(s) => (
            3,
            "#text".to_string(),
            String::new(),
            s.clone(),
            Vec::new(),
            Vec::new(),
        ),
        NodeData::Comment(s) => (
            8,
            "#comment".to_string(),
            String::new(),
            s.clone(),
            Vec::new(),
            Vec::new(),
        ),
    };
    let mut children_arr = Vec::new();
    for &child in &child_ids {
        let (cid, cjson) = build_cdp_node(tree, child, id, next_id);
        children_arr.push(cjson);
        let _ = cid;
    }
    let mut m = BTreeMap::new();
    m.insert("nodeId".to_string(), Json::Number(my_id as f64));
    m.insert("parentId".to_string(), Json::Number(parent as f64));
    m.insert("nodeType".to_string(), Json::Number(node_type as f64));
    m.insert("nodeName".to_string(), Json::String(node_name));
    m.insert("localName".to_string(), Json::String(local_name));
    m.insert("nodeValue".to_string(), Json::String(node_value));
    if !attrs_flat.is_empty() {
        m.insert("attributes".to_string(), Json::Array(attrs_flat));
    }
    if !children_arr.is_empty() {
        m.insert("children".to_string(), Json::Array(children_arr));
    }
    (my_id, Json::Object(m))
}

/// Serialize a DOM subtree to an HTML string (for `DOM.getOuterHTML`).
fn serialize_html(tree: &Tree, id: NodeId) -> String {
    let mut out = String::new();
    serialize_node(tree, id, &mut out);
    out
}

fn serialize_node(tree: &Tree, id: NodeId, out: &mut String) {
    match tree.data(id) {
        NodeData::Document => {
            for &child in tree.children_of(id) {
                serialize_node(tree, child, out);
            }
        }
        NodeData::Doctype { name } => {
            out.push_str("<!DOCTYPE ");
            out.push_str(name);
            out.push('>');
        }
        NodeData::Element { tag, attrs } => {
            out.push('<');
            out.push_str(tag);
            for (k, v) in attrs {
                out.push(' ');
                out.push_str(k);
                out.push_str("=\"");
                out.push_str(v);
                out.push('"');
            }
            out.push('>');
            let void = matches!(
                tag.to_ascii_lowercase().as_str(),
                "br" | "img"
                    | "hr"
                    | "input"
                    | "meta"
                    | "link"
                    | "area"
                    | "base"
                    | "col"
                    | "embed"
                    | "param"
                    | "source"
                    | "track"
                    | "wbr"
            );
            if !void {
                for &child in tree.children_of(id) {
                    serialize_node(tree, child, out);
                }
                out.push_str("</");
                out.push_str(tag);
                out.push('>');
            }
        }
        NodeData::Text(s) => {
            out.push_str(s);
        }
        NodeData::Comment(s) => {
            out.push_str("<!--");
            out.push_str(s);
            out.push_str("-->");
        }
    }
}

/// A parsed simple selector: optional tag, optional id, optional classes.
#[derive(Debug, Default, Clone)]
struct SimpleSelector {
    tag: Option<String>,
    id: Option<String>,
    classes: Vec<String>,
}

/// Parse a simple selector string (`div`, `#id`, `.cls`, `div#id.cls`).
/// Returns `None` if it contains unsupported combinators (descendant ` `).
fn parse_simple_selector(s: &str) -> Option<SimpleSelector> {
    let s = s.trim();
    if s.is_empty() || s.contains(' ') || s.contains('>') {
        return None; // descendant/child combinators unsupported
    }
    let mut sel = SimpleSelector::default();
    // Tokenize by splitting on # and . while keeping the prefix char.
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let start = i;
        let prefix = if bytes[i] == b'#' || bytes[i] == b'.' {
            let c = bytes[i];
            i += 1;
            Some(c)
        } else {
            None
        };
        while i < bytes.len() && bytes[i] != b'#' && bytes[i] != b'.' {
            i += 1;
        }
        let token: String = s[start + (if prefix.is_some() { 1 } else { 0 })..i].to_string();
        match prefix {
            Some(b'#') => sel.id = Some(token),
            Some(b'.') => sel.classes.push(token),
            _ => sel.tag = Some(token.to_ascii_lowercase()),
        }
    }
    Some(sel)
}

/// Check if an element node matches a simple selector.
fn matches(tree: &Tree, id: NodeId, sel: &SimpleSelector) -> bool {
    let NodeData::Element { tag, attrs } = tree.data(id) else {
        return false;
    };
    if let Some(ref want) = sel.tag {
        if !tag.eq_ignore_ascii_case(want) {
            return false;
        }
    }
    let mut id_attr: Option<&str> = None;
    let mut class_attr: Option<&str> = None;
    for (k, v) in attrs {
        if k.eq_ignore_ascii_case("id") {
            id_attr = Some(v.as_str());
        }
        if k.eq_ignore_ascii_case("class") {
            class_attr = Some(v.as_str());
        }
    }
    if let Some(ref want) = sel.id {
        if id_attr != Some(want.as_str()) {
            return false;
        }
    }
    if !sel.classes.is_empty() {
        let have: Vec<&str> = class_attr
            .map(|c| c.split_whitespace().collect())
            .unwrap_or_default();
        for want in &sel.classes {
            if !have.contains(&want.as_str()) {
                return false;
            }
        }
    }
    true
}

/// Find first element matching `selector` (depth-first from root).
fn query_first(tree: &Tree, root: NodeId, selector: &str) -> Option<NodeId> {
    let sel = parse_simple_selector(selector)?;
    let mut stack = vec![root];
    while let Some(id) = stack.pop() {
        if matches(tree, id, &sel) {
            return Some(id);
        }
        // Push children in reverse to preserve document order.
        for &child in tree.children_of(id).iter().rev() {
            stack.push(child);
        }
    }
    None
}

/// Find all elements matching `selector` (document order).
fn query_all(tree: &Tree, root: NodeId, selector: &str) -> Vec<NodeId> {
    let Some(sel) = parse_simple_selector(selector) else {
        return Vec::new();
    };
    let mut result = Vec::new();
    let mut stack: Vec<NodeId> = vec![root];
    // Use pre-order traversal: collect matches in document order.
    // Reverse-stack gives correct order when popped.
    let mut ordered: Vec<NodeId> = Vec::new();
    while let Some(id) = stack.pop() {
        ordered.push(id);
        for &child in tree.children_of(id).iter().rev() {
            stack.push(child);
        }
    }
    for id in ordered {
        if matches(tree, id, &sel) {
            result.push(id);
        }
    }
    result
}

/// Dispatch a `DOM.*` CDP method.
pub fn dispatch(
    id: i64,
    method: &str,
    params: Option<&Json>,
    state: &PageState,
) -> Result<String, CdpError> {
    match method {
        "DOM.getDocument" => {
            let tree = &state.tree;
            let (_, root_node) = build_cdp_node(tree, tree.root(), 0, &mut 1);
            let mut result = BTreeMap::new();
            result.insert("root".to_string(), root_node);
            Ok(CdpMessage::ok_response(id, Json::Object(result)))
        }
        "DOM.getOuterHTML" => {
            // M46 simplified: always serialize from root (nodeId mapping not
            // persisted across calls in this minimal impl).
            let html = serialize_html(&state.tree, state.tree.root());
            Ok(CdpMessage::ok_response(id, Json::String(html)))
        }
        "DOM.querySelector" => {
            let selector = params
                .and_then(|p| p.get_str("selector"))
                .ok_or_else(|| CdpError::InvalidJson("missing selector".to_string()))?;
            let found = query_first(&state.tree, state.tree.root(), selector);
            let node_id = found.map(|nid| nid as f64).unwrap_or(0.0);
            let mut result = BTreeMap::new();
            result.insert("nodeId".to_string(), Json::Number(node_id));
            Ok(CdpMessage::ok_response(id, Json::Object(result)))
        }
        "DOM.describeNode" => {
            let node_id = params
                .and_then(|p| p.get("nodeId"))
                .and_then(|p| match p {
                    Json::Number(n) => Some(*n as usize),
                    _ => None,
                })
                .ok_or_else(|| CdpError::InvalidJson("missing nodeId or not number".to_string()))?;
            let mut next_id = 0;
            let (_, node) = build_cdp_node(&state.tree, node_id, 0, &mut next_id);
            let mut result = BTreeMap::new();
            result.insert("node".to_string(), node);
            Ok(CdpMessage::ok_response(id, Json::Object(result)))
        }
        "DOM.querySelectorAll" => {
            let selector = params
                .and_then(|p| p.get_str("selector"))
                .ok_or_else(|| CdpError::InvalidJson("missing selector".to_string()))?;
            let found = query_all(&state.tree, state.tree.root(), selector);
            let ids: Vec<Json> = found.iter().map(|nid| Json::Number(*nid as f64)).collect();
            let mut result = BTreeMap::new();
            result.insert("nodeIds".to_string(), Json::Array(ids));
            Ok(CdpMessage::ok_response(id, Json::Object(result)))
        }
        _ => Err(CdpError::MethodNotFound(method.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_state(html: &str) -> PageState {
        let mut st = PageState::default();
        st.render(html, "test://x", 80);
        st
    }

    #[test]
    fn serialize_simple_html() {
        let st = make_state("<html><body><p>Hello</p></body></html>");
        let html = serialize_html(&st.tree, st.tree.root());
        assert!(html.contains("<p>Hello</p>"), "got: {html}");
        assert!(html.contains("<body>"), "got: {html}");
    }

    #[test]
    fn serialize_void_elements_no_closing_tag() {
        let st = make_state("<html><body><br><img src=\"x.png\"></body></html>");
        let html = serialize_html(&st.tree, st.tree.root());
        assert!(html.contains("<br>"), "got: {html}");
        assert!(html.contains("<img src=\"x.png\">"), "got: {html}");
        assert!(!html.contains("</br>"), "got: {html}");
        assert!(!html.contains("</img>"), "got: {html}");
    }

    #[test]
    fn parse_simple_selector_tag() {
        let s = parse_simple_selector("div").unwrap();
        assert_eq!(s.tag.as_deref(), Some("div"));
        assert!(s.id.is_none());
        assert!(s.classes.is_empty());
    }

    #[test]
    fn parse_simple_selector_id_and_class() {
        let s = parse_simple_selector("div#main.active").unwrap();
        assert_eq!(s.tag.as_deref(), Some("div"));
        assert_eq!(s.id.as_deref(), Some("main"));
        assert_eq!(s.classes, vec!["active".to_string()]);
    }

    #[test]
    fn parse_simple_selector_rejects_descendant() {
        assert!(parse_simple_selector("div p").is_none());
        assert!(parse_simple_selector("div>p").is_none());
    }

    #[test]
    fn query_first_finds_by_tag() {
        let st = make_state("<html><body><p>A</p><p>B</p></body></html>");
        let found = query_first(&st.tree, st.tree.root(), "p");
        assert!(found.is_some(), "should find <p>");
    }

    #[test]
    fn query_first_finds_by_id() {
        let st = make_state("<html><body><div id=\"main\">X</div></body></html>");
        let found = query_first(&st.tree, st.tree.root(), "#main");
        assert!(found.is_some());
    }

    #[test]
    fn query_first_finds_by_class() {
        let st = make_state("<html><body><span class=\"hi\">Y</span></body></html>");
        let found = query_first(&st.tree, st.tree.root(), ".hi");
        assert!(found.is_some());
    }

    #[test]
    fn query_first_not_found() {
        let st = make_state("<html><body><p>X</p></body></html>");
        let found = query_first(&st.tree, st.tree.root(), ".missing");
        assert!(found.is_none());
    }

    #[test]
    fn query_all_finds_multiple() {
        let st = make_state("<html><body><p>A</p><p>B</p><p>C</p></body></html>");
        let found = query_all(&st.tree, st.tree.root(), "p");
        assert_eq!(found.len(), 3, "should find 3 <p> elements");
    }

    #[test]
    fn build_cdp_node_has_correct_types() {
        let st = make_state("<html><body><p>hi</p></body></html>");
        let (_, root) = build_cdp_node(&st.tree, st.tree.root(), 0, &mut 1);
        // Document node has nodeType 9.
        let nt = root.get_id("nodeType");
        assert_eq!(nt, Some(9));
        // Has children.
        assert!(root.get("children").is_some());
    }

    #[test]
    fn dispatch_get_outer_html() {
        let st = make_state("<html><body><p>Hi</p></body></html>");
        let resp = dispatch(1, "DOM.getOuterHTML", None, &st).unwrap();
        assert!(resp.contains("<p>Hi</p>"), "got: {resp}");
    }

    #[test]
    fn dispatch_query_selector() {
        let st = make_state("<html><body><div id=\"x\">Y</div></body></html>");
        let mut params = BTreeMap::new();
        params.insert("selector".to_string(), Json::String("#x".to_string()));
        let resp = dispatch(1, "DOM.querySelector", Some(&Json::Object(params)), &st).unwrap();
        // Should return a nodeId > 0.
        assert!(resp.contains("\"nodeId\""), "got: {resp}");
        assert!(!resp.contains("\"nodeId\":0"), "got: {resp}");
    }

    #[test]
    fn dispatch_query_selector_all() {
        let st = make_state("<html><body><p>1</p><p>2</p></body></html>");
        let mut params = BTreeMap::new();
        params.insert("selector".to_string(), Json::String("p".to_string()));
        let resp = dispatch(1, "DOM.querySelectorAll", Some(&Json::Object(params)), &st).unwrap();
        assert!(resp.contains("\"nodeIds\""), "got: {resp}");
    }

    #[test]
    fn dispatch_unknown_method() {
        let st = make_state("<html><body></body></html>");
        let result = dispatch(1, "DOM.totallyMadeUp", None, &st);
        assert!(result.is_err());
    }
}
