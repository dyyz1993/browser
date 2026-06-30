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
use browser_js_runtime::{eval_in_tree_engine, EngineKind};

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
            match eval_in_tree_engine(tree.clone(), url_or_default(url), expr, engine_kind) {
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
            // 构造 (<functionDecl>)(arg0, arg1, ...) —— IIFE 形式，避免函数声明
            // 需要名字。不套 JSON.stringify：boa display() 对字符串会加引号，
            // classify_value 据此归类；若再 stringify 会导致字符串双重引号。
            let expr = format!(
                "({FD})({ARGS})",
                FD = function_decl,
                ARGS = args_str.join(",")
            );
            match eval_in_tree_engine(tree.clone(), url_or_default(url), &expr, engine_kind) {
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
fn classify_value(s: &str) -> (String, Json) {
    // boa's display() quotes string values like "\"hello\"" — strip outer quotes.
    let s = if s.len() >= 2 && s.starts_with('\"') && s.ends_with('\"') {
        &s[1..s.len() - 1]
    } else {
        s
    };
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
