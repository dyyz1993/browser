//! Serialize a DOM subtree back to an HTML string.
//!
//! Used by `browser spa` CLI command and CDP `DOM.getOuterHTML`.
//! Tests are in `browser-cdp::dom_domain` (which already has full coverage).

use crate::node::NodeData;
use crate::tree::{NodeId, Tree};

/// Serialize a DOM subtree to an HTML string.
pub fn serialize_html(tree: &Tree, id: NodeId) -> String {
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
            // M79: JS bridge 的伪标签不得泄漏进序列化输出。
            // `__text__` = createTextNode 的透明包装（Element+Text 子节点）→
            // 只输出子节点；`__comment__` = createComment 包装（树内无数据）→
            // 不可见（注释本就不参与文本/HTML 语义）。此前 solidjs.com 等
            // SPA 的 html 输出会漏出 `<__text__>` 垃圾标签。
            match tag.as_str() {
                "__text__" => {
                    for &child in tree.children_of(id) {
                        serialize_node(tree, child, out);
                    }
                    return;
                }
                "__comment__" => return,
                _ => {}
            }
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
