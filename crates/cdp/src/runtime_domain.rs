//! M45: CDP `Runtime` domain — evaluate + consoleLog.
//!
//! Lets clients execute JS expressions via `Runtime.evaluate`. Note: per
//! the M40 assessment, real-website JS compatibility is capped by the boa
//! engine (ES6 shorthand unsupported). This domain is best for simple
//! expressions (arithmetic, string ops, DOM queries via our shims), not
//! for running modern SPA bundles.
//!
//! ## Methods (M45 scope)
//!
//! - `Runtime.evaluate` — evaluate a JS expression, return result as a
//!   CDP `RemoteObject` + `exceptionDetails` (if thrown).
//! - `Runtime.enable` — no-op ack (Puppeteer probes this on connect).

use std::collections::BTreeMap;

use browser_dom::Tree;
use browser_js_runtime::{eval_in_tree_engine_await, EngineKind};

use crate::jsonrpc::{CdpError, CdpMessage, Json};

/// Evaluate a JS expression against the current page's DOM tree and return a
/// CDP response.
///
/// `tree` / `url` describe the page that navigate() built. M48: evaluate /
/// callFunctionOn now run in a shimmed context bound to this tree, so
/// `document.title`, `document.querySelector`, etc. read the real DOM (previously
/// they hit an empty `JsRuntime::new()` → "document is not defined").
///
/// M67: `engine_kind` selects the JS backend (QuickJs default, Boa fallback).
/// Both produce a unified display-format string consumed by `classify_value`.
pub fn dispatch(
    id: i64,
    method: &str,
    params: Option<&Json>,
    tree: &Tree,
    url: &str,
    engine_kind: &EngineKind,
) -> Result<String, CdpError> {
    match method {
        "Runtime.enable" | "Runtime.disable" | "Runtime.runIfWaitingForDebugger" => {
            // No-op ack — we don't emit console/log events yet.
            // M53: runIfWaitingForDebugger is sent by puppeteer after auto-attach
            // (because we set waitingForDebugger=true in attachedToTarget).
            Ok(CdpMessage::ok_empty(id))
        }
        "Runtime.evaluate" => {
            let expr = params
                .and_then(|p| p.get_str("expression"))
                .ok_or_else(|| CdpError::InvalidJson("missing expression".to_string()))?;
            // M81(B4): `awaitPromise: true` —— 表达式返回 Promise 时驱动至
            // Fulfilled/Rejected，返回真实值而非 "[object Promise]"。
            let await_promise =
                params.and_then(|p| p.get("awaitPromise")) == Some(&Json::Bool(true));
            match eval_in_tree_engine_await(
                tree.clone(),
                url_or_default(url),
                expr,
                engine_kind,
                await_promise,
            ) {
                Ok(value) => {
                    // Build RemoteObject { type, value }.
                    let (val_type, val_json) = classify_value(&value);
                    let mut remote = BTreeMap::new();
                    remote.insert("type".to_string(), Json::String(val_type));
                    remote.insert("value".to_string(), val_json);
                    let mut result = BTreeMap::new();
                    result.insert("result".to_string(), Json::Object(remote));
                    Ok(CdpMessage::ok_response(id, Json::Object(result)))
                }
                Err(e) => {
                    // Return exceptionDetails.
                    let mut exc = BTreeMap::new();
                    exc.insert("text".to_string(), Json::String(e.clone()));
                    exc.insert("exceptionId".to_string(), Json::Number(1.0));
                    let mut result = BTreeMap::new();
                    result.insert("exceptionDetails".to_string(), Json::Object(exc));
                    Ok(CdpMessage::ok_response(id, Json::Object(result)))
                }
            }
        }
        // M48: Runtime.callFunctionOn —— puppeteer 的 page.evaluate/$/title
        // 全走这个（不是 Runtime.evaluate）。在指定 executionContext 里执行一个
        // 函数声明。boa 模型：构造 (functionDeclaration).apply(null, args) eval。
        // 注意：boa 0.20 不支持箭头函数/ES6，现代 puppeteer 序列化的函数声明
        // 可能解析失败 —— 那种情况返回 exceptionDetails（不卡，puppeteer 会报错）。
        "Runtime.callFunctionOn" => {
            let function_decl = params
                .and_then(|p| p.get_str("functionDeclaration"))
                .ok_or_else(|| CdpError::InvalidJson("missing functionDeclaration".to_string()))?;
            // 序列化参数（arguments 数组，每项是 {value:...}）。
            let args_json = params
                .and_then(|p| p.get("arguments"))
                .and_then(|a| match a {
                    Json::Array(arr) => Some(arr.clone()),
                    _ => None,
                })
                .unwrap_or_default();
            let args_str: Vec<String> = args_json
                .iter()
                .map(|a| match a {
                    Json::Object(m) => match m.get("value") {
                        Some(v) => v.to_json_string(),
                        None => "undefined".to_string(),
                    },
                    _ => a.to_json_string(),
                })
                .collect();
            // 构造 eval 程序。
            // M81(A1): Playwright 的所有 evaluate 都经 `Runtime.callFunctionOn`
            // 且声明固定为 `(utilityScript, ...args) => utilityScript.evaluate(...args)`
            // （arguments = [isFunction, returnByValue, expose, expression, argCount, ...],
            // 见 Playwright javascript.js `evaluateExpression`）。我们没有持久 JS
            // 对象模型（objectId 句柄是 A2），无法给它真对象 —— 但可以在 eval
            // 作用域里**合成**一个等价 utilityScript（evaluate = 内层 eval 用户
            // 函数再调用），让 by-value 的 evaluate（title / textContent /
            // getAttribute 等）端到端工作。非 utilityScript 形态（puppeteer 的
            // callFunctionOn）保持原 `(FD)(ARGS)` 不变。
            let expr = if function_decl.contains("utilityScript") {
                // Playwright 把 utilityScript 作为 arguments[0] 传入（我们收到的
                // 是 {}/undefined，因为没有真 objectId）。合成对象**替换**第一参，
                // 其余参数保持原位（[isFunction, returnByValue, expose, expr, ...]）。
                let rest = args_str.iter().skip(1).cloned().collect::<Vec<_>>();
                format!(
                    "(function(){{ var __psUtilityScript = {{ \
                       evaluate: function(isFunction, returnByValue, exposeUtilityScript, expression, argCount) {{ \
                         var result = globalThis.eval(expression); \
                         if (isFunction === true || (isFunction !== false && typeof result === 'function')) {{ \
                           var psArgs = Array.prototype.slice.call(arguments, 5); \
                           result = result.apply(null, psArgs.slice(0, argCount)); \
                         }} \
                         return result; \
                       }}, \
                       evaluateHandle: function() {{ throw new Error('browser-rs: objectId element handles are not supported yet (M81 A2)'); }}, \
                       jsonValue: function(returnByValue, value) {{ return value; }} \
                     }}; \
                     return ({FD}).apply(null, [__psUtilityScript].concat({REST})); }})()",
                    FD = function_decl,
                    REST = rest.join(",")
                )
            } else {
                // IIFE 形式，避免函数声明需要名字。不套 JSON.stringify：boa
                // display() 对字符串会加引号，classify_value 据此归类；若再
                // stringify 会导致字符串双重引号。
                format!(
                    "({FD})({ARGS})",
                    FD = function_decl,
                    ARGS = args_str.join(",")
                )
            };
            // M81(B4): awaitPromise 透传（async 函数声明的 Playwright evaluate
            // 返回 Promise——等 resolve 后返回真实值）。
            let await_promise =
                params.and_then(|p| p.get("awaitPromise")) == Some(&Json::Bool(true));
            match eval_in_tree_engine_await(
                tree.clone(),
                url_or_default(url),
                &expr,
                engine_kind,
                await_promise,
            ) {
                Ok(value) => {
                    // boa display() 的输出，classify_value 据此归类。
                    let (val_type, val_json) = classify_value(&value);
                    let mut remote = BTreeMap::new();
                    remote.insert("type".to_string(), Json::String(val_type));
                    remote.insert("value".to_string(), val_json);
                    let mut result = BTreeMap::new();
                    result.insert("result".to_string(), Json::Object(remote));
                    Ok(CdpMessage::ok_response(id, Json::Object(result)))
                }
                Err(e) => {
                    let mut exc = BTreeMap::new();
                    exc.insert("text".to_string(), Json::String(e.clone()));
                    exc.insert("exceptionId".to_string(), Json::Number(1.0));
                    let mut result = BTreeMap::new();
                    result.insert("exceptionDetails".to_string(), Json::Object(exc));
                    Ok(CdpMessage::ok_response(id, Json::Object(result)))
                }
            }
        }
        // M53: unknown methods → no-op ack (puppeteer sends many enable/disable)
        _ => Ok(CdpMessage::ok_empty(id)),
    }
}

/// Empty url → about:blank, otherwise `Some(url)` for the navigation shim.
fn url_or_default(url: &str) -> Option<String> {
    if url.is_empty() {
        Some("about:blank".to_string())
    } else {
        Some(url.to_string())
    }
}

/// Classify a JS string result into CDP type + value JSON.
/// Tries number → bool → string.
/// M81(B4): awaitPromise 后 object/array 结果是 JSON 文本（js-runtime
/// `display_resolved_value` 用 JSON.stringify 序列化）——解析归类为
/// CDP `object` 类型（returnByValue 语义），解析失败仍按字符串兜底。
fn classify_value(s: &str) -> (String, Json) {
    // boa's display() quotes string values like "\"hello\"" — strip outer quotes.
    let s = if s.len() >= 2 && s.starts_with('\"') && s.ends_with('\"') {
        &s[1..s.len() - 1]
    } else {
        s
    };
    // M81(B4): object/array（JSON 文本）。
    if s.starts_with('{') || s.starts_with('[') {
        if let Ok(v) = crate::jsonrpc::parse_json(s) {
            return ("object".to_string(), v);
        }
    }
    // Try integer/float.
    if let Ok(n) = s.parse::<f64>() {
        return ("number".to_string(), Json::Number(n));
    }
    let lower = s.to_ascii_lowercase();
    if lower == "true" {
        return ("boolean".to_string(), Json::Bool(true));
    }
    if lower == "false" {
        return ("boolean".to_string(), Json::Bool(false));
    }
    if lower == "undefined" || lower == "null" {
        return ("undefined".to_string(), Json::Null);
    }
    ("string".to_string(), Json::String(s.to_string()))
}

#[cfg(all(test, feature = "boa"))]
mod tests {
    use super::*;

    #[test]
    fn evaluate_arithmetic() {
        let mut p = BTreeMap::new();
        p.insert("expression".to_string(), Json::String("2 + 3".to_string()));
        let resp = dispatch(
            1,
            "Runtime.evaluate",
            Some(&Json::Object(p)),
            &Tree::new(),
            "",
            &EngineKind::Boa,
        )
        .unwrap();
        assert!(resp.contains("\"value\":5"), "got: {resp}");
        assert!(resp.contains("\"type\":\"number\""), "got: {resp}");
    }

    #[test]
    fn evaluate_string() {
        let mut p = BTreeMap::new();
        p.insert(
            "expression".to_string(),
            Json::String("\"hello\".toUpperCase()".to_string()),
        );
        let resp = dispatch(
            1,
            "Runtime.evaluate",
            Some(&Json::Object(p)),
            &Tree::new(),
            "",
            &EngineKind::Boa,
        )
        .unwrap();
        assert!(resp.contains("\"value\":\"HELLO\""), "got: {resp}"); // boa quotes; classify strips
    }

    #[test]
    fn evaluate_boolean() {
        let mut p = BTreeMap::new();
        p.insert("expression".to_string(), Json::String("1 < 2".to_string()));
        let resp = dispatch(
            1,
            "Runtime.evaluate",
            Some(&Json::Object(p)),
            &Tree::new(),
            "",
            &EngineKind::Boa,
        )
        .unwrap();
        assert!(resp.contains("\"type\":\"boolean\""), "got: {resp}");
        assert!(resp.contains("\"value\":true"), "got: {resp}");
    }

    #[test]
    fn evaluate_syntax_error_returns_exception() {
        let mut p = BTreeMap::new();
        p.insert(
            "expression".to_string(),
            Json::String("}}invalid{{".to_string()),
        );
        let resp = dispatch(
            1,
            "Runtime.evaluate",
            Some(&Json::Object(p)),
            &Tree::new(),
            "",
            &EngineKind::Boa,
        )
        .unwrap();
        assert!(resp.contains("\"exceptionDetails\""), "got: {resp}");
    }

    #[test]
    fn enable_returns_ok_empty() {
        let resp = dispatch(
            1,
            "Runtime.enable",
            None,
            &Tree::new(),
            "",
            &EngineKind::Boa,
        )
        .unwrap();
        assert_eq!(resp, r#"{"id":1,"result":{}}"#);
    }

    #[test]
    fn classify_number() {
        let (t, v) = classify_value("42");
        assert_eq!(t, "number");
        match v {
            Json::Number(n) => assert!((n - 42.0).abs() < 1e-9),
            _ => panic!(),
        }
    }

    #[test]
    fn classify_string() {
        let (t, v) = classify_value("hello world");
        assert_eq!(t, "string");
        match v {
            Json::String(s) => assert_eq!(s, "hello world"),
            _ => panic!(),
        }
    }

    #[test]
    fn unknown_method_noop() {
        // M53: unknown methods now return no-op ack instead of error
        let resp = dispatch(
            1,
            "Runtime.totallyFake",
            None,
            &Tree::new(),
            "",
            &EngineKind::Boa,
        )
        .unwrap();
        assert_eq!(resp, r#"{"id":1,"result":{}}"#);
    }

    // ── M67: QuickJS 引擎路径回归测试 ──
    // 验证 eval_in_tree_engine 的 QuickJS 分支返回值格式与 boa 对齐
    // （classify_value 不分引擎，必须统一格式）。

    #[cfg(feature = "quickjs")]
    #[test]
    fn evaluate_arithmetic_quickjs() {
        let mut p = BTreeMap::new();
        p.insert("expression".to_string(), Json::String("2 + 3".to_string()));
        let resp = dispatch(
            1,
            "Runtime.evaluate",
            Some(&Json::Object(p)),
            &Tree::new(),
            "",
            &EngineKind::QuickJs,
        )
        .unwrap();
        assert!(resp.contains("\"value\":5"), "got: {resp}");
        assert!(resp.contains("\"type\":\"number\""), "got: {resp}");
    }

    #[cfg(feature = "quickjs")]
    #[test]
    fn evaluate_string_quickjs() {
        let mut p = BTreeMap::new();
        p.insert(
            "expression".to_string(),
            Json::String("\"hello\".toUpperCase()".to_string()),
        );
        let resp = dispatch(
            1,
            "Runtime.evaluate",
            Some(&Json::Object(p)),
            &Tree::new(),
            "",
            &EngineKind::QuickJs,
        )
        .unwrap();
        // QuickJS 分支模拟 boa display 格式：字符串结果带引号。
        assert!(resp.contains("\"value\":\"HELLO\""), "got: {resp}");
    }

    #[cfg(feature = "quickjs")]
    #[test]
    fn evaluate_boolean_quickjs() {
        let mut p = BTreeMap::new();
        p.insert("expression".to_string(), Json::String("1 < 2".to_string()));
        let resp = dispatch(
            1,
            "Runtime.evaluate",
            Some(&Json::Object(p)),
            &Tree::new(),
            "",
            &EngineKind::QuickJs,
        )
        .unwrap();
        assert!(resp.contains("\"type\":\"boolean\""), "got: {resp}");
        assert!(resp.contains("\"value\":true"), "got: {resp}");
    }

    #[cfg(feature = "quickjs")]
    #[test]
    fn evaluate_reads_dom_quickjs() {
        // 验证 QuickJS 分支的 tree guard 生效：evaluate 能读真实 DOM。
        let html = "<html><head><title>QJS Title</title></head><body><p>x</p></body></html>";
        let tree = browser_html_parser::parse(html);
        let mut p = BTreeMap::new();
        p.insert(
            "expression".to_string(),
            Json::String("document.title".to_string()),
        );
        let resp = dispatch(
            1,
            "Runtime.evaluate",
            Some(&Json::Object(p)),
            &tree,
            "http://example.com/",
            &EngineKind::QuickJs,
        )
        .unwrap();
        assert!(resp.contains("\"QJS Title\""), "got: {resp}");
    }
}

// ── M81(B4): awaitPromise 回归测试（QuickJS，默认 feature 即跑）──
#[cfg(all(test, feature = "quickjs"))]
mod await_promise_tests {
    use super::*;

    fn evaluate(expr: &str, await_promise: bool) -> String {
        let mut p = BTreeMap::new();
        p.insert("expression".to_string(), Json::String(expr.to_string()));
        p.insert("returnByValue".to_string(), Json::Bool(true));
        p.insert("awaitPromise".to_string(), Json::Bool(await_promise));
        dispatch(
            1,
            "Runtime.evaluate",
            Some(&Json::Object(p)),
            &Tree::new(),
            "",
            &EngineKind::QuickJs,
        )
        .unwrap()
    }

    #[test]
    fn await_promise_resolve_returns_value() {
        let resp = evaluate("Promise.resolve(42)", true);
        assert!(
            resp.contains("\"value\":42") && resp.contains("\"type\":\"number\""),
            "awaitPromise must unwrap resolved value, got: {resp}"
        );
    }

    #[test]
    fn await_promise_async_iife_returns_value() {
        let resp = evaluate("(async function(){ return 42; })()", true);
        assert!(
            resp.contains("\"value\":42"),
            "async IIFE awaited to 42, got: {resp}"
        );
    }

    #[test]
    fn await_promise_then_chain_returns_value() {
        let resp = evaluate(
            "Promise.resolve(1).then(function(v){ return v + 1; })",
            true,
        );
        assert!(
            resp.contains("\"value\":2"),
            ".then chain awaited through microtasks, got: {resp}"
        );
    }

    #[test]
    fn await_promise_fetch_resolves_via_timer_drain() {
        // fetch 的 resolve 挂 setTimeout(0)（macrotask）——验证 timer drain。
        let resp = evaluate(
            "fetch('http://127.0.0.1:9/').then(function(){ return 'FETCHED'; })\
             .catch(function(){ return 'FETCH-ERR-OK'; })",
            true,
        );
        assert!(
            resp.contains("FETCH"),
            "fetch promise must settle via timer drain, got: {resp}"
        );
    }

    #[test]
    fn await_promise_rejection_returns_exception_details() {
        let resp = evaluate("Promise.reject(new Error('boom-b1'))", true);
        assert!(
            resp.contains("exceptionDetails") && resp.contains("boom-b1"),
            "rejected promise → exceptionDetails with reason, got: {resp}"
        );
    }

    #[test]
    fn await_promise_object_result_classified_as_object() {
        let resp = evaluate("Promise.resolve({a: 1, b: 'x'})", true);
        assert!(
            resp.contains("\"type\":\"object\"") && resp.contains("\"a\":1"),
            "object result must be JSON-classified, got: {resp}"
        );
    }

    #[test]
    fn await_promise_false_keeps_promise_display() {
        // 无 awaitPromise → 旧行为：String(Promise) = "[object Promise]"。
        let resp = evaluate("Promise.resolve(42)", false);
        assert!(
            resp.contains("[object Promise]"),
            "awaitPromise=false must keep legacy display, got: {resp}"
        );
    }

    #[test]
    fn await_promise_non_promise_value_unchanged() {
        let resp = evaluate("1 + 1", true);
        assert!(
            resp.contains("\"value\":2"),
            "non-promise results unaffected by awaitPromise, got: {resp}"
        );
    }

    #[test]
    fn call_function_on_await_promise_passthrough() {
        let html = "<html><body><p>x</p></body></html>";
        let tree = browser_html_parser::parse(html);
        let mut p = BTreeMap::new();
        p.insert(
            "functionDeclaration".to_string(),
            Json::String("async function(){ return 7; }".to_string()),
        );
        p.insert("awaitPromise".to_string(), Json::Bool(true));
        let resp = dispatch(
            2,
            "Runtime.callFunctionOn",
            Some(&Json::Object(p)),
            &tree,
            "http://example.com/",
            &EngineKind::QuickJs,
        )
        .unwrap();
        assert!(
            resp.contains("\"value\":7"),
            "callFunctionOn awaitPromise must unwrap async fn, got: {resp}"
        );
    }
}
