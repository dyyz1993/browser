//! `browser-js-runtime` — JavaScript engine embedding + DOM bridge.
//!
//! M3.1: basic eval
//! M3.2: write-only DOM bridge
//! M3.3: extract & execute `<script>` tags from a DOM tree

#![forbid(unsafe_code)]

pub mod bridge;
pub mod runtime;
pub mod scripts;

pub use bridge::{
    install as install_bridge, install_current, install_shared, SharedTree, TreeGuard,
};
pub use runtime::JsRuntime;
pub use scripts::{execute_scripts, extract_scripts, run_scripts};
