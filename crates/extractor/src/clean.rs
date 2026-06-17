//! 噪声过滤（`--only-main-content`，默认 true）。
//!
//! 借鉴 Firecrawl `EXCLUDE_NON_MAIN_TAGS`（42 选择器）+ `FORCE_INCLUDE`
//! 保护机制。Firecrawl 用结构性排除而非评分算法，简单可控，符合本项目
//! 「自研优先、复杂度门槛」原则。
//!
//! ## 实现约束
//! `Tree.nodes` 是私有字段，extractor 不能直接改树结构。所以采用
//! 「黑名单 NodeId 集合」方式：计算应排除的节点，各 format 遍历时跳过。
//!
//! ## 规则
//! 1. 无条件排除：`script`/`style`/`noscript`/`head`/`meta`/`iframe`。
//! 2. only_main_content=true 时排除：Firecrawl 42 选择器（nav/footer/ad 等）。
//! 3. force-include 保护：被排除节点若含 `#main`/`article`/`[role=main]` 后代，
//!    则保留该节点（防止误删主内容容器）。

use std::collections::HashSet;

use browser_dom::{NodeId, Tree};

use crate::selector::query_all;

/// Firecrawl EXCLUDE_NON_MAIN_TAGS（42 选择器），only_main_content=true 时生效。
/// 直接搬运已验证的噪声选择器集。来源：Firecrawl `apps/api/native/src/html.rs:334-377`。
const EXCLUDE_NON_MAIN_TAGS: &[&str] = &[
    // 结构性
    "header",
    "footer",
    "nav",
    "aside",
    // header/footer 类
    ".header",
    ".top",
    ".navbar",
    "#header",
    ".footer",
    ".bottom",
    "#footer",
    // 侧栏
    ".sidebar",
    ".side",
    ".aside",
    "#sidebar",
    // 弹窗
    ".modal",
    ".popup",
    "#modal",
    ".overlay",
    // 广告
    ".ad",
    ".ads",
    ".advert",
    "#ad",
    // 语言选择
    ".lang-selector",
    ".language",
    "#language-selector",
    // 社交
    ".social",
    ".social-media",
    ".social-links",
    "#social",
    // 导航
    ".menu",
    ".navigation",
    "#nav",
    // 面包屑
    ".breadcrumbs",
    "#breadcrumbs",
    // 分享
    ".share",
    "#share",
    // 小部件
    ".widget",
    "#widget",
    // cookie
    ".cookie",
    "#cookie",
];

/// 强制保留的选择器（即使祖先被排除）。保护主内容容器。
/// Firecrawl FORCE_INCLUDE_MAIN_TAGS + 通用 `article`/`[role=main]` 等价物。
/// 注：css-engine 不支持属性选择器，`[role=main]` 用 `#main`/`article` 近似。
const FORCE_INCLUDE_TAGS: &[&str] = &["#main", "article", "main"];

/// 无条件移除的 tag（不论 only_main_content）。
const ALWAYS_REMOVE_TAGS: &[&str] = &[
    "script", "style", "noscript", "head", "meta", "iframe", "svg",
];

/// 计算应排除的 NodeId 集合。
///
/// 返回的集合包含所有应被跳过的节点（含其全部后代——遍历方在命中时
/// 应整棵子树跳过）。
///
/// # Errors
/// 选择器解析失败时返回 `Err(String)`。
pub fn excluded_nodes(tree: &Tree, only_main_content: bool) -> Result<HashSet<NodeId>, String> {
    let mut excluded: HashSet<NodeId> = HashSet::new();

    // 1. 无条件排除的 tag。
    for &tag in ALWAYS_REMOVE_TAGS {
        for id in by_tag(tree, tag) {
            mark_subtree(tree, id, &mut excluded);
        }
    }

    if only_main_content {
        // 2. 先找 force-include 节点，记录它们的祖先（这些祖先即使匹配排除也不删）。
        let mut protected: HashSet<NodeId> = HashSet::new();
        for &sel in FORCE_INCLUDE_TAGS {
            for id in query_all(tree, sel)? {
                protect_ancestors(tree, id, &mut protected);
            }
        }
        // 3. 排除匹配 EXCLUDE_NON_MAIN_TAGS 的节点（除非受保护）。
        for &sel in EXCLUDE_NON_MAIN_TAGS {
            for id in query_all(tree, sel)? {
                if protected.contains(&id) {
                    continue; // force-include 保护
                }
                mark_subtree(tree, id, &mut excluded);
            }
        }
    }

    Ok(excluded)
}

/// 判断 `id` 是否被排除（各 format 遍历时调用）。
#[must_use]
pub fn is_excluded(id: NodeId, excluded: &HashSet<NodeId>) -> bool {
    excluded.contains(&id)
}

/// 按 tag 名查找所有元素（大小写不敏感）。
fn by_tag(tree: &Tree, tag: &str) -> Vec<NodeId> {
    let mut out = Vec::new();
    tree.traverse(tree.root(), |id, _| {
        if let browser_dom::NodeData::Element { tag: t, .. } = &tree.get(id).data {
            if t.eq_ignore_ascii_case(tag) {
                out.push(id);
            }
        }
        true
    });
    out
}

/// 把 `id` 及其全部后代标记为排除。
fn mark_subtree(tree: &Tree, id: NodeId, excluded: &mut HashSet<NodeId>) {
    excluded.insert(id);
    for &child in tree.children_of(id) {
        mark_subtree(tree, child, excluded);
    }
}

/// 把 `id` 的所有祖先（不含自身）标记为受保护。
fn protect_ancestors(tree: &Tree, id: NodeId, protected: &mut HashSet<NodeId>) {
    let mut current = tree.get(id).parent;
    while let Some(p) = current {
        protected.insert(p);
        current = tree.get(p).parent;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(html: &str) -> Tree {
        browser_html_parser::parse(html)
    }

    #[test]
    fn always_removes_script_style_head() {
        let tree = parse("<head><style>x</style></head><body><script>js</script><p>ok</p></body>");
        let excluded = excluded_nodes(&tree, false).expect("no main content filter");
        // script/style/head 被排除
        assert!(contains_tag(&tree, &excluded, "script"));
        assert!(contains_tag(&tree, &excluded, "style"));
        assert!(contains_tag(&tree, &excluded, "head"));
        // body 内容保留
        assert!(!contains_tag(&tree, &excluded, "p"));
    }

    #[test]
    fn only_main_content_removes_nav_footer() {
        let tree = parse(
            "<body>\
             <nav><a>link</a></nav>\
             <article><p>main content</p></article>\
             <footer>copyright</footer>\
             </body>",
        );
        let excluded = excluded_nodes(&tree, true).expect("main content filter");
        assert!(contains_tag(&tree, &excluded, "nav"));
        assert!(contains_tag(&tree, &excluded, "footer"));
        // article 主内容保留
        assert!(!contains_tag(&tree, &excluded, "article"));
        assert!(!contains_tag(&tree, &excluded, "p"));
    }

    #[test]
    fn force_include_protects_sidebar_container() {
        // article 放在 .sidebar 容器里：通常 .sidebar 会被排除，但因为有
        // force-include 的 article 后代，整个 .sidebar（含 article）应保留。
        let tree = parse(
            "<body>\
             <div class='sidebar'>\
             <article><p>main</p></article>\
             </div>\
             </body>",
        );
        let excluded = excluded_nodes(&tree, true).expect("force include");
        // 找到 .sidebar div，确认它未被排除（受 force-include 保护）。
        let sidebar_excluded = find_first_node(&tree, |tag, attrs| {
            tag == "div" && attrs.iter().any(|(k, v)| k == "class" && v == "sidebar")
        })
        .map(|id| excluded.contains(&id))
        .unwrap_or(true);
        assert!(
            !sidebar_excluded,
            ".sidebar should be protected by force-include"
        );
        // article 保留
        assert!(!contains_tag(&tree, &excluded, "article"));
    }

    #[test]
    fn excluded_set_includes_descendants() {
        let tree = parse("<nav><ul><li><a>deep</a></li></ul></nav>");
        let excluded = excluded_nodes(&tree, true).expect("descendants");
        // nav 的所有后代（ul/li/a）都应被排除
        assert!(contains_tag(&tree, &excluded, "nav"));
        assert!(contains_tag(&tree, &excluded, "ul"));
        assert!(contains_tag(&tree, &excluded, "li"));
        assert!(contains_tag(&tree, &excluded, "a"));
    }

    #[test]
    fn no_main_content_keeps_nav_when_disabled() {
        let tree = parse("<nav><a>link</a></nav><p>content</p>");
        let excluded = excluded_nodes(&tree, false).expect("disabled");
        // only_main_content=false → nav 不被排除
        assert!(!contains_tag(&tree, &excluded, "nav"));
    }

    /// 辅助：excluded 集合里是否存在某 tag 的节点。
    fn contains_tag(tree: &Tree, excluded: &HashSet<NodeId>, tag: &str) -> bool {
        let mut found = false;
        tree.traverse(tree.root(), |id, _| {
            if excluded.contains(&id) {
                if let browser_dom::NodeData::Element { tag: t, .. } = &tree.get(id).data {
                    if t.eq_ignore_ascii_case(tag) {
                        found = true;
                        return false;
                    }
                }
            }
            true
        });
        found
    }

    /// 辅助：找第一个 tag+attrs 匹配的节点 NodeId。
    fn find_first_node<F>(tree: &Tree, mut predicate: F) -> Option<NodeId>
    where
        F: FnMut(&str, &[(String, String)]) -> bool,
    {
        let mut hit = None;
        tree.traverse(tree.root(), |id, _| {
            if let browser_dom::NodeData::Element { tag, attrs } = &tree.get(id).data {
                if predicate(tag, attrs) {
                    hit = Some(id);
                    return false;
                }
            }
            true
        });
        hit
    }
}
