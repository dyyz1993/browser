//! `browser-layout` — block / inline / anonymous layout engine.
//!
//! M2.4 scope: tree construction.

#![forbid(unsafe_code)]

pub mod boxes;
pub mod construct;

pub use boxes::{BoxType, Dimensions, LayoutBox, LayoutTree};
pub use construct::construct_layout_tree;
