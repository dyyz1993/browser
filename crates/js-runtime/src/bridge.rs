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
use browser_dom::{NodeData, NodeId, Tree};

/// A shared, mutate-able handle to the DOM tree that JS sees.
pub type SharedTree = Rc<RefCell<Tree>>;

thread_local! {
    static CURRENT_TREE: RefCell<Option<SharedTree>> = const { RefCell::new(None) };
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
#[must_use]
pub fn install_shared(shared: SharedTree) -> TreeGuard {
    CURRENT_TREE.with(|slot| {
        *slot.borrow_mut() = Some(shared);
    });
    TreeGuard { _private: () }
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
}

type NativeFn = fn(&JsValue, &[JsValue], &mut Context) -> JsResult<JsValue>;

fn register_fn(ctx: &mut Context, name: &str, f: NativeFn) {
    let native = NativeFunction::from_fn_ptr(f);
    let _ = ctx.register_global_callable(name.into(), 1, native);
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
    let url = arg_string(args, 0).unwrap_or_default();
    if url.is_empty() {
        return Ok(JsValue::undefined());
    }
    match fetch_sync(&url) {
        Ok(text) => with_tree(|t| set_body_inner_html(t, &text)),
        Err(e) => eprintln!("[js-fetch] {url} failed: {e}"),
    }
    Ok(JsValue::undefined())
}

fn fetch_append_body(_this: &JsValue, args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let url = arg_string(args, 0).unwrap_or_default();
    if url.is_empty() {
        return Ok(JsValue::undefined());
    }
    match fetch_sync(&url) {
        Ok(text) => with_tree(|t| append_body_text(t, &text)),
        Err(e) => eprintln!("[js-fetch] {url} failed: {e}"),
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
