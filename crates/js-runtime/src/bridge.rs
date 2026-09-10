//! JS ↔ DOM bridge via simplified global functions.
//!
//! M3.2 strategy: expose a small set of JS-callable globals that mutate
//! the DOM. Write-only by design — boa 0.20's JsString construction
//! is awkward from Rust `String`, so we sidestep it by not returning
//! strings to JS.
//!
//! Globals provided:
//! - `__setBody(html: string)` — replace `<body>`'s text content
//! - `__appendBody(text: string)` — append a text node to `<body>`
//! - `__setTitle(s: string)` — set `<title>` element's text
//! - `__log(s)` — print to Rust stderr (debugging helper)
//!
//! ## Implementation note
//! boa's closure-based `NativeFunction` variants require captures to
//! implement `Trace`, which our `Tree` cannot. We use function pointers
//! + a thread-local `Rc<RefCell<Tree>>` slot instead.

use std::cell::RefCell;
use std::rc::Rc;

#[cfg(feature = "boa")]
use boa_engine::{object::JsObject, Context, JsArgs, JsResult, JsValue, NativeFunction};
use browser_cookie::CookieHandle;
use browser_dom::{NodeData, NodeId, Tree};
#[allow(unused_imports)]
use browser_eventloop::{TimerId, TimerWheel};
use browser_navigation::NavigationHandle;
use browser_storage::StorageHandle;

/// A shared, mutate-able handle to the DOM tree that JS sees.
pub type SharedTree = Rc<RefCell<Tree>>;

/// M62: setInterval entry: (callback, delay_ms, trigger_count)。
#[cfg(feature = "boa")]
type IntervalEntry = (JsObject, u64, u32);

thread_local! {
    static CURRENT_TREE: RefCell<Option<SharedTree>> = const { RefCell::new(None) };
    static BASE_URL: RefCell<Option<String>> = const { RefCell::new(None) };
    // M13.2: localStorage / sessionStorage backend.
    static CURRENT_STORAGE: RefCell<Option<StorageHandle>> = const { RefCell::new(None) };
    // M14.2: history / location backend.
    static CURRENT_NAV: RefCell<Option<NavigationHandle>> = const { RefCell::new(None) };
    // M15.3: cookie jar backend.
    static CURRENT_COOKIE: RefCell<Option<CookieHandle>> = const { RefCell::new(None) };
    // M71.1: 以下三个是 boa 专属 timer 后端（QuickJS 有独立 timer 实现，不碰这些）。
    // 因 thread_local! 内不能 cfg 单个 static，放独立 thread_local! 门控。
    // M16.2: setTimeout 后端。wheel 存时间+id，callbacks 存 JsObject（boa GC 保活）。
    static TIMER_WHEEL: RefCell<Option<TimerWheel>> = const { RefCell::new(None) };
    // M17.1: XMLHttpRequest 后端。id → 状态（method/url/response_text）。
    // 爬虫场景：responseText 存 String，onload 由 JS shim 用 setTimeout(0) 触发
    // （复用 M16 event loop），responseText 通过 __xhrGetResponseText 读。
    static XHR_INSTANCES: RefCell<Option<std::collections::HashMap<u64, XhrState>>> =
        const { RefCell::new(None) };
    static XHR_NEXT_ID: std::cell::Cell<u64> = const { std::cell::Cell::new(1) };
    // M18.1: in-flight 网络请求计数器（fetch_sync / xhr_send 进入+1，退出-1）。
    // networkidle = pending_timers==0 && pending_requests==0。
    static PENDING_REQUESTS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    // M23.5: WebSocket 多连接管理器（后台线程 + 命令/事件队列）。
    // 与 XHR/fetch 同线程同步不同：WS 是长连接异步，每个 connect spawn 一个
    // OS 线程 recv，事件走 WsManager.drain_events() → pump_event_loop dispatch。
    static WS_MANAGER: RefCell<Option<browser_ws::WsManager>> = const { RefCell::new(None) };
}

// M71.1: boa 专属 timer 后端（QuickJS 有独立 timer 实现）。单独 thread_local! 门控。
#[cfg(feature = "boa")]
thread_local! {
    static TIMER_CALLBACKS: RefCell<Option<std::collections::HashMap<TimerId, JsObject>>> =
        const { RefCell::new(None) };
    // M62: setInterval 后端。存 (callback, delay_ms, count)，drain 时触发后重新 schedule。
    static INTERVAL_INFO: RefCell<Option<std::collections::HashMap<TimerId, IntervalEntry>>> =
        const { RefCell::new(None) };
}

/// M74: A captured console event (log/warn/error/info/debug).
#[derive(Debug, Clone)]
pub struct CapturedConsoleEvent {
    pub level: String,
    pub text: String,
}

/// M74: A captured JS runtime error.
#[derive(Debug, Clone)]
pub struct CapturedJsError {
    pub message: String,
    pub stack: Option<String>,
}

thread_local! {
    /// M74: Captured console events.
    static CAPTURED_CONSOLE: RefCell<Vec<CapturedConsoleEvent>> = const { RefCell::new(Vec::new()) };
    /// M74: Captured JS errors.
    static CAPTURED_JS_ERRORS: RefCell<Vec<CapturedJsError>> = const { RefCell::new(Vec::new()) };
}

/// M74: Drain captured console events.
#[must_use]
pub fn drain_captured_console_events() -> Vec<CapturedConsoleEvent> {
    CAPTURED_CONSOLE.with(|slot| slot.borrow_mut().drain(..).collect())
}

/// M74: Drain captured JS errors.
#[must_use]
pub fn drain_captured_js_errors() -> Vec<CapturedJsError> {
    CAPTURED_JS_ERRORS.with(|slot| slot.borrow_mut().drain(..).collect())
}

/// M74: Record a console event (called from QuickJS shim).
pub fn capture_console_event(level: &str, text: &str) {
    CAPTURED_CONSOLE.with(|slot| {
        slot.borrow_mut().push(CapturedConsoleEvent {
            level: level.to_string(),
            text: text.to_string(),
        });
    });
}

/// M74: Record a JS error (called from QuickJS shim).
pub fn capture_js_error(message: &str, stack: Option<&str>) {
    CAPTURED_JS_ERRORS.with(|slot| {
        slot.borrow_mut().push(CapturedJsError {
            message: message.to_string(),
            stack: stack.map(|s| s.to_string()),
        });
    });
}

/// M70.4: A captured network event from JS fetch/XHR (Send-safe).
/// Collected into a thread_local queue during script execution, drained
/// by CDP navigate to emit `Network.*` events.
#[derive(Debug, Clone)]
pub struct CapturedNetworkEvent {
    pub url: String,
    pub method: String,
    pub status: u16,
    pub mime_type: String,
    pub body_size: usize,
}

thread_local! {
    /// M70.4: JS fetch/XHR 捕获的网络事件队列。navigate 的 JS 执行结束后 drain。
    static CAPTURED_NETWORK: RefCell<Vec<CapturedNetworkEvent>> = const { RefCell::new(Vec::new()) };
}

/// M70.4: Drain and clear the captured network events (called by CDP navigate
/// after JS execution). Returns owned Vec (Send-safe for crossing spawn_blocking).
#[must_use]
pub fn drain_captured_network_events() -> Vec<CapturedNetworkEvent> {
    CAPTURED_NETWORK.with(|slot| slot.borrow_mut().drain(..).collect())
}

// M82: 全局 JS 执行墙钟 deadline（P0-1 挂死修复）。
//
// 背景：`browser fetch` 对常驻事件循环站点（juejin：WebSocket + setInterval
// + 百级外链脚本）曾挂 4 分钟+。根因不是单个 pump 无界（pump 有 2s 上限），
// 而是「外链预取 180s + 模块图 BFS + 串行同步 fetch」各阶段叠加无全局预算。
//
// 设计：CLI 在 JS 阶段开始前 `set_js_deadline(Some(budget))`，管线各阶段
// （fetch_external_script / fetch_sync_with_method / 脚本遍历 / 事件循环 /
// QuickJS interrupt handler）协同检查。选进程级 static 而非 thread_local：
// 预取 worker 线程也要看到同一 deadline。超时后返回"当前已渲染内容"，
// 由 CLI 层打 warning——进程绝不因单页挂死。
static JS_DEADLINE: std::sync::OnceLock<std::sync::Mutex<Option<std::time::Instant>>> =
    std::sync::OnceLock::new();

fn js_deadline_slot() -> &'static std::sync::Mutex<Option<std::time::Instant>> {
    JS_DEADLINE.get_or_init(|| std::sync::Mutex::new(None))
}

/// M82: 设置全局 JS deadline（now + budget）。`None` 清除（无上限）。
/// 零预算视为清除（测试/无限等待场景）。
pub fn set_js_deadline(budget: Option<std::time::Duration>) {
    let deadline = budget
        .filter(|d| !d.is_zero())
        .map(|d| std::time::Instant::now() + d);
    if let Ok(mut slot) = js_deadline_slot().lock() {
        *slot = deadline;
    }
}

/// M82: deadline 是否已到（未设置 = false，永不说超时）。
#[must_use]
pub fn js_deadline_exceeded() -> bool {
    js_deadline_slot()
        .lock()
        .ok()
        .and_then(|slot| *slot)
        .is_some_and(|d| std::time::Instant::now() >= d)
}

/// M82: 距 deadline 剩余时间。未设置 = `None`（无限制）；已超 = `Some(ZERO)`。
/// 各网络调用用它把自身 timeout 收紧到 `min(默认, 剩余)`，保证超时漂移有界。
#[must_use]
pub fn js_deadline_remaining() -> Option<std::time::Duration> {
    js_deadline_slot()
        .lock()
        .ok()
        .and_then(|slot| *slot)
        .map(|d| d.saturating_duration_since(std::time::Instant::now()))
}

// M81(B1): 最近一次 JS 侧 `Element.prototype.focus()` 的元素 NodeId。
// CDP `Input.dispatchKeyEvent` 的 activeElement 同步用：focus shim 经
// `__psReportFocus` 原生桥上报；cdp 在 `Runtime.evaluate` / `callFunctionOn`
// 之后 drain 进 `PageState.focused_node`（eval 与 drain 同一 OS 线程，
// thread_local 可见）。只存 usize，不存 JS 对象引用（GC 安全）。
thread_local! {
    static LAST_FOCUS_NODE: std::cell::Cell<Option<usize>> =
        const { std::cell::Cell::new(None) };
}

/// `__psReportFocus(nodeId)` 原生桥目标：记录 JS 侧 focus() 的元素。
/// 负数（shim 的缺省哨兵）忽略。
pub fn report_focus_node(id: f64) {
    if id >= 0.0 {
        LAST_FOCUS_NODE.with(|c| c.set(Some(id as usize)));
    }
}

/// 取走并清空上报的焦点 NodeId（cdp 侧在 evaluate 之后调用，同线程）。
#[must_use]
pub fn take_focus_node() -> Option<usize> {
    LAST_FOCUS_NODE.with(std::cell::Cell::take)
}

/// M70.4: Record a network event (called by fetch_sync_with_method).
fn record_network_event(
    url: &str,
    method: &str,
    status: u16,
    headers: &[(String, String)],
    body_size: usize,
) {
    let mime_type = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-type"))
        .map(|(_, v)| {
            v.split(';')
                .next()
                .unwrap_or("text/plain")
                .trim()
                .to_string()
        })
        .unwrap_or_else(|| "text/plain".to_string());
    CAPTURED_NETWORK.with(|slot| {
        slot.borrow_mut().push(CapturedNetworkEvent {
            url: url.to_string(),
            method: method.to_string(),
            status,
            mime_type,
            body_size,
        });
    });
}

/// RAII guard: keeps the thread-local tree installed until drop.
pub struct TreeGuard {
    _private: (),
}

impl Drop for TreeGuard {
    fn drop(&mut self) {
        CURRENT_TREE.with(|slot| {
            *slot.borrow_mut() = None;
        });
        BASE_URL.with(|slot| {
            *slot.borrow_mut() = None;
        });
        CURRENT_STORAGE.with(|slot| {
            *slot.borrow_mut() = None;
        });
        CURRENT_NAV.with(|slot| {
            *slot.borrow_mut() = None;
        });
        CURRENT_COOKIE.with(|slot| {
            *slot.borrow_mut() = None;
        });
        // M16.2/M71.1: 清理 timer slots（防止上一次 run_scripts 的 timer 残留）。
        // TIMER_CALLBACKS 是 boa 专属（QuickJS 有独立 timer），cfg 门控。
        TIMER_WHEEL.with(|slot| {
            *slot.borrow_mut() = None;
        });
        #[cfg(feature = "boa")]
        TIMER_CALLBACKS.with(|slot| {
            *slot.borrow_mut() = None;
        });
        // M17.1: 清理 XHR 状态。
        XHR_INSTANCES.with(|slot| {
            *slot.borrow_mut() = None;
        });
        XHR_NEXT_ID.with(|slot| slot.set(1));
        // M18.1: 重置网络请求计数器。
        PENDING_REQUESTS.with(|slot| slot.set(0));
        // M23.5: 清理 WS 管理器（线程退出时后台线程 drop）。
        WS_MANAGER.with(|slot| {
            *slot.borrow_mut() = None;
        });
    }
}

/// Install `tree` as the current thread's tree. Returns the shared
/// handle and a guard whose drop uninstalls.
#[must_use]
pub fn install_current(tree: Tree) -> (SharedTree, TreeGuard) {
    let shared: SharedTree = Rc::new(RefCell::new(tree));
    CURRENT_TREE.with(|slot| {
        *slot.borrow_mut() = Some(shared.clone());
    });
    (shared, TreeGuard { _private: () })
}

/// Install an already-shared tree as the current thread's tree.
/// No base URL is installed — relative URLs in __fetch* calls will
/// fail to parse (which is logged, not panicked).
#[must_use]
pub fn install_shared(shared: SharedTree) -> TreeGuard {
    CURRENT_TREE.with(|slot| {
        *slot.borrow_mut() = Some(shared);
    });
    TreeGuard { _private: () }
}

/// Install an already-shared tree plus an optional base URL used to
/// QuickJS: 初始化 BASE_URL——script.shims.rs 里的 __fetchSync 依赖它解析相对 URL。
/// 之前只对 boa 路径设了 BASE_URL；QuickJS 的 `run_scripts_quickjs` 也调了
/// `install_shared_with_base`（scripts.rs:836），但 `__fetchSync` 的 resolve_url
/// 只对绝对 URL 有效——BASE_URL 的正确性决定相对 chunk 能否加载。
/// resolve relative URLs in `__fetchSetBody` / `__fetchAppendBody`.
/// `base_url` should typically be the URL of the page being rendered.
#[must_use]
pub fn install_shared_with_base(shared: SharedTree, base_url: Option<String>) -> TreeGuard {
    CURRENT_TREE.with(|slot| {
        *slot.borrow_mut() = Some(shared);
    });
    BASE_URL.with(|slot| {
        *slot.borrow_mut() = base_url;
    });
    TreeGuard { _private: () }
}

/// Read the currently-installed base URL (may be `None`).
#[must_use]
pub fn current_base_url() -> Option<String> {
    BASE_URL.with(|slot| slot.borrow().clone())
}

/// Resolve a possibly-relative URL against the current base URL.
/// - Absolute URLs (with scheme) returned unchanged.
/// - Relative URLs resolved against the installed base.
/// - Relative URL with no base installed → returns the original
///   string (the eventual fetch will fail with InvalidUrl, which is
///   logged but not panicked).
pub fn resolve_url(url: &str) -> String {
    // Absolute if it parses standalone with a scheme.
    if url::Url::parse(url).is_ok() {
        return url.to_string();
    }
    if let Some(base) = current_base_url() {
        if let Ok(base_url) = url::Url::parse(&base) {
            if let Ok(joined) = base_url.join(url) {
                return joined.to_string();
            }
        }
    }
    url.to_string()
}

fn with_tree<F, R>(f: F) -> R
where
    F: FnOnce(&mut Tree) -> R,
{
    CURRENT_TREE.with(|slot| {
        let shared = slot
            .borrow()
            .as_ref()
            .expect("with_tree called without an installed tree")
            .clone();
        let mut tree = shared.borrow_mut();
        f(&mut tree)
    })
}

#[cfg(feature = "boa")]
fn arg_string(args: &[JsValue], idx: usize) -> Option<String> {
    args.get(idx)
        .and_then(|v| v.as_string().map(|s| s.to_std_string_escaped()))
}

/// Register all bridge globals on the given boa context.
#[cfg(feature = "boa")]
pub fn install(ctx: &mut Context) {
    register_fn(ctx, "__setBody", set_body as NativeFn);
    register_fn(ctx, "__appendBody", append_body as NativeFn);
    register_fn(ctx, "__setTitle", set_title as NativeFn);
    register_fn(ctx, "__log", log_fn as NativeFn);
    register_fn(ctx, "__fetchSetBody", fetch_set_body as NativeFn);
    register_fn(ctx, "__fetchAppendBody", fetch_append_body as NativeFn);
    register_fn(ctx, "__fetchSync", fetch_sync_bridge as NativeFn);
    register_fn(
        ctx,
        "__fetchSyncMethod",
        fetch_sync_method_bridge as NativeFn,
    );
    // M7.2.1: real DOM API bridges.
    register_fn0(ctx, "__createEl", create_el as NativeFn);
    register_fn2(ctx, "__appendChild", append_child as NativeFn);
    register_fn3(ctx, "__insertBefore", insert_before as NativeFn);
    register_fn3(ctx, "__setAttr", set_attr as NativeFn);
    register_fn2(ctx, "__removeAttr", remove_attr as NativeFn);
    register_fn1(ctx, "__getElById", get_el_by_id as NativeFn);
    register_fn1(ctx, "__qs", qs as NativeFn);
    // M62: querySelectorAll 后端（返回所有匹配）。
    register_fn1(ctx, "__qsAll", qs_all as NativeFn);
    register_fn2(ctx, "__setText", set_text as NativeFn);
    register_fn1(ctx, "__getText", get_text as NativeFn);
    register_fn1(ctx, "__getTag", get_tag as NativeFn);
    register_fn1(ctx, "__getTagName", get_tag as NativeFn);
    register_fn1(ctx, "__getParent", get_parent as NativeFn);
    register_fn1(ctx, "__children", get_children as NativeFn);
    register_fn2(ctx, "__removeChild", remove_child as NativeFn);
    register_fn2(ctx, "__getAttr", get_attr as NativeFn);
    register_fn2(ctx, "__findChild", find_child as NativeFn);
    register_fn1(ctx, "__getBody", get_body as NativeFn);
    // M8.1: form value bridges.
    register_fn1(ctx, "__getValue", get_value as NativeFn);
    register_fn2(ctx, "__setValue", set_value as NativeFn);
    // M8.3: button click bridge.
    register_fn1(ctx, "__click", click as NativeFn);
    // M8.4: form submit bridge.
    register_fn1(ctx, "__submit", submit as NativeFn);
    // M62: innerHTML/outerHTML 支持——解析 HTML 字符串为真实 DOM 元素。
    // docsify 等框架用 innerHTML/outerHTML 设置完整页面结构，再用 querySelector
    // 查找元素（.markdown-section, .sidebar-nav 等）。必须解析 HTML 创建真实节点。
    register_fn2(ctx, "__parseHtml", parse_html as NativeFn);
    // M13.2: localStorage / sessionStorage bridges.
    register_fn1(ctx, "__storageGet", storage_get_bridge as NativeFn);
    register_fn2(ctx, "__storageSet", storage_set_bridge as NativeFn);
    register_fn1(ctx, "__storageRemove", storage_remove_bridge as NativeFn);
    register_fn0(ctx, "__storageClear", storage_clear_bridge as NativeFn);
    register_fn0(ctx, "__storageLen", storage_len_bridge as NativeFn);
    register_fn1(ctx, "__storageKey", storage_key_bridge as NativeFn);
    // M14.2: history / location bridges.
    register_fn3(ctx, "__historyPush", history_push_bridge as NativeFn);
    register_fn3(ctx, "__historyReplace", history_replace_bridge as NativeFn);
    register_fn1(ctx, "__historyBack", history_back_bridge as NativeFn);
    register_fn1(ctx, "__historyForward", history_forward_bridge as NativeFn);
    register_fn1(ctx, "__historyGo", history_go_bridge as NativeFn);
    register_fn0(ctx, "__historyLen", history_len_bridge as NativeFn);
    register_fn0(ctx, "__historyState", history_state_bridge as NativeFn);
    register_fn0(ctx, "__locationHref", location_href_bridge as NativeFn);
    register_fn1(
        ctx,
        "__locationReplace",
        location_replace_bridge as NativeFn,
    );
    register_fn1(ctx, "__locationAssign", location_assign_bridge as NativeFn);
    register_fn0(ctx, "__locationParts", location_parts_bridge as NativeFn);
    // M16.2: setTimeout / clearTimeout bridges.
    // __setTimeout: 内部名（统一 __* 约定）；setTimeout: Web 标准全局名（JS 直接用）。
    register_fn2(ctx, "__setTimeout", set_timeout_bridge as NativeFn);
    register_fn1(ctx, "__clearTimeout", clear_timeout_bridge as NativeFn);
    register_fn2(ctx, "setTimeout", set_timeout_bridge as NativeFn);
    register_fn1(ctx, "clearTimeout", clear_timeout_bridge as NativeFn);
    // M62: setInterval / clearInterval（Web 标准，文档之前假声明已实现）。
    register_fn2(ctx, "__setInterval", set_interval_bridge as NativeFn);
    register_fn1(ctx, "__clearInterval", clear_timeout_bridge as NativeFn); // 复用 clear 逻辑
    register_fn2(ctx, "setInterval", set_interval_bridge as NativeFn);
    register_fn1(ctx, "clearInterval", clear_timeout_bridge as NativeFn);
    // M17.1: XMLHttpRequest bridges（__xhr* 内部名，XMLHttpRequest shim 用）。
    register_fn0(ctx, "__xhrCreate", xhr_create_bridge as NativeFn);
    register_fn3(ctx, "__xhrOpen", xhr_open_bridge as NativeFn);
    register_fn1(ctx, "__xhrSend", xhr_send_bridge as NativeFn);
    register_fn1(
        ctx,
        "__xhrGetResponseText",
        xhr_get_response_text_bridge as NativeFn,
    );
    // M23.5: WebSocket bridges（__ws* 内部名，WebSocket shim 用）。
    register_fn1(ctx, "__wsCreate", ws_create_bridge as NativeFn);
    register_fn2(ctx, "__wsSend", ws_send_bridge as NativeFn);
    register_fn1(ctx, "__wsClose", ws_close_bridge as NativeFn);
    // M69: 动态 script 执行队列——appendChild(scriptEl) 时 JS shim 调它入队代码，
    // pump 循环用 qjs_bridge::drain_dynamic_scripts() 取出 eval。
    register_fn1(
        ctx,
        "__enqueueDynamicScript",
        enqueue_dynamic_script_bridge as NativeFn,
    );
}

#[cfg(feature = "boa")]
type NativeFn = fn(&JsValue, &[JsValue], &mut Context) -> JsResult<JsValue>;

#[cfg(feature = "boa")]
fn register_fn(ctx: &mut Context, name: &str, f: NativeFn) {
    let native = NativeFunction::from_fn_ptr(f);
    let _ = ctx.register_global_callable(name.into(), 1, native);
}

/// Register with arity 0 (variadic signature is the same — this is
/// purely a documentation marker for bridges that take no args and
/// match boa's `register_global_callable(name, 0, ...)` arity hint).
#[cfg(feature = "boa")]
fn register_fn0(ctx: &mut Context, name: &str, f: NativeFn) {
    let native = NativeFunction::from_fn_ptr(f);
    let _ = ctx.register_global_callable(name.into(), 0, native);
}

#[cfg(feature = "boa")]
fn register_fn1(ctx: &mut Context, name: &str, f: NativeFn) {
    let native = NativeFunction::from_fn_ptr(f);
    let _ = ctx.register_global_callable(name.into(), 1, native);
}

#[cfg(feature = "boa")]
fn register_fn2(ctx: &mut Context, name: &str, f: NativeFn) {
    let native = NativeFunction::from_fn_ptr(f);
    let _ = ctx.register_global_callable(name.into(), 2, native);
}

#[cfg(feature = "boa")]
fn register_fn3(ctx: &mut Context, name: &str, f: NativeFn) {
    let native = NativeFunction::from_fn_ptr(f);
    let _ = ctx.register_global_callable(name.into(), 3, native);
}

#[cfg(feature = "boa")]
fn arg_usize(args: &[JsValue], idx: usize) -> Option<usize> {
    args.get(idx)
        .and_then(|v| v.as_number())
        .map(|n| n as usize)
}

#[cfg(feature = "boa")]
fn arg_usize_or_none(args: &[JsValue], idx: usize) -> Option<usize> {
    match args.get(idx) {
        Some(v) if v.is_undefined() || v.is_null() => None,
        Some(v) => v.as_number().map(|n| n as usize),
        None => None,
    }
}

#[cfg(feature = "boa")]
fn set_body(_this: &JsValue, args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let html = arg_string(args, 0).unwrap_or_default();
    with_tree(|t| set_body_inner_html(t, &html));
    Ok(JsValue::undefined())
}

#[cfg(feature = "boa")]
fn append_body(_this: &JsValue, args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let text = arg_string(args, 0).unwrap_or_default();
    with_tree(|t| append_body_text(t, &text));
    Ok(JsValue::undefined())
}

#[cfg(feature = "boa")]
fn set_title(_this: &JsValue, args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let title = arg_string(args, 0).unwrap_or_default();
    with_tree(|t| set_title_text(t, &title));
    Ok(JsValue::undefined())
}

#[cfg(feature = "boa")]
fn log_fn(_this: &JsValue, args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let msg = args.get_or_undefined(0).display().to_string();
    eprintln!("[js] {msg}");
    Ok(JsValue::undefined())
}

#[cfg(feature = "boa")]
fn fetch_set_body(_this: &JsValue, args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let raw = arg_string(args, 0).unwrap_or_default();
    if raw.is_empty() {
        return Ok(JsValue::undefined());
    }
    let url = resolve_url(&raw);
    match fetch_sync(&url) {
        Ok(text) => with_tree(|t| set_body_inner_html(t, &text)),
        Err(e) => eprintln!("[js-fetch] {raw} failed: {e}"),
    }
    Ok(JsValue::undefined())
}

#[cfg(feature = "boa")]
fn fetch_append_body(_this: &JsValue, args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let raw = arg_string(args, 0).unwrap_or_default();
    if raw.is_empty() {
        return Ok(JsValue::undefined());
    }
    let url = resolve_url(&raw);
    match fetch_sync(&url) {
        Ok(text) => with_tree(|t| append_body_text(t, &text)),
        Err(e) => eprintln!("[js-fetch] {raw} failed: {e}"),
    }
    Ok(JsValue::undefined())
}

/// `__fetchSync(url) -> string`：标准 fetch 的同步后端。
/// 返回编码 `"status\nbody"`（成功）或 `""`（失败，错误打到 stderr）。
/// JS fetch shim 用此桥拿原始响应，再用 Promise 包装成异步语义。
/// (M19.1)
#[cfg(feature = "boa")]
fn fetch_sync_bridge(_this: &JsValue, args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let raw = args
        .first()
        .and_then(|v| v.as_string())
        .map(|s| s.to_std_string_escaped())
        .unwrap_or_default();
    if raw.is_empty() {
        return Ok(JsValue::from(boa_engine::JsString::from("")));
    }
    let url = resolve_url(&raw);
    eprintln!("[js-fetch-bridge] GET {url}");
    match fetch_sync(&url) {
        // 编码：首行 status=200，后续行是 body（body 可能含换行，用 splitn(2) 解）。
        Ok(body) => Ok(JsValue::from(boa_engine::JsString::from(format!(
            "200\n{body}"
        )))),
        Err(e) => {
            eprintln!("[js-fetch] {raw} failed: {e}");
            Ok(JsValue::from(boa_engine::JsString::from("")))
        }
    }
}

/// `__fetchSyncMethod(url, method, body?, contentType?) -> string`：
/// 通用 fetch 后端（任意 method）。M20.3：POST/PUT/DELETE 表单/API 调用。
/// 返回编码 `"status\nbody"`（成功）或 `""`（失败）。
#[cfg(feature = "boa")]
fn fetch_sync_method_bridge(
    _this: &JsValue,
    args: &[JsValue],
    _ctx: &mut Context,
) -> JsResult<JsValue> {
    let url = args
        .first()
        .and_then(|v| v.as_string())
        .map(|s| s.to_std_string_escaped())
        .unwrap_or_default();
    let method = args
        .get(1)
        .and_then(|v| v.as_string())
        .map(|s| s.to_std_string_escaped())
        .unwrap_or_else(|| "GET".to_string());
    let body = args.get(2).and_then(|v| {
        if v.is_null() || v.is_undefined() {
            None
        } else {
            v.as_string().map(|s| s.to_std_string_escaped())
        }
    });
    let content_type = args.get(3).and_then(|v| {
        if v.is_null() || v.is_undefined() {
            None
        } else {
            v.as_string().map(|s| s.to_std_string_escaped())
        }
    });
    if url.is_empty() {
        return Ok(JsValue::from(boa_engine::JsString::from("")));
    }
    let resolved = resolve_url(&url);
    eprintln!("[js-fetch-bridge] {method} {resolved}");
    match fetch_sync_with_method(&resolved, &method, body.as_deref(), content_type.as_deref()) {
        Ok((status, body)) => Ok(JsValue::from(boa_engine::JsString::from(format!(
            "{status}\n{body}"
        )))),
        Err(e) => {
            eprintln!("[js-fetch] {method} {url} failed: {e}");
            Ok(JsValue::from(boa_engine::JsString::from("")))
        }
    }
}

/// Synchronously fetch a URL. Spawns a detached thread with its own
/// tokio runtime so we can be called from inside an outer runtime
/// (boa eval runs on the main thread which is already inside a
/// tokio current_thread runtime).
///
/// M15.3: 如果安装了 cookie jar，自动带 Cookie 请求头并把响应
/// Set-Cookie 存入 jar（同主请求共享会话）。jar 是 `Rc<RefCell<>>`
/// 不跨线程，所以在主线程读出 header、写入 jar；新线程只拿 String。
pub(crate) fn fetch_sync(url: &str) -> Result<String, String> {
    // M65: 检查预取缓存（Vite chunk prefetch 写入）
    if let Some(cached) = FETCH_CACHE.with(|c| c.borrow_mut().remove(url)) {
        return Ok(cached);
    }
    // PERF-M80: 共享读 SCRIPT_CACHE（外链 script / 动态 chunk MIME 探测写入）。
    // 同一 URL 同一资源 = 浏览器 HTTP 缓存语义（script/XHR/iframe 共享缓存）。
    // 只读不写：XHR GET 的动态响应不进缓存（无 validator/过期语义，写回会破坏
    // 期望新鲜响应的用例）；SCRIPT_CACHE 只由 script 加载路径写入。
    if let Ok(cache) = crate::scripts::script_cache_public().lock() {
        if let Some(code) = cache.get(url) {
            return Ok(code.clone());
        }
    }
    fetch_sync_with_method(url, "GET", None, None).map(|(_status, body)| body)
}

// M65: 预取缓存——URL → 响应体。fetch_sync 命中后移除（一次性）。
thread_local! {
    static FETCH_CACHE: std::cell::RefCell<std::collections::HashMap<String, String>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

/// M65: 批量预取 URL 到缓存（单 runtime + 单 client 并发 fetch）。
/// 关键：用一个 reqwest::Client 在同一个 tokio runtime 里并发请求，
/// reqwest + hyper 自动对同 host 的请求做 HTTP/2 多路复用（一个 TCP 连接
/// 并发多个请求），而非每个请求独立 TLS 握手。
pub(crate) fn prefetch_to_cache(urls: &[String]) {
    use std::sync::mpsc;
    if urls.is_empty() {
        return;
    }
    // 过滤掉已在缓存的
    let to_fetch: Vec<String> = urls
        .iter()
        .filter(|url| !FETCH_CACHE.with(|c| c.borrow().contains_key(*url)))
        .cloned()
        .collect();
    if to_fetch.is_empty() {
        return;
    }
    let (tx, rx) = mpsc::channel();
    let handle = std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("prefetch runtime");
        let client = browser_net::HttpClient::new();
        let tx = std::sync::Arc::new(std::sync::Mutex::new(tx));
        rt.block_on(async {
            let sem = std::sync::Arc::new(tokio::sync::Semaphore::new(8));
            let mut tasks = Vec::new();
            for url in &to_fetch {
                let permit = sem.clone().acquire_owned().await.unwrap();
                let client = client.clone();
                let tx = tx.clone();
                let url = url.clone();
                tasks.push(tokio::spawn(async move {
                    let _permit = permit;
                    if let Ok(bytes) = client.get(&url).await {
                        if let Ok(body) = String::from_utf8(bytes) {
                            if let Ok(t) = tx.lock() {
                                let _ = t.send((url, body));
                            }
                        }
                    }
                }));
            }
            for t in tasks {
                let _ = t.await;
            }
        });
        // tx (Arc) drops here → rx.iter() 收到 channel 关闭信号退出
    });
    // 先 drain rx（等待所有响应 + channel 关闭），再 join
    for (url, body) in rx.iter() {
        FETCH_CACHE.with(|c| {
            c.borrow_mut().insert(url, body);
        });
    }
    let _ = handle.join();
}

/// M65: 持久化的网络线程——复用 tokio runtime + HttpClient 连接池。
/// 之前每次 fetch_sync 都 spawn 新线程 + 新 runtime + 新 HttpClient，
/// 导致每个请求都重新 TLS 握手（~1.5s/次）。复用后同 host 连接池命中。
/// 用 channel 把请求发到网络线程，等结果回来。
struct NetWorker {
    tx: std::sync::mpsc::Sender<NetRequest>,
}

struct NetRequest {
    url: String,
    method: String,
    body: Option<String>,
    content_type: Option<String>,
    cookie_header: Option<String>,
    reply: std::sync::mpsc::Sender<NetResult>,
}

type NetResult = Result<(u16, Vec<u8>, Vec<(String, String)>), String>;

thread_local! {
    static NET_WORKER: std::cell::OnceCell<NetWorker> = const { std::cell::OnceCell::new() };
}

fn with_net_worker<F, R>(f: F) -> R
where
    F: FnOnce(&std::sync::mpsc::Sender<NetRequest>) -> R,
{
    NET_WORKER.with(|cell| {
        let worker = cell.get_or_init(|| {
            let (tx, rx) = std::sync::mpsc::channel::<NetRequest>();
            let _handle = std::thread::spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("net worker runtime");
                // M65: HttpClient 复用——reqwest 内部有连接池，同 host 的
                // 后续请求复用 TLS 连接（省 ~1.5s/次握手）。
                let client = browser_net::HttpClient::new();
                for req in rx {
                    let result = rt.block_on(client.request_full_str(
                        &req.url,
                        &req.method,
                        req.body.as_deref(),
                        req.content_type.as_deref(),
                        req.cookie_header.as_deref(),
                    ));
                    let reply = match result {
                        Ok((status, bytes, headers)) => {
                            let hdrs: Vec<(String, String)> = headers
                                .iter()
                                .map(|(k, v)| {
                                    (k.as_str().to_string(), v.to_str().unwrap_or("").to_string())
                                })
                                .collect();
                            Ok((status, bytes, hdrs))
                        }
                        Err(e) => Err(format!("{e:?}")),
                    };
                    let _ = req.reply.send(reply);
                }
            });
            NetWorker { tx }
        });
        f(&worker.tx)
    })
}

/// M20.3: 通用同步 fetch（任意 method + body + content_type）。
/// 复用 fetch_sync 的 cookie jar / networkidle / 跨线程模式。
fn fetch_sync_with_method(
    url: &str,
    method: &str,
    body: Option<&str>,
    content_type: Option<&str>,
) -> Result<(u16, String), String> {
    // M18.1: 标记网络请求进行中（networkidle 信号源）。
    inc_pending_requests();
    struct RequestGuard;
    impl Drop for RequestGuard {
        fn drop(&mut self) {
            dec_pending_requests();
        }
    }
    let _guard = RequestGuard;
    // M82: 全局 deadline 已到 → 不再发起请求（错误冒泡给 JS 的 catch/reject）。
    if js_deadline_exceeded() {
        return Err("global JS deadline exceeded".to_string());
    }
    let url = url.to_string();
    let method = method.to_string();
    let body = body.map(String::from);
    let content_type = content_type.map(String::from);
    // 主线程读 cookie header（thread-local jar）。
    let cookie_header = CURRENT_COOKIE.with(|slot| {
        slot.borrow().as_ref().and_then(|h| {
            let parsed = url::Url::parse(&url).ok()?;
            let header = h.borrow().to_cookie_header(&parsed);
            if header.is_empty() {
                None
            } else {
                Some(header)
            }
        })
    });
    // M65: 通过持久化网络线程复用 HttpClient 连接池（省 TLS 握手）。
    let (reply_tx, reply_rx) = std::sync::mpsc::channel();
    with_net_worker(|tx| {
        let _ = tx.send(NetRequest {
            url: url.clone(),
            method: method.clone(),
            body: body.clone(),
            content_type: content_type.clone(),
            cookie_header: cookie_header.clone(),
            reply: reply_tx,
        });
    });
    // M82: 等待上限收紧到 min(8s, 距全局 deadline 剩余)——超时漂移有界。
    let recv_wait = js_deadline_remaining()
        .unwrap_or(std::time::Duration::from_secs(8))
        .min(std::time::Duration::from_secs(8));
    let (status, bytes, headers) = reply_rx
        .recv_timeout(recv_wait)
        .map_err(|_| "fetch timeout (8s)".to_string())??;
    // 收集 Set-Cookie 返回给主线程写 jar。
    let set_cookies: Vec<String> = headers
        .iter()
        .filter(|(k, _)| k.eq_ignore_ascii_case("set-cookie"))
        .map(|(_, v)| v.clone())
        .collect();
    let body = String::from_utf8(bytes).map_err(|e| format!("non-utf8 response: {e}"))?;
    // M70.4: 记录这个网络请求到 CDP Network 事件队列（供 navigate drain）。
    record_network_event(&url, &method, status, &headers, body.len());
    // 主线程写回 jar。
    if !set_cookies.is_empty() {
        CURRENT_COOKIE.with(|slot| {
            if let Some(h) = slot.borrow().as_ref() {
                if let Ok(request_url) = url::Url::parse(&url) {
                    for sc in &set_cookies {
                        h.borrow_mut().store_set_cookie(sc, &request_url);
                    }
                }
            }
        });
    }
    Ok((status, body))
}

// ---------------------------------------------------------------------------
// DOM helpers
// ---------------------------------------------------------------------------

pub(crate) fn find_first_element(tree: &Tree, tag: &str) -> Option<NodeId> {
    let mut found = None;
    tree.traverse(tree.root(), |id, node| {
        if let NodeData::Element { tag: node_tag, .. } = &node.data {
            if node_tag.eq_ignore_ascii_case(tag) {
                found = Some(id);
                return false;
            }
        }
        true
    });
    found
}

fn find_child_element(tree: &Tree, parent: NodeId, tag: &str) -> Option<NodeId> {
    for &child in tree.children_of(parent) {
        if let NodeData::Element { tag: node_tag, .. } = &tree.data(child) {
            if node_tag.eq_ignore_ascii_case(tag) {
                return Some(child);
            }
        }
    }
    None
}

fn set_body_inner_html(tree: &mut Tree, html: &str) {
    let body = match find_first_element(tree, "body") {
        Some(id) => id,
        None => return,
    };
    tree.get_mut(body).children.clear();
    tree.insert(Some(body), NodeData::Text(html.into()));
}

/// M62: `__parseHtml(nodeId, html)` — 解析 HTML 字符串为真实 DOM 节点，替换目标元素子节点。
/// 用于 innerHTML setter 实现。使用 html5ever 解析 HTML，递归创建 DOM 元素。
#[cfg(feature = "boa")]
fn parse_html(_this: &JsValue, args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let node_id = match arg_usize(args, 0) {
        Some(id) => id,
        None => return Ok(JsValue::undefined()),
    };
    let html = arg_string(args, 1).unwrap_or_default();
    with_tree(|t| {
        if node_id >= t.len() {
            return;
        }
        // 使用 html5ever 解析 HTML（innerHTML 语义 = 片段解析：
        // 开头是 <script>/<style> 的片段必须留在 body 上下文里，
        // document 解析会把它们挪进 <head> 导致 setter 丢失节点）
        let parsed = browser_html_parser::parse_fragment(&html);
        // 找到 body（html5ever 总是生成完整 html/head/body 结构）
        let body_id = find_first_element(&parsed, "body");
        if let Some(body_id) = body_id {
            // 清空目标元素子节点
            t.get_mut(node_id).children.clear();
            // 复制 body 下所有子节点到目标元素
            let children = parsed.children_of(body_id).to_vec();
            for &child in &children {
                copy_subtree(&parsed, child, t, node_id);
            }
        } else {
            // fallback: 无 body 则用文本插入
            t.get_mut(node_id).children.clear();
            t.insert(Some(node_id), NodeData::Text(html));
        }
    });
    Ok(JsValue::undefined())
}

/// M69: `__enqueueDynamicScript(code)` — 把动态加载的 JS 代码塞入队列。
/// 由 appendChild 的 JS shim 在检测到 script 标签时调用。
/// event loop pump（boa 的 pump_event_loop / QuickJS 的 run_scripts_quickjs）
/// 每轮用 `qjs_bridge::drain_dynamic_scripts()` 取出，调 eval 执行。
#[cfg(feature = "boa")]
fn enqueue_dynamic_script_bridge(
    _this: &JsValue,
    args: &[JsValue],
    _ctx: &mut Context,
) -> JsResult<JsValue> {
    let code = arg_string(args, 0).unwrap_or_default();
    if !code.is_empty() {
        enqueue_dynamic_script(code);
    }
    Ok(JsValue::undefined())
}

/// 递归复制 parsed tree 的节点到目标 tree。
fn copy_subtree(src: &Tree, src_id: NodeId, dst: &mut Tree, dst_parent: NodeId) {
    let node = src.get(src_id);
    let new_id = match &node.data {
        NodeData::Element { tag, attrs } => dst.insert(
            Some(dst_parent),
            NodeData::Element {
                tag: tag.clone(),
                attrs: attrs.clone(),
            },
        ),
        NodeData::Text(text) => dst.insert(Some(dst_parent), NodeData::Text(text.clone())),
        NodeData::Comment(text) => dst.insert(Some(dst_parent), NodeData::Comment(text.clone())),
        NodeData::Document | NodeData::Doctype { .. } => return,
    };
    for &child in src.children_of(src_id) {
        copy_subtree(src, child, dst, new_id);
    }
}

/// M-cls.3: pub(crate) —— 给 spa_fallback 注入正文用。
pub(crate) fn append_body_text(tree: &mut Tree, text: &str) {
    let body = match find_first_element(tree, "body") {
        Some(id) => id,
        None => return,
    };
    tree.insert(Some(body), NodeData::Text(text.into()));
}

/// Concatenate all text descendants of the `<body>` element.
pub fn body_text_content(tree: &Tree) -> String {
    let body = match find_first_element(tree, "body") {
        Some(id) => id,
        None => return String::new(),
    };
    let mut out = String::new();
    tree.traverse(body, |_id, node| {
        if let NodeData::Text(s) = &node.data {
            out.push_str(s);
        }
        true
    });
    out
}

fn set_title_text(tree: &mut Tree, text: &str) {
    let head = match find_first_element(tree, "head") {
        Some(id) => id,
        None => return,
    };
    let title = match find_child_element(tree, head, "title") {
        Some(id) => id,
        None => return,
    };
    tree.get_mut(title).children.clear();
    tree.insert(Some(title), NodeData::Text(text.into()));
}

// ---------------------------------------------------------------------------
// M7.2.1: real DOM API bridges (NodeIds returned as f64 to JS).
// ---------------------------------------------------------------------------

#[cfg(feature = "boa")]
fn create_el(_this: &JsValue, args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let tag = arg_string(args, 0).unwrap_or_else(|| "div".into());
    let new_id = with_tree(|t| {
        let parent = find_first_element(t, "body").unwrap_or_else(|| t.root());
        t.insert(
            Some(parent),
            NodeData::Element {
                tag,
                attrs: Vec::new(),
            },
        )
    });
    Ok(JsValue::new(new_id as f64))
}

#[cfg(feature = "boa")]
fn append_child(_this: &JsValue, args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let parent_id = match arg_usize(args, 0) {
        Some(id) => id,
        None => return Ok(JsValue::undefined()),
    };
    let child_id = match arg_usize(args, 1) {
        Some(id) => id,
        None => return Ok(JsValue::undefined()),
    };
    with_tree(|t| move_subtree(t, parent_id, child_id));
    Ok(JsValue::undefined())
}

#[cfg(feature = "boa")]
fn insert_before(_this: &JsValue, args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let parent_id = match arg_usize(args, 0) {
        Some(id) => id,
        None => return Ok(JsValue::undefined()),
    };
    let child_id = match arg_usize(args, 1) {
        Some(id) => id,
        None => return Ok(JsValue::undefined()),
    };
    let reference_id = arg_usize_or_none(args, 2);
    with_tree(|t| insert_before_inner(t, parent_id, child_id, reference_id));
    Ok(JsValue::undefined())
}

/// Detach `child` from its current parent and re-attach under `new_parent`.
fn move_subtree(tree: &mut Tree, new_parent: NodeId, child: NodeId) {
    if child >= tree.len() || new_parent >= tree.len() {
        return;
    }
    if let Some(op) = tree.get(child).parent {
        tree.get_mut(op).children.retain(|&c| c != child);
    }
    tree.get_mut(new_parent).children.retain(|&c| c != child);
    tree.get_mut(child).parent = Some(new_parent);
    tree.get_mut(new_parent).children.push(child);
}

fn insert_before_inner(
    tree: &mut Tree,
    parent_id: NodeId,
    child_id: NodeId,
    before_id: Option<NodeId>,
) {
    if parent_id >= tree.len() || child_id >= tree.len() {
        return;
    }
    if let Some(current_parent) = tree.get(child_id).parent {
        tree.get_mut(current_parent)
            .children
            .retain(|&c| c != child_id);
    }
    tree.get_mut(parent_id).children.retain(|&c| c != child_id);

    let insert_pos = before_id
        .and_then(|before| {
            tree.get(parent_id)
                .children
                .iter()
                .position(|&id| id == before)
        })
        .unwrap_or_else(|| tree.get(parent_id).children.len());
    tree.get_mut(parent_id)
        .children
        .insert(insert_pos, child_id);
    tree.get_mut(child_id).parent = Some(parent_id);
}

#[cfg(feature = "boa")]
fn get_parent(_this: &JsValue, args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let id = match arg_usize(args, 0) {
        Some(id) => id,
        None => return Ok(JsValue::undefined()),
    };
    let parent = with_tree(|t| t.get(id).parent);
    Ok(match parent {
        Some(parent_id) => JsValue::new(parent_id as f64),
        None => JsValue::undefined(),
    })
}
#[cfg(feature = "boa")]
fn get_children(_this: &JsValue, args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let parent_id = match arg_usize(args, 0) {
        Some(id) => id,
        None => return Ok(JsValue::from(boa_engine::JsString::from(""))),
    };
    let child_ids = with_tree(|t| {
        t.children_of(parent_id)
            .iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>()
            .join(",")
    });
    Ok(JsValue::from(boa_engine::JsString::from(child_ids)))
}

#[cfg(feature = "boa")]
fn remove_child(_this: &JsValue, args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let parent_id = match arg_usize(args, 0) {
        Some(id) => id,
        None => return Ok(JsValue::undefined()),
    };
    let child_id = match arg_usize(args, 1) {
        Some(id) => id,
        None => return Ok(JsValue::undefined()),
    };
    with_tree(|t| {
        if child_id >= t.len() || parent_id >= t.len() {
            return;
        }
        t.get_mut(parent_id).children.retain(|&c| c != child_id);
        if let Some(current_parent) = t.get(child_id).parent {
            if current_parent == parent_id {
                t.get_mut(child_id).parent = None;
            }
        }
    });
    Ok(JsValue::undefined())
}

#[cfg(feature = "boa")]
fn set_attr(_this: &JsValue, args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let id = match arg_usize(args, 0) {
        Some(id) => id,
        None => return Ok(JsValue::undefined()),
    };
    let key = arg_string(args, 1).unwrap_or_default();
    let value = arg_string(args, 2).unwrap_or_default();
    if key.is_empty() {
        return Ok(JsValue::undefined());
    }
    with_tree(|t| set_attr_inner(t, id, &key, &value));
    Ok(JsValue::undefined())
}

fn set_attr_inner(tree: &mut Tree, id: NodeId, key: &str, value: &str) {
    let node = tree.get_mut(id);
    if let NodeData::Element { attrs, .. } = &mut node.data {
        if let Some(existing) = attrs.iter_mut().find(|(k, _)| k == key) {
            existing.1 = value.into();
        } else {
            attrs.push((key.into(), value.into()));
        }
    }
}

#[cfg(feature = "boa")]
fn remove_attr(_this: &JsValue, args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let id = match arg_usize(args, 0) {
        Some(id) => id,
        None => return Ok(JsValue::undefined()),
    };
    let key = arg_string(args, 1).unwrap_or_default();
    if key.is_empty() {
        return Ok(JsValue::undefined());
    }
    with_tree(|t| remove_attr_inner(t, id, &key));
    Ok(JsValue::undefined())
}

fn remove_attr_inner(tree: &mut Tree, id: NodeId, key: &str) {
    let node = tree.get_mut(id);
    if let NodeData::Element { attrs, .. } = &mut node.data {
        attrs.retain(|(k, _)| !k.eq_ignore_ascii_case(key));
    }
}

#[cfg(feature = "boa")]
fn get_el_by_id(_this: &JsValue, args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let id_str = arg_string(args, 0).unwrap_or_default();
    let found = with_tree(|t| find_by_id(t, &id_str));
    Ok(JsValue::new(match found {
        Some(id) => id as f64,
        None => -1.0,
    }))
}

fn find_by_id(tree: &Tree, target: &str) -> Option<NodeId> {
    let mut found = None;
    tree.traverse(tree.root(), |id, node| {
        if let NodeData::Element { attrs, .. } = &node.data {
            if attrs
                .iter()
                .any(|(k, v)| k.eq_ignore_ascii_case("id") && v == target)
            {
                found = Some(id);
                return false;
            }
        }
        true
    });
    found
}

#[cfg(feature = "boa")]
fn qs(_this: &JsValue, args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    // M7.2.1 minimal querySelector: tag selectors (`div`) and id (`#foo`).
    let sel = arg_string(args, 0).unwrap_or_default();
    let found = with_tree(|t| find_by_selector(t, &sel));
    Ok(JsValue::new(match found {
        Some(id) => id as f64,
        None => -1.0,
    }))
}

/// `__qsAll(selector) -> number[]`：返回所有匹配的 NodeId（M62）。
#[cfg(feature = "boa")]
fn qs_all(_this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let sel = arg_string(args, 0).unwrap_or_default();
    let ids = with_tree(|t| find_all_by_selector(t, &sel));
    // 用 eval 构造 JS 数组（避开 JsArray 路径差异）。
    let id_str: Vec<String> = ids.iter().map(|id| id.to_string()).collect();
    let js = format!("[{}]", id_str.join(","));
    ctx.eval(boa_engine::Source::from_bytes(&js))
}

fn find_by_selector(tree: &Tree, sel: &str) -> Option<NodeId> {
    let sel = sel.trim();
    // id 选择器短路：仅当选择器是纯 id（#xxx，不含空格/组合器）时才走快路径。
    // 否则 "#dyn1 .p" 这种后代选择器会被误判：strip_prefix('#')="dyn1 .p"
    // 再当 id 去找，必然落空。M71.3 GAP-I。
    // M78.130: 含 : . [ # 的复杂选择器也必须走 css-engine（"#div1:dir(ltr)"
    // 被当纯 id 查找直接 return None——:dir() 系列 21 个 WPT 全军覆没）。
    if !sel.contains(' ')
        && !sel.contains('+')
        && !sel.contains(':')
        && !sel.contains('.')
        && !sel.contains('[')
    {
        if let Some(tag) = sel.strip_prefix('#') {
            return find_by_id(tree, tag);
        }
    }
    // M78.7: 委托 css-engine 选择器——完整后代/相邻(`+`)组合器 + 属性 + 伪类
    // 子集，与样式管线共享单一实现（替代旧"只匹配最后一段"的近似）。
    // M78.77: :scope 伪类——近似替换为 *（WPT :scope 系列）。
    let sel_processed = sel.replace(":scope", "*");
    let parsed = match browser_css_engine::Selector::parse(&sel_processed) {
        Ok(s) => s,
        Err(_) => return None,
    };
    let mut found = None;
    // M78.45: 全 arena 扫描——游离元素（parent=None，不在 root 子树）也要
    // 被 querySelector 命中（createElement 后未插入文档的元素查询是合法操作）。
    for id in 0..tree.len() {
        if matches!(tree.data(id), NodeData::Element { .. }) && parsed.matches(tree, id) {
            found = Some(id);
            break;
        }
    }
    found
}

/// M62: find_all_by_selector —— 返回所有匹配节点（querySelectorAll 后端）。
fn find_all_by_selector(tree: &Tree, sel: &str) -> Vec<NodeId> {
    let sel = sel.trim();
    // id 选择器短路：仅当选择器是纯 id（不含空格/组合器）时走快路径。
    // 含空格的复合选择器（如 "#dyn1 .p"）不能走 id 快路径，否则误判。M71.3 GAP-I。
    // M78.130: 含 : . [ 的复杂选择器走 css-engine（同 find_by_selector）。
    if !sel.contains(' ')
        && !sel.contains('+')
        && !sel.contains(':')
        && !sel.contains('.')
        && !sel.contains('[')
    {
        if let Some(tag) = sel.strip_prefix('#') {
            // id 选择器最多一个
            if let Some(id) = find_by_id(tree, tag) {
                return vec![id];
            }
            return vec![];
        }
    }
    // M78.7: 同 find_by_selector——委托 css-engine 完整匹配。
    // M78.77: :scope 近似。
    let sel_processed2 = sel.replace(":scope", "*");
    let parsed = match browser_css_engine::Selector::parse(&sel_processed2) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    let mut found = Vec::new();
    for id in 0..tree.len() {
        if matches!(tree.data(id), NodeData::Element { .. }) && parsed.matches(tree, id) {
            found.push(id);
        }
    }
    found
}

/// M78: querySelector 语法校验（WPT 要求非法选择器抛 SYNTAX_ERR）。
/// 返回 Some(原因) 表示非法。覆盖：已知伪类的参数形状 + 括号配平。
pub fn qs_syntax_error(sel: &str) -> Option<String> {
    let b = sel.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b':' {
            i += 1;
            let name_start = i;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'-') {
                i += 1;
            }
            let name = &sel[name_start..i];
            let known = matches!(name, "lang" | "dir" | "nth-child");
            if i >= b.len() || b[i] != b'(' {
                if known {
                    return Some(format!("pseudo-class :{name} requires arguments"));
                }
                continue; // 未知伪类不在此校验（matcher 自行不匹配）
            }
            // 读括号内参数
            let arg_start = i + 1;
            let mut depth = 1;
            i += 1;
            while i < b.len() && depth > 0 {
                if b[i] == b'(' {
                    depth += 1;
                } else if b[i] == b')' {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                i += 1;
            }
            if depth != 0 {
                return Some("unbalanced parentheses".to_string());
            }
            let args = &sel[arg_start..i];
            i += 1; // consume ')'
            match name {
                "dir" => {
                    let d = args.trim();
                    if d.is_empty() {
                        return Some("':dir' requires exactly one ident".to_string());
                    }
                    if d.contains(',') || d.contains('"') || d.contains('\'') {
                        return Some(
                            "':dir' accepts a single ident, not strings or lists".to_string(),
                        );
                    }
                }
                "lang" => {
                    if args
                        .split(',')
                        .any(|p| p.trim().trim_matches(|c| c == '"' || c == '\'').is_empty())
                    {
                        return Some("':lang' requires at least one language range".to_string());
                    }
                }
                "nth-child" if args.trim().parse::<u32>().is_err() => {
                    return Some("unsupported :nth-child argument".to_string());
                }
                _ => {}
            }
        } else {
            i += 1;
        }
    }
    None
}

#[cfg(feature = "boa")]
fn set_text(_this: &JsValue, args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let id = match arg_usize(args, 0) {
        Some(id) => id,
        None => return Ok(JsValue::undefined()),
    };
    let text = arg_string(args, 1).unwrap_or_default();
    with_tree(|t| set_text_inner(t, id, &text));
    Ok(JsValue::undefined())
}

fn set_text_inner(tree: &mut Tree, id: NodeId, text: &str) {
    // M78.128: id 本身是 Text 节点时直接改 data——normalize 合并相邻文本时
    // 目标就是 Text 节点，旧"清子建子"把合并值挂成 Text 的子节点（序列化
    // 永远不可达），第二个文本又被删，内容即丢失。
    if let NodeData::Text(t) = &mut tree.get_mut(id).data {
        *t = text.into();
        return;
    }
    tree.get_mut(id).children.clear();
    tree.insert(Some(id), NodeData::Text(text.into()));
}

/// M37: `__getText(id) -> string` — 读元素文本内容（拼接所有子文本节点）。
/// Element 对象的 textContent getter 需要此桥。
#[cfg(feature = "boa")]
fn get_text(_this: &JsValue, args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let id = match arg_usize(args, 0) {
        Some(id) => id,
        None => return Ok(JsValue::undefined()),
    };
    let text = with_tree(|t| collect_text(t, id));
    Ok(boa_engine::JsString::from(text).into())
}

/// 递归收集元素的所有子文本节点内容（textContent 语义）。
fn collect_text(tree: &Tree, id: NodeId) -> String {
    let mut out = String::new();
    collect_text_inner(tree, id, &mut out);
    out
}

fn collect_text_inner(tree: &Tree, id: NodeId, out: &mut String) {
    for &child in tree.children_of(id) {
        if let NodeData::Text(s) = tree.data(child) {
            out.push_str(s);
        } else {
            collect_text_inner(tree, child, out);
        }
    }
}

#[cfg(feature = "boa")]
fn get_tag(_this: &JsValue, args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    if let Some(id) = arg_usize(args, 0) {
        let tag = with_tree(|t| match t.data(id) {
            NodeData::Element { tag, .. } => Some(tag.clone()),
            _ => None,
        });
        return Ok(match tag {
            Some(t) => boa_engine::JsString::from(t).into(),
            None => JsValue::undefined(),
        });
    }
    let tag_name = arg_string(args, 0)
        .unwrap_or_default()
        .trim()
        .to_lowercase();
    if tag_name.is_empty() {
        return Ok(JsValue::undefined());
    }
    let found = with_tree(|t| find_first_element(t, &tag_name));
    Ok(match found {
        Some(id) => JsValue::new(id as f64),
        None => JsValue::undefined(),
    })
}

/// M37: `__getAttr(id, key) -> string` — 读元素属性（Element 对象的 getter 用）。
#[cfg(feature = "boa")]
fn get_attr(_this: &JsValue, args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let id = match arg_usize(args, 0) {
        Some(id) => id,
        None => return Ok(JsValue::undefined()),
    };
    let key = arg_string(args, 1).unwrap_or_default();
    if key.is_empty() {
        return Ok(JsValue::undefined());
    }
    let val = with_tree(|t| match t.data(id) {
        NodeData::Element { attrs, .. } => attrs
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(&key))
            .map(|(_, v)| v.clone()),
        _ => None,
    });
    match val {
        Some(v) => Ok(boa_engine::JsString::from(v).into()),
        None => Ok(JsValue::null()),
    }
}

/// M37: `__findChild(parentId, id) -> number | undefined` —
/// 在 parent 后代中查找指定 id 的元素（Element.getElementById 用）。
#[cfg(feature = "boa")]
fn find_child(_this: &JsValue, args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let parent_id = match arg_usize(args, 0) {
        Some(id) => id,
        None => return Ok(JsValue::undefined()),
    };
    let target = arg_string(args, 1).unwrap_or_default();
    if target.is_empty() {
        return Ok(JsValue::undefined());
    }
    let found = with_tree(|t| find_by_id_in_subtree(t, parent_id, &target));
    match found {
        Some(id) => Ok(JsValue::new(id as f64)),
        None => Ok(JsValue::undefined()),
    }
}

/// 在指定节点的后代中按 id 查找（get_el_by_id 的全树版的子树限定变体）。
#[allow(dead_code)]
fn find_by_id_in_subtree(tree: &Tree, root: NodeId, target: &str) -> Option<NodeId> {
    let mut found = None;
    tree.traverse(root, |id, node| {
        if let NodeData::Element { attrs, .. } = &node.data {
            if attrs
                .iter()
                .any(|(k, v)| k.eq_ignore_ascii_case("id") && v == target)
            {
                found = Some(id);
                return false;
            }
        }
        true
    });
    found
}

#[cfg(feature = "boa")]
fn get_body(_this: &JsValue, _args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let id = with_tree(|t| find_first_element(t, "body"));
    Ok(JsValue::new(match id {
        Some(id) => id as f64,
        None => -1.0,
    }))
}

// M8.1: form value bridges.
#[cfg(feature = "boa")]
fn get_value(_this: &JsValue, args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let id = match arg_usize(args, 0) {
        Some(id) => id,
        None => return Ok(JsValue::undefined()),
    };
    let value = with_tree(|t| match t.data(id) {
        NodeData::Element { tag, attrs, .. } if tag == "input" || tag == "textarea" => attrs
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("value"))
            .map(|(_, v)| v.clone())
            .unwrap_or_default(),
        _ => String::new(),
    });
    eprintln!("[dom-getValue] #{id} = {value}");
    Ok(JsValue::undefined())
}

#[cfg(feature = "boa")]
fn set_value(_this: &JsValue, args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let id = match arg_usize(args, 0) {
        Some(id) => id,
        None => return Ok(JsValue::undefined()),
    };
    let value = arg_string(args, 1).unwrap_or_default();
    with_tree(|t| set_attr_inner(t, id, "value", &value));
    Ok(JsValue::undefined())
}

// M8.3: button click bridge.
#[cfg(feature = "boa")]
fn click(_this: &JsValue, args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let id = match arg_usize(args, 0) {
        Some(id) => id,
        None => return Ok(JsValue::undefined()),
    };
    // TODO: execute onclick handler when JS execution engine supports it.
    // For M8.3, just log.
    eprintln!("[dom-click] #{id}");
    Ok(JsValue::undefined())
}

// M8.4: form submit bridge.
#[cfg(feature = "boa")]
fn submit(_this: &JsValue, args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let id = match arg_usize(args, 0) {
        Some(id) => id,
        None => return Ok(JsValue::undefined()),
    };
    eprintln!("[dom-submit] #{id}");
    Ok(JsValue::undefined())
}

// M13.2: Web Storage bridges.
//
// SPA 场景：很多 React/Vue 应用在 localStorage 里塞 token、用户偏好、
// 缓存数据。这些都是同步 API，直接从 thread-local slot 拿 StorageHandle
// 调 browser_storage 函数。
//
// 注意：localStorage 和 sessionStorage 共享同一个 handle（MVP 爬虫
// 场景不区分 session/local，session 一般也不会跨刷新）。

fn with_storage<F, R>(f: F) -> R
where
    F: FnOnce(&StorageHandle) -> R,
{
    CURRENT_STORAGE.with(|slot| {
        let borrowed = slot.borrow();
        let handle = borrowed
            .as_ref()
            .expect("storage bridge called without install_storage()");
        f(handle)
    })
}

/// Install `handle` as the current thread's storage backend.
/// Must be called before invoking any JS that uses localStorage/
/// sessionStorage. Cleared automatically by TreeGuard drop.
pub fn install_storage(handle: StorageHandle) {
    CURRENT_STORAGE.with(|slot| {
        *slot.borrow_mut() = Some(handle);
    });
}

#[cfg(feature = "boa")]
fn storage_get_bridge(_this: &JsValue, args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let key = match arg_string(args, 0) {
        Some(k) => k,
        None => return Ok(JsValue::undefined()),
    };
    let value = with_storage(|s| browser_storage::storage_get(s, &key));
    match value {
        Some(v) => Ok(JsValue::from(boa_engine::string::JsString::from(v))),
        None => Ok(JsValue::null()),
    }
}

#[cfg(feature = "boa")]
fn storage_set_bridge(_this: &JsValue, args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let key = match arg_string(args, 0) {
        Some(k) => k,
        None => return Ok(JsValue::undefined()),
    };
    let value = arg_string(args, 1).unwrap_or_default();
    with_storage(|s| browser_storage::storage_set(s, &key, &value));
    eprintln!("[storage-set] {key}={value}");
    Ok(JsValue::undefined())
}

#[cfg(feature = "boa")]
fn storage_remove_bridge(
    _this: &JsValue,
    args: &[JsValue],
    _ctx: &mut Context,
) -> JsResult<JsValue> {
    let key = match arg_string(args, 0) {
        Some(k) => k,
        None => return Ok(JsValue::undefined()),
    };
    with_storage(|s| browser_storage::storage_remove(s, &key));
    Ok(JsValue::undefined())
}

#[cfg(feature = "boa")]
fn storage_clear_bridge(
    _this: &JsValue,
    _args: &[JsValue],
    _ctx: &mut Context,
) -> JsResult<JsValue> {
    with_storage(browser_storage::storage_clear);
    Ok(JsValue::undefined())
}

#[cfg(feature = "boa")]
fn storage_len_bridge(_this: &JsValue, _args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let len = with_storage(browser_storage::storage_len);
    Ok(JsValue::new(len as f64))
}

#[cfg(feature = "boa")]
fn storage_key_bridge(_this: &JsValue, args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let idx = match arg_usize(args, 0) {
        Some(i) => i,
        None => return Ok(JsValue::undefined()),
    };
    let key = with_storage(|s| browser_storage::storage_key(s, idx));
    match key {
        Some(k) => Ok(JsValue::from(boa_engine::string::JsString::from(k))),
        None => Ok(JsValue::null()),
    }
}

// M14.2: History / Location bridges.
//
// SPA 路由依赖 history.pushState / popstate。React Router / Vue Router
// 都用这些 API。我们提供同步调用，pushState 不触发真实 fetch（MVP
// 爬虫场景：JS 中 pushState 通常只改 URL 不重新加载）。

fn with_navigation<F, R>(f: F) -> R
where
    F: FnOnce(&NavigationHandle) -> R,
{
    CURRENT_NAV.with(|slot| {
        let borrowed = slot.borrow();
        let handle = borrowed
            .as_ref()
            .expect("navigation bridge called without install_navigation()");
        f(handle)
    })
}

/// Install `handle` as the current thread's navigation backend.
pub fn install_navigation(handle: NavigationHandle) {
    CURRENT_NAV.with(|slot| {
        *slot.borrow_mut() = Some(handle);
    });
}

/// Install `handle` as the current thread's cookie jar backend. (M15.3)
pub fn install_cookie(handle: CookieHandle) {
    CURRENT_COOKIE.with(|slot| {
        *slot.borrow_mut() = Some(handle);
    });
}

/// Ensure a cookie jar exists on the current thread. If one is already
/// installed (e.g. by cli before the main fetch), reuse it; otherwise
/// install a fresh empty jar. (M15.4)
pub fn ensure_cookie_jar() {
    CURRENT_COOKIE.with(|slot| {
        if slot.borrow().is_none() {
            *slot.borrow_mut() = Some(browser_cookie::new_cookie_jar());
        }
    });
}

/// Clone the current thread's cookie jar handle (None if not installed). (M15.4)
/// Used by cli to share the same jar across main fetch + JS fetch.
pub fn current_cookie_jar() -> Option<CookieHandle> {
    CURRENT_COOKIE.with(|slot| slot.borrow().as_ref().map(Clone::clone))
}

// ===== M62: setInterval event loop 后端 =====

/// `__setInterval(callback: Function, delay: number) -> number`
/// 注册一个重复 timer。每次触发后自动重新 schedule（除非 clearInterval）。
#[cfg(feature = "boa")]
fn set_interval_bridge(_this: &JsValue, args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let callback = match args.first().and_then(|v| v.as_object()) {
        Some(obj) if obj.is_callable() => obj.clone(),
        _ => return Ok(JsValue::undefined()),
    };
    let delay_ms = args
        .get(1)
        .and_then(|v| v.as_number())
        .map(|n| if n < 0.0 { 0u64 } else { n as u64 })
        .unwrap_or(0);
    ensure_eventloop();
    // 确保 INTERVAL_INFO 存在
    INTERVAL_INFO.with(|slot| {
        if slot.borrow().is_none() {
            *slot.borrow_mut() = Some(std::collections::HashMap::new());
        }
    });
    let id = TIMER_WHEEL.with(|slot| {
        let mut slot = slot.borrow_mut();
        let wheel = slot.as_mut().expect("wheel ensured");
        wheel.schedule(delay_ms, std::time::Instant::now())
    });
    // 存 callback 两份：TIMER_CALLBACKS（drain 取）+ INTERVAL_INFO（触发后重新 schedule）
    TIMER_CALLBACKS.with(|slot| {
        slot.borrow_mut()
            .as_mut()
            .expect("callbacks ensured")
            .insert(id, callback.clone());
    });
    INTERVAL_INFO.with(|slot| {
        slot.borrow_mut()
            .as_mut()
            .expect("interval info ensured")
            .insert(id, (callback, delay_ms, 0u32));
    });
    Ok(JsValue::new(id.raw() as f64))
}

// ===== M16.2: setTimeout / clearTimeout event loop 后端 =====

/// Ensure a timer wheel + callback map exist on the current thread.
/// Idempotent: reuses if already installed (e.g. across run_scripts calls).
/// (M16.2)
#[cfg(feature = "boa")]
fn ensure_eventloop() {
    TIMER_WHEEL.with(|slot| {
        if slot.borrow().is_none() {
            *slot.borrow_mut() = Some(TimerWheel::new());
        }
    });
    TIMER_CALLBACKS.with(|slot| {
        if slot.borrow().is_none() {
            *slot.borrow_mut() = Some(std::collections::HashMap::new());
        }
    });
}

/// `__setTimeout(callback: Function, delay: number) -> number`
/// 注册一个 timer，返回 id 给 JS。callback 在到期时由 event loop 调用。
#[cfg(feature = "boa")]
fn set_timeout_bridge(_this: &JsValue, args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    // 1. 提取 callback（必须是 callable object）。clone 成 owned（JsObject 是 GC 引用计数）。
    let callback = match args.first().and_then(|v| v.as_object()) {
        Some(obj) if obj.is_callable() => obj.clone(),
        _ => return Ok(JsValue::undefined()),
    };
    // 2. 提取 delay（毫秒，number；默认 0）
    let delay_ms = args
        .get(1)
        .and_then(|v| v.as_number())
        .map(|n| if n < 0.0 { 0u64 } else { n as u64 })
        .unwrap_or(0);
    ensure_eventloop();
    let id = TIMER_WHEEL.with(|slot| {
        let mut slot = slot.borrow_mut();
        let wheel = slot.as_mut().expect("wheel ensured above");
        wheel.schedule(delay_ms, std::time::Instant::now())
    });
    TIMER_CALLBACKS.with(|slot| {
        let mut slot = slot.borrow_mut();
        let cbs = slot.as_mut().expect("callbacks ensured above");
        cbs.insert(id, callback);
    });
    Ok(JsValue::new(id.raw() as f64))
}

/// `__clearTimeout(id: number) -> undefined`
/// 取消一个 timer。不存在的 id 安全调用（幂等）。
#[cfg(feature = "boa")]
fn clear_timeout_bridge(
    _this: &JsValue,
    args: &[JsValue],
    _ctx: &mut Context,
) -> JsResult<JsValue> {
    let id_raw = match args.first().and_then(|v| v.as_number()) {
        Some(n) => n as u64,
        None => return Ok(JsValue::undefined()),
    };
    let id = TimerId::from_raw(id_raw);
    TIMER_WHEEL.with(|slot| {
        if let Some(wheel) = slot.borrow_mut().as_mut() {
            wheel.cancel(id);
        }
    });
    TIMER_CALLBACKS.with(|slot| {
        if let Some(cbs) = slot.borrow_mut().as_mut() {
            cbs.remove(&id);
        }
    });
    // M62: clearInterval 也清 INTERVAL_INFO，防止 drain 重新 schedule。
    INTERVAL_INFO.with(|slot| {
        if let Some(intervals) = slot.borrow_mut().as_mut() {
            intervals.remove(&id);
        }
    });
    Ok(JsValue::undefined())
}

/// Drain all due timer callbacks, returning them in FIFO order. (M16.2)
/// Event loop（run_scripts_with_base 的收尾循环）调用此函数，拿到到期
/// 的 JsObject 列表，逐个 `.call(&JsValue::undefined(), ctx)` 执行。
///
/// Returns `Vec<JsObject>`（空的 vec 表示没有到期 timer）。
#[cfg(feature = "boa")]
pub fn drain_due_timer_callbacks() -> Vec<JsObject> {
    let due_ids = TIMER_WHEEL.with(|slot| {
        slot.borrow_mut()
            .as_mut()
            .map(|w| w.drain_due(std::time::Instant::now()))
            .unwrap_or_default()
    });
    let mut callbacks = Vec::with_capacity(due_ids.len());
    TIMER_CALLBACKS.with(|slot| {
        let mut slot = slot.borrow_mut();
        if let Some(cbs) = slot.as_mut() {
            for id in due_ids {
                if let Some(cb) = cbs.remove(&id) {
                    callbacks.push(cb.clone());
                    // M62: 如果是 interval，触发后重新 schedule（不从 INTERVAL_INFO 删除）。
                    INTERVAL_INFO.with(|islot| {
                        let mut islot = islot.borrow_mut();
                        if let Some(intervals) = islot.as_mut() {
                            if let Some((icb, delay, count)) = intervals.get(&id) {
                                // M62: 爬虫场景防无限循环，最多触发 100 次。
                                if *count >= 100 {
                                    intervals.remove(&id);
                                    return;
                                }
                                let new_id = TIMER_WHEEL.with(|wslot| {
                                    wslot
                                        .borrow_mut()
                                        .as_mut()
                                        .expect("wheel")
                                        .schedule(*delay, std::time::Instant::now())
                                });
                                cbs.insert(new_id, icb.clone());
                                intervals.insert(new_id, (icb.clone(), *delay, count + 1));
                                intervals.remove(&id);
                            }
                        }
                    });
                }
            }
        }
    });
    callbacks
}

/// Pending timer count（M18 networkidle 信号源）。0 = idle。
pub fn pending_timers() -> usize {
    TIMER_WHEEL.with(|slot| slot.borrow().as_ref().map(|w| w.pending()).unwrap_or(0))
}

/// M37: 最近未取消 timer 的 deadline，用于 pump_event_loop sleep。
/// 没有 pending timer 时返回 None。
#[must_use]
pub fn next_timer_deadline() -> Option<std::time::Instant> {
    TIMER_WHEEL.with(|slot| slot.borrow().as_ref().and_then(|w| w.next_deadline()))
}

// ===== M18.1: networkidle 信号 =====

/// 当前 in-flight 网络请求数（fetch_sync / xhr_send 进行中）。
#[must_use]
pub fn pending_requests() -> usize {
    PENDING_REQUESTS.with(|slot| slot.get())
}

/// networkidle = 没有 pending timer 且没有 in-flight 请求。
/// 爬虫用此信号判断 SPA 是否渲染完（Playwright/Puppeteer 同款能力）。
#[must_use]
pub fn is_network_idle() -> bool {
    pending_timers() == 0 && pending_requests() == 0
}

// ===== M69: 动态 script 执行队列 =====
//
// 当 JS 调 `document.createElement("script") + head.appendChild(s)` 时，
// appendChild 的 JS shim 检测到 script 标签后，把代码（inline textContent 或
// __fetchSync 拿到的外链源码）塞进这个队列。event loop pump 每轮用
// `drain_dynamic_scripts()` 取出，调 engine.eval_safe(code) 执行。
//
// 为什么走队列而非 JS 层直接 eval：
// 1. GC 安全——QuickJS 间接 eval 在 appendChild 闭包内创建的 JS 值有泄漏风险
//    （AGENTS.md 第 13 条）；eval_safe 用 CatchResultExt::catch，CaughtError
//    在 ctx.with 闭包内 drop。
// 2. 时序符合 HTML5——动态 script 的执行推迟到 event loop 下一轮（微任务/宏任务语义），
//    不阻塞当前同步 JS 栈。
// 3. 多层链式加载——webpack runtime→vue chunk→app chunk 的递归 appendChild
//    天然支持：每轮 pump 处理一层，新入队的下一轮处理。
//
// 放顶层（非 qjs_bridge mod 内），不受 quickjs feature 门控——boa 和 QuickJS 共用。
thread_local! {
    static PENDING_DYNAMIC_SCRIPTS: std::cell::RefCell<Vec<String>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// 把动态 script 代码塞入队列（由 appendChild JS shim 调用）。
pub fn enqueue_dynamic_script(code: String) {
    PENDING_DYNAMIC_SCRIPTS.with(|q| q.borrow_mut().push(code));
}

/// 取出所有待执行的动态 script（由 event loop pump 每轮调用）。
/// 返回的 Vec 顺序 = 入队顺序（FIFO），取出后队列清空。
pub fn drain_dynamic_scripts() -> Vec<String> {
    PENDING_DYNAMIC_SCRIPTS.with(|q| std::mem::take(&mut *q.borrow_mut()))
}

fn inc_pending_requests() {
    PENDING_REQUESTS.with(|slot| slot.set(slot.get().saturating_add(1)));
}

fn dec_pending_requests() {
    PENDING_REQUESTS.with(|slot| slot.set(slot.get().saturating_sub(1)));
}

// ===== M23.5: WebSocket 后端 =====

/// Ensure a WsManager exists on the current thread. Idempotent.
#[allow(dead_code)]
fn ensure_ws_manager() {
    WS_MANAGER.with(|slot| {
        if slot.borrow().is_none() {
            *slot.borrow_mut() = Some(browser_ws::WsManager::new());
        }
    });
}

/// 引擎无关：发起 ws:// 连接，返回连接 id。
/// QuickJS 和 boa 共用。
pub fn ws_create(url: String) -> u32 {
    let resolved = resolve_url(&url);
    ensure_ws_manager();
    WS_MANAGER.with(|slot| {
        slot.borrow()
            .as_ref()
            .map(|m| m.connect(resolved))
            .unwrap_or(0)
    })
}

/// `__wsCreate(url: string) -> number`：发起 ws:// 连接，返回 id。
/// 实际握手在后台线程异步进行；Open/Error 事件经 drain_ws_events 分派。
#[cfg(feature = "boa")]
fn ws_create_bridge(_this: &JsValue, args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let url = args
        .first()
        .and_then(|v| v.as_string())
        .map(|s| s.to_std_string_escaped())
        .unwrap_or_default();
    Ok(JsValue::new(ws_create(url) as f64))
}

/// 引擎无关：队列文本消息。
pub fn ws_send(id: u32, data: String) {
    WS_MANAGER.with(|slot| {
        if let Some(m) = slot.borrow().as_ref() {
            m.send_text(id, data);
        }
    });
}

/// `__wsSend(id: number, data: string) -> undefined`：队列文本消息到后台线程。
#[cfg(feature = "boa")]
fn ws_send_bridge(_this: &JsValue, args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let id = match args.first().and_then(|v| v.as_number()).map(|n| n as u32) {
        Some(n) => n,
        None => return Ok(JsValue::undefined()),
    };
    let data = args
        .get(1)
        .and_then(|v| v.as_string())
        .map(|s| s.to_std_string_escaped())
        .unwrap_or_default();
    ws_send(id, data);
    Ok(JsValue::undefined())
}

/// 引擎无关：队列关闭帧。
pub fn ws_close(id: u32) {
    WS_MANAGER.with(|slot| {
        if let Some(m) = slot.borrow().as_ref() {
            m.close(id);
        }
    });
}

/// `__wsClose(id: number) -> undefined`：队列关闭帧。
#[cfg(feature = "boa")]
fn ws_close_bridge(_this: &JsValue, args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let id = match args.first().and_then(|v| v.as_number()).map(|n| n as u32) {
        Some(n) => n,
        None => return Ok(JsValue::undefined()),
    };
    ws_close(id);
    Ok(JsValue::undefined())
}

/// Drain all pending WebSocket events as JS-dispatchable strings.
/// Returns Vec<(id, type, data)>，pump_event_loop 逐个 eval
/// `__wsDispatchEvent(id, type, data)` 分派到 JS 回调。
///
/// type: "open" | "message" | "close" | "error"
#[must_use]
pub fn drain_ws_events() -> Vec<(u32, &'static str, String)> {
    WS_MANAGER.with(|slot| {
        let mut slot = slot.borrow_mut();
        let Some(mgr) = slot.as_mut() else {
            return Vec::new();
        };
        let events = mgr.drain_events();
        let mut out = Vec::with_capacity(events.len());
        for e in events {
            let item = match e {
                browser_ws::WsEvent::Open { id } => (id, "open", String::new()),
                browser_ws::WsEvent::Text { id, data } => (id, "message", data),
                browser_ws::WsEvent::Binary { id, data } => {
                    // 爬虫场景以文本为主；binary 转 lossy UTF-8。
                    (id, "message", String::from_utf8_lossy(&data).into_owned())
                }
                browser_ws::WsEvent::Closed { id, reason, .. } => {
                    mgr.remove_connection(id);
                    (id, "close", reason)
                }
                browser_ws::WsEvent::Error { id, message } => {
                    mgr.remove_connection(id);
                    (id, "error", message)
                }
            };
            out.push(item);
        }
        out
    })
}

/// 当前跟踪的 WS 连接数（用于 pump_event_loop 决定是否多 poll）。
#[must_use]
pub fn ws_connection_count() -> usize {
    WS_MANAGER.with(|slot| {
        slot.borrow()
            .as_ref()
            .map(browser_ws::WsManager::connection_count)
            .unwrap_or(0)
    })
}

// ===== M17.1: XMLHttpRequest 后端 =====

/// 单个 XHR 实例的状态。open() 记录请求参数，send() 执行同步 fetch
/// 并把响应存到 response_text，JS shim 用 setTimeout(0) 触发 onload。
#[derive(Debug, Clone, Default)]
#[allow(dead_code)]
struct XhrState {
    method: String,
    url: String,
    response_text: String,
    status: u16,
    error: Option<String>,
}

/// Ensure the XHR instance map exists. Idempotent. (M17.1)
#[allow(dead_code)]
fn ensure_xhr() {
    XHR_INSTANCES.with(|slot| {
        if slot.borrow().is_none() {
            *slot.borrow_mut() = Some(std::collections::HashMap::new());
        }
    });
}

/// `__xhrCreate() -> number`：新建一个 XHR 实例，返回 id 给 JS。
#[cfg(feature = "boa")]
fn xhr_create_bridge(_this: &JsValue, _args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    ensure_xhr();
    let id = XHR_NEXT_ID.with(|slot| {
        let n = slot.get();
        slot.set(n + 1);
        n
    });
    XHR_INSTANCES.with(|slot| {
        if let Some(map) = slot.borrow_mut().as_mut() {
            map.insert(id, XhrState::default());
        }
    });
    Ok(JsValue::new(id as f64))
}

/// `__xhrOpen(id, method, url) -> undefined`：记录请求参数（不立即 fetch）。
#[cfg(feature = "boa")]
fn xhr_open_bridge(_this: &JsValue, args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let id = match args.first().and_then(|v| v.as_number()).map(|n| n as u64) {
        Some(n) => n,
        None => return Ok(JsValue::undefined()),
    };
    let method = args
        .get(1)
        .and_then(|v| v.as_string())
        .map(|s| s.to_std_string_escaped())
        .unwrap_or_else(|| "GET".to_string());
    let url = args
        .get(2)
        .and_then(|v| v.as_string())
        .map(|s| s.to_std_string_escaped())
        .unwrap_or_default();
    let resolved = resolve_url(&url);
    eprintln!("[js-xhr] open {method} {url}");
    XHR_INSTANCES.with(|slot| {
        if let Some(map) = slot.borrow_mut().as_mut() {
            if let Some(state) = map.get_mut(&id) {
                state.method = method;
                state.url = resolved;
            }
        }
    });
    Ok(JsValue::undefined())
}

/// `__xhrSend(id) -> undefined`：执行同步 fetch（复用 fetch_sync），
/// 把响应存到 response_text。JS shim 随后用 setTimeout(0) 触发 onload。
#[cfg(feature = "boa")]
fn xhr_send_bridge(_this: &JsValue, args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let id = match args.first().and_then(|v| v.as_number()).map(|n| n as u64) {
        Some(n) => n,
        None => return Ok(JsValue::undefined()),
    };
    let url = XHR_INSTANCES.with(|slot| {
        slot.borrow()
            .as_ref()
            .and_then(|map| map.get(&id).map(|s| s.url.clone()))
    });
    let Some(url) = url else {
        return Ok(JsValue::undefined());
    };
    eprintln!("[js-xhr] send {}", url);
    let (response_text, status, error) = match fetch_sync(&url) {
        Ok(body) => (body, 200u16, None),
        Err(e) => (String::new(), 0u16, Some(e)),
    };
    XHR_INSTANCES.with(|slot| {
        if let Some(map) = slot.borrow_mut().as_mut() {
            if let Some(state) = map.get_mut(&id) {
                state.response_text = response_text;
                state.status = status;
                state.error = error;
            }
        }
    });
    Ok(JsValue::undefined())
}

/// `__xhrGetResponseText(id) -> string | null`：读取响应体。
/// JS shim 在 onload 回调里调用此函数拿到 responseText。
#[cfg(feature = "boa")]
fn xhr_get_response_text_bridge(
    _this: &JsValue,
    args: &[JsValue],
    _ctx: &mut Context,
) -> JsResult<JsValue> {
    let id = match args.first().and_then(|v| v.as_number()).map(|n| n as u64) {
        Some(n) => n,
        None => return Ok(JsValue::null()),
    };
    let text = XHR_INSTANCES.with(|slot| {
        slot.borrow()
            .as_ref()
            .and_then(|map| map.get(&id).map(|s| s.response_text.clone()))
    });
    match text {
        Some(t) => Ok(JsValue::from(boa_engine::JsString::from(t))),
        None => Ok(JsValue::null()),
    }
}

#[cfg(feature = "boa")]
fn history_push_bridge(_this: &JsValue, args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let state = arg_string(args, 0);
    let url = arg_string(args, 2).unwrap_or_default();
    with_navigation(|h| browser_navigation::history_push(h, state, url));
    Ok(JsValue::undefined())
}

#[cfg(feature = "boa")]
fn history_replace_bridge(
    _this: &JsValue,
    args: &[JsValue],
    _ctx: &mut Context,
) -> JsResult<JsValue> {
    let state = arg_string(args, 0);
    let url = arg_string(args, 2).unwrap_or_default();
    with_navigation(|h| browser_navigation::history_replace(h, state, url));
    Ok(JsValue::undefined())
}

#[cfg(feature = "boa")]
fn history_back_bridge(
    _this: &JsValue,
    _args: &[JsValue],
    _ctx: &mut Context,
) -> JsResult<JsValue> {
    let ok = with_navigation(browser_navigation::history_back);
    Ok(JsValue::new(ok))
}

#[cfg(feature = "boa")]
fn history_forward_bridge(
    _this: &JsValue,
    _args: &[JsValue],
    _ctx: &mut Context,
) -> JsResult<JsValue> {
    let ok = with_navigation(browser_navigation::history_forward);
    Ok(JsValue::new(ok))
}

#[cfg(feature = "boa")]
fn history_go_bridge(_this: &JsValue, args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let n = match args.first().and_then(|v| v.as_number()) {
        Some(n) => n as i64,
        None => return Ok(JsValue::undefined()),
    };
    let ok = with_navigation(|h| browser_navigation::history_go(h, n));
    Ok(JsValue::new(ok))
}

#[cfg(feature = "boa")]
fn history_len_bridge(_this: &JsValue, _args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let len = with_navigation(browser_navigation::history_len);
    Ok(JsValue::new(len as f64))
}

#[cfg(feature = "boa")]
fn history_state_bridge(
    _this: &JsValue,
    _args: &[JsValue],
    _ctx: &mut Context,
) -> JsResult<JsValue> {
    let state = with_navigation(browser_navigation::history_state);
    match state {
        Some(s) => Ok(JsValue::from(boa_engine::string::JsString::from(s))),
        None => Ok(JsValue::null()),
    }
}

#[cfg(feature = "boa")]
fn location_href_bridge(
    _this: &JsValue,
    _args: &[JsValue],
    _ctx: &mut Context,
) -> JsResult<JsValue> {
    let url = with_navigation(browser_navigation::current_url);
    Ok(JsValue::from(boa_engine::string::JsString::from(url)))
}

#[cfg(feature = "boa")]
fn location_replace_bridge(
    _this: &JsValue,
    args: &[JsValue],
    _ctx: &mut Context,
) -> JsResult<JsValue> {
    let url = arg_string(args, 0).unwrap_or_default();
    with_navigation(|h| browser_navigation::location_replace(h, url));
    Ok(JsValue::undefined())
}

#[cfg(feature = "boa")]
fn location_assign_bridge(
    _this: &JsValue,
    args: &[JsValue],
    _ctx: &mut Context,
) -> JsResult<JsValue> {
    let url = arg_string(args, 0).unwrap_or_default();
    with_navigation(|h| browser_navigation::location_assign(h, url));
    Ok(JsValue::undefined())
}

#[cfg(feature = "boa")]
fn location_parts_bridge(
    _this: &JsValue,
    _args: &[JsValue],
    ctx: &mut Context,
) -> JsResult<JsValue> {
    let href = with_navigation(browser_navigation::current_url);
    let parts = browser_navigation::parse_url_parts(&href);
    // 用 eval 构造对象，避免 JsObject::set 的 PropertyKey trait bound
    // 复杂性（同 storage_shim 的做法）。
    let code = format!(
        "({{href:{:?},protocol:{:?},host:{:?},hostname:{:?},port:{:?},pathname:{:?},search:{:?},hash:{:?}}})",
        parts.href,
        parts.protocol,
        parts.host,
        parts.hostname,
        parts.port,
        parts.pathname,
        parts.search,
        parts.hash
    );
    ctx.eval(boa_engine::Source::from_bytes(&code))
}

#[cfg(all(test, feature = "boa"))]
mod tests {
    use super::*;
    use boa_engine::Source;

    fn tree_with_body(body_text: &str) -> Tree {
        let html = format!("<html><head><title>old</title></head><body>{body_text}</body></html>");
        browser_html_parser::parse(&html)
    }

    fn make_ctx() -> boa_engine::Context {
        let mut ctx = boa_engine::Context::default();
        install(&mut ctx);
        ctx
    }

    #[test]
    fn js_set_body_replaces_body_content() {
        let mut ctx = make_ctx();
        let tree = tree_with_body("old");
        let (shared, _guard) = install_current(tree);
        ctx.eval(Source::from_bytes(r#"__setBody("hello from js")"#))
            .unwrap();
        drop(_guard);
        assert_eq!(body_text_content(&shared.borrow()), "hello from js");
    }

    #[test]
    fn js_append_body_adds_text() {
        let mut ctx = make_ctx();
        let tree = tree_with_body("head");
        let (shared, _guard) = install_current(tree);
        ctx.eval(Source::from_bytes(r#"__appendBody(" tail")"#))
            .unwrap();
        drop(_guard);
        assert_eq!(body_text_content(&shared.borrow()), "head tail");
    }

    #[test]
    fn js_set_title_updates_title_element() {
        let mut ctx = make_ctx();
        let tree = tree_with_body("x");
        let (shared, _guard) = install_current(tree);
        ctx.eval(Source::from_bytes(r#"__setTitle("new title")"#))
            .unwrap();
        drop(_guard);
        let t = shared.borrow();
        let head = find_first_element(&t, "head").unwrap();
        let title = find_child_element(&t, head, "title").unwrap();
        let text_id = t.children_of(title)[0];
        if let NodeData::Text(s) = t.data(text_id) {
            assert_eq!(s, "new title");
        } else {
            panic!("expected text node");
        }
    }

    #[test]
    fn js_log_does_not_panic() {
        let mut ctx = make_ctx();
        let tree = tree_with_body("");
        let (_shared, _guard) = install_current(tree);
        let _ = ctx.eval(Source::from_bytes(r#"__log("hello")"#));
    }

    #[test]
    fn js_set_body_with_html_like_string() {
        let mut ctx = make_ctx();
        let tree = tree_with_body("");
        let (shared, _guard) = install_current(tree);
        ctx.eval(Source::from_bytes(r#"__setBody("<h1>title</h1>")"#))
            .unwrap();
        drop(_guard);
        assert_eq!(body_text_content(&shared.borrow()), "<h1>title</h1>");
    }

    #[test]
    fn js_calling_unknown_function_returns_error() {
        let mut ctx = make_ctx();
        let tree = tree_with_body("");
        let (_shared, _guard) = install_current(tree);
        let result = ctx.eval(Source::from_bytes("__nonexistent()"));
        assert!(result.is_err());
    }
}

// M16.2 (done): setTimeout / clearTimeout 已实现，见上面的
// set_timeout_bridge / clear_timeout_bridge + drain_due_timer_callbacks。
// 决策见 docs/decisions/0002-boa-settimeout-vs-deno-core.md（推翻了
// M7.3 当初的 defer 理由——boa 0.20 的 NativeFunction::call /
// JsFunction::call 其实都是 pub）。event loop 接入在 M16.3 完成。

#[cfg(all(test, feature = "boa"))]
mod fetch_tests {
    use super::*;
    use boa_engine::Source;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn tree_with_body(body_text: &str) -> Tree {
        let html = format!("<html><head><title>x</title></head><body>{body_text}</body></html>");
        browser_html_parser::parse(&html)
    }

    fn make_ctx() -> boa_engine::Context {
        let mut ctx = boa_engine::Context::default();
        install(&mut ctx);
        ctx
    }

    #[tokio::test]
    async fn js_fetch_set_body_replaces_body_with_response() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/data"))
            .respond_with(ResponseTemplate::new(200).set_body_string("fetched content"))
            .mount(&server)
            .await;
        let url = format!("{}/data", server.uri());

        let mut ctx = make_ctx();
        let tree = tree_with_body("placeholder");
        let (shared, _guard) = install_current(tree);
        let script = format!(r#"__fetchSetBody("{}")"#, url);
        ctx.eval(Source::from_bytes(&script)).unwrap();
        drop(_guard);
        assert_eq!(body_text_content(&shared.borrow()), "fetched content");
    }

    #[tokio::test]
    async fn js_fetch_append_body_adds_response_to_body() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/tail"))
            .respond_with(ResponseTemplate::new(200).set_body_string(" appended"))
            .mount(&server)
            .await;
        let url = format!("{}/tail", server.uri());

        let mut ctx = make_ctx();
        let tree = tree_with_body("head");
        let (shared, _guard) = install_current(tree);
        let script = format!(r#"__fetchAppendBody("{}")"#, url);
        ctx.eval(Source::from_bytes(&script)).unwrap();
        drop(_guard);
        assert_eq!(body_text_content(&shared.borrow()), "head appended");
    }

    #[tokio::test]
    async fn js_fetch_404_logs_error_and_leaves_body_unchanged() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/missing"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        let url = format!("{}/missing", server.uri());

        let mut ctx = make_ctx();
        let tree = tree_with_body("original");
        let (shared, _guard) = install_current(tree);
        let script = format!(r#"__fetchSetBody("{}")"#, url);
        // Script should not throw — error is logged to stderr.
        ctx.eval(Source::from_bytes(&script)).unwrap();
        drop(_guard);
        // Body untouched.
        assert_eq!(body_text_content(&shared.borrow()), "original");
    }

    #[test]
    fn js_fetch_with_empty_url_is_noop() {
        let mut ctx = make_ctx();
        let tree = tree_with_body("untouched");
        let (shared, _guard) = install_current(tree);
        // Empty string → no fetch attempt, no panic.
        ctx.eval(Source::from_bytes(r#"__fetchSetBody("")"#))
            .unwrap();
        drop(_guard);
        assert_eq!(body_text_content(&shared.borrow()), "untouched");
    }

    #[test]
    fn resolve_url_passthrough_for_absolute() {
        assert_eq!(
            resolve_url("https://example.com/x"),
            "https://example.com/x"
        );
    }

    #[test]
    fn resolve_url_no_base_returns_input_unchanged() {
        // No base installed → can't resolve, return as-is.
        assert_eq!(resolve_url("/api/x"), "/api/x");
    }

    #[test]
    fn resolve_url_with_base_joins_relative() {
        // We need a Tree to install (the guard's contract). Just
        // build an empty one.
        use std::cell::RefCell;
        use std::rc::Rc;
        let empty_tree = Tree::with_root(browser_dom::NodeData::Document);
        let shared = Rc::new(RefCell::new(empty_tree));
        let _guard =
            install_shared_with_base(shared, Some("https://example.com/page/index.html".into()));
        assert_eq!(resolve_url("/api/x"), "https://example.com/api/x");
        assert_eq!(resolve_url("api/y"), "https://example.com/page/api/y");
        assert_eq!(resolve_url("../z"), "https://example.com/z");
        // Absolute URLs are returned unchanged.
        assert_eq!(
            resolve_url("https://other.com/foo"),
            "https://other.com/foo"
        );
    }

    #[tokio::test]
    async fn js_fetch_two_urls_chained_appends_both() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/a"))
            .respond_with(ResponseTemplate::new(200).set_body_string("A"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/b"))
            .respond_with(ResponseTemplate::new(200).set_body_string("B"))
            .mount(&server)
            .await;

        let mut ctx = make_ctx();
        let tree = tree_with_body("");
        let (shared, _guard) = install_current(tree);
        let base = server.uri();
        let script = format!(r#"__fetchAppendBody("{base}/a"); __fetchAppendBody("{base}/b");"#);
        ctx.eval(Source::from_bytes(&script)).unwrap();
        drop(_guard);
        assert_eq!(body_text_content(&shared.borrow()), "AB");
    }
}

// ===========================================================================
// M66-B: QuickJS 后端的公开 bridge 辅助函数。
// 这些函数是对 with_tree / thread_local 后端的轻量封装，
// 让 QuickJS 引擎可以复用同样的 DOM/Storage/Navigation 后端。
// ===========================================================================
#[cfg(feature = "quickjs")]
mod quickjs_bridge_helpers {
    // TODO: M66-B 完整实现时填充。
    // 这些函数需要调用 bridge.rs 的私有 helper（set_body_inner_html 等），
    // 但 Tree 没有高级方法（create_element 等），需要用 with_tree + 私有 helper。
    // 暂时留空，feature=quickjs 时编译 engine_quickjs.rs 会引用这些。
}

// ===========================================================================
// M66-B: QuickJS bridge 公开函数。
// 复用 bridge.rs 的私有 helper（with_tree/find_by_selector 等），
// 但接受 Rust 原生类型（f64/String/Option）而非 boa JsValue。
// ===========================================================================

#[cfg(feature = "quickjs")]
#[allow(
    clippy::needless_borrow,
    unused_variables,
    clippy::uninlined_format_args,
    clippy::duplicated_attributes
)]
pub mod qjs_bridge {
    use super::*;

    /// log(msg) —— JS 日志输出。
    pub fn log(msg: String) {
        eprintln!("[js] {msg}");
    }

    /// createEl(tag) -> NodeId —— 创建元素，挂到 body。
    /// M78.45: createDetachedEl(tag) -> NodeId——游离元素（不挂 body）。
    /// WPT createElement 语义：新元素不在文档中（removeChild 应抛 NotFound）。
    /// appendChild/insertBefore 照常可插入。
    pub fn create_detached_el(tag: String) -> f64 {
        let tag = if tag.is_empty() {
            "div".to_string()
        } else {
            tag
        };
        with_tree(|t| {
            t.insert(
                None,
                NodeData::Element {
                    tag,
                    attrs: Vec::new(),
                },
            ) as f64
        })
    }

    pub fn create_el(tag: String) -> f64 {
        let tag = if tag.is_empty() {
            "div".to_string()
        } else {
            tag
        };
        with_tree(|t| {
            let parent = find_first_element(t, "body").unwrap_or_else(|| t.root());
            t.insert(
                Some(parent),
                NodeData::Element {
                    tag,
                    attrs: Vec::new(),
                },
            ) as f64
        })
    }

    /// appendChild(parent, child) —— 移动子树。
    pub fn append_child(parent: f64, child: f64) {
        with_tree(|t| move_subtree(t, parent as usize, child as usize));
    }

    /// insertBefore(parent, child, ref)。
    pub fn insert_before(parent: f64, child: f64, reference: f64) {
        with_tree(|t| {
            insert_before_inner(t, parent as usize, child as usize, Some(reference as usize))
        });
    }

    /// removeChild(parent, child)。
    /// M78.58-修二: 摘除后 detach（parent=None）——旧 move-to-root 在游离
    /// 语义（M78.45）后让已删节点被 querySelector 复活。
    pub fn remove_child(parent: f64, child: f64) {
        with_tree(|t| {
            let (pid, cid) = (parent as usize, child as usize);
            if pid >= t.len() || cid >= t.len() {
                return;
            }
            t.get_mut(pid).children.retain(|&c| c != cid);
            t.get_mut(cid).parent = None;
        });
    }

    /// setText(id, text) —— 设置文本内容。
    pub fn set_text(id: f64, text: String) {
        with_tree(|t| set_text_inner(t, id as usize, &text));
    }

    /// getText(id) -> String —— 获取文本内容。
    pub fn get_text(id: f64) -> String {
        with_tree(|t| collect_text(t, id as usize))
    }

    /// getTag(id) -> String —— 获取标签名。
    pub fn get_tag(id: f64) -> String {
        with_tree(|t| match t.data(id as usize) {
            NodeData::Element { tag, .. } => tag.clone(),
            _ => String::new(),
        })
    }

    /// M67: getTagByName(tag) -> f64 —— 按标签名找第一个匹配节点的 NodeId。
    /// 找不到返回 -1.0（CDP/JS 侧判 < 0）。对齐 boa `get_tag` 的字符串模式。
    pub fn get_tag_by_name(tag: String) -> f64 {
        let t = tag.trim().to_lowercase();
        if t.is_empty() {
            return -1.0;
        }
        with_tree(|tree| match crate::bridge::find_first_element(tree, &t) {
            Some(id) => id as f64,
            None => -1.0,
        })
    }

    /// getAttr(id, key) -> Option<String>。
    pub fn get_attr(id: f64, key: String) -> Option<String> {
        with_tree(|t| {
            if let NodeData::Element { attrs, .. } = t.data(id as usize) {
                attrs
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case(&key))
                    .map(|(_, v)| v.clone())
            } else {
                None
            }
        })
    }

    /// setAttr(id, key, val)。
    pub fn set_attr(id: f64, key: String, val: String) {
        with_tree(|t| set_attr_inner(t, id as usize, &key, &val));
    }

    /// removeAttr(id, key)。
    pub fn remove_attr(id: f64, key: String) {
        with_tree(|t| remove_attr_inner(t, id as usize, &key));
    }

    /// getElById(id) -> NodeId（-1 = 未找到）。
    pub fn get_el_by_id(id: String) -> f64 {
        with_tree(|t| find_by_id(t, &id).map(|n| n as f64).unwrap_or(-1.0))
    }

    /// qs(selector) -> NodeId（-1 = 未找到）。
    pub fn qs(selector: String) -> f64 {
        with_tree(|t| {
            find_by_selector(t, &selector)
                .map(|n| n as f64)
                .unwrap_or(-1.0)
        })
    }

    /// qsAll(selector) -> 逗号分隔 NodeId 字符串。
    pub fn qs_all(selector: String) -> String {
        with_tree(|t| {
            find_all_by_selector(t, &selector)
                .iter()
                .map(|n| n.to_string())
                .collect::<Vec<_>>()
                .join(",")
        })
    }

    /// qsMatch(nodeId, selector) -> bool —— Element.matches() 后端。
    pub fn qs_match(node_id: f64, selector: String) -> bool {
        with_tree(|t| {
            let id = node_id as usize;
            if id >= t.len() {
                return false;
            }
            // M78.7: 委托 css-engine（完整组合器/属性/伪类；解析失败=不匹配）。
            match browser_css_engine::Selector::parse(selector.trim()) {
                Ok(s) => s.matches(t, id),
                Err(_) => false,
            }
        })
    }

    /// qsClosest(nodeId, selector) -> NodeId (-1 未匹配) —— Element.closest() 后端。
    /// 从 node 往上走 parent 链，找第一个匹配 selector 的祖先（含自身）。
    pub fn qs_closest(node_id: f64, selector: String) -> f64 {
        with_tree(|t| {
            let mut id = node_id as usize;
            if id >= t.len() {
                return -1.0;
            }
            // M78.7: 委托 css-engine 选择器（解析失败=不匹配）。
            let parsed = match browser_css_engine::Selector::parse(selector.trim()) {
                Ok(s) => s,
                Err(_) => return -1.0,
            };
            loop {
                if parsed.matches(t, id) {
                    return id as f64;
                }
                match t.get(id).parent {
                    Some(p) => {
                        if p == id {
                            break;
                        }
                        id = p;
                    }
                    None => break,
                }
            }
            -1.0
        })
    }

    /// M78: offsetWidth(id) -> f64 —— 经 css-engine mini 级联取元素 width。
    /// 收集当前 tree 里所有 `<style>` 文本 → parse → compute_styles →
    /// 取该元素最后一条 width 声明的 px 值（近似：仅显式 px 宽，无 px 返回 0）。
    /// WPT :lang 系列测试靠它断言 `#box:lang(es){width:100px}` 生效。
    pub fn offset_width(node_id: f64) -> f64 {
        with_tree(|t| {
            let id = node_id as usize;
            if id >= t.len() || !matches!(t.data(id), NodeData::Element { .. }) {
                return 0.0;
            }
            let mut css = String::new();
            t.traverse(t.root(), |sid, node| {
                if let NodeData::Element { tag, .. } = &node.data {
                    if tag.eq_ignore_ascii_case("style") {
                        css.push_str(&collect_text(t, sid));
                        css.push('\n');
                    }
                }
                true
            });
            if css.trim().is_empty() {
                return 0.0;
            }
            let sheet = browser_css_engine::parse(&css);
            let styles = browser_css_engine::compute_styles(t, &sheet);
            let Some(decls) = styles.get(&id) else {
                return 0.0;
            };
            // display:none → 元素不渲染，offsetWidth 为 0（WPT :lang 控制元素靠它断言）。
            if let Some(d) = decls
                .iter()
                .rev()
                .find(|d| d.property.eq_ignore_ascii_case("display"))
            {
                if d.value.trim().eq_ignore_ascii_case("none") {
                    return 0.0;
                }
            }
            decls
                .iter()
                .rev()
                .find(|d| d.property.eq_ignore_ascii_case("width"))
                .and_then(|d| match browser_css_engine::parse_length(&d.value) {
                    Some(browser_css_engine::Length::Px(v)) => Some(f64::from(v)),
                    _ => None,
                })
                .unwrap_or(0.0)
        })
    }

    /// M78: visibleBodyTextLen() —— body 可见文本长度（跳过 script/style/noscript/
    /// textarea 的文本）。M77 的 dom_ready 早退用 __getText(body) 把 inline script
    /// 源码也当可见文本，WPT 页面（body 内嵌大段 JS）被误判"内容已就绪"而提前
    /// 退出事件循环，testharness 完成链路被掐断（跨类别 no-results 的根因之一）。
    pub fn visible_body_text_len() -> f64 {
        fn walk(t: &Tree, id: NodeId, out: &mut usize) {
            for &child in t.children_of(id) {
                match t.data(child) {
                    NodeData::Text(s) => *out += s.trim().len(),
                    NodeData::Element { tag, .. } => {
                        let skip = tag.eq_ignore_ascii_case("script")
                            || tag.eq_ignore_ascii_case("style")
                            || tag.eq_ignore_ascii_case("noscript")
                            || tag.eq_ignore_ascii_case("template");
                        if !skip {
                            walk(t, child, out);
                        }
                    }
                    _ => {}
                }
            }
        }
        with_tree(|t| {
            let mut len = 0usize;
            if let Some(body) = find_first_element(t, "body") {
                walk(t, body, &mut len);
            }
            len as f64
        })
    }

    /// M78: allIds() -> 逗号分隔的所有 id 属性值（window 命名访问用）。
    /// WPT 大量测试直接裸引用元素 id（`div2_3`）——HTML 规范的 named access。
    pub fn all_ids() -> String {
        with_tree(|t| {
            let mut ids = Vec::new();
            t.traverse(t.root(), |_, node| {
                if let NodeData::Element { attrs, .. } = &node.data {
                    if let Some((_, v)) = attrs.iter().find(|(k, _)| k == "id") {
                        if !v.is_empty() {
                            ids.push(v.clone());
                        }
                    }
                }
                true
            });
            ids.join(",")
        })
    }

    /// M78.10: textData(id) -> 节点自身 Text data（__getText 聚合子树，
    /// 对文本节点本身返回空；innerText getter 逐节点遍历需要自身值）。
    pub fn text_data(node_id: f64) -> String {
        with_tree(|t| match t.data(node_id as usize) {
            NodeData::Text(s) => s.clone(),
            _ => String::new(),
        })
    }

    /// M78: attrsOf(id) -> "k=v\nk=v" —— element.attributes（NamedNodeMap）反射。
    pub fn attrs_of(node_id: f64) -> String {
        with_tree(|t| {
            if let NodeData::Element { attrs, .. } = t.data(node_id as usize) {
                attrs
                    .iter()
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            } else {
                String::new()
            }
        })
    }

    /// getBody() -> NodeId。
    pub fn get_body() -> f64 {
        with_tree(|t| {
            find_first_element(t, "body")
                .map(|n| n as f64)
                .unwrap_or(-1.0)
        })
    }

    /// setBody(html) —— 替换 `<body>` 的全部内容（清空子节点 + 插入文本）。
    /// 与 boa 的 `__setBody`（bridge.rs set_body）行为一致：走 set_body_inner_html，
    /// 而非把 "innerHTML" 当普通 attribute 设置（那样渲染时仍读旧子节点）。
    pub fn set_body(html: String) {
        with_tree(|t| set_body_inner_html(t, &html));
    }

    /// appendBody(html) —— 追加文本到 `<body>` 末尾（不清空已有内容）。
    /// 镜像 boa 的 `__appendBody`（走 append_body_text）。
    pub fn append_body(html: String) {
        with_tree(|t| append_body_text(t, &html));
    }

    /// fetchSetBody(url) —— 同步 fetch url，成功后替换 `<body>` 内容。
    /// 镜像 boa 的 `__fetchSetBody`（bridge.rs fetch_set_body）。
    pub fn fetch_set_body(url: String) {
        if url.is_empty() {
            return;
        }
        let resolved = resolve_url(&url);
        match super::fetch_sync(&resolved) {
            Ok(text) => with_tree(|t| set_body_inner_html(t, &text)),
            Err(e) => eprintln!("[js-fetch] {url} failed: {e}"),
        }
    }

    /// fetchAppendBody(url) —— 同步 fetch url，成功后把文本追加到 `<body>`。
    /// 镜像 boa 的 `__fetchAppendBody`（bridge.rs fetch_append_body）。
    pub fn fetch_append_body(url: String) {
        if url.is_empty() {
            return;
        }
        let resolved = resolve_url(&url);
        match super::fetch_sync(&resolved) {
            Ok(text) => with_tree(|t| append_body_text(t, &text)),
            Err(e) => eprintln!("[js-fetch] {url} failed: {e}"),
        }
    }

    /// setTitle(title)。
    pub fn set_title(title: String) {
        with_tree(|t| set_title_text(t, &title));
    }

    /// getParent(id) -> NodeId。
    pub fn get_parent(id: f64) -> f64 {
        with_tree(|t| t.get(id as usize).parent.map(|n| n as f64).unwrap_or(-1.0))
    }

    /// children(id) -> 逗号分隔 NodeId 字符串。
    pub fn children(id: f64) -> String {
        with_tree(|t| {
            t.children_of(id as usize)
                .iter()
                .map(|n| n.to_string())
                .collect::<Vec<_>>()
                .join(",")
        })
    }

    /// fetchSync(url) -> Option<String> —— 同步 HTTP fetch。
    pub fn fetch_sync(url: String) -> Option<String> {
        let resolved = resolve_url(&url);
        super::fetch_sync(&resolved).ok()
    }

    /// fetchSyncMethod(url, method, body, contentType) -> Option<String>。
    pub fn fetch_sync_method(
        url: String,
        method: String,
        body: Option<String>,
        ct: Option<String>,
    ) -> Option<String> {
        let resolved = resolve_url(&url);
        super::fetch_sync_with_method(&resolved, &method, body.as_deref(), ct.as_deref())
            .ok()
            .map(|(status, b)| format!("{status}\n{b}"))
    }

    /// storageGet(key) -> Option<String>。
    pub fn storage_get(key: String) -> Option<String> {
        with_storage(|h| browser_storage::storage_get(&h, &key))
    }

    /// storageSet(key, val)。
    pub fn storage_set(key: String, val: String) {
        with_storage(|h| browser_storage::storage_set(&h, &key, &val));
    }

    /// storageRemove(key)。
    pub fn storage_remove(key: String) {
        with_storage(|h| browser_storage::storage_remove(&h, &key));
    }

    /// locationHref() -> String。
    pub fn location_href() -> String {
        with_navigation(|h| browser_navigation::current_url(&h))
    }

    /// parseHtml(targetNodeId, htmlString) —— 用 html5ever 解析 HTML，
    /// 创建真实 DOM 子节点到目标元素。复用 boa 版本的 parse_html 逻辑。
    pub fn parse_html(target: f64, html: String) {
        let node_id = target as usize;
        with_tree(|t| {
            if node_id >= t.len() {
                return;
            }
            // innerHTML 语义 = 片段解析（见 boa 版 parse_html 注释）：
            // document 解析会把片段开头的 <script> 挪进 <head>，setter 丢节点
            let parsed = browser_html_parser::parse_fragment(&html);
            let body_id = super::find_first_element(&parsed, "body");
            if let Some(body_id) = body_id {
                t.get_mut(node_id).children.clear();
                let children = parsed.children_of(body_id).to_vec();
                for &child in &children {
                    super::copy_subtree(&parsed, child, t, node_id);
                }
            } else {
                t.get_mut(node_id).children.clear();
                t.insert(Some(node_id), NodeData::Text(html));
            }
        });
    }
}

#[cfg(all(test, feature = "boa"))]
mod m7_dom_api_tests {
    use super::*;
    use boa_engine::Source;

    fn tree_with_body(body_html: &str) -> Tree {
        let html = format!("<html><head><title>t</title></head><body>{body_html}</body></html>");
        browser_html_parser::parse(&html)
    }

    fn make_ctx() -> boa_engine::Context {
        let mut ctx = boa_engine::Context::default();
        install(&mut ctx);
        ctx
    }

    #[test]
    fn js_create_el_returns_nodeid_under_body() {
        let mut ctx = make_ctx();
        let tree = tree_with_body("");
        let (shared, _guard) = install_current(tree);
        let result = ctx.eval(Source::from_bytes(r#"__createEl("div")"#));
        let val = result.unwrap();
        let id = val.as_number().unwrap() as usize;
        assert!(id > 0, "id should be a positive usize, got {id}");
        drop(_guard);
        let t = shared.borrow();
        let body = find_first_element(&t, "body").unwrap();
        assert!(
            t.children_of(body).contains(&id),
            "new element should be under <body>"
        );
        assert!(matches!(t.data(id), NodeData::Element { tag, .. } if tag == "div"));
    }

    #[test]
    fn js_set_text_clears_children_and_inserts_text() {
        let mut ctx = make_ctx();
        let tree = tree_with_body("<p id='t'>old</p>");
        let (shared, _guard) = install_current(tree);
        ctx.eval(Source::from_bytes(
            r#"(function(){ var p = __getElById("t"); __setText(p, "fresh"); })()"#,
        ))
        .unwrap();
        drop(_guard);
        let t = shared.borrow();
        let p = find_by_id(&t, "t").unwrap();
        let children = t.children_of(p);
        assert_eq!(children.len(), 1);
        assert!(matches!(t.data(children[0]), NodeData::Text(s) if s == "fresh"));
    }

    #[test]
    fn js_set_attr_adds_and_updates_attribute() {
        let mut ctx = make_ctx();
        let tree = tree_with_body("<p id='t'>x</p>");
        let (shared, _guard) = install_current(tree);
        ctx.eval(Source::from_bytes(
            r#"(function(){ var p = __getElById("t"); __setAttr(p, "class", "red"); })()"#,
        ))
        .unwrap();
        drop(_guard);
        let t = shared.borrow();
        let p = find_by_id(&t, "t").unwrap();
        match t.data(p) {
            NodeData::Element { attrs, .. } => {
                // The element already has id='t' from <p id='t'>;
                // __setAttr(p, 'class', 'red') should add class='red'.
                let class_attr = attrs.iter().find(|(k, _)| k == "class");
                assert_eq!(class_attr, Some(&("class".to_string(), "red".to_string())));
            }
            _ => panic!("expected element"),
        }
    }

    #[test]
    fn js_get_el_by_id_returns_minus_one_when_missing() {
        let mut ctx = make_ctx();
        let tree = tree_with_body("<p>nothing here</p>");
        let (_shared, _guard) = install_current(tree);
        let val = ctx
            .eval(Source::from_bytes(r#"__getElById("nope")"#))
            .unwrap();
        assert_eq!(val.as_number(), Some(-1.0));
    }

    #[test]
    fn js_qs_finds_by_tag() {
        let mut ctx = make_ctx();
        let tree = tree_with_body("<section><p>hi</p></section>");
        let (shared, _guard) = install_current(tree);
        let val = ctx.eval(Source::from_bytes(r#"__qs("section")"#)).unwrap();
        let id = val.as_number().unwrap() as usize;
        drop(_guard);
        let t = shared.borrow();
        assert!(matches!(t.data(id), NodeData::Element { tag, .. } if tag == "section"));
    }

    #[test]
    fn js_qs_finds_by_id_with_hash_prefix() {
        let mut ctx = make_ctx();
        let tree = tree_with_body("<div id='a'><p>x</p></div>");
        let (_shared, _guard) = install_current(tree);
        let val = ctx.eval(Source::from_bytes(r##"__qs("#a")"##)).unwrap();
        assert!(val.as_number().unwrap() > 0.0);
    }

    #[test]
    fn js_append_child_moves_subtree() {
        let mut ctx = make_ctx();
        // <body><div id="a"></div><span id="b">x</span></body>
        // After appendChild(a, b), <div id="a"><span id="b">x</span></div>
        let tree = tree_with_body(r#"<div id="a"></div><span id="b">x</span>"#);
        let (shared, _guard) = install_current(tree);
        ctx.eval(Source::from_bytes(
            r#"(function(){
                var a = __getElById("a");
                var b = __getElById("b");
                __appendChild(a, b);
            })()"#,
        ))
        .unwrap();
        drop(_guard);
        let t = shared.borrow();
        let a = find_by_id(&t, "a").unwrap();
        let b = find_by_id(&t, "b").unwrap();
        // <span id="b"> should now be a child of <div id="a">.
        assert!(
            t.children_of(a).contains(&b),
            "span was not moved under div; a's children = {:?}",
            t.children_of(a)
        );
    }

    #[test]
    fn qs_class_selector_matches_class_attr() {
        let mut ctx = make_ctx();
        // <div class="container">x</div><p class="red">y</p>
        let tree = tree_with_body(r#"<div class="container">x</div><p class="red">y</p>"#);
        let (shared, _guard) = install_current(tree);
        let val = ctx.eval(Source::from_bytes(r#"__qs(".red")"#)).unwrap();
        let id = val.as_number().unwrap() as usize;
        drop(_guard);
        let t = shared.borrow();
        let found = t.data(id);
        if let NodeData::Element { tag, attrs, .. } = found {
            assert_eq!(tag, "p");
            assert!(attrs.iter().any(|(k, v)| k == "class" && v.contains("red")));
        } else {
            panic!("Expected element, got {found:?}");
        }
    }

    #[test]
    fn qs_compound_tag_and_class() {
        let mut ctx = make_ctx();
        let tree = tree_with_body(r#"<div class="box">x</div><p class="box">y</p>"#);
        let (shared, _guard) = install_current(tree);
        // p.box should match <p class="box">, not <div class="box">.
        let val = ctx.eval(Source::from_bytes(r#"__qs("p.box")"#)).unwrap();
        let id = val.as_number().unwrap() as usize;
        drop(_guard);
        let t = shared.borrow();
        let found = t.data(id);
        if let NodeData::Element { tag, .. } = found {
            assert_eq!(tag, "p");
        } else {
            panic!("Expected element, got {found:?}");
        }
    }

    #[test]
    fn qs_multi_class_spaces_split() {
        let mut ctx = make_ctx();
        // <div class="container fluid">x</div>
        let tree = tree_with_body(r#"<div class="container fluid">x</div>"#);
        let (shared, _guard) = install_current(tree);
        // .fluid should match.
        let val = ctx.eval(Source::from_bytes(r#"__qs(".fluid")"#)).unwrap();
        let id = val.as_number().unwrap() as usize;
        drop(_guard);
        let t = shared.borrow();
        let found = t.data(id);
        if let NodeData::Element { attrs, .. } = found {
            assert!(attrs
                .iter()
                .any(|(k, v)| k == "class" && v.contains("fluid")));
        } else {
            panic!("Expected element, got {found:?}");
        }
    }

    #[test]
    fn qs_attribute_selector_matches_attr_exists() {
        let mut ctx = make_ctx();
        let tree = tree_with_body(r#"<div data-test="1">x</div><div id="b">y</div>"#);
        let (shared, _guard) = install_current(tree);
        let val = ctx
            .eval(Source::from_bytes(r#"__qs("[data-test]")"#))
            .unwrap();
        let id = val.as_number().unwrap() as usize;
        drop(_guard);
        let t = shared.borrow();
        let found = t.data(id);
        if let NodeData::Element { attrs, .. } = found {
            assert!(attrs.iter().any(|(k, v)| k == "data-test" && v == "1"));
        } else {
            panic!("Expected element, got {found:?}");
        }
    }

    #[test]
    fn qs_descendant_selector_uses_last_segment() {
        let mut ctx = make_ctx();
        let tree = tree_with_body(
            r#"<div class="box"><span class="target">A</span></div><section><span class="target" id="target-2">B</span></section>"#,
        );
        let (shared, _guard) = install_current(tree);
        let val = ctx
            .eval(Source::from_bytes(r#"__qs("section .target")"#))
            .unwrap();
        let id = val.as_number().unwrap() as usize;
        drop(_guard);
        let t = shared.borrow();
        let found = t.data(id);
        if let NodeData::Element { attrs, .. } = found {
            assert!(attrs.iter().any(|(k, v)| k == "class" && v == "target"));
        } else {
            panic!("Expected element, got {found:?}");
        }
    }

    #[test]
    fn js_get_body_returns_body_nodeid() {
        let mut ctx = make_ctx();
        let tree = tree_with_body("hello");
        let (shared, _guard) = install_current(tree);
        let val = ctx.eval(Source::from_bytes(r#"__getBody()"#)).unwrap();
        let id = val.as_number().unwrap() as usize;
        drop(_guard);
        let t = shared.borrow();
        assert!(matches!(t.data(id), NodeData::Element { tag, .. } if tag == "body"));
    }
}

#[cfg(all(test, feature = "boa"))]
mod m69_dynamic_script_tests {
    use super::{drain_dynamic_scripts, enqueue_dynamic_script};

    // 测试间隔离：每个 test 前清空队列，避免相互污染。
    fn reset_queue() {
        let _ = drain_dynamic_scripts();
    }

    #[test]
    fn drain_empty_returns_empty_vec() {
        reset_queue();
        let drained = drain_dynamic_scripts();
        assert!(drained.is_empty(), "empty queue should drain to empty vec");
    }

    #[test]
    fn enqueue_then_drain_returns_codes_in_fifo_order() {
        reset_queue();
        enqueue_dynamic_script("var a = 1;".to_string());
        enqueue_dynamic_script("var b = 2;".to_string());
        enqueue_dynamic_script("var c = 3;".to_string());
        let drained = drain_dynamic_scripts();
        assert_eq!(drained, vec!["var a = 1;", "var b = 2;", "var c = 3;"]);
    }

    #[test]
    fn drain_clears_queue() {
        reset_queue();
        enqueue_dynamic_script("code1".to_string());
        let first = drain_dynamic_scripts();
        assert_eq!(first.len(), 1);
        // 二次 drain 应该为空（已清空）
        let second = drain_dynamic_scripts();
        assert!(second.is_empty(), "drain should clear the queue");
    }

    #[test]
    fn enqueue_after_drain_works() {
        reset_queue();
        enqueue_dynamic_script("first".to_string());
        let _ = drain_dynamic_scripts();
        // drain 后可以继续 enqueue 新代码
        enqueue_dynamic_script("second".to_string());
        let drained = drain_dynamic_scripts();
        assert_eq!(drained, vec!["second"]);
    }
}

/// M71.3 GAP-I 回归测试：querySelector/querySelectorAll 对后代选择器（含空格）
/// 的处理。纯函数测试，不依赖任何 JS 引擎 feature。
/// 之前 bug：以 `#` 开头的复合选择器（如 `#dyn1 .p`）被 strip_prefix('#')
/// 误判为纯 id 选择器，导致 `find_by_id("dyn1 .p")` 落空。
#[cfg(test)]
mod gap_i_descendant_selector_tests {
    use super::*;
    use browser_dom::{NodeData, Tree};

    /// 构造一棵树：body > div#container > p.child，用于测后代选择器。
    fn build_tree() -> Tree {
        let mut t = Tree::new();
        // root(0) > html(1) > body(2)
        let html = t.insert(
            None,
            NodeData::Element {
                tag: "html".into(),
                attrs: vec![],
            },
        );
        let body = t.insert(
            Some(html),
            NodeData::Element {
                tag: "body".into(),
                attrs: vec![],
            },
        );
        // body > div#container
        let container = t.insert(
            Some(body),
            NodeData::Element {
                tag: "div".into(),
                attrs: vec![("id".into(), "container".into())],
            },
        );
        // div#container > p.child
        t.insert(
            Some(container),
            NodeData::Element {
                tag: "p".into(),
                attrs: vec![("class".into(), "child".into())],
            },
        );
        // body > p.lonely （不在 #container 内，用于验证后代约束差异）
        t.insert(
            Some(body),
            NodeData::Element {
                tag: "p".into(),
                attrs: vec![("class".into(), "lonely".into())],
            },
        );
        t
    }

    #[test]
    fn pure_id_selector_finds_node() {
        let t = build_tree();
        let found = find_by_selector(&t, "#container");
        assert!(found.is_some(), "纯 id 选择器 #container 应找到节点");
    }

    #[test]
    fn class_selector_finds_all_matching() {
        let t = build_tree();
        let found = find_all_by_selector(&t, ".child");
        assert_eq!(found.len(), 1, ".class 应匹配 1 个");
    }

    /// 核心回归：`#id .class`（以 # 开头的后代选择器）之前返回空，现在应匹配。
    #[test]
    fn descendant_selector_starting_with_hash_finds_child() {
        let t = build_tree();
        // GAP-I bug: 这个曾因 strip_prefix('#') 误判为纯 id 而返回空。
        let found = find_all_by_selector(&t, "#container .child");
        assert!(
            !found.is_empty(),
            "后代选择器 #container .child 应找到 .child 节点（GAP-I 修复前返回空）"
        );
    }

    /// querySelector（单个）版本的回归。
    #[test]
    fn descendant_selector_qs_starting_with_hash_finds_child() {
        let t = build_tree();
        let found = find_by_selector(&t, "#container .child");
        assert!(
            found.is_some(),
            "querySelector(#container .child) 应找到节点"
        );
    }

    /// 静态场景也覆盖：`div .child` 这种 tag 开头的后代选择器应正常工作。
    #[test]
    fn descendant_selector_tag_prefix_finds_child() {
        let t = build_tree();
        let found = find_all_by_selector(&t, "div .child");
        assert!(!found.is_empty(), "div .child 后代选择器应找到节点");
    }
}
