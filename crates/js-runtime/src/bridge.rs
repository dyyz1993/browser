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
    // M7.2.1: real DOM API bridges.
    register_fn0(ctx, "__createEl", create_el as NativeFn);
    register_fn2(ctx, "__appendChild", append_child as NativeFn);
    register_fn3(ctx, "__setAttr", set_attr as NativeFn);
    register_fn1(ctx, "__getElById", get_el_by_id as NativeFn);
    register_fn1(ctx, "__qs", qs as NativeFn);
    register_fn2(ctx, "__setText", set_text as NativeFn);
    register_fn1(ctx, "__getTag", get_tag as NativeFn);
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

/// Synchronously fetch a URL. Spawns a detached thread with its own
/// tokio runtime so we can be called from inside an outer runtime
/// (boa eval runs on the main thread which is already inside a
/// tokio current_thread runtime).
///
/// M15.3: 如果安装了 cookie jar，自动带 Cookie 请求头并把响应
/// Set-Cookie 存入 jar（同主请求共享会话）。jar 是 `Rc<RefCell<>>`
/// 不跨线程，所以在主线程读出 header、写入 jar；新线程只拿 String。
fn fetch_sync(url: &str) -> Result<String, String> {
    let url = url.to_string();
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
        let (bytes, headers) = rt
            .block_on(client.get_with_headers(&url_clone, cookie_header.as_deref()))
            .map_err(|e| format!("{e:?}"))?;
        // 收集所有 Set-Cookie 行返回给主线程写 jar。
        let set_cookies: Vec<String> = headers
            .get_all("set-cookie")
            .iter()
            .filter_map(|v| v.to_str().ok().map(String::from))
            .collect();
        let body = String::from_utf8(bytes).map_err(|e| format!("non-utf8 response: {e}"))?;
        Ok::<_, String>((body, set_cookies))
    });
    let (body, set_cookies) = handle
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
    Ok(body)
}

// ---------------------------------------------------------------------------
// DOM helpers
// ---------------------------------------------------------------------------

fn find_first_element(tree: &Tree, tag: &str) -> Option<NodeId> {
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

fn append_body_text(tree: &mut Tree, text: &str) {
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

/// Detach `child` from its current parent and re-attach under `new_parent`.
fn move_subtree(tree: &mut Tree, new_parent: NodeId, child: NodeId) {
    let mut old_parent: Option<NodeId> = None;
    for i in 0..tree.len() {
        let id = i;
        if tree.children_of(id).contains(&child) {
            old_parent = Some(id);
            break;
        }
    }
    if let Some(op) = old_parent {
        tree.get_mut(op).children.retain(|&c| c != child);
    }
    tree.get_mut(new_parent).children.push(child);
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
    // M7.2.4: universal selector (*), class selector (.foo), and
    // compound selectors (div.container, p.red).
    // Strategy: tokenize into components (e.g., ["div", ".container"]),
    // then match each.
    let tokens = tokenize_selector(sel);
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
        } else if c == '*' {
            tokens.push(SelectorToken::Universal);
            chars.next();
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

fn get_tag(_this: &JsValue, args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let id = match arg_usize(args, 0) {
        Some(id) => id,
        None => return Ok(JsValue::undefined()),
    };
    let tag = with_tree(|t| match t.data(id) {
        NodeData::Element { tag, .. } => Some(tag.clone()),
        _ => None,
    });
    match tag {
        // M7.2.4 will return a real JsString once we sort out the boa 0.20
        // JsString::from lifetime story. For M7.2.1 the placeholder logs
        // to stderr and returns undefined — keeps the bridge callable
        // for parity with other reader bridges.
        Some(t) => {
            eprintln!("[dom-getTag] #{id} = {t}");
            Ok(JsValue::undefined())
        }
        None => Ok(JsValue::undefined()),
    }
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
