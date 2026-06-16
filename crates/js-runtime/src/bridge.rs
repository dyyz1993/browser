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

use boa_engine::{object::JsObject, Context, JsArgs, JsResult, JsValue, NativeFunction};
use browser_cookie::CookieHandle;
use browser_dom::{Node, NodeData, NodeId, Tree};
use browser_eventloop::{TimerId, TimerWheel};
use browser_navigation::NavigationHandle;
use browser_storage::StorageHandle;

/// A shared, mutate-able handle to the DOM tree that JS sees.
pub type SharedTree = Rc<RefCell<Tree>>;

thread_local! {
    static CURRENT_TREE: RefCell<Option<SharedTree>> = const { RefCell::new(None) };
    static BASE_URL: RefCell<Option<String>> = const { RefCell::new(None) };
    // M13.2: localStorage / sessionStorage backend.
    static CURRENT_STORAGE: RefCell<Option<StorageHandle>> = const { RefCell::new(None) };
    // M14.2: history / location backend.
    static CURRENT_NAV: RefCell<Option<NavigationHandle>> = const { RefCell::new(None) };
    // M15.3: cookie jar backend.
    static CURRENT_COOKIE: RefCell<Option<CookieHandle>> = const { RefCell::new(None) };
    // M16.2: setTimeout 后端。wheel 存时间+id，callbacks 存 JsObject（boa GC 保活）。
    static TIMER_WHEEL: RefCell<Option<TimerWheel>> = const { RefCell::new(None) };
    static TIMER_CALLBACKS: RefCell<Option<std::collections::HashMap<TimerId, JsObject>>> =
        const { RefCell::new(None) };
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
        // M16.2: 清理 timer slots（防止上一次 run_scripts 的 timer 残留）。
        TIMER_WHEEL.with(|slot| {
            *slot.borrow_mut() = None;
        });
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

fn arg_string(args: &[JsValue], idx: usize) -> Option<String> {
    args.get(idx)
        .and_then(|v| v.as_string().map(|s| s.to_std_string_escaped()))
}

/// Register all bridge globals on the given boa context.
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
}

type NativeFn = fn(&JsValue, &[JsValue], &mut Context) -> JsResult<JsValue>;

fn register_fn(ctx: &mut Context, name: &str, f: NativeFn) {
    let native = NativeFunction::from_fn_ptr(f);
    let _ = ctx.register_global_callable(name.into(), 1, native);
}

/// Register with arity 0 (variadic signature is the same — this is
/// purely a documentation marker for bridges that take no args and
/// match boa's `register_global_callable(name, 0, ...)` arity hint).
fn register_fn0(ctx: &mut Context, name: &str, f: NativeFn) {
    let native = NativeFunction::from_fn_ptr(f);
    let _ = ctx.register_global_callable(name.into(), 0, native);
}

fn register_fn1(ctx: &mut Context, name: &str, f: NativeFn) {
    let native = NativeFunction::from_fn_ptr(f);
    let _ = ctx.register_global_callable(name.into(), 1, native);
}

fn register_fn2(ctx: &mut Context, name: &str, f: NativeFn) {
    let native = NativeFunction::from_fn_ptr(f);
    let _ = ctx.register_global_callable(name.into(), 2, native);
}

fn register_fn3(ctx: &mut Context, name: &str, f: NativeFn) {
    let native = NativeFunction::from_fn_ptr(f);
    let _ = ctx.register_global_callable(name.into(), 3, native);
}

fn arg_usize(args: &[JsValue], idx: usize) -> Option<usize> {
    args.get(idx)
        .and_then(|v| v.as_number())
        .map(|n| n as usize)
}

fn arg_usize_or_none(args: &[JsValue], idx: usize) -> Option<usize> {
    match args.get(idx) {
        Some(v) if v.is_undefined() || v.is_null() => None,
        Some(v) => v.as_number().map(|n| n as usize),
        None => None,
    }
}

fn set_body(_this: &JsValue, args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let html = arg_string(args, 0).unwrap_or_default();
    with_tree(|t| set_body_inner_html(t, &html));
    Ok(JsValue::undefined())
}

fn append_body(_this: &JsValue, args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let text = arg_string(args, 0).unwrap_or_default();
    with_tree(|t| append_body_text(t, &text));
    Ok(JsValue::undefined())
}

fn set_title(_this: &JsValue, args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let title = arg_string(args, 0).unwrap_or_default();
    with_tree(|t| set_title_text(t, &title));
    Ok(JsValue::undefined())
}

fn log_fn(_this: &JsValue, args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let msg = args.get_or_undefined(0).display().to_string();
    eprintln!("[js] {msg}");
    Ok(JsValue::undefined())
}

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
fn fetch_sync_bridge(_this: &JsValue, args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let raw = args
        .first()
        .and_then(|v| v.as_string())
        .map(|s| s.to_std_string_escaped())
        .unwrap_or_default();
    if raw.is_empty() {
        return Ok(JsValue::String(boa_engine::JsString::from("")));
    }
    let url = resolve_url(&raw);
    eprintln!("[js-fetch-bridge] GET {url}");
    match fetch_sync(&url) {
        // 编码：首行 status=200，后续行是 body（body 可能含换行，用 splitn(2) 解）。
        Ok(body) => Ok(JsValue::String(boa_engine::JsString::from(format!(
            "200\n{body}"
        )))),
        Err(e) => {
            eprintln!("[js-fetch] {raw} failed: {e}");
            Ok(JsValue::String(boa_engine::JsString::from("")))
        }
    }
}

/// `__fetchSyncMethod(url, method, body?, contentType?) -> string`：
/// 通用 fetch 后端（任意 method）。M20.3：POST/PUT/DELETE 表单/API 调用。
/// 返回编码 `"status\nbody"`（成功）或 `""`（失败）。
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
        return Ok(JsValue::String(boa_engine::JsString::from("")));
    }
    let resolved = resolve_url(&url);
    eprintln!("[js-fetch-bridge] {method} {resolved}");
    match fetch_sync_with_method(&resolved, &method, body.as_deref(), content_type.as_deref()) {
        Ok((status, body)) => Ok(JsValue::String(boa_engine::JsString::from(format!(
            "{status}\n{body}"
        )))),
        Err(e) => {
            eprintln!("[js-fetch] {method} {url} failed: {e}");
            Ok(JsValue::String(boa_engine::JsString::from("")))
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
fn fetch_sync(url: &str) -> Result<String, String> {
    fetch_sync_with_method(url, "GET", None, None).map(|(_status, body)| body)
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
    let url_clone = url.clone();
    let handle = std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| format!("tokio runtime build failed: {e}"))?;
        let client = browser_net::HttpClient::new();
        let result = rt.block_on(client.request_full_str(
            &url_clone,
            &method,
            body.as_deref(),
            content_type.as_deref(),
            cookie_header.as_deref(),
        ));
        let (status, bytes, headers) = result.map_err(|e| format!("{e:?}"))?;
        // 收集所有 Set-Cookie 行返回给主线程写 jar。
        let set_cookies: Vec<String> = headers
            .get_all("set-cookie")
            .iter()
            .filter_map(|v| v.to_str().ok().map(String::from))
            .collect();
        let body = String::from_utf8(bytes).map_err(|e| format!("non-utf8 response: {e}"))?;
        Ok::<_, String>((status, body, set_cookies))
    });
    let (status, body, set_cookies) = handle
        .join()
        .map_err(|_| "fetch thread panicked".to_string())??;
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
fn get_children(_this: &JsValue, args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let parent_id = match arg_usize(args, 0) {
        Some(id) => id,
        None => return Ok(JsValue::String(boa_engine::JsString::from(""))),
    };
    let child_ids = with_tree(|t| {
        t.children_of(parent_id)
            .iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>()
            .join(",")
    });
    Ok(JsValue::String(boa_engine::JsString::from(child_ids)))
}

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

fn qs(_this: &JsValue, args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    // M7.2.1 minimal querySelector: tag selectors (`div`) and id (`#foo`).
    let sel = arg_string(args, 0).unwrap_or_default();
    let found = with_tree(|t| find_by_selector(t, &sel));
    Ok(JsValue::new(match found {
        Some(id) => id as f64,
        None => -1.0,
    }))
}

fn find_by_selector(tree: &Tree, sel: &str) -> Option<NodeId> {
    let sel = sel.trim();
    if let Some(tag) = sel.strip_prefix('#') {
        return find_by_id(tree, tag);
    }
    let segments: Vec<&str> = sel
        .split_whitespace()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    let tokens = if segments.len() > 1 {
        tokenize_selector(segments.last().copied().unwrap_or(sel))
    } else {
        tokenize_selector(sel)
    };
    // M7.2.4: universal selector (*), class selector (.foo), id, tag, and
    // limited attribute selector ([foo], [foo=value]).
    // For 简化版 descendant 选择器，仅匹配最后一段 selector。
    let mut found = None;
    tree.traverse(tree.root(), |id, node| {
        if matches_selector(node, &tokens) {
            found = Some(id);
            return false;
        }
        true
    });
    found
}

/// Simple tokenizer for M7.2.4 selectors. Splits into:
/// - "*" (universal)
/// - "div", "p", "h1" (tags)
/// - ".container", ".red" (class)
///
/// – no support for combinators (space, >, +) yet (deferred to M7.2.5).
fn tokenize_selector(sel: &str) -> Vec<SelectorToken> {
    let mut tokens = Vec::new();
    let mut chars = sel.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_ascii_whitespace() {
            chars.next();
            continue;
        } else if c == '*' {
            tokens.push(SelectorToken::Universal);
            chars.next();
        } else if c == '#' {
            chars.next();
            let mut id = String::new();
            while let Some(&next) = chars.peek() {
                if next.is_ascii_alphanumeric() || next == '-' || next == '_' {
                    id.push(next);
                    chars.next();
                } else {
                    break;
                }
            }
            if !id.is_empty() {
                tokens.push(SelectorToken::Id(id));
            } else {
                // 无效 id 选择器，丢弃该符号防止死循环
                tokens.push(SelectorToken::Universal);
            }
        } else if c == '.' {
            chars.next();
            let mut cls = String::new();
            while let Some(&next) = chars.peek() {
                if next.is_ascii_alphanumeric() || next == '-' {
                    cls.push(next);
                    chars.next();
                } else {
                    break;
                }
            }
            if !cls.is_empty() {
                tokens.push(SelectorToken::Class(cls));
            }
        } else if c == '[' {
            chars.next();
            while let Some(&space) = chars.peek() {
                if space.is_ascii_whitespace() {
                    chars.next();
                } else {
                    break;
                }
            }
            let mut attr = String::new();
            while let Some(&next) = chars.peek() {
                if next.is_ascii_alphanumeric() || next == '-' || next == '_' {
                    attr.push(next);
                    chars.next();
                } else {
                    break;
                }
            }
            while let Some(&space) = chars.peek() {
                if space.is_ascii_whitespace() {
                    chars.next();
                } else {
                    break;
                }
            }
            if let Some(&'=') = chars.peek() {
                chars.next();
                while let Some(&space) = chars.peek() {
                    if space.is_ascii_whitespace() {
                        chars.next();
                    } else {
                        break;
                    }
                }
                let mut value = String::new();
                if let Some(&quote) = chars.peek() {
                    if quote == '\'' || quote == '"' {
                        let q = quote;
                        chars.next();
                        while let Some(&next) = chars.peek() {
                            if next == q {
                                break;
                            }
                            value.push(next);
                            chars.next();
                        }
                        if let Some(&_q) = chars.peek() {
                            chars.next();
                        }
                    } else {
                        while let Some(&next) = chars.peek() {
                            if next.is_ascii_whitespace() || next == ']' {
                                break;
                            }
                            value.push(next);
                            chars.next();
                        }
                    }
                }
                while let Some(&space) = chars.peek() {
                    if space.is_ascii_whitespace() {
                        chars.next();
                    } else {
                        break;
                    }
                }
                if let Some(&']') = chars.peek() {
                    chars.next();
                }
                if !attr.is_empty() {
                    tokens.push(SelectorToken::AttrEquals(attr, value));
                }
            } else {
                while let Some(&ch) = chars.peek() {
                    if ch == ']' {
                        break;
                    }
                    chars.next();
                }
                if let Some(&']') = chars.peek() {
                    chars.next();
                }
                if !attr.is_empty() {
                    tokens.push(SelectorToken::AttrExists(attr));
                }
            }
        } else {
            // Tag name.
            let mut tag = String::new();
            while let Some(&next) = chars.peek() {
                if next.is_ascii_alphanumeric() || next == '-' {
                    tag.push(next);
                    chars.next();
                } else {
                    break;
                }
            }
            if !tag.is_empty() {
                tokens.push(SelectorToken::Tag(tag));
            } else {
                // 其他字符（如 ':'、'['、']'）不应停滞，直接消费避免死循环。
                let _ = chars.next();
            }
        }
    }
    tokens
}

#[derive(Debug, Clone, PartialEq)]
enum SelectorToken {
    Universal,
    Tag(String),
    Class(String),
    Id(String),
    AttrExists(String),
    AttrEquals(String, String),
}

fn matches_selector(node: &Node, tokens: &[SelectorToken]) -> bool {
    if let NodeData::Element { tag, attrs } = &node.data {
        for tok in tokens {
            let ok = match tok {
                SelectorToken::Universal => true,
                SelectorToken::Tag(t) => t.eq_ignore_ascii_case(tag),
                SelectorToken::Class(cls) => attrs.iter().any(|(k, v)| {
                    k.eq_ignore_ascii_case("class") && v.split_whitespace().any(|c| c == cls)
                }),
                SelectorToken::Id(id) => attrs
                    .iter()
                    .any(|(k, v)| k.eq_ignore_ascii_case("id") && v == id),
                SelectorToken::AttrExists(attr) => attrs.iter().any(|(k, _)| k == attr),
                SelectorToken::AttrEquals(attr, value) => {
                    attrs.iter().any(|(k, v)| k == attr && v == value)
                }
            };
            if !ok {
                return false;
            }
        }
        true
    } else {
        false
    }
}

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
    tree.get_mut(id).children.clear();
    tree.insert(Some(id), NodeData::Text(text.into()));
}

/// M37: `__getText(id) -> string` — 读元素文本内容（拼接所有子文本节点）。
/// Element 对象的 textContent getter 需要此桥。
fn get_text(_this: &JsValue, args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let id = match arg_usize(args, 0) {
        Some(id) => id,
        None => return Ok(JsValue::undefined()),
    };
    let text = with_tree(|t| collect_text(t, id));
    Ok(JsValue::String(text.into()))
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

fn get_tag(_this: &JsValue, args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    if let Some(id) = arg_usize(args, 0) {
        let tag = with_tree(|t| match t.data(id) {
            NodeData::Element { tag, .. } => Some(tag.clone()),
            _ => None,
        });
        return Ok(match tag {
            Some(t) => JsValue::String(t.into()),
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
        Some(v) => Ok(JsValue::String(v.into())),
        None => Ok(JsValue::null()),
    }
}

/// M37: `__findChild(parentId, id) -> number | undefined` —
/// 在 parent 后代中查找指定 id 的元素（Element.getElementById 用）。
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

fn get_body(_this: &JsValue, _args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let id = with_tree(|t| find_first_element(t, "body"));
    Ok(JsValue::new(match id {
        Some(id) => id as f64,
        None => -1.0,
    }))
}

// M8.1: form value bridges.
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

fn storage_get_bridge(_this: &JsValue, args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let key = match arg_string(args, 0) {
        Some(k) => k,
        None => return Ok(JsValue::undefined()),
    };
    let value = with_storage(|s| browser_storage::storage_get(s, &key));
    match value {
        Some(v) => Ok(JsValue::String(boa_engine::string::JsString::from(v))),
        None => Ok(JsValue::null()),
    }
}

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

fn storage_clear_bridge(
    _this: &JsValue,
    _args: &[JsValue],
    _ctx: &mut Context,
) -> JsResult<JsValue> {
    with_storage(browser_storage::storage_clear);
    Ok(JsValue::undefined())
}

fn storage_len_bridge(_this: &JsValue, _args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let len = with_storage(browser_storage::storage_len);
    Ok(JsValue::new(len as f64))
}

fn storage_key_bridge(_this: &JsValue, args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let idx = match arg_usize(args, 0) {
        Some(i) => i,
        None => return Ok(JsValue::undefined()),
    };
    let key = with_storage(|s| browser_storage::storage_key(s, idx));
    match key {
        Some(k) => Ok(JsValue::String(boa_engine::string::JsString::from(k))),
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

// ===== M16.2: setTimeout / clearTimeout event loop 后端 =====

/// Ensure a timer wheel + callback map exist on the current thread.
/// Idempotent: reuses if already installed (e.g. across run_scripts calls).
/// (M16.2)
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
    Ok(JsValue::undefined())
}

/// Drain all due timer callbacks, returning them in FIFO order. (M16.2)
/// Event loop（run_scripts_with_base 的收尾循环）调用此函数，拿到到期
/// 的 JsObject 列表，逐个 `.call(&JsValue::undefined(), ctx)` 执行。
///
/// Returns `Vec<JsObject>`（空的 vec 表示没有到期 timer）。
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
                    callbacks.push(cb);
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

fn inc_pending_requests() {
    PENDING_REQUESTS.with(|slot| slot.set(slot.get().saturating_add(1)));
}

fn dec_pending_requests() {
    PENDING_REQUESTS.with(|slot| slot.set(slot.get().saturating_sub(1)));
}

// ===== M23.5: WebSocket 后端 =====

/// Ensure a WsManager exists on the current thread. Idempotent.
fn ensure_ws_manager() {
    WS_MANAGER.with(|slot| {
        if slot.borrow().is_none() {
            *slot.borrow_mut() = Some(browser_ws::WsManager::new());
        }
    });
}

/// `__wsCreate(url: string) -> number`：发起 ws:// 连接，返回 id。
/// 实际握手在后台线程异步进行；Open/Error 事件经 drain_ws_events 分派。
fn ws_create_bridge(_this: &JsValue, args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let url = args
        .first()
        .and_then(|v| v.as_string())
        .map(|s| s.to_std_string_escaped())
        .unwrap_or_default();
    let resolved = resolve_url(&url);
    ensure_ws_manager();
    let id = WS_MANAGER.with(|slot| {
        slot.borrow()
            .as_ref()
            .map(|m| m.connect(resolved))
            .unwrap_or(0)
    });
    Ok(JsValue::new(id as f64))
}

/// `__wsSend(id: number, data: string) -> undefined`：队列文本消息到后台线程。
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
    WS_MANAGER.with(|slot| {
        if let Some(m) = slot.borrow().as_ref() {
            m.send_text(id, data);
        }
    });
    Ok(JsValue::undefined())
}

/// `__wsClose(id: number) -> undefined`：队列关闭帧。
fn ws_close_bridge(_this: &JsValue, args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let id = match args.first().and_then(|v| v.as_number()).map(|n| n as u32) {
        Some(n) => n,
        None => return Ok(JsValue::undefined()),
    };
    WS_MANAGER.with(|slot| {
        if let Some(m) = slot.borrow().as_ref() {
            m.close(id);
        }
    });
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
struct XhrState {
    method: String,
    url: String,
    response_text: String,
    status: u16,
    error: Option<String>,
}

/// Ensure the XHR instance map exists. Idempotent. (M17.1)
fn ensure_xhr() {
    XHR_INSTANCES.with(|slot| {
        if slot.borrow().is_none() {
            *slot.borrow_mut() = Some(std::collections::HashMap::new());
        }
    });
}

/// `__xhrCreate() -> number`：新建一个 XHR 实例，返回 id 给 JS。
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
        Some(t) => Ok(JsValue::String(boa_engine::JsString::from(t))),
        None => Ok(JsValue::null()),
    }
}

fn history_push_bridge(_this: &JsValue, args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let state = arg_string(args, 0);
    let url = arg_string(args, 2).unwrap_or_default();
    with_navigation(|h| browser_navigation::history_push(h, state, url));
    Ok(JsValue::undefined())
}

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

fn history_back_bridge(
    _this: &JsValue,
    _args: &[JsValue],
    _ctx: &mut Context,
) -> JsResult<JsValue> {
    let ok = with_navigation(browser_navigation::history_back);
    Ok(JsValue::new(ok))
}

fn history_forward_bridge(
    _this: &JsValue,
    _args: &[JsValue],
    _ctx: &mut Context,
) -> JsResult<JsValue> {
    let ok = with_navigation(browser_navigation::history_forward);
    Ok(JsValue::new(ok))
}

fn history_go_bridge(_this: &JsValue, args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let n = match args.first().and_then(|v| v.as_number()) {
        Some(n) => n as i64,
        None => return Ok(JsValue::undefined()),
    };
    let ok = with_navigation(|h| browser_navigation::history_go(h, n));
    Ok(JsValue::new(ok))
}

fn history_len_bridge(_this: &JsValue, _args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let len = with_navigation(browser_navigation::history_len);
    Ok(JsValue::new(len as f64))
}

fn history_state_bridge(
    _this: &JsValue,
    _args: &[JsValue],
    _ctx: &mut Context,
) -> JsResult<JsValue> {
    let state = with_navigation(browser_navigation::history_state);
    match state {
        Some(s) => Ok(JsValue::String(boa_engine::string::JsString::from(s))),
        None => Ok(JsValue::null()),
    }
}

fn location_href_bridge(
    _this: &JsValue,
    _args: &[JsValue],
    _ctx: &mut Context,
) -> JsResult<JsValue> {
    let url = with_navigation(browser_navigation::current_url);
    Ok(JsValue::String(boa_engine::string::JsString::from(url)))
}

fn location_replace_bridge(
    _this: &JsValue,
    args: &[JsValue],
    _ctx: &mut Context,
) -> JsResult<JsValue> {
    let url = arg_string(args, 0).unwrap_or_default();
    with_navigation(|h| browser_navigation::location_replace(h, url));
    Ok(JsValue::undefined())
}

fn location_assign_bridge(
    _this: &JsValue,
    args: &[JsValue],
    _ctx: &mut Context,
) -> JsResult<JsValue> {
    let url = arg_string(args, 0).unwrap_or_default();
    with_navigation(|h| browser_navigation::location_assign(h, url));
    Ok(JsValue::undefined())
}

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

#[cfg(test)]
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

#[cfg(test)]
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

#[cfg(test)]
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
