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
pub mod html_ser;
pub mod node;
pub mod print;
pub mod tree;

pub use document::Document;
pub use html_ser::serialize_html;
pub use node::NodeData;
pub use print::pretty_print;
pub use tree::{Node, NodeId, Tree};

#[cfg(test)]
mod tests {
    #[test]
    fn ping() {
        assert_eq!(env!("CARGO_PKG_NAME"), "browser-dom");
    }
}
