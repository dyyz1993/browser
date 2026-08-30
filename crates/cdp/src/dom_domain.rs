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
//! - M81(B5): `DOM.getBoxModel` — nodeId → 布局树盒子的 CSS px 四角 quad
//!   （content/padding/border/margin + width/height）。无布局盒返回错误
//!   （CDP 标准行为）。
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

/// Get CDP nodeId (1-based) for an internal NodeId.
/// Helper for querySelector to return consistent nodeId with getDocument.
pub fn get_cdp_node_id(tree: &Tree, internal_id: NodeId) -> Option<u32> {
    let mut next_id = 1u32;
    let mut target_id = None;

    fn find_id(
        tree: &Tree,
        node_id: NodeId,
        internal_id: NodeId,
        next_id: &mut u32,
        target_id: &mut Option<u32>,
    ) {
        if target_id.is_some() {
            return;
        }

        if node_id == internal_id {
            *target_id = Some(*next_id);
            return;
        }

        *next_id += 1;

        for child in tree.children_of(node_id) {
            find_id(tree, *child, internal_id, next_id, target_id);
        }
    }

    find_id(tree, tree.root(), internal_id, &mut next_id, &mut target_id);
    target_id
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

/// M81(B5): CDP nodeId → 内部 NodeId —— [`get_cdp_node_id`] 的 1-based
/// 先序编号的逆映射（getDocument 也用同一编号），供 getBoxModel 等按
/// nodeId 定位 DOM 节点的方法复用。
pub fn internal_node_id_for_cdp(tree: &Tree, cdp_id: u32) -> Option<NodeId> {
    fn walk(tree: &Tree, node: NodeId, target: u32, counter: &mut u32) -> Option<NodeId> {
        if *counter == target {
            return Some(node);
        }
        *counter += 1;
        for &child in tree.children_of(node) {
            if let Some(found) = walk(tree, child, target, counter) {
                return Some(found);
            }
        }
        None
    }
    // CDP nodeId 从 1 开始（0 是 "not found" 哨兵，无节点对应）。
    if cdp_id == 0 {
        return None;
    }
    walk(tree, tree.root(), cdp_id, &mut 1)
}

/// M81(B5): 先序遍历布局树，找第一个 `element_id == element` 的盒子。
/// 元素通常对应一个盒（Anonymous 盒 `element_id` 为 None 自动跳过）。
fn find_layout_box(
    root: &browser_layout::LayoutBox,
    element: NodeId,
) -> Option<&browser_layout::LayoutBox> {
    if root.element_id == Some(element) {
        return Some(root);
    }
    for child in &root.children {
        if let Some(found) = find_layout_box(child, element) {
            return Some(found);
        }
    }
    None
}

/// M81(B5): f32 格单位 → 保留 2 位小数的 f64（JSON 数字稳定，避免
/// 浮点噪声：8.523809523809524 → 8.52）。
fn r2(v: f32) -> f64 {
    (f64::from(v) * 100.0).round() / 100.0
}

/// M81(B5): 用布局盒构造 CDP `BoxModel`（坐标换算 CSS px，与
/// `Input.dispatchMouseEvent`/pixel 渲染同一映射 `cell_metrics`）。
///
/// quad 为四角坐标（顺时针：左上→右上→右下→左下），各 8 个数。
/// 简化：本布局模型的内容区即盒子 dimensions（margin/padding 折算在
/// 流式布局里），四个 quad 相同；`width`/`height` 为内容区 px 尺寸。
fn box_model_json(bx: &browser_layout::LayoutBox) -> Json {
    let (cell_w, cell_h) = browser_render::cell_metrics();
    let d = &bx.dimensions;
    let (x, y) = (d.x * cell_w, d.y * cell_h);
    let (w, h) = (d.width * cell_w, d.height * cell_h);
    let quad = Json::Array(vec![
        Json::Number(r2(x)),
        Json::Number(r2(y)),
        Json::Number(r2(x + w)),
        Json::Number(r2(y)),
        Json::Number(r2(x + w)),
        Json::Number(r2(y + h)),
        Json::Number(r2(x)),
        Json::Number(r2(y + h)),
    ]);
    let mut model = BTreeMap::new();
    model.insert("content".to_string(), quad.clone());
    model.insert("padding".to_string(), quad.clone());
    model.insert("border".to_string(), quad.clone());
    model.insert("margin".to_string(), quad);
    model.insert("width".to_string(), Json::Number(r2(w)));
    model.insert("height".to_string(), Json::Number(r2(h)));
    let mut result = BTreeMap::new();
    result.insert("model".to_string(), Json::Object(model));
    Json::Object(result)
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
            //
            // M68-fix: 之前返回 `Json::String(html)`（裸字符串），但 CDP 协议
            // 要求 `{"result":{"outerHTML":"..."}}` 对象。puppeteer 期望从
            // `r.outerHTML` 取值，裸字符串会被它当 iterable 解构成
            // `{0:'<',1:'!',...}`，导致 `evaluate(()=>outerHTML)` 之外的所有
            // DOM 序列化路径全失效（completeness.py 评分因此全 D/F）。
            let html = serialize_html(&state.tree, state.tree.root());
            let mut result = BTreeMap::new();
            result.insert("outerHTML".to_string(), Json::String(html));
            Ok(CdpMessage::ok_response(id, Json::Object(result)))
        }
        "DOM.querySelector" => {
            let selector = params
                .and_then(|p| p.get_str("selector"))
                .ok_or_else(|| CdpError::InvalidJson("missing selector".to_string()))?;
            let found = query_first(&state.tree, state.tree.root(), selector);
            let cdp_id = found
                .and_then(|nid| get_cdp_node_id(&state.tree, nid))
                .unwrap_or(0);
            let mut result = BTreeMap::new();
            result.insert("nodeId".to_string(), Json::Number(cdp_id as f64));
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
            let ids: Vec<Json> = found
                .iter()
                .filter_map(|nid| get_cdp_node_id(&state.tree, *nid))
                .map(|cdp_id| Json::Number(cdp_id as f64))
                .collect();
            let mut result = BTreeMap::new();
            result.insert("nodeIds".to_string(), Json::Array(ids));
            Ok(CdpMessage::ok_response(id, Json::Object(result)))
        }
        // M81(B5): DOM.getBoxModel — nodeId（或 backendNodeId）→ 布局盒的
        // CSS px 四角 quad。无布局（未 navigate/未渲染）或该节点没有对应
        // 布局盒（文本节点、display:none、越界 id）→ CDP 错误（Chrome
        // 返回 "Could not compute box model for given node"）。
        "DOM.getBoxModel" => {
            let cdp_id = params
                .and_then(|p| p.get_id("nodeId").or_else(|| p.get_id("backendNodeId")))
                .ok_or_else(|| {
                    CdpError::InvalidJson("missing nodeId or backendNodeId".to_string())
                })?;
            let no_box =
                || CdpError::NotFound("Could not compute box model for given node".to_string());
            let node = internal_node_id_for_cdp(&state.tree, u32::try_from(cdp_id).unwrap_or(0))
                .ok_or_else(no_box)?;
            let layout = state.layout.as_ref().ok_or_else(no_box)?;
            let bx = find_layout_box(&layout.root, node).ok_or_else(no_box)?;
            Ok(CdpMessage::ok_response(id, box_model_json(bx)))
        }
        // M53: unknown methods → no-op ack (puppeteer sends many enable/disable)
        _ => Ok(CdpMessage::ok_empty(id)),
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

    /// 测试辅助：从 CDP 响应 `{"id":..,"result":{"nodeId":N}}` 里取 nodeId。
    fn response_node_id(resp: &str) -> i64 {
        crate::jsonrpc::parse_json(resp)
            .unwrap()
            .get("result")
            .and_then(|r| r.get_id("nodeId"))
            .unwrap_or(0)
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
        // M68-fix: 必须是 {"outerHTML":"..."} 对象，不是裸字符串。
        // 裸字符串会被 puppeteer 当 iterable 解构，外层代码取 r.outerHTML 得到 undefined。
        assert!(
            resp.contains("\"outerHTML\""),
            "response must wrap html in outerHTML field, got: {resp}"
        );
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
    fn dispatch_unknown_method_noop() {
        let st = make_state("<html></html>");
        // M53: unknown methods return no-op ack
        let resp = dispatch(1, "DOM.fakeMethod", None, &st).unwrap();
        assert!(resp.contains(r#""result":{}"#), "got: {resp}");
    }

    // ── M81(B5): DOM.getBoxModel ──

    #[test]
    fn internal_node_id_roundtrips_with_get_cdp_node_id() {
        let st =
            make_state("<html><body><p>one</p><div id=\"m\"><span>two</span></div></body></html>");
        // 对每个元素节点：内部 id → CDP id → 内部 id 应闭合。
        let mut stack = vec![st.tree.root()];
        let mut checked = 0;
        while let Some(id) = stack.pop() {
            if matches!(st.tree.data(id), NodeData::Element { .. }) {
                let cdp = get_cdp_node_id(&st.tree, id).expect("element gets a cdp id");
                assert_eq!(
                    internal_node_id_for_cdp(&st.tree, cdp),
                    Some(id),
                    "roundtrip failed for cdp id {cdp}"
                );
                checked += 1;
            }
            for &c in st.tree.children_of(id) {
                stack.push(c);
            }
        }
        assert!(checked >= 4, "should check several elements, got {checked}");
        // 0 与越界 id 无对应节点。
        assert_eq!(internal_node_id_for_cdp(&st.tree, 0), None);
        assert_eq!(internal_node_id_for_cdp(&st.tree, 9999), None);
    }

    #[test]
    fn find_layout_box_matches_element_and_skips_anonymous() {
        let st = make_state("<html><body><p>FindMe</p></body></html>");
        let p = query_first(&st.tree, st.tree.root(), "p").expect("p exists");
        let layout = st.layout.as_ref().expect("rendered page has layout");
        let bx = find_layout_box(&layout.root, p).expect("p has a layout box");
        assert_eq!(bx.element_id, Some(p));
        assert!(bx.dimensions.width > 0.0, "box must be laid out");
    }

    #[test]
    fn dispatch_get_box_model_returns_quad_in_px() {
        let st = make_state("<html><body><p>BoxMe</p></body></html>");
        // 用 querySelector 拿 body 的 CDP nodeId（真实客户端流程）。
        let mut sel = BTreeMap::new();
        sel.insert("selector".to_string(), Json::String("body".to_string()));
        let resp = dispatch(1, "DOM.querySelector", Some(&Json::Object(sel)), &st).unwrap();
        let node_id = response_node_id(&resp);
        assert!(node_id > 0, "querySelector must return a nodeId: {resp}");

        let mut params = BTreeMap::new();
        params.insert("nodeId".to_string(), Json::Number(node_id as f64));
        let resp = dispatch(2, "DOM.getBoxModel", Some(&Json::Object(params)), &st).unwrap();
        // 形状：result.model.content = [8 个数]，width/height > 0。
        assert!(resp.contains("\"model\""), "got: {resp}");
        assert!(
            resp.contains("\"content\":[")
                && resp.contains("\"border\":[")
                && resp.contains("\"padding\":[")
                && resp.contains("\"margin\":["),
            "got: {resp}"
        );
        // 坐标必须是 CSS px（x = 格单位 × cell_w），且 body 盒从 (0,0) 起。
        let (cw, lh) = browser_render::cell_metrics();
        let layout = st.layout.as_ref().unwrap();
        let body = query_first(&st.tree, st.tree.root(), "body").unwrap();
        let bx = find_layout_box(&layout.root, body).unwrap();
        let expect_x = (bx.dimensions.x * cw * 100.0).round() / 100.0;
        let expect_y = (bx.dimensions.y * lh * 100.0).round() / 100.0;
        let expect_w = (bx.dimensions.width * cw * 100.0).round() / 100.0;
        let expect_h = (bx.dimensions.height * lh * 100.0).round() / 100.0;
        let frag = format!(
            "\"content\":[{expect_x},{expect_y},{},{},{},{},{},{}]",
            r2(bx.dimensions.x * cw + bx.dimensions.width * cw),
            expect_y,
            r2(bx.dimensions.x * cw + bx.dimensions.width * cw),
            r2(bx.dimensions.y * lh + bx.dimensions.height * lh),
            expect_x,
            r2(bx.dimensions.y * lh + bx.dimensions.height * lh),
        );
        assert!(resp.contains(&frag), "expected {frag} in: {resp}");
        assert!(
            resp.contains(&format!("\"width\":{expect_w}"))
                && resp.contains(&format!("\"height\":{expect_h}")),
            "got: {resp}"
        );
    }

    #[test]
    fn dispatch_get_box_model_backend_node_id_alias() {
        // backendNodeId 与 nodeId 同义（单树模型无独立 backend store）。
        let st = make_state("<html><body><p>hi</p></body></html>");
        let mut sel = BTreeMap::new();
        sel.insert("selector".to_string(), Json::String("body".to_string()));
        let resp = dispatch(1, "DOM.querySelector", Some(&Json::Object(sel)), &st).unwrap();
        let node_id = response_node_id(&resp);
        let mut params = BTreeMap::new();
        params.insert("backendNodeId".to_string(), Json::Number(node_id as f64));
        let resp = dispatch(2, "DOM.getBoxModel", Some(&Json::Object(params)), &st).unwrap();
        assert!(resp.contains("\"model\""), "got: {resp}");
    }

    #[test]
    fn dispatch_get_box_model_errors_without_layout_or_node() {
        // ① 越界 nodeId（有布局也查不到盒）→ NotFound。
        let st = make_state("<html><body><p>x</p></body></html>");
        let mut params = BTreeMap::new();
        params.insert("nodeId".to_string(), Json::Number(9999.0));
        let err = dispatch(1, "DOM.getBoxModel", Some(&Json::Object(params)), &st).unwrap_err();
        assert!(
            err.to_string().contains("Could not compute box model"),
            "got: {err}"
        );
        // ② 未渲染（layout=None）：构造只 parse 不 layout 的状态。
        let mut st2 = PageState::default();
        st2.parse_only("<html><body><p>x</p></body></html>", "t://x", 80);
        let mut params = BTreeMap::new();
        params.insert("nodeId".to_string(), Json::Number(2.0));
        let err = dispatch(1, "DOM.getBoxModel", Some(&Json::Object(params)), &st2).unwrap_err();
        assert!(
            err.to_string().contains("Could not compute box model"),
            "got: {err}"
        );
        // ③ 缺 nodeId 参数 → InvalidJson。
        let st3 = make_state("<html><body><p>x</p></body></html>");
        let err = dispatch(1, "DOM.getBoxModel", None, &st3).unwrap_err();
        assert!(err.to_string().contains("missing nodeId"), "got: {err}");
    }
}
