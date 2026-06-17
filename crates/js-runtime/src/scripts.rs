//! Extract and execute `<script>` bodies from a DOM tree.
//!
//! M3.3 scope: walk a parsed tree, pull the text content of every
//! `<script>` element, then run them in order through a boa `Context`
//! (with the bridge installed). The DOM mutations made by JS become
//! visible to the subsequent layout + render passes.

use boa_engine::{Context, JsValue, Source};
use browser_dom::{NodeData, NodeId, Tree};

use crate::bridge::install;

/// M-cls.2: 收紧 JS 运行时限制（纵深防御第二层）。
///
/// 历史 `main.js`(Next.js bundle) 在 boa 0.20 下 eval 时把循环迭代吃到
/// 250_000 上限仍未抛错，但期间分配了数 GB 内存触发 OOM。把上限降到 40_000
/// 既足够跑常见 SPA 的内联脚本（秒级几百次迭代的渲染逻辑），又能在 runaway
/// 循环早期抛 `loop iteration limit reached`，配合子进程内存护栏（M-cls.1）
/// 双保险。stack/recursion 也从 boa 默认(10240/512)收紧到 4096/256。
const JS_LOOP_ITERATION_LIMIT: u64 = 40_000;
const JS_STACK_SIZE_LIMIT: usize = 4096;
const JS_RECURSION_LIMIT: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
enum ScriptEntry {
    Inline(String),
    External(String),
}

/// Collect the text content of every `<script>` element in `tree`,
/// in document order. Empty scripts are filtered out.
#[must_use]
pub fn extract_scripts(tree: &Tree) -> Vec<String> {
    extract_script_entries(tree)
        .into_iter()
        .filter_map(|entry| match entry {
            ScriptEntry::Inline(script) => Some(script),
            ScriptEntry::External(_) => None,
        })
        .collect()
}

fn extract_script_entries(tree: &Tree) -> Vec<ScriptEntry> {
    let mut scripts = Vec::new();
    let mut stack: Vec<NodeId> = vec![tree.root()];
    while let Some(id) = stack.pop() {
        let node = tree.get(id);
        if let NodeData::Element { tag, attrs } = &node.data {
            if tag.eq_ignore_ascii_case("script") {
                if !script_tag_has_executable_type(attrs) {
                    continue;
                }
                if let Some(src) = script_src(attrs) {
                    scripts.push(ScriptEntry::External(src));
                    continue;
                }
                let mut text = String::new();
                for &child_id in &node.children {
                    if let NodeData::Text(s) = tree.data(child_id) {
                        text.push_str(s);
                    }
                }
                if !text.trim().is_empty() {
                    scripts.push(ScriptEntry::Inline(text));
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

fn script_src(attrs: &[(String, String)]) -> Option<String> {
    attrs
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("src"))
        .map(|(_, value)| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn script_tag_has_executable_type(attrs: &[(String, String)]) -> bool {
    if attrs
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case("nomodule"))
    {
        return false;
    }
    let mut script_type: Option<&str> = None;
    let mut language: Option<&str> = None;
    for (name, value) in attrs {
        if name.eq_ignore_ascii_case("type") {
            script_type = Some(value);
        } else if language.is_none() && name.eq_ignore_ascii_case("language") {
            language = Some(value);
        }
    }
    if let Some(ty) = script_type {
        is_js_script_type(ty)
    } else if let Some(lang) = language {
        is_js_script_type(lang)
    } else {
        true
    }
}

fn is_js_script_type(raw: &str) -> bool {
    let mut token = raw.trim();
    if token.is_empty() {
        return true;
    }
    if let Some((head, _)) = token.split_once(';') {
        token = head;
    }
    let token = token.trim();
    if token.eq_ignore_ascii_case("module") {
        return true;
    }
    if token.eq_ignore_ascii_case("text/javascript")
        || token.eq_ignore_ascii_case("application/javascript")
        || token.eq_ignore_ascii_case("text/ecmascript")
        || token.eq_ignore_ascii_case("application/ecmascript")
        || token.eq_ignore_ascii_case("application/x-javascript")
        || token.eq_ignore_ascii_case("text/jscript")
    {
        return true;
    }
    let token = token.to_ascii_lowercase();
    token.contains("javascript") || token.contains("ecmascript")
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
    let limits = ctx.runtime_limits_mut();
    limits.set_loop_iteration_limit(JS_LOOP_ITERATION_LIMIT);
    limits.set_stack_size_limit(JS_STACK_SIZE_LIMIT);
    limits.set_recursion_limit(JS_RECURSION_LIMIT);
    let scripts: Vec<ScriptEntry> = {
        let borrowed = tree_shared.borrow();
        extract_script_entries(&borrowed)
    };
    let script_base_url = base_url.clone();
    let _guard = crate::bridge::install_shared_with_base(tree_shared.clone(), base_url);
    let mut executed = 0;
    let trace_scripts = std::env::var("BROWSER_TRACE_SCRIPTS").is_ok();
    let raw_script_mode = std::env::var("BROWSER_RAW_SCRIPT_ERRORS").is_ok();
    for (idx, script) in scripts.iter().enumerate() {
        let script_code = match script {
            ScriptEntry::Inline(code) => {
                if trace_scripts {
                    eprintln!("[js-runtime] script[{idx}] inline len={}", code.len());
                }
                Some((code.clone(), format!("inline[{idx}]")))
            }
            ScriptEntry::External(src) => match resolve_script_url(src, script_base_url.as_deref())
            {
                Some(url) => match fetch_external_script(&url) {
                    Ok(code) => {
                        if trace_scripts {
                            eprintln!(
                                "[js-runtime] script[{idx}] external {url} len={}",
                                code.len()
                            );
                        }
                        Some((code, format!("external[{idx}] {url}")))
                    }
                    Err(e) => {
                        eprintln!("[js-runtime] external script fetch failed: {url}: {e}");
                        None
                    }
                },
                None => {
                    eprintln!("[js-runtime] external script skipped: {src}");
                    None
                }
            },
        };
        let Some((script_code, script_label)) = script_code else {
            continue;
        };
        if raw_script_mode {
            if trace_scripts {
                eprintln!("[js-runtime] script[{idx}] {script_label} raw eval start");
            }
            match ctx.eval(Source::from_bytes(script_code.as_bytes())) {
                Ok(_) => {
                    executed += 1;
                    if trace_scripts {
                        eprintln!("[js-runtime] script[{idx}] {script_label} raw eval end");
                    }
                }
                Err(e) => {
                    eprintln!(
                        "[js-runtime] script[{idx}] {script_label} raw eval error: {:?}",
                        e
                    );
                }
            }
            if trace_scripts {
                eprintln!("[js-runtime] script[{idx}] eval end");
            }
            continue;
        }
        let mut wrapped_code = String::new();
        wrapped_code.push_str("(function(){\ntry{\n");
        wrapped_code.push_str(&script_code);
        let catch_prefix = "\n}catch(__err){\nif(typeof __log === 'function'){\nvar __parts = [];\nvar __errObj = (__err !== null && __err !== undefined) ? __err : {};\nif (typeof __errObj.message === 'string') {\n    __parts.push('message=' + __errObj.message);\n} else if (typeof __errObj.toString === 'function') {\n    __parts.push('message=' + String(__errObj.toString()));\n} else {\n    __parts.push('message=' + String(__errObj));\n}\nif (typeof __errObj.name !== 'undefined') __parts.push('name=' + String(__errObj.name));\nif (typeof __errObj.fileName !== 'undefined') __parts.push('fileName=' + String(__errObj.fileName));\nif (typeof __errObj.lineNumber !== 'undefined') __parts.push('lineNumber=' + String(__errObj.lineNumber));\nif (typeof __errObj.columnNumber !== 'undefined') __parts.push('columnNumber=' + String(__errObj.columnNumber));\nif (typeof __errObj.stack !== 'undefined') __parts.push('stack=' + String(__errObj.stack));\nif (typeof __errObj.constructor === 'function' && __errObj.constructor.name) __parts.push('constructor=' + String(__errObj.constructor.name));\ntry { __parts.push('type=' + (typeof __errObj)); } catch(e) {}\ntry { if (typeof __errObj === 'object' && __errObj !== null) { __parts.push('errKeys=' + String(Object.keys(__errObj))); __parts.push('errToJSON=' + String(JSON.stringify(__errObj))); } } catch(e) {}\ntry { if (typeof __errObj.toString === 'function') __parts.push('errToString=' + String(__errObj.toString())); } catch(e) {}\n__log('[";
        wrapped_code.push_str(catch_prefix);
        wrapped_code.push_str(&script_label);
        wrapped_code.push_str("] ' + __parts.join(' | '));\n}\n}\n})();\n//# sourceURL=");
        wrapped_code.push_str(&script_label);
        wrapped_code.push('\n');
        if trace_scripts {
            eprintln!("[js-runtime] script[{idx}] eval start");
        }
        match ctx.eval(Source::from_bytes(wrapped_code.as_bytes())) {
            Ok(_) => executed += 1,
            Err(e) => {
                eprintln!("[js-runtime] script[{idx}] {script_label} eval error: {e}");
            }
        }
        if trace_scripts {
            eprintln!("[js-runtime] script[{idx}] eval end");
        }
    }
    // M16.3: pump the event loop. 执行完所有 script 后，drain 到期 timer
    // 回调，回调可能 schedule 新 timer（或本身 schedule），重复直到 idle。
    // 防死循环：最多迭代 MAX_TICKS 次（防止 setTimeout 无限递归卡死爬虫）。
    executed += pump_event_loop(ctx);
    // M62: dispatch DOMContentLoaded + load 事件。SPA 框架（React/Vue/jQuery）
    // 常在 document.addEventListener('DOMContentLoaded', init) 里初始化，
    // 爬虫场景脚本执行完即可视为 DOM 就绪。best-effort，失败不阻断。
    let _ = ctx.eval(boa_engine::Source::from_bytes(
        r#"
        try {
            if (typeof document !== 'undefined' && typeof document.dispatchEvent === 'function') {
                var ev1 = (typeof Event === 'function') ? new Event('DOMContentLoaded') : { type: 'DOMContentLoaded' };
                document.dispatchEvent(ev1);
                var ev2 = (typeof Event === 'function') ? new Event('load') : { type: 'load' };
                document.dispatchEvent(ev2);
                if (typeof window !== 'undefined' && typeof window.dispatchEvent === 'function') {
                    window.dispatchEvent(ev2);
                }
            }
        } catch(e) {}
        "#,
    ));
    // dispatch 后可能 schedule 了新 timer（框架初始化逻辑），再 pump 一次。
    executed += pump_event_loop(ctx);
    executed
}

fn resolve_script_url(src: &str, base_url: Option<&str>) -> Option<String> {
    if let Ok(url) = url::Url::parse(src) {
        return Some(url.to_string());
    }
    if let Some(base) = base_url {
        if let Ok(base) = url::Url::parse(base) {
            if let Ok(url) = base.join(src) {
                return Some(url.to_string());
            }
        }
    }
    None
}

fn fetch_external_script(url: &str) -> Result<String, String> {
    let url = url.to_string();
    let handle = std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| format!("tokio runtime build failed: {e}"))?;
        let client = browser_net::HttpClient::new();
        let bytes = rt
            .block_on(client.get(&url))
            .map_err(|e| format!("{e:?}"))?;
        String::from_utf8(bytes).map_err(|e| format!("non-utf8 response: {e}"))
    });
    handle
        .join()
        .map_err(|_| "external script fetch thread panicked".to_string())?
}

/// M16.3: Drain due timer callbacks until the wheel is idle or the
/// safety cap is hit. Returns the number of callbacks invoked.
/// M16.4: 每轮 tick 先 `ctx.run_jobs()`（执行 Promise then 回调 microtask），
/// 再 drain 到期 timer。两者交叉驱动，直到都 idle。
/// `ctx.eval` 不返回值我们也不关心（回调的副作用在 DOM 上，不在返回值）。
fn pump_event_loop(ctx: &mut Context) -> usize {
    const MAX_TICKS: usize = 1000;
    const MAX_TOTAL: std::time::Duration = std::time::Duration::from_secs(8);
    const MAX_SLEEP_MS: u64 = 200;
    let started_at = std::time::Instant::now();
    let mut invoked = 0;
    // M23.5: WS 是长连接异步，握手/收消息在后台线程。即使 timer idle，
    // 也要 poll WS 事件直到所有连接关闭（否则 onopen/onmessage 永不触发）。
    let mut ws_idle_polls = 0u32;
    const WS_MAX_IDLE_POLLS: u32 = 500; // ~2.5s（500 × 5ms）安全裕度
    for _ in 0..MAX_TICKS {
        if started_at.elapsed() >= MAX_TOTAL {
            eprintln!(
                "[js-runtime] event loop hard timeout ({}s)",
                MAX_TOTAL.as_secs()
            );
            break;
        }
        // M16.4: 先执行 Promise microtask（then 回调）。可能 schedule 新 timer。
        let _ = ctx.run_jobs(); // 0.21: 返回 JsResult，drain microtask 失败忽略
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
        // M37: 没事件。但可能有 pending timer 尚未到期。
        // 此时必须 sleep 到最近 deadline 再 drain，否则永远 break
        // 而不会触发 setTimeout 回调（之前的 bug：直接 break）。
        if let Some(deadline) = crate::bridge::next_timer_deadline() {
            let now = std::time::Instant::now();
            if deadline > now {
                let wait = deadline - now;
                // 限制单次等待与总等待，避免页面通过超长 timer 长时间卡住。
                let mut wait = wait.min(std::time::Duration::from_millis(MAX_SLEEP_MS));
                let left = MAX_TOTAL.saturating_sub(started_at.elapsed());
                wait = wait.min(left);
                if wait > std::time::Duration::ZERO {
                    std::thread::sleep(wait);
                }
            }
            ws_idle_polls = 0;
            continue; // sleep 后回到循环顶部 drain 到期 timer
        }
        // 没有 pending timer。判断是否还有活跃 WS 连接。
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
    let trace_scripts = std::env::var("BROWSER_TRACE_SCRIPTS").is_ok();
    {
        let limits = ctx.runtime_limits_mut();
        limits.set_loop_iteration_limit(JS_LOOP_ITERATION_LIMIT);
        limits.set_stack_size_limit(JS_STACK_SIZE_LIMIT);
        limits.set_recursion_limit(JS_RECURSION_LIMIT);
    }
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
    // M28.2: 安装 window 全局对象（window===globalThis 自引用 + 视口数据属性）。
    // 必须在 navigator/location 等之后，window.navigator 经 globalThis 自动可见。
    let _ = crate::window_shim::install_window(&mut ctx);
    // M28.3: 安装 document 全局对象（getElementById/querySelector/createElement
    // 包装 __* 桥 + body/head/cookie/title/location 数据属性）。必须在 window
    // 之后（document.location 指向 location 全局对象）。
    let _ = crate::document_shim::install_document(&mut ctx);
    // M37: 安装 Element 对象（包装 NodeId + textContent/id/tagName getter/setter
    // 反射到 bridge）。必须在 document 之后（document.getElementById 返回 Element）。
    let _ = crate::element_shim::install_element(&mut ctx);
    // M28.4: 安装 screen 全局对象（width/height/colorDepth/orientation，响应式布局
    // 特性检测常用）。静态默认值（无显示器环境）。
    let _ = crate::screen_shim::install_screen(&mut ctx);
    // M41: Image 构造器（爬虫友好——设 src 后 setTimeout(0) 假触发 onload，不 fetch）。
    // 消除 baidu SPA 实测的 `Image is not defined` 错误。
    let _ = crate::image_shim::install_image(&mut ctx);
    // M57: 基础兼容 shim，补齐常见前端运行时入口。
    let _ = crate::compat_shim::install_compat_shims(&mut ctx);

    if trace_scripts {
        let diag = r#"(function() {
            if (globalThis.__objectDiagInstalled) return;
            globalThis.__objectDiagInstalled = true;
            if (typeof globalThis.__log !== 'function') return;

            function __diag(name, args) {
                var first = args && args.length > 0 ? args[0] : undefined;
                if (first === null) {
                    globalThis.__log('[diag] ' + name + ' first arg = null');
                } else if (first === undefined) {
                    globalThis.__log('[diag] ' + name + ' first arg = undefined');
                }
            }

            var _keys = Object.keys;
            Object.keys = function(obj) {
                __diag('Object.keys', arguments);
                return _keys(obj);
            };

            var _entries = Object.entries;
            Object.entries = function(obj) {
                __diag('Object.entries', arguments);
                return _entries(obj);
            };

            var _fromEntries = Object.fromEntries;
            if (typeof _fromEntries === 'function') {
                Object.fromEntries = function() {
                    __diag('Object.fromEntries', arguments);
                    return _fromEntries.apply(Object, arguments);
                };
            }

            var _values = Object.values;
            Object.values = function(obj) {
                __diag('Object.values', arguments);
                return _values(obj);
            };

            var _assign = Object.assign;
            Object.assign = function(target) {
                __diag('Object.assign', arguments);
                return _assign.apply(Object, arguments);
            };

            var _hasOwn = Object.prototype.hasOwnProperty;
            if (typeof _hasOwn === 'function') {
                Object.prototype.hasOwnProperty = function(prop) {
                    __diag('Object.prototype.hasOwnProperty', arguments);
                    return _hasOwn.call(this, prop);
                };
            }

            var _defineProperty = Object.defineProperty;
            Object.defineProperty = function(obj, prop, desc) {
                __diag('Object.defineProperty', arguments);
                return _defineProperty(obj, prop, desc);
            };

            var _defineProperties = Object.defineProperties;
            Object.defineProperties = function(obj, props) {
                __diag('Object.defineProperties', arguments);
                return _defineProperties(obj, props);
            };

            var _create = Object.create;
            Object.create = function(obj) {
                __diag('Object.create', arguments);
                return _create.apply(Object, arguments);
            };

            var _setPrototypeOf = Object.setPrototypeOf;
            Object.setPrototypeOf = function(obj, proto) {
                __diag('Object.setPrototypeOf', arguments);
                return _setPrototypeOf(obj, proto);
            };

            var _getPrototypeOf = Object.getPrototypeOf;
            Object.getPrototypeOf = function(obj) {
                __diag('Object.getPrototypeOf', arguments);
                return _getPrototypeOf(obj);
            };

            var _getOwnPropertyNames = Object.getOwnPropertyNames;
            Object.getOwnPropertyNames = function(obj) {
                __diag('Object.getOwnPropertyNames', arguments);
                return _getOwnPropertyNames(obj);
            };

            var _fromEntries = Object.fromEntries;
            if (typeof _fromEntries === 'function') {
                Object.fromEntries = function() {
                    __diag('Object.fromEntries', arguments);
                    return _fromEntries.apply(Object, arguments);
                };
            }

            var _reflectApply = Reflect.apply;
            if (typeof _reflectApply === 'function') {
                Reflect.apply = function() {
                    __diag('Reflect.apply', arguments);
                    return _reflectApply.apply(Reflect, arguments);
                };
            }

            var _reflectConstruct = Reflect.construct;
            if (typeof _reflectConstruct === 'function') {
                Reflect.construct = function() {
                    __diag('Reflect.construct', arguments);
                    return _reflectConstruct.apply(Reflect, arguments);
                };
            }

            if (typeof Array.from === 'function') {
                var _arrayFrom = Array.from;
                Array.from = function() {
                    __diag('Array.from', arguments);
                    return _arrayFrom.apply(Array, arguments);
                };
            }

            if (typeof Array.prototype.slice === 'function') {
                var _arraySlice = Array.prototype.slice;
                Array.prototype.slice = function() {
                    __diag('Array.prototype.slice', arguments);
                    return _arraySlice.apply(this, arguments);
                };
            }

            var _deleteProperty = Reflect.deleteProperty;
            if (typeof _deleteProperty === 'function') {
                Reflect.deleteProperty = function(obj, key) {
                    __diag('Reflect.deleteProperty', arguments);
                    return _deleteProperty(obj, key);
                };
            }
        })();"#;
        if let Err(e) = ctx.eval(boa_engine::Source::from_bytes(diag)) {
            eprintln!("[js-runtime] object-diag install failed: {e}");
        }
    }
    let count = execute_scripts_with_base(&shared, &mut ctx, base_url);
    (shared, count)
}

/// M48: 构建一个装好全部 shims（document/window/navigator/fetch/...）的 boa
/// `Context`，供 `run_scripts_with_base` 和 CDP `Runtime.evaluate`/`callFunctionOn`
/// 复用。返回的 ctx 还没绑定任何 tree —— 调用方需用
/// `bridge::install_shared_with_base` 装 tree guard 后才能 eval。
///
/// `base_url` 用于 location/navigation 后端的初始 URL（None → about:blank）。
fn build_shimmed_context(base_url: &Option<String>) -> Context {
    let mut ctx = Context::default();
    {
        let limits = ctx.runtime_limits_mut();
        limits.set_loop_iteration_limit(JS_LOOP_ITERATION_LIMIT);
        limits.set_stack_size_limit(JS_STACK_SIZE_LIMIT);
        limits.set_recursion_limit(JS_RECURSION_LIMIT);
    }
    install(&mut ctx);
    let storage = browser_storage::new_storage();
    crate::bridge::install_storage(storage);
    let _ = crate::storage_shim::install_storage_globals(&mut ctx);
    let initial_url = base_url
        .clone()
        .unwrap_or_else(|| "about:blank".to_string());
    let nav = browser_navigation::new_navigation(&initial_url);
    crate::bridge::install_navigation(nav);
    let _ = crate::navigation_shim::install_navigation_globals(&mut ctx);
    crate::bridge::ensure_cookie_jar();
    let _ = crate::xhr_shim::install_xml_http_request(&mut ctx);
    let _ = crate::fetch_shim::install_fetch(&mut ctx);
    let _ = crate::ws_shim::install_websocket(&mut ctx);
    let _ = crate::navigator_shim::install_navigator(&mut ctx);
    let _ = crate::window_shim::install_window(&mut ctx);
    let _ = crate::document_shim::install_document(&mut ctx);
    let _ = crate::element_shim::install_element(&mut ctx);
    let _ = crate::screen_shim::install_screen(&mut ctx);
    let _ = crate::image_shim::install_image(&mut ctx);
    let _ = crate::compat_shim::install_compat_shims(&mut ctx);
    ctx
}

/// M48: 在一个**已构建好的 DOM tree** 上执行单个 JS 表达式，返回结果字符串
/// （boa `display()` 格式）。供 CDP `Runtime.evaluate` / `callFunctionOn` 用 ——
/// 让 `document.title`、`document.querySelector` 等能访问真实页面 DOM。
///
/// 与 `run_scripts_with_base` 不同：不执行页面里的 `<script>`，只 eval 调用方
/// 传入的表达式。tree 的 thread-local 安装在函数返回时由 `TreeGuard::drop` 清理。
///
/// # Errors
/// 返回 `Err(msg)` 如果 JS 解析或执行失败。
pub fn eval_in_tree(tree: Tree, base_url: Option<String>, expr: &str) -> Result<String, String> {
    use std::cell::RefCell;
    use std::rc::Rc;
    let shared: crate::bridge::SharedTree = Rc::new(RefCell::new(tree));
    let mut ctx = build_shimmed_context(&base_url);
    // 安装 tree guard：让 document/window shims 的 __* 桥能访问 DOM。
    // guard 在作用域结束时自动清理 thread-local slot。
    let _guard = crate::bridge::install_shared_with_base(shared, base_url);
    let result: JsValue = ctx
        .eval(Source::from_bytes(expr))
        .map_err(|e| format!("js eval error: {e}"))?;
    Ok(result.display().to_string())
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
    fn extract_skips_json_and_template_scripts() {
        let tree = parse(
            "<html><body>\
             <script type=\"application/json\">{\"x\":1}</script>\
             <script type=\"text/template\">{{name}}</script>\
             <script>__setBody('ok')</script>\
             </body></html>",
        );
        let scripts = extract_scripts(&tree);
        assert_eq!(scripts.len(), 1);
        assert!(scripts[0].contains("__setBody('ok')"));
    }

    #[test]
    fn extract_accepts_js_and_module_scripts() {
        let tree = parse(
            "<html><body>\
             <script type=\"text/javascript\">__setBody('js')</script>\
             <script type=\"module\">__appendBody('module')</script>\
             </body></html>",
        );
        let scripts = extract_scripts(&tree);
        assert_eq!(scripts.len(), 2);
        assert!(scripts[0].contains("__setBody('js')"));
        assert!(scripts[1].contains("__appendBody('module')"));
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
        let (shared, _executed) = run_scripts(parse(html));
        // Each script is wrapped in try/catch (M57 compat), so the thrown
        // error is logged but does not abort the run. The key guarantee this
        // test verifies: a later script still runs and its DOM mutation
        // survives — the throw must not poison the pipeline.
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

    #[test]
    fn infinite_loop_script_is_bounded_by_runtime_limit() {
        // M-cls.2: a runaway `while(true)` must throw (loop iteration limit)
        // instead of hanging or OOMing. The script sets a sentinel *before*
        // the loop; because per-script eval is wrapped in try/catch by
        // execute_scripts_with_base, the throw is swallowed and the next
        // script's sentinel still runs — proving the loop did not hang.
        let html = "<html><body>\
                    <script>__appendBody('before')</script>\
                    <script>var i=0; while(true){i++;}</script>\
                    <script>__appendBody('after')</script>\
                    </body></html>";
        let start = std::time::Instant::now();
        let (shared, _) = run_scripts(parse(html));
        let elapsed = start.elapsed();
        let text = body_text_content(&shared.borrow());
        assert!(text.contains("before"), "got: {text}");
        assert!(text.contains("after"), "got: {text}");
        // Must finish fast (limit kicks in), not hang for seconds.
        assert!(
            elapsed < std::time::Duration::from_secs(5),
            "runaway loop took {elapsed:?}, limit not enforced"
        );
    }
}
