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

use browser_js_runtime::JsRuntime;

use crate::jsonrpc::{CdpError, CdpMessage, Json};

/// Evaluate a JS expression with a fresh JsRuntime and return a CDP response.
pub fn dispatch(id: i64, method: &str, params: Option<&Json>) -> Result<String, CdpError> {
    match method {
        "Runtime.enable" | "Runtime.disable" => {
            // No-op ack — we don't emit console/log events yet.
            Ok(CdpMessage::ok_empty(id))
        }
        "Runtime.evaluate" => {
            let expr = params
                .and_then(|p| p.get_str("expression"))
                .ok_or_else(|| CdpError::InvalidJson("missing expression".to_string()))?;
            let mut rt = JsRuntime::new();
            match rt.eval(expr) {
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
        _ => Err(CdpError::MethodNotFound(method.to_string())),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evaluate_arithmetic() {
        let mut p = BTreeMap::new();
        p.insert("expression".to_string(), Json::String("2 + 3".to_string()));
        let resp = dispatch(1, "Runtime.evaluate", Some(&Json::Object(p))).unwrap();
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
        let resp = dispatch(1, "Runtime.evaluate", Some(&Json::Object(p))).unwrap();
        assert!(resp.contains("\"value\":\"HELLO\""), "got: {resp}");  // boa quotes; classify strips
    }

    #[test]
    fn evaluate_boolean() {
        let mut p = BTreeMap::new();
        p.insert("expression".to_string(), Json::String("1 < 2".to_string()));
        let resp = dispatch(1, "Runtime.evaluate", Some(&Json::Object(p))).unwrap();
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
        let resp = dispatch(1, "Runtime.evaluate", Some(&Json::Object(p))).unwrap();
        assert!(resp.contains("\"exceptionDetails\""), "got: {resp}");
    }

    #[test]
    fn enable_returns_ok_empty() {
        let resp = dispatch(1, "Runtime.enable", None).unwrap();
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
    fn unknown_method_errors() {
        assert!(dispatch(1, "Runtime.totallyFake", None).is_err());
    }
}
