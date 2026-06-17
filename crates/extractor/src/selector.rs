//! `--selector` CSS 选择器过滤。
//!
//! 复用 `css_engine::Selector`（已支持 tag/class/id/后代/选择器列表）。
//! 注意：css-engine **不支持属性选择器**（`a[href]` 这类），所以 links 格式
//! 内部直接遍历 `<a>` 而不依赖属性选择器。

use browser_css_engine::Selector;
use browser_dom::{NodeId, Tree};

/// 在 `tree` 中查找所有匹配 `selector_str` 的元素，返回 NodeId 列表
/// （按文档顺序）。
///
/// # Errors
/// 返回 `Err(String)` 当选择器解析失败（不支持的语法等）。
pub fn query_all(tree: &Tree, selector_str: &str) -> Result<Vec<NodeId>, String> {
    let sel = Selector::parse(selector_str)?;
    let mut out = Vec::new();
    tree.traverse(tree.root(), |id, _| {
        if sel.matches(tree, id) {
            out.push(id);
        }
        true
    });
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(html: &str) -> Tree {
        browser_html_parser::parse(html)
    }

    #[test]
    fn query_all_finds_by_tag() {
        let tree = parse("<ul><li>a</li><li>b</li></ul>");
        let ids = query_all(&tree, "li").expect("li selector");
        assert_eq!(ids.len(), 2, "should find both <li>");
    }

    #[test]
    fn query_all_finds_by_descendant() {
        let tree = parse("<table><tr><td>x</td></tr></table>");
        let ids = query_all(&tree, "table tr").expect("descendant selector");
        assert_eq!(ids.len(), 1, "should find <tr> inside <table>");
    }

    #[test]
    fn query_all_finds_by_class() {
        let tree = parse("<div><p class='lead'>hi</p><p>no</p></div>");
        let ids = query_all(&tree, ".lead").expect("class selector");
        assert_eq!(ids.len(), 1);
    }

    #[test]
    fn query_all_empty_when_no_match() {
        let tree = parse("<div>nothing</div>");
        let ids = query_all(&tree, "article").expect("valid selector");
        assert!(ids.is_empty());
    }

    #[test]
    fn query_all_attribute_selector_not_supported_matches_nothing() {
        let tree = parse("<a href='x'>link</a>");
        // css-engine 不支持属性选择器：`a[href]` 会被当成 tag="a[href]"，
        // 解析不报错（lenient），但永远匹配不到任何元素。
        // 这正是 format_links 内部直接遍历 <a> 而不依赖属性选择器的原因。
        let ids = query_all(&tree, "a[href]").expect("lenient parse, no error");
        assert!(ids.is_empty(), "attribute selector matches nothing");
    }
}
