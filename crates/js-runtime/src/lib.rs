//! `browser-js-runtime` — JavaScript engine embedding + DOM bridge.
//!
//! M3.1: basic eval
//! M3.2: write-only DOM bridge
//! M3.3: extract & execute `<script>` tags from a DOM tree

#![forbid(unsafe_code)]

pub mod bridge;
pub mod runtime;
pub mod scripts;
pub mod storage_shim;

pub use bridge::{
    current_base_url, install as install_bridge, install_current, install_shared,
    install_shared_with_base, install_storage, resolve_url, SharedTree, TreeGuard,
};
pub use runtime::JsRuntime;
pub use scripts::{
    execute_scripts, execute_scripts_with_base, extract_scripts, run_scripts, run_scripts_with_base,
};
pub use storage_shim::install_storage_globals;
