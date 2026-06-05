//! `browser-dom` — arena-backed DOM data structures.
//!
//! See `docs/decisions/0001-arena-vs-refcell.md` for the design rationale.
//!
//! Quick tour:
//! - [`NodeData`] — what kind of node (Element, Text, ...)
//! - [`Tree`] — arena of nodes, indexed by [`NodeId`]
//! - [`Document`] — wrapper that adds document metadata (URL, ...)

#![forbid(unsafe_code)]

pub mod document;
pub mod node;
pub mod tree;

pub use document::Document;
pub use node::NodeData;
pub use tree::{Node, NodeId, Tree};

#[cfg(test)]
mod tests {
    #[test]
    fn ping() {
        assert_eq!(env!("CARGO_PKG_NAME"), "browser-dom");
    }
}
