//! `Document` — the top-level owner of a DOM [`Tree`] plus
//! document-level metadata (URL, content-type, etc).
//!
//! For M1.2 we keep this minimal — just a wrapper around [`Tree`]
//! plus the document URL. The full WHATWG `Document` interface
//! (cookies, fonts, images, etc.) lands in later milestones.

use crate::tree::Tree;

#[derive(Debug, Clone)]
pub struct Document {
    tree: Tree,
    url: Option<String>,
}

impl Default for Document {
    fn default() -> Self {
        Self::new()
    }
}

impl Document {
    /// Create an empty document with no URL.
    #[must_use]
    pub fn new() -> Self {
        Self {
            tree: Tree::new(),
            url: None,
        }
    }

    /// Create a document from an existing [`Tree`].
    #[must_use]
    pub fn from_tree(tree: Tree) -> Self {
        Self { tree, url: None }
    }

    /// Attach a URL to this document.
    #[must_use]
    pub fn with_url(mut self, url: impl Into<String>) -> Self {
        self.url = Some(url.into());
        self
    }

    /// The document URL, if any.
    #[must_use]
    pub fn url(&self) -> Option<&str> {
        self.url.as_deref()
    }

    /// Borrow the underlying tree.
    #[must_use]
    pub fn tree(&self) -> &Tree {
        &self.tree
    }

    /// Mutably borrow the underlying tree.
    pub fn tree_mut(&mut self) -> &mut Tree {
        &mut self.tree
    }

    /// Consume the document and return the underlying tree.
    #[must_use]
    pub fn into_tree(self) -> Tree {
        self.tree
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::NodeData;
    use crate::tree::Tree;

    #[test]
    fn document_holds_url() {
        let doc = Document::new().with_url("https://example.com");
        assert_eq!(doc.url(), Some("https://example.com"));
    }

    #[test]
    fn document_wraps_tree() {
        let mut tree = Tree::with_root(NodeData::Document);
        let root = tree.root();
        tree.insert(Some(root), NodeData::Text("hi".into()));
        let doc = Document::from_tree(tree).with_url("https://x.test");
        assert_eq!(doc.tree().len(), 2);
        assert_eq!(doc.url(), Some("https://x.test"));
    }
}
