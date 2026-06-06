//! M10.1: layout 缓存（避免重复计算）

use std::collections::HashMap;
use crate::{LayoutTree, construct_layout_tree};
use browser_css_engine::Declaration;
use browser_dom::NodeId;

/// 简单的 layout 缓存：hash(DOM树 + CSS) → LayoutTree
///
/// M10.1: 使用 NodeId 的树结构 hash 作为 key。完整 DOM 树 hash
/// 计算较复杂，简化为根 NodeId + styles hash 的组合。
pub struct LayoutCache {
    cache: HashMap<String, LayoutTree>,
}

impl LayoutCache {
    pub fn new() -> Self {
        Self {
            cache: HashMap::new(),
        }
    }

    /// 尝试从缓存获取 layout。如果没有，则计算并存入。
    pub fn get_or_compute(
        &mut self,
        tree: &browser_dom::Tree,
        styles: &HashMap<NodeId, Vec<Declaration>>,
    ) -> &LayoutTree {
        // M10.1 简化 key：根 NodeId + styles 数量（真实场景需完整 hash）
        let key = format!("root:{:?}:styles:{}", tree.root(), styles.len());
        self.cache.entry(key).or_insert_with(|| construct_layout_tree(tree, styles))
    }

    /// 清空缓存（DOM 变化时调用）
    pub fn clear(&mut self) {
        self.cache.clear();
    }

    /// 返回缓存条目数（用于测试）
    pub fn len(&self) -> usize {
        self.cache.len()
    }
}

impl Default for LayoutCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_returns_same_instance() {
        let mut cache = LayoutCache::new();
        let tree = browser_html_parser::parse("<div><p>hello</p></div>");
        let styles = HashMap::new();
        let first = cache.get_or_compute(&tree, &styles);
        let second = cache.get_or_compute(&tree, &styles);
        // 同一个引用（缓存命中）
        assert!(std::ptr::eq(first, second));
    }

    #[test]
    fn cache_grows_on_miss() {
        let mut cache = LayoutCache::new();
        let tree1 = browser_html_parser::parse("<div>one</div>");
        let tree2 = browser_html_parser::parse("<div>two</div>");
        let styles = HashMap::new();
        cache.get_or_compute(&tree1, &styles);
        cache.get_or_compute(&tree2, &styles);
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn clear_resets_cache() {
        let mut cache = LayoutCache::new();
        let tree = browser_html_parser::parse("<div>hello</div>");
        let styles = HashMap::new();
        cache.get_or_compute(&tree, &styles);
        cache.clear();
        assert_eq!(cache.len(), 0);
    }
}
