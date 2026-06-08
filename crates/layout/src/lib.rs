//! `browser-layout` — block / inline / anonymous / flex / grid layout engine.

#![forbid(unsafe_code)]

pub mod block;
pub mod boxes;
pub mod construct;
pub mod flex;
pub mod grid;
pub mod inline;

pub use block::{layout, LayoutConfig};
pub use boxes::{
    AlignItems, BoxType, Dimensions, FlexDirection, FlexProps, FlexWrap, GridProps, GridTrack,
    JustifyContent, LayoutBox, LayoutTree,
};
pub use construct::construct_layout_tree;
pub use inline::layout_inline_run;
