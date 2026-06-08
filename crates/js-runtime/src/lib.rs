//! `browser-js-runtime` — JavaScript engine embedding + DOM bridge.
//!
//! M3.1: basic eval
//! M3.2: write-only DOM bridge
//! M3.3: extract & execute `<script>` tags from a DOM tree

#![forbid(unsafe_code)]

pub mod bridge;
pub mod document_shim;
pub mod element_shim;
pub mod fetch_shim;
pub mod navigation_shim;
pub mod navigator_shim;
pub mod runtime;
pub mod screen_shim;
pub mod scripts;
pub mod storage_shim;
pub mod window_shim;
pub mod ws_shim;
pub mod xhr_shim;

pub use bridge::{
    current_base_url, current_cookie_jar, drain_due_timer_callbacks, ensure_cookie_jar,
    install as install_bridge, install_current, install_navigation, install_shared,
    install_shared_with_base, install_storage, is_network_idle, pending_requests, pending_timers,
    resolve_url, SharedTree, TreeGuard,
};
pub use navigation_shim::install_navigation_globals;
pub use runtime::JsRuntime;
pub use scripts::{
    execute_scripts, execute_scripts_with_base, extract_scripts, run_scripts, run_scripts_with_base,
};
pub use storage_shim::install_storage_globals;
pub use ws_shim::install_websocket;
pub use xhr_shim::install_xml_http_request;
