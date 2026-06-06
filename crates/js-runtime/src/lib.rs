//! `browser-js-runtime` — JavaScript engine embedding + DOM bridge.
//!
//! M3.1 scope: basic eval.
//! M3.2 scope: write-only DOM bridge (`__setBody`, `__appendBody`,
//!             `__setTitle`, `__log`).

#![forbid(unsafe_code)]

pub mod bridge;
pub mod runtime;

pub use bridge::{install as install_bridge, install_current, SharedTree, TreeGuard};
pub use runtime::JsRuntime;
