//! Extract and execute `<script>` bodies from a DOM tree.
//!
//! M3.3 scope: walk a parsed tree, pull the text content of every
//! `<script>` element, then run them in order through a boa `Context`
//! (with the bridge installed). The DOM mutations made by JS become
//! visible to the subsequent layout + render passes.

use boa_engine::{Context, JsValue, Source};
use browser_dom::{NodeData, NodeId, Tree};

use crate::bridge::install;

/// Collect the text content of every `<script>` element in `tree`,
/// in document order. Empty scripts are filtered out.
#[must_use]
pub fn extract_scripts(tree: &Tree) -> Vec<String> {
    let mut scripts = Vec::new();
    let mut stack: Vec<NodeId> = vec![tree.root()];
    while let Some(id) = stack.pop() {
        let node = tree.get(id);
        if let NodeData::Element { tag, .. } = &node.data {
            if tag.eq_ignore_ascii_case("script") {
                let mut text = String::new();
                for &child_id in &node.children {
                    if let NodeData::Text(s) = tree.data(child_id) {
                        text.push_str(s);
                    }
                }
                if !text.trim().is_empty() {
                    scripts.push(text);
                }
                // Don't recurse into scripts (no nested scripts allowed by HTML5).
                continue;
            }
        }
        // Push children in reverse so left-most is popped first
        // (preserves document order).
        for &child in node.children.iter().rev() {
            stack.push(child);
        }
    }
    scripts
}

/// Run all `<script>` bodies in `tree` against `ctx`, with the bridge
/// installed so JS can mutate the DOM. Returns the count of scripts
/// that executed without throwing.
///
/// # Errors
/// Individual script errors are logged to stderr and don't abort the
/// run; the count returned reflects only successful executions.
pub fn execute_scripts(tree_shared: &crate::bridge::SharedTree, ctx: &mut Context) -> usize {
    execute_scripts_with_base(tree_shared, ctx, None)
}

/// Same as [`execute_scripts`], but also installs a base URL used to
/// resolve relative URLs in `__fetchSetBody` / `__fetchAppendBody`.
pub fn execute_scripts_with_base(
    tree_shared: &crate::bridge::SharedTree,
    ctx: &mut Context,
    base_url: Option<String>,
) -> usize {
    let scripts: Vec<String> = {
        let borrowed = tree_shared.borrow();
        extract_scripts(&borrowed)
    };
    let _guard = crate::bridge::install_shared_with_base(tree_shared.clone(), base_url);
    let mut executed = 0;
    for script in scripts {
        match ctx.eval(Source::from_bytes(&script)) {
            Ok(_) => executed += 1,
            Err(e) => {
                eprintln!("[js-runtime] script error: {e}");
            }
        }
    }
    // M16.3: pump the event loop. 执行完所有 script 后，drain 到期 timer
    // 回调，回调可能 schedule 新 timer（或本身 schedule），重复直到 idle。
    // 防死循环：最多迭代 MAX_TICKS 次（防止 setTimeout 无限递归卡死爬虫）。
    executed += pump_event_loop(ctx);
    executed
}

/// M16.3: Drain due timer callbacks until the wheel is idle or the
/// safety cap is hit. Returns the number of callbacks invoked.
/// M16.4: 每轮 tick 先 `ctx.run_jobs()`（执行 Promise then 回调 microtask），
/// 再 drain 到期 timer。两者交叉驱动，直到都 idle。
/// `ctx.eval` 不返回值我们也不关心（回调的副作用在 DOM 上，不在返回值）。
fn pump_event_loop(ctx: &mut Context) -> usize {
    const MAX_TICKS: usize = 1000;
    let mut invoked = 0;
    // M23.5: WS 是长连接异步，握手/收消息在后台线程。即使 timer idle，
    // 也要 poll WS 事件直到所有连接关闭（否则 onopen/onmessage 永不触发）。
    let mut ws_idle_polls = 0u32;
    const WS_MAX_IDLE_POLLS: u32 = 500; // ~2.5s（500 × 5ms）安全裕度
    for _ in 0..MAX_TICKS {
        // M16.4: 先执行 Promise microtask（then 回调）。可能 schedule 新 timer。
        ctx.run_jobs();
        let mut tick_invoked = 0;
        // Timer 回调（setTimeout）。
        let due = crate::bridge::drain_due_timer_callbacks();
        for callback in due {
            // 调用 setTimeout 回调：this = undefined，无参。
            // 回调内部如果 schedule 新 timer 或修改 DOM，会在下一轮 tick 处理。
            if let Err(e) = callback.call(&JsValue::undefined(), &[], ctx) {
                eprintln!("[js-runtime] timer callback error: {e}");
            }
            invoked += 1;
            tick_invoked += 1;
        }
        // M23.5: WebSocket 事件（Open/Text/Binary/Closed/Error）。
        let ws_events = crate::bridge::drain_ws_events();
        for (id, etype, data) in ws_events {
            let escaped = escape_js_ws_data(&data);
            let js = format!("__wsDispatchEvent({id}, '{etype}', '{escaped}')");
            if let Err(e) = ctx.eval(Source::from_bytes(&js)) {
                eprintln!("[js-runtime] ws dispatch error: {e}");
            }
            invoked += 1;
            tick_invoked += 1;
        }
        if tick_invoked > 0 {
            // 有事件触发（可能 schedule 新操作），重置 WS idle 计数继续。
            ws_idle_polls = 0;
            continue;
        }
        // 没事件。判断是否还有活跃 WS 连接。
        if crate::bridge::ws_connection_count() == 0 {
            break; // timer + WS 都 idle，结束
        }
        // 有活跃 WS 但暂无事件：短暂 sleep 让后台线程收消息，再 poll。
        if ws_idle_polls >= WS_MAX_IDLE_POLLS {
            eprintln!("[js-runtime] WS idle poll 超时（{WS_MAX_IDLE_POLLS} 轮），连接可能未关闭");
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
        ws_idle_polls += 1;
    }
    invoked
}

/// M23.5: 转义 WS 消息载荷为安全的 JS 字符串字面量（单引号包裹）。
/// 处理反斜杠/单引号/换行/回车/制表符，避免 eval 注入或语法错误。
fn escape_js_ws_data(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out
}

/// Convenience: install bridge + execute scripts in one call.
/// Returns (executed_count, SharedTree). The caller can drop the
/// `Rc<RefCell<Tree>>` clones when done rendering.
#[must_use]
pub fn run_scripts(tree: Tree) -> (crate::bridge::SharedTree, usize) {
    run_scripts_with_base(tree, None)
}

/// Same as [`run_scripts`], but installs a base URL for relative-URL
/// resolution in `__fetch*` calls.
#[must_use]
pub fn run_scripts_with_base(
    tree: Tree,
    base_url: Option<String>,
) -> (crate::bridge::SharedTree, usize) {
    use std::cell::RefCell;
    use std::rc::Rc;
    let shared: crate::bridge::SharedTree = Rc::new(RefCell::new(tree));
    let mut ctx = Context::default();
    install(&mut ctx);
    // M13.3: 安装 localStorage / sessionStorage 后端 + JS 对象
    let storage = browser_storage::new_storage();
    crate::bridge::install_storage(storage);
    let _ = crate::storage_shim::install_storage_globals(&mut ctx);
    // M14.3: 安装 history / location 后端 + JS 对象（初始 URL = base_url）
    let initial_url = base_url
        .clone()
        .unwrap_or_else(|| "about:blank".to_string());
    let nav = browser_navigation::new_navigation(&initial_url);
    crate::bridge::install_navigation(nav);
    let _ = crate::navigation_shim::install_navigation_globals(&mut ctx);
    // M15.3/M15.4: 安装 cookie jar 后端。如果 cli 已装（主请求）复用，否则新建。
    crate::bridge::ensure_cookie_jar();
    // M17.2: 安装 XMLHttpRequest 全局构造器。
    let _ = crate::xhr_shim::install_xml_http_request(&mut ctx);
    // M19.1: 安装全局 fetch（标准 Promise-based API）。
    let _ = crate::fetch_shim::install_fetch(&mut ctx);
    // M23.5: 安装 WebSocket 全局构造器（ws:// 实时连接）。
    let _ = crate::ws_shim::install_websocket(&mut ctx);
    // M28.1: 安装 navigator 全局对象（userAgent/platform/language，反爬必需）。
    let _ = crate::navigator_shim::install_navigator(&mut ctx);
    let count = execute_scripts_with_base(&shared, &mut ctx, base_url);
    (shared, count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::body_text_content;

    fn parse(html: &str) -> Tree {
        browser_html_parser::parse(html)
    }

    #[test]
    fn extract_no_scripts() {
        let tree = parse("<html><body><p>hi</p></body></html>");
        assert!(extract_scripts(&tree).is_empty());
    }

    #[test]
    fn extract_one_inline_script() {
        let tree = parse("<html><body><script>__setBody('x')</script></body></html>");
        let scripts = extract_scripts(&tree);
        assert_eq!(scripts.len(), 1);
        assert_eq!(scripts[0], "__setBody('x')");
    }

    #[test]
    fn extract_multiple_scripts_in_order() {
        let tree = parse(
            "<html><body>\
             <script>a()</script>\
             <script>b()</script>\
             <script>c()</script>\
             </body></html>",
        );
        let scripts = extract_scripts(&tree);
        assert_eq!(scripts.len(), 3);
        assert!(scripts[0].contains("a()"));
        assert!(scripts[1].contains("b()"));
        assert!(scripts[2].contains("c()"));
    }

    #[test]
    fn extract_skips_empty_scripts() {
        let tree = parse(
            "<html><body>\
             <script>  </script>\
             <script>real()</script>\
             <script></script>\
             </body></html>",
        );
        let scripts = extract_scripts(&tree);
        assert_eq!(scripts.len(), 1);
        assert!(scripts[0].contains("real"));
    }

    #[test]
    fn execute_mutates_dom_via_bridge() {
        let html = "<html><body>\
                    <script>__setBody(\"dynamic content\")</script>\
                    </body></html>";
        let (shared, executed) = run_scripts(parse(html));
        assert_eq!(executed, 1);
        assert_eq!(body_text_content(&shared.borrow()), "dynamic content");
    }

    #[test]
    fn execute_chained_scripts_share_dom_state() {
        let html = "<html><body>\
                    <script>__setBody(\"first\")</script>\
                    <script>__appendBody(\" second\")</script>\
                    </body></html>";
        let (shared, executed) = run_scripts(parse(html));
        assert_eq!(executed, 2);
        assert_eq!(body_text_content(&shared.borrow()), "first second");
    }

    #[test]
    fn execute_script_error_does_not_abort_run() {
        let html = "<html><body>\
                    <script>throw new Error('boom')</script>\
                    <script>__setBody('recovered')</script>\
                    </body></html>";
        let (shared, executed) = run_scripts(parse(html));
        // The first script threw, the second succeeded.
        assert_eq!(executed, 1);
        assert_eq!(body_text_content(&shared.borrow()), "recovered");
    }

    #[test]
    fn execute_preserves_non_script_dom() {
        // Static content should remain visible even with a script that
        // mutates a different part of the tree.
        let html = "<html><body>\
                    <p>static</p>\
                    <script>__setTitle('dynamic title')</script>\
                    </body></html>";
        let (shared, _) = run_scripts(parse(html));
        let text = body_text_content(&shared.borrow());
        assert!(text.contains("static"), "got: {text}");
    }
}
