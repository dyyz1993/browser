//! `browser-js-runtime` — JavaScript engine embedding + DOM bridge.
//!
//! M3.1: basic eval
//! M3.2: write-only DOM bridge
//! M3.3: extract & execute `<script>` tags from a DOM tree

#![forbid(unsafe_code)]

pub mod bridge;
mod compat_shim;
pub mod document_shim;
pub mod element_shim;
pub mod engine;
pub use engine::{EngineKind, JsEngine};
pub mod engine_boa;
#[cfg(feature = "quickjs")]
pub mod engine_quickjs;
pub mod esm_loader;
pub mod fetch_shim;
pub mod image_shim;
pub mod navigation_shim;
pub mod navigator_shim;
pub mod runtime;
pub mod screen_shim;
pub mod scripts;
// M-cls.3: CSR 数据兜底（JS 跑空时直接拉 SSR 数据页注入正文）。
pub mod spa_fallback;
pub mod storage_shim;
pub mod window_shim;
pub mod ws_shim;
pub mod xhr_shim;

pub use bridge::{
    current_base_url, current_cookie_jar, drain_captured_network_events, drain_due_timer_callbacks,
    ensure_cookie_jar, install as install_bridge, install_current, install_navigation,
    install_shared, install_shared_with_base, install_storage, is_network_idle, pending_requests,
    pending_timers, resolve_url, CapturedNetworkEvent, SharedTree, TreeGuard,
};
pub use image_shim::install_image;
pub use navigation_shim::install_navigation_globals;
pub use runtime::JsRuntime;
pub use scripts::{
    eval_in_tree, eval_in_tree_engine, execute_scripts, execute_scripts_with_base, extract_scripts,
    run_scripts, run_scripts_with_base, run_scripts_with_base_engine, script_cache_public,
};
pub use spa_fallback::try_csr_fallback;
pub use storage_shim::install_storage_globals;
pub use ws_shim::install_websocket;
pub use xhr_shim::install_xml_http_request;
