//! `--format highlights`: 提取页面的强调/高亮内容。
//!
//! 对标 Firecrawl `formats: ["highlights"]`。收集 `<mark>`, `<strong>`,
//! `<em>`, `<b>`, `<i>` 的文本内容，按文档顺序输出 `[tag] text`。

use browser_dom::{NodeData, NodeId, Tree};
use std::collections::HashSet;

/// 提取强调内容。
pub fn to_highlights(
    tree: &Tree,
    _selector: Option<&str>,
    excluded: &HashSet<NodeId>,
) -> Result<String, String> {
    let mut results: Vec<String> = Vec::new();

    tree.traverse(tree.root(), |id, _| {
        if excluded.contains(&id) {
            return true;
        }
        if let NodeData::Element { tag, .. } = &tree.get(id).data {
            let highlight_tags = ["mark", "strong", "em", "b"];
            if highlight_tags.iter().any(|t| tag.eq_ignore_ascii_case(t)) {
                let text = collect_text(tree, id, excluded);
                let text = text.trim();
                // 跳过过短内容（通常是图标字体或装饰性元素，非真正强调）
                if text.len() >= 3 && text.chars().any(|c| c.is_ascii_alphanumeric()) {
                    results.push(format!("[{tag}] {text}"));
                }
            }
        }
        true
    });

    if results.is_empty() {
        Ok("(no highlights found)".to_string())
    } else {
        Ok(results.join("\n"))
    }
}

/// 收集子树纯文本。
fn collect_text(tree: &Tree, id: NodeId, excluded: &HashSet<NodeId>) -> String {
    let mut out = String::new();
    for &child in tree.children_of(id) {
        if excluded.contains(&child) {
            continue;
        }
        match &tree.get(child).data {
            NodeData::Text(s) => out.push_str(s),
            NodeData::Element { .. } => {
                out.push_str(&collect_text(tree, child, excluded));
            }
            _ => {}
        }
    }
    // 折叠空白
    let parts: Vec<&str> = out.split_whitespace().collect();
    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use browser_html_parser::parse;

    #[test]
    fn extracts_strong() {
        let html = r#"<p>this is <strong>important</strong> text</p>"#;
        let tree = parse(html);
        let out = to_highlights(&tree, None, &HashSet::new()).unwrap();
        assert!(out.contains("[strong] important"), "got: {out}");
    }

    #[test]
    fn extracts_mark_and_em() {
        let html = r#"<p><mark>highlight</mark> and <em>emphasized</em></p>"#;
        let tree = parse(html);
        let out = to_highlights(&tree, None, &HashSet::new()).unwrap();
        assert!(out.contains("[mark] highlight"), "got: {out}");
        assert!(out.contains("[em] emphasized"), "got: {out}");
    }

    #[test]
    fn empty_when_no_highlights() {
        let html = r#"<p>plain text</p>"#;
        let tree = parse(html);
        let out = to_highlights(&tree, None, &HashSet::new()).unwrap();
        assert!(out.contains("no highlights"), "got: {out}");
    }
}
