//! `--format text`：纯文本提取。
//!
//! DFS 遍历 DOM，块级元素（p/div/h1-h6/li/ul/ol/tr/br/blockquote/pre）后加换行，
//! inline 元素直接拼文本，`<br>` 也触发换行。连续空白折叠为单个空格。

use browser_dom::{NodeId, Tree};

use crate::selector::query_all;

/// HTML 块级元素集合（遇到这些元素的前后需要换行）。
const BLOCK_TAGS: &[&str] = &[
    "p",
    "div",
    "section",
    "article",
    "header",
    "footer",
    "nav",
    "aside",
    "main",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "ul",
    "ol",
    "li",
    "table",
    "tr",
    "thead",
    "tbody",
    "blockquote",
    "pre",
    "hr",
];

fn is_block(tag: &str) -> bool {
    BLOCK_TAGS.contains(&tag)
}

/// 提取纯文本。
///
/// - `selector` 为 `None`：从文档根开始。
/// - `selector` 为 `Some(s)`：只提取匹配元素子树的文本。
///
/// # Errors
/// 选择器解析失败时返回 `Err(String)`。
pub fn to_text(tree: &Tree, selector: Option<&str>) -> Result<String, String> {
    let roots: Vec<NodeId> = match selector {
        Some(sel) => query_all(tree, sel)?,
        None => vec![tree.root()],
    };
    let mut out = String::new();
    for root in roots {
        walk_text(tree, root, &mut out);
    }
    // 后处理：折叠连续空白、去除行首尾空白、合并多余空行。
    Ok(postprocess(&out))
}

/// 递归收集文本，按块级元素插入换行。
fn walk_text(tree: &Tree, id: NodeId, out: &mut String) {
    let node = tree.get(id);
    match &node.data {
        browser_dom::NodeData::Text(s) => {
            out.push_str(s);
        }
        browser_dom::NodeData::Element { tag, .. } => {
            // <br> 直接换行。
            if tag.eq_ignore_ascii_case("br") {
                out.push('\n');
                return;
            }
            // 块级元素前换行（若 out 非空且不以换行结尾）。
            if is_block(tag) && !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            for &child in tree.children_of(id) {
                walk_text(tree, child, out);
            }
            // 块级元素后换行。
            if is_block(tag) && !out.ends_with('\n') {
                out.push('\n');
            }
        }
        browser_dom::NodeData::Document | browser_dom::NodeData::Doctype { .. } => {
            for &child in tree.children_of(id) {
                walk_text(tree, child, out);
            }
        }
        browser_dom::NodeData::Comment(_) => {}
    }
}

/// 折叠连续空白为单个空格，去除每行首尾空白，合并 3+ 连续空行为最多 1 个空行。
fn postprocess(s: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    for raw_line in s.split('\n') {
        // 行内连续空白折叠为单个空格。
        let folded: String = {
            let mut result = String::with_capacity(raw_line.len());
            let mut prev_space = false;
            for ch in raw_line.chars() {
                if ch.is_whitespace() {
                    if !prev_space {
                        result.push(' ');
                    }
                    prev_space = true;
                } else {
                    result.push(ch);
                    prev_space = false;
                }
            }
            result.trim().to_string()
        };
        lines.push(folded);
    }
    // 合并连续空行：最多保留 1 个空行分隔。
    let mut out = String::new();
    let mut blank_run = 0;
    for line in &lines {
        if line.is_empty() {
            blank_run += 1;
            if blank_run <= 1 && !out.is_empty() {
                out.push('\n');
            }
        } else {
            blank_run = 0;
            out.push_str(line);
            out.push('\n');
        }
    }
    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(html: &str) -> Tree {
        browser_html_parser::parse(html)
    }

    #[test]
    fn to_text_extracts_plain_text() {
        let tree = parse("<p>hello world</p>");
        let text = to_text(&tree, None).expect("text");
        assert_eq!(text, "hello world");
    }

    #[test]
    fn to_text_separates_block_elements() {
        let tree = parse("<p>one</p><p>two</p>");
        let text = to_text(&tree, None).expect("text");
        assert!(text.contains("one"));
        assert!(text.contains("two"));
        // 两个段落应在不同行
        assert!(text.contains("one\ntwo") || text.contains("one\n\ntwo"));
    }

    #[test]
    fn to_text_collapses_whitespace() {
        let tree = parse("<p>  multiple    spaces  </p>");
        let text = to_text(&tree, None).expect("text");
        assert!(!text.contains("  "), "no double spaces: {text:?}");
    }

    #[test]
    fn to_text_handles_br() {
        let tree = parse("<p>line1<br>line2</p>");
        let text = to_text(&tree, None).expect("text");
        assert!(text.contains("line1") && text.contains("line2"));
        assert!(text.contains('\n'), "br should produce newline");
    }

    #[test]
    fn to_text_with_selector() {
        let tree = parse("<div><p class='x'>target</p><p>other</p></div>");
        let text = to_text(&tree, Some(".x")).expect("selector");
        assert_eq!(text, "target");
    }

    #[test]
    fn to_text_ignores_comments() {
        let tree = parse("<p>visible</p><!-- hidden comment -->");
        let text = to_text(&tree, None).expect("text");
        assert_eq!(text, "visible");
        assert!(!text.contains("hidden"));
    }
}
