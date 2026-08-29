//! M43: HTTP discovery endpoints (the `/json/*` family).
//!
//! CDP clients (Puppeteer, Playwright) **probe these before connecting** to
//! discover available targets and browser metadata. Without them, the
//! standard connection flow fails.
//!
//! ## Endpoints
//!
//! | Path | Method | Returns |
//! |------|--------|---------|
//! | `/json/version` | GET | Browser metadata + `webSocketDebuggerUrl` |
//! | `/json` or `/json/list` | GET | JSON array of targets |
//! | `/json/new` | GET/PUT | New (or existing) target, with `webSocketDebuggerUrl` |
//! | `/json/activate/:id` | GET | "Target activated" (no-op for single-tab) |
//! | `/json/close/:id` | GET | "Target is closing" (no-op for single-tab) |
//!
//! All responses are `application/json; charset=UTF-8`.
//!
//! ## Single-tab model
//!
//! M43 scope is a **single tab** (one browser, one page). We expose exactly
//! one target with a fixed id. M48+ can add multi-tab.

use std::collections::BTreeMap;

use crate::jsonrpc::Json;

/// Fixed target id for the single browser tab (M43 single-tab scope).
pub const TARGET_ID: &str = "browser-rs-target-0";

/// Build the `/json/version` response body.
///
/// `ws_host` is e.g. `127.0.0.1:9222` — used to construct the
/// `webSocketDebuggerUrl`.
#[must_use]
pub fn version_body(ws_host: &str) -> String {
    let mut m = BTreeMap::new();
    m.insert(
        "Browser".to_string(),
        Json::String("browser-rs/0.0.1".to_string()),
    );
    m.insert(
        "Protocol-Version".to_string(),
        Json::String("1.3".to_string()),
    );
    m.insert(
        "User-Agent".to_string(),
        Json::String("Mozilla/5.0 (compatible; browser-rs/0.0.1)".to_string()),
    );
    m.insert("V8-Version".to_string(), Json::String("boa".to_string()));
    m.insert(
        "WebKit-Version".to_string(),
        Json::String("537.36".to_string()),
    );
    m.insert(
        "webSocketDebuggerUrl".to_string(),
        // M48: browser-level endpoint (/devtools/browser/) — required by
        // puppeteer.connect({browserWSEndpoint}). /json/list provides the
        // page-level endpoint (/devtools/page/) separately.
        Json::String(format!("ws://{ws_host}/devtools/browser/{TARGET_ID}")),
    );
    Json::Object(m).to_json_string()
}

/// Build the `/json` (or `/json/list`) response body: a JSON array of targets.
#[must_use]
pub fn list_body(ws_host: &str) -> String {
    Json::Array(vec![target_object(ws_host)]).to_json_string()
}

/// Build a single target object (used by `/json`, `/json/new`).
#[must_use]
pub fn target_object(ws_host: &str) -> Json {
    let mut m = BTreeMap::new();
    m.insert(
        "description".to_string(),
        Json::String("browser-rs page".to_string()),
    );
    m.insert(
        "devtoolsFrontendUrl".to_string(),
        Json::String(format!(
            "/devtools.html?ws={ws_host}/devtools/page/{TARGET_ID}"
        )),
    );
    m.insert("id".to_string(), Json::String(TARGET_ID.to_string()));
    // M48: CDP 的 Target.targetInfo 用 `targetId`（不是 `id`）。puppeteer 的
    // TargetManager 读 event.targetInfo.targetId 建 target。`id` 留给 /json HTTP 发现。
    m.insert("targetId".to_string(), Json::String(TARGET_ID.to_string()));
    m.insert("title".to_string(), Json::String("browser-rs".to_string()));
    m.insert("type".to_string(), Json::String("page".to_string()));
    m.insert("attached".to_string(), Json::Bool(true)); // M53: puppeteer checks this
    m.insert("url".to_string(), Json::String("about:blank".to_string()));
    m.insert(
        "webSocketDebuggerUrl".to_string(),
        Json::String(format!("ws://{ws_host}/devtools/page/{TARGET_ID}")),
    );
    m.insert(
        "browserUrl".to_string(),
        Json::String(format!("http://{ws_host}")),
    );
    Json::Object(m)
}

/// Build the `/json/new` response body: a single target object.
#[must_use]
pub fn new_target_body(ws_host: &str) -> String {
    target_object(ws_host).to_json_string()
}

/// Build the `/json/activate/:id` response body.
#[must_use]
pub fn activate_body() -> String {
    Json::String("Target activated".to_string()).to_json_string()
}

/// Build the `/json/close/:id` response body.
#[must_use]
pub fn close_body() -> String {
    Json::String("Target is closing".to_string()).to_json_string()
}

/// Build a complete HTTP response (status + headers + body).
#[must_use]
pub fn http_response(status: &str, content_type: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\n\
         Content-Type: {content_type}\r\n\
         Content-Length: {}\r\n\
         Cache-Control: no-cache\r\n\
         Connection: close\r\n\
         \r\n\
         {body}",
        body.len()
    )
}

/// Build a 200 JSON response.
#[must_use]
pub fn json_ok(body: &str) -> String {
    http_response("200 OK", "application/json; charset=UTF-8", body)
}

/// Build a 404 response.
#[must_use]
pub fn http_404() -> String {
    http_response(
        "404 Not Found",
        "text/plain; charset=UTF-8",
        "404 Not Found",
    )
}

/// Dispatch a request path to the right discovery response.
///
/// Returns the full HTTP response string (including status line + headers).
/// `ws_host` is e.g. `127.0.0.1:9222`.
#[must_use]
pub fn handle_discovery(path: &str, ws_host: &str) -> String {
    // Strip query string.
    let path = path.split('?').next().unwrap_or(path);
    // Playwright `connect_over_cdp` 探测 `/json/version/`（带尾斜杠）——
    // 统一去掉尾斜杠再匹配，否则握手前就被 404 拒之门外。
    let path = path.strip_suffix('/').unwrap_or(path);
    match path {
        "/json/version" => json_ok(&version_body(ws_host)),
        "/json" | "/json/list" => json_ok(&list_body(ws_host)),
        "/json/new" => json_ok(&new_target_body(ws_host)),
        p if p.starts_with("/json/activate/") => json_ok(&activate_body()),
        p if p.starts_with("/json/close/") => json_ok(&close_body()),
        _ => http_404(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_body_has_required_fields() {
        let body = version_body("127.0.0.1:9222");
        assert!(
            body.contains("\"Browser\":\"browser-rs/0.0.1\""),
            "got: {body}"
        );
        assert!(body.contains("\"Protocol-Version\":\"1.3\""), "got: {body}");
        assert!(body.contains("\"V8-Version\":\"boa\""), "got: {body}");
        assert!(
            body.contains("\"webSocketDebuggerUrl\":\"ws://127.0.0.1:9222/devtools/browser/browser-rs-target-0\""),
            "got: {body}"
        );
    }

    #[test]
    fn list_body_is_array_with_one_target() {
        let body = list_body("127.0.0.1:9222");
        // Should start with [ and contain the target.
        assert!(body.starts_with('['), "got: {body}");
        assert!(
            body.contains("\"id\":\"browser-rs-target-0\""),
            "got: {body}"
        );
        assert!(body.contains("\"type\":\"page\""), "got: {body}");
        assert!(body.contains("\"url\":\"about:blank\""), "got: {body}");
        assert!(body.ends_with(']'), "got: {body}");
    }

    #[test]
    fn new_target_has_websocket_url() {
        let body = new_target_body("127.0.0.1:9222");
        // new_target_body uses target_object → page-level endpoint.
        assert!(body.contains("ws://127.0.0.1:9222/devtools/page/browser-rs-target-0"));
    }

    #[test]
    fn handle_discovery_routes_paths() {
        let v = handle_discovery("/json/version", "127.0.0.1:9222");
        assert!(v.starts_with("HTTP/1.1 200 OK"));
        assert!(v.contains("application/json"));
        assert!(v.contains("\"Browser\""));

        let l = handle_discovery("/json", "127.0.0.1:9222");
        assert!(l.contains("["));

        let n = handle_discovery("/json/new", "127.0.0.1:9222");
        assert!(n.contains("\"id\""));

        let a = handle_discovery("/json/activate/xyz", "127.0.0.1:9222");
        assert!(a.contains("Target activated"));
    }

    #[test]
    fn handle_discovery_strips_query_string() {
        let v = handle_discovery("/json/version?foo=bar", "127.0.0.1:9222");
        assert!(v.starts_with("HTTP/1.1 200 OK"), "got: {v}");
    }

    #[test]
    fn handle_discovery_strips_trailing_slash() {
        // Playwright connect_over_cdp 探测 `/json/version/`（带尾斜杠）。
        let v = handle_discovery("/json/version/", "127.0.0.1:9222");
        assert!(v.starts_with("HTTP/1.1 200 OK"), "got: {v}");
        assert!(v.contains("\"Browser\""));
    }

    #[test]
    fn unknown_path_returns_404() {
        let r = handle_discovery("/totally/unknown", "127.0.0.1:9222");
        assert!(r.starts_with("HTTP/1.1 404"));
    }

    #[test]
    fn http_response_has_content_length() {
        let r = json_ok("{}");
        assert!(r.contains("Content-Length: 2"));
        assert!(r.contains("\r\n\r\n{}"));
    }
}
