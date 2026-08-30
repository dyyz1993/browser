//! `browser-js-runtime` — JavaScript engine embedding + DOM bridge.
//!
//! M3.1: basic eval
//! M3.2: write-only DOM bridge
//! M3.3: extract & execute `<script>` tags from a DOM tree

#![forbid(unsafe_code)]

pub mod bridge;
// M71.1: boa 专属模块——仅在 --features boa 时编译（默认 quickjs）。
#[cfg(feature = "boa")]
mod compat_shim;
#[cfg(feature = "boa")]
pub mod document_shim;
#[cfg(feature = "boa")]
pub mod element_shim;
pub mod engine;
pub use engine::{EngineKind, JsEngine};
#[cfg(feature = "boa")]
pub mod engine_boa;
#[cfg(feature = "quickjs")]
pub mod engine_quickjs;
#[cfg(feature = "boa")]
pub mod esm_loader;
#[cfg(feature = "boa")]
pub mod fetch_shim;
#[cfg(feature = "boa")]
pub mod image_shim;
#[cfg(feature = "boa")]
pub mod navigation_shim;
#[cfg(feature = "boa")]
pub mod navigator_shim;
#[cfg(feature = "boa")]
pub mod runtime;
#[cfg(feature = "boa")]
pub mod screen_shim;
pub mod scripts;
// M-cls.3: CSR 数据兜底（JS 跑空时直接拉 SSR 数据页注入正文）。
pub mod spa_fallback;
#[cfg(feature = "boa")]
pub mod storage_shim;
#[cfg(feature = "boa")]
pub mod window_shim;
#[cfg(feature = "boa")]
pub mod ws_shim;
#[cfg(feature = "boa")]
pub mod xhr_shim;

pub use bridge::{
    capture_console_event, capture_js_error, current_base_url, current_cookie_jar,
    drain_captured_console_events, drain_captured_js_errors, drain_captured_network_events,
    ensure_cookie_jar, install_current, install_navigation, install_shared,
    install_shared_with_base, install_storage, is_network_idle, pending_requests, pending_timers,
    resolve_url, take_focus_node, CapturedConsoleEvent, CapturedJsError, CapturedNetworkEvent,
    SharedTree, TreeGuard,
};
// M71.1: bridge::install 是 boa 专属（注册所有 NativeFn bridge 函数）。
#[cfg(feature = "boa")]
pub use bridge::{drain_due_timer_callbacks, install as install_bridge};
#[cfg(feature = "boa")]
pub use image_shim::install_image;
#[cfg(feature = "boa")]
pub use navigation_shim::install_navigation_globals;
#[cfg(feature = "boa")]
pub use runtime::JsRuntime;
pub use scripts::{
    eval_in_tree, eval_in_tree_engine, eval_in_tree_engine_await, extract_scripts, run_scripts,
    run_scripts_with_base, run_scripts_with_base_engine, run_scripts_with_post_exprs,
    script_cache_public,
};
// M71.1: execute_scripts* 是 boa 专属（吃 boa Context）。
#[cfg(feature = "boa")]
pub use scripts::{execute_scripts, execute_scripts_with_base};
pub use spa_fallback::try_csr_fallback;
#[cfg(feature = "boa")]
pub use storage_shim::install_storage_globals;
#[cfg(feature = "boa")]
pub use ws_shim::install_websocket;
#[cfg(feature = "boa")]
pub use xhr_shim::install_xml_http_request;
