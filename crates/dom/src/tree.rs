//! Arena-backed DOM tree.
//!
//! Design rationale: see `docs/decisions/0001-arena-vs-refcell.md`.
//!
//! Quick summary:
//! - All nodes live in a single `Vec<Node>`
//! - Nodes are addressed by [`NodeId`](`usize`)
//! - Parent / child links are `NodeId` values, not references
//! - No `Rc` / `RefCell` / runtime borrow panics

use crate::node::NodeData;

/// Index into the arena. Cheap to copy, store, pass across FFI / JS bridge.
pub type NodeId = usize;

/// A single DOM node. The links are arena indices, not pointers.
#[derive(Debug, Clone)]
pub struct Node {
    pub data: NodeData,
    pub parent: Option<NodeId>,
    pub children: Vec<NodeId>,
}

impl Node {
    /// Create a node with the given data and no parent / children.
    #[must_use]
    pub fn new(data: NodeData) -> Self {
        Self {
            data,
            parent: None,
            children: Vec::new(),
        }
    }
}

/// The arena owning every node in a document.
#[derive(Debug, Clone)]
pub struct Tree {
    nodes: Vec<Node>,
}

impl Default for Tree {
    fn default() -> Self {
        Self::new()
    }
}

impl Tree {
    /// Create an empty tree. You usually want to immediately call
    /// [`Tree::with_root`] or [`Tree::insert`] to establish a root.
    #[must_use]
    pub fn new() -> Self {
        Self { nodes: Vec::new() }
    }

    /// Create a tree with the given node as the root (id 0).
    #[must_use]
    pub fn with_root(data: NodeData) -> Self {
        let mut tree = Self::new();
        tree.insert(None, data);
        tree
    }

    /// Returns the id of the root node (always 0 if the tree is non-empty).
    ///
    /// # Panics
    /// Panics if the tree is empty. Callers should ensure a root has been
    /// inserted via [`Tree::with_root`] or [`Tree::insert`] first.
    #[must_use]
    pub fn root(&self) -> NodeId {
        assert!(!self.nodes.is_empty(), "Tree::root on empty tree");
        0
    }

    /// Number of nodes currently stored.
    #[must_use]
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Whether the tree is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Insert a new node under `parent`. If `parent` is `None` the node
    /// is added at the top level (this is only valid for the very first
    /// node, i.e. the document root).
    ///
    /// Returns the new node's id.
    ///
    /// # Panics
    /// Panics if `parent` is `Some(id)` and `id` is out of bounds.
    pub fn insert(&mut self, parent: Option<NodeId>, data: NodeData) -> NodeId {
        let id = self.nodes.len();
        let mut node = Node::new(data);
        node.parent = parent;
        self.nodes.push(node);
        if let Some(pid) = parent {
            self.nodes[pid].children.push(id);
        }
        id
    }

    /// Borrow a node by id.
    ///
    /// # Panics
    /// Panics if `id` is out of bounds. This is intentional — ids are
    /// handed out by this tree itself and should always be valid.
    #[must_use]
    pub fn get(&self, id: NodeId) -> &Node {
        &self.nodes[id]
    }

    /// Mutably borrow a node by id.
    ///
    /// # Panics
    /// Panics if `id` is out of bounds.
    pub fn get_mut(&mut self, id: NodeId) -> &mut Node {
        &mut self.nodes[id]
    }

    /// Convenience accessor for a node's children ids.
    #[must_use]
    pub fn children_of(&self, id: NodeId) -> &[NodeId] {
        &self.nodes[id].children
    }

    /// Borrow the data of a node.
    #[must_use]
    pub fn data(&self, id: NodeId) -> &NodeData {
        &self.nodes[id].data
    }

    /// Pre-order depth-first traversal starting at `id`, calling `f` for
    /// every visited node. Children are visited in source order.
    ///
    /// Stops early if `f` returns `false`.
    pub fn traverse<F>(&self, start: NodeId, mut f: F)
    where
        F: FnMut(NodeId, &Node) -> bool,
    {
        let mut stack: Vec<NodeId> = vec![start];
        while let Some(id) = stack.pop() {
            let node = &self.nodes[id];
            if !f(id, node) {
                return;
            }
            // Push children in reverse so the left-most is popped first.
            for &child in node.children.iter().rev() {
                stack.push(child);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::NodeData;

    /// Builds the canonical test tree:
    /// ```text
    /// Document
    ///   └─ Element(html)
    ///      ├─ Element(head)
    ///      └─ Element(body)
    ///         └─ Element(p)
    ///            └─ Text("hi")
    /// ```
    fn build_simple_tree() -> Tree {
        let mut tree = Tree::with_root(NodeData::Document);
        let root = tree.root();
        let html = tree.insert(
            Some(root),
            NodeData::Element {
                tag: "html".into(),
                attrs: vec![],
            },
        );
        let head = tree.insert(
            Some(html),
            NodeData::Element {
                tag: "head".into(),
                attrs: vec![],
            },
        );
        let body = tree.insert(
            Some(html),
            NodeData::Element {
                tag: "body".into(),
                attrs: vec![],
            },
        );
        let p = tree.insert(
            Some(body),
            NodeData::Element {
                tag: "p".into(),
                attrs: vec![],
            },
        );
        let _text = tree.insert(Some(p), NodeData::Text("hi".into()));
        // Sanity: the unused head variable must still be a valid id.
        assert!(head < tree.len());
        tree
    }

    #[test]
    fn test_build_simple_tree() {
        let tree = build_simple_tree();
        assert_eq!(tree.len(), 6);

        let root = tree.root();
        assert!(matches!(tree.data(root), NodeData::Document));
        assert_eq!(tree.children_of(root), &[1]); // html

        let html = 1;
        assert!(matches!(tree.data(html), NodeData::Element { tag, .. } if tag == "html"));
        assert_eq!(tree.children_of(html), &[2, 3]); // head, body

        let body = 3;
        assert!(matches!(tree.data(body), NodeData::Element { tag, .. } if tag == "body"));
        assert_eq!(tree.children_of(body), &[4]); // p

        let p = 4;
        assert!(matches!(tree.data(p), NodeData::Element { tag, .. } if tag == "p"));
        assert_eq!(tree.children_of(p), &[5]); // text

        // The text node has the right content and no children.
        let text = 5;
        assert_eq!(tree.data(text).as_text(), Some("hi"));
        assert!(tree.children_of(text).is_empty());

        // Parent links are correct.
        assert_eq!(tree.get(html).parent, Some(root));
        assert_eq!(tree.get(body).parent, Some(html));
        assert_eq!(tree.get(text).parent, Some(p));
    }

    #[test]
    fn test_attrs_roundtrip() {
        let mut tree = Tree::with_root(NodeData::Document);
        let root = tree.root();
        let a = tree.insert(
            Some(root),
            NodeData::Element {
                tag: "a".into(),
                attrs: vec![("href".into(), "x".into()), ("class".into(), "c".into())],
            },
        );
        // Read back via `data()` and check order is preserved.
        let (tag, attrs) = tree.data(a).as_element().expect("must be element");
        assert_eq!(tag, "a");
        assert_eq!(attrs.len(), 2);
        assert_eq!(attrs[0].0, "href");
        assert_eq!(attrs[0].1, "x");
        assert_eq!(attrs[1].0, "class");
        assert_eq!(attrs[1].1, "c");
    }

    #[test]
    fn test_traverse_pre_order() {
        let tree = build_simple_tree();
        let mut visited = Vec::new();
        tree.traverse(tree.root(), |id, _| {
            visited.push(id);
            true
        });
        // Pre-order: root, html, head, body, p, text.
        assert_eq!(visited, vec![0, 1, 2, 3, 4, 5]);
    }

    #[test]
    fn test_traverse_can_short_circuit() {
        let tree = build_simple_tree();
        let mut count = 0;
        tree.traverse(tree.root(), |_, _| {
            count += 1;
            count < 3
        });
        assert_eq!(count, 3);
    }

    #[test]
    fn test_empty_tree_panics_on_root() {
        let tree = Tree::new();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = tree.root();
        }));
        assert!(result.is_err(), "Tree::root on empty tree should panic");
    }

    #[test]
    fn test_get_mut_modifies_data() {
        let mut tree = Tree::with_root(NodeData::Text("hello".into()));
        let root = tree.root();
        if let NodeData::Text(s) = &mut tree.get_mut(root).data {
            s.push_str(" world");
        }
        assert_eq!(tree.data(root).as_text(), Some("hello world"));
    }
}
