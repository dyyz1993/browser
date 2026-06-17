//! `--format html`：序列化渲染后 DOM 为 HTML 字符串。
//!
//! 薄包装 `dom::serialize_html`。若指定了 `--selector`，只序列化匹配的子树
//! （多个匹配则依次拼接）。

use browser_dom::{serialize_html, NodeId, Tree};
use std::collections::HashSet;

use crate::selector::query_all;

/// 序列化为 HTML。
///
/// - `selector` 为 `None`：序列化整个文档。
/// - `selector` 为 `Some(s)`：只序列化匹配 `s` 的元素子树，按文档顺序拼接。
///
/// # Errors
/// 选择器解析失败时返回 `Err(String)`。
pub fn to_html(
    tree: &Tree,
    selector: Option<&str>,
    excluded: &HashSet<NodeId>,
) -> Result<String, String> {
    // selector 模式：序列化匹配子树（子树内部不再做噪声过滤，selector 已精确指定）。
    let Some(sel) = selector else {
        // 整文档模式：若无非噪声过滤，直接序列化整棵；否则逐子树序列化跳过 excluded。
        if excluded.is_empty() {
            return Ok(serialize_html(tree, tree.root()));
        }
        let mut out = String::new();
        for &child in tree.children_of(tree.root()) {
            if excluded.contains(&child) {
                continue;
            }
            out.push_str(&serialize_html(tree, child));
        }
        return Ok(out);
    };
    let ids: Vec<NodeId> = query_all(tree, sel)?;
    let mut out = String::new();
    for id in ids {
        if excluded.contains(&id) {
            continue;
        }
        out.push_str(&serialize_html(tree, id));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(html: &str) -> Tree {
        browser_html_parser::parse(html)
    }

    #[test]
    fn to_html_full_document() {
        let tree = parse("<p>hello</p>");
        let html = to_html(&tree, None, &HashSet::new()).expect("full doc");
        assert!(html.contains("hello"));
        assert!(html.contains("<p>"));
    }

    #[test]
    fn to_html_with_selector_only_matching_subtree() {
        let tree = parse("<div><p class='x'>keep</p><p>drop</p></div>");
        let html = to_html(&tree, Some(".x"), &HashSet::new()).expect("selector");
        assert!(html.contains("keep"), "matched content present");
        assert!(!html.contains("drop"), "non-matched content excluded");
    }

    #[test]
    fn to_html_selector_multiple_matches_concatenated() {
        let tree = parse("<ul><li>a</li><li>b</li></ul>");
        let html = to_html(&tree, Some("li"), &HashSet::new()).expect("multiple");
        assert!(html.contains(">a<"));
        assert!(html.contains(">b<"));
    }
}
