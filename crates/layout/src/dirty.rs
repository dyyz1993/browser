//! M10.2: dirty tracking（增量渲染的脏节点追踪）

use std::collections::HashSet;
use browser_dom::NodeId;

/// 脏节点追踪器：记录哪些 DOM 节点需要重新布局
pub struct DirtyTracker {
    dirty: HashSet<NodeId>,
}

impl DirtyTracker {
    pub fn new() -> Self {
        Self {
            dirty: HashSet::new(),
        }
    }

    /// 标记节点为脏（布局变化时调用）
    pub fn mark(&mut self, id: NodeId) {
        self.dirty.insert(id);
    }

    /// 标记子树为脏（例如：插入/删除节点后）
    pub fn mark_subtree(&mut self, tree: &browser_dom::Tree, root_id: NodeId) {
        let mut stack = vec![root_id];
        while let Some(id) = stack.pop() {
            self.dirty.insert(id);
            for &child in tree.children_of(id) {
                stack.push(child);
            }
        }
    }

    /// 检查节点是否为脏
    pub fn is_dirty(&self, id: NodeId) -> bool {
        self.dirty.contains(&id)
    }

    /// 清空脏标记（布局完成后调用）
    pub fn clear(&mut self) {
        self.dirty.clear();
    }

    /// 返回脏节点数量（用于测试）
    pub fn len(&self) -> usize {
        self.dirty.len()
    }
}

impl Default for DirtyTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mark_single_node() {
        let mut tracker = DirtyTracker::new();
        let id = NodeId(42);
        tracker.mark(id);
        assert!(tracker.is_dirty(id));
        assert_eq!(tracker.len(), 1);
    }

    #[test]
    fn mark_subtree_marks_all_descendants() {
        let mut tracker = DirtyTracker::new();
        let tree = browser_html_parser::parse("<div><p>hello<span>world</span></p></div>");
        let div = find_first_element(&tree, "div").unwrap();
        tracker.mark_subtree(&tree, div);
        // div + p + span 都应该被标记
        assert!(tracker.is_dirty(div));
        let p = tree.children_of(div).next().unwrap();
        assert!(tracker.is_dirty(p));
        let span = tree.children_of(p).next().unwrap();
        assert!(tracker.is_dirty(span));
        assert_eq!(tracker.len(), 3);
    }

    #[test]
    fn clear_resets_dirty() {
        let mut tracker = DirtyTracker::new();
        tracker.mark(NodeId(1));
        tracker.clear();
        assert_eq!(tracker.len(), 0);
    }
}
