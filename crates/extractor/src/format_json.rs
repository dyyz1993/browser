//! `--format json`：以 JSON 结构输出 DOM 内容 + 元数据。
//!
//! JSON 嵌入所有已有格式的输出（text/html/links/images）为 JSON 字段。
//! console/error/network 不在 extractor 层——这些来自 js-runtime，
//! 在 CLI 层组装。

use browser_dom::{NodeId, Tree};
use std::collections::HashSet;

use crate::{format_html, format_images, format_links, format_text, selector::query_all};

/// 以 JSON 格式输出 DOM 内容。
///
/// 嵌入所有已有格式的输出为 JSON 字段。
///
/// # Errors
/// 同其他 extractor 格式——选择器解析失败返回 `Err`。
pub fn to_json(
    tree: &Tree,
    base_url: Option<&str>,
    selector: Option<&str>,
    excluded: &HashSet<NodeId>,
) -> Result<String, String> {
    let _roots: Vec<NodeId> = match selector {
        Some(sel) => query_all(tree, sel)?,
        None => {
            if tree.is_empty() {
                // 空树直接返回空 JSON。tree.root() 在空树 panic。
                return Ok(r#"{"title":"","text":"","html":"","links":"","images":""}"#.to_string());
            }
            vec![tree.root()]
        }
    };
    let excl = excluded.clone();

    let text = format_text::to_text(tree, selector, &excl).unwrap_or_default();
    let html = format_html::to_html(tree, selector, &excl).unwrap_or_default();
    let links = format_links::to_links(tree, base_url, selector, &excl).unwrap_or_default();
    let images = format_images::to_images(tree, base_url, selector, &excl).unwrap_or_default();
    // 从 tree 找 title
    let title = extract_title(tree);

    let title_e = json_escape(&title.unwrap_or_default());
    let text_e = json_escape(&text);
    let html_e = json_escape(&html);
    let links_e = json_escape(&links);
    let images_e = json_escape(&images);

    Ok(format!(
        r#"{{"title":"{t}","text":"{x}","html":"{h}","links":"{l}","images":"{i}"}}"#,
        t = title_e,
        x = text_e,
        h = html_e,
        l = links_e,
        i = images_e,
    ))
}

/// DOM 树里找 `<title>` 文本。
fn extract_title(tree: &Tree) -> Option<String> {
    let mut found: Option<String> = None;
    tree.traverse(tree.root(), |id, _| {
        if let browser_dom::NodeData::Element { tag, .. } = &tree.get(id).data {
            if tag.eq_ignore_ascii_case("title") {
                for &child in tree.children_of(id) {
                    if let browser_dom::NodeData::Text(s) = &tree.get(child).data {
                        found = Some(s.trim().to_string());
                        return false;
                    }
                }
            }
        }
        true
    });
    found
}

/// JSON 字符串转义（纯手写，无 serde 依赖）。
fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 16);
    for ch in s.chars() {
        match ch {
            '"' => out.push_str(r#"\""#),
            '\\' => out.push_str(r#"\\"#),
            '\n' => out.push_str(r#"\n"#),
            '\r' => out.push_str(r#"\r"#),
            '\t' => out.push_str(r#"\t"#),
            c if c < ' ' => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(html: &str) -> Tree {
        browser_html_parser::parse(html)
    }

    #[test]
    fn to_json_contains_all_fields() {
        let tree = parse("<html><head><title>Test</title></head><body><p>hello</p></body></html>");
        let json = to_json(&tree, None, None, &HashSet::new()).expect("json");
        assert!(json.contains(r#""title":"Test""#), "title");
        assert!(json.contains(r#""text""#), "text field");
        assert!(json.contains(r#""html""#), "html field");
        assert!(json.contains(r#""links""#), "links field");
        assert!(json.contains(r#""images""#), "images field");
    }

    #[test]
    fn to_json_escapes_quotes() {
        let tree = parse(r#"<p>hello "world"</p>"#);
        let json = to_json(&tree, None, None, &HashSet::new()).expect("json");
        assert!(json.contains(r#"\""#), "quotes escaped: {json}");
    }

    #[test]
    fn to_json_handles_empty() {
        let tree = Tree::new();
        let json = to_json(&tree, None, None, &HashSet::new()).unwrap_or_default();
        assert!(json.contains(r#""title":"""#));
    }
}
