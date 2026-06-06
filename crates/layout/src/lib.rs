//! `browser-layout` — block / inline / anonymous layout engine.
//!
//! M2.4 scope: tree construction.
//! M2.5 scope: block layout (dimensions assignment).

#![forbid(unsafe_code)]

pub mod block;
pub mod boxes;
pub mod construct;

pub use block::{layout, LayoutConfig};
pub use boxes::{BoxType, Dimensions, LayoutBox, LayoutTree};
pub use construct::construct_layout_tree;
