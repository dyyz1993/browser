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

use boa_engine::{Context, JsArgs, JsResult, JsValue, NativeFunction};
use browser_dom::{Node, NodeData, NodeId, Tree};

/// A shared, mutate-able handle to the DOM tree that JS sees.
pub type SharedTree = Rc<RefCell<Tree>>;

thread_local! {
    static CURRENT_TREE: RefCell<Option<SharedTree>> = const { RefCell::new(None) };
    static BASE_URL: RefCell<Option<String>> = const { RefCell::new(None) };
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
fn fetch_sync(url: &str) -> Result<String, String> {
    let url = url.to_string();
    let handle = std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| format!("tokio runtime build failed: {e}"))?;
        let bytes = rt
            .block_on(browser_net::get(&url))
            .map_err(|e| format!("{e:?}"))?;
        String::from_utf8(bytes).map_err(|e| format!("non-utf8 response: {e}"))
    });
    handle
        .join()
        .map_err(|_| "fetch thread panicked".to_string())?
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
