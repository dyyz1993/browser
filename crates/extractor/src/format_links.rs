//! `--format links`：超链接地图。
//!
//! 遍历所有 `<a href>`，用 `Url::join(base)` 绝对化，去重，输出
//! `链接文本 → URL` 列表（一行一条）。不依赖属性选择器（css-engine 不支持），
//! 直接 DFS 找 `<a>` 元素。

use std::collections::HashSet;

use browser_dom::{NodeId, Tree};
use url::Url;

use crate::selector::query_all;

/// 提取超链接地图。
///
/// - `base_url`：用于把相对 URL 解析为绝对 URL（页面 URL）。
/// - `selector`：可选，只提取匹配元素子树内的链接。
///
/// 输出格式：每行 `文本 → URL`，按文档顺序，URL 去重。
///
/// # Errors
/// 选择器解析失败时返回 `Err(String)`。
pub fn to_links(
    tree: &Tree,
    base_url: Option<&str>,
    selector: Option<&str>,
    excluded: &HashSet<NodeId>,
) -> Result<String, String> {
    let base = base_url.and_then(|b| Url::parse(b).ok());
    let roots: Vec<NodeId> = match selector {
        Some(sel) => query_all(tree, sel)?,
        None => vec![tree.root()],
    };

    let mut out = String::new();
    let mut seen: HashSet<String> = HashSet::new();
    for root in roots {
        collect_links(tree, root, base.as_ref(), &mut out, &mut seen, excluded);
    }
    Ok(out.trim_end().to_string())
}

/// 递归收集 `<a href>`，绝对化 + 去重。
fn collect_links(
    tree: &Tree,
    id: NodeId,
    base: Option<&Url>,
    out: &mut String,
    seen: &mut HashSet<String>,
    excluded: &HashSet<NodeId>,
) {
    if excluded.contains(&id) {
        return; // 噪声节点整棵子树跳过
    }
    if let browser_dom::NodeData::Element { tag, attrs } = &tree.get(id).data {
        if tag.eq_ignore_ascii_case("a") {
            let href = attrs.iter().find_map(|(k, v)| {
                if k.eq_ignore_ascii_case("href") {
                    Some(v.as_str())
                } else {
                    None
                }
            });
            if let Some(href) = href {
                // 跳过空、锚点、javascript: 等。
                if !href.is_empty() && !href.starts_with("javascript:") {
                    let absolute = resolve(href, base);
                    if !absolute.is_empty() && seen.insert(absolute.clone()) {
                        let text = anchor_text(tree, id);
                        out.push_str(&text);
                        out.push_str(" → ");
                        out.push_str(&absolute);
                        out.push('\n');
                    }
                }
            }
        }
    }
    for &child in tree.children_of(id) {
        collect_links(tree, child, base, out, seen, excluded);
    }
}

/// 解析相对/绝对 URL 为绝对 URL。失败或相对且无 base 时返回原始 href。
fn resolve(href: &str, base: Option<&Url>) -> String {
    if let Ok(abs) = Url::parse(href) {
        return abs.to_string();
    }
    if let Some(b) = base {
        if let Ok(joined) = b.join(href) {
            return joined.to_string();
        }
    }
    href.to_string()
}

/// 提取 `<a>` 元素的可见文本（递归子节点）。
fn anchor_text(tree: &Tree, id: NodeId) -> String {
    let mut s = String::new();
    collect_text(tree, id, &mut s);
    let t = s.trim().to_string();
    if t.is_empty() {
        "(untitled)".to_string()
    } else {
        t
    }
}

/// 递归收集纯文本（不含块级换行逻辑，链接文本通常简短）。
fn collect_text(tree: &Tree, id: NodeId, out: &mut String) {
    match &tree.get(id).data {
        browser_dom::NodeData::Text(s) => out.push_str(s),
        browser_dom::NodeData::Element { .. } => {
            for &child in tree.children_of(id) {
                collect_text(tree, child, out);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(html: &str) -> Tree {
        browser_html_parser::parse(html)
    }

    #[test]
    fn to_links_extracts_absolute_urls() {
        let tree =
            parse("<a href='https://example.com/a'>A</a><a href='https://example.com/b'>B</a>");
        let links = to_links(&tree, None, None, &HashSet::new()).expect("links");
        assert!(links.contains("A → https://example.com/a"));
        assert!(links.contains("B → https://example.com/b"));
    }

    #[test]
    fn to_links_resolves_relative_against_base() {
        let tree = parse("<a href='/page'>Page</a>");
        let links =
            to_links(&tree, Some("https://site.com/dir/"), None, &HashSet::new()).expect("links");
        assert!(
            links.contains("https://site.com/page"),
            "relative resolved: {links}"
        );
    }

    #[test]
    fn to_links_resolves_dot_relative() {
        let tree = parse("<a href='./sub.json'>data</a>");
        let links = to_links(
            &tree,
            Some("https://seo.box/referring/"),
            None,
            &HashSet::new(),
        )
        .expect("links");
        assert!(
            links.contains("https://seo.box/referring/sub.json"),
            "dot-relative resolved: {links}"
        );
    }

    #[test]
    fn to_links_deduplicates() {
        let tree = parse("<a href='https://x.com/'>X</a><a href='https://x.com/'>X2</a>");
        let links = to_links(&tree, None, None, &HashSet::new()).expect("links");
        // 同一 URL 只出现一次
        assert_eq!(links.matches("https://x.com/").count(), 1);
    }

    #[test]
    fn to_links_skips_javascript_and_empty() {
        let tree = parse("<a href='javascript:void(0)'>js</a><a href=''>empty</a><a href='https://ok.com/'>ok</a>");
        let links = to_links(&tree, None, None, &HashSet::new()).expect("links");
        assert!(!links.contains("javascript"));
        assert!(links.contains("ok.com"));
    }

    #[test]
    fn to_links_with_selector_scopes_extraction() {
        let tree = parse(
            "<div class='nav'><a href='/nav1'>N</a></div><article><a href='/art1'>A</a></article>",
        );
        let links = to_links(
            &tree,
            Some("https://x.com/"),
            Some("article"),
            &HashSet::new(),
        )
        .expect("links");
        assert!(links.contains("/art1") || links.contains("x.com/art1"));
        assert!(!links.contains("nav1"));
    }
}
