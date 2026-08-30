//! M47/M70.4/M81(B6): CDP `Network` domain.
//!
//! - M47: `getResponseBody` (return main document body)
//! - M70.4: `getCookies` / `deleteCookies` / `setCookie` / `getAllCookies`
//!   + Network.* events now emitted by `Page.navigate` (see page.rs).
//! - M81(B6): `getResponseBody` 支持按 requestId 查 `PageState.network_bodies`
//!   （navigate 填主文档 requestId "1"）；未知 requestId 返回 CDP 错误
//!   "No resource with given identifier found"（Chrome 标准行为）。
//!
//! Cookie management reads/writes `PageState.cookies` (Send-safe owned cookies).

use std::collections::BTreeMap;

use crate::jsonrpc::{CdpError, CdpMessage, Json};
use crate::page::{NetworkCookie, PageState};

/// Dispatch a `Network.*` CDP method.
pub fn dispatch(
    id: i64,
    method: &str,
    params: Option<&Json>,
    state: &PageState,
) -> Result<String, CdpError> {
    match method {
        "Network.enable" | "Network.disable" => {
            // M70.4: events are emitted by Page.navigate regardless of this flag.
            Ok(CdpMessage::ok_empty(id))
        }
        "Network.getResponseBody" => {
            // M81(B6): params.requestId → 查 PageState.network_bodies；
            // 缺 requestId 时兼容旧行为返回主文档体。查不到 → CDP 错误
            // （Chrome: "No resource with given identifier found"）。
            let body = match params.and_then(|p| p.get_str("requestId")) {
                None => Some(state.raw_html.clone()),
                Some(rid) => state.network_bodies.get(rid).cloned(),
            };
            match body {
                Some(b) => {
                    let mut result = BTreeMap::new();
                    result.insert("body".to_string(), Json::String(b));
                    result.insert("base64Encoded".to_string(), Json::Bool(false));
                    Ok(CdpMessage::ok_response(id, Json::Object(result)))
                }
                None => Err(CdpError::NotFound(
                    "No resource with given identifier found".to_string(),
                )),
            }
        }
        "Network.getCookies" | "Network.getAllCookies" => {
            // M70.4: return all cookies (optional `urls` filter ignored for simplicity).
            let cookies = state.cookies.iter().map(cookie_to_json).collect::<Vec<_>>();
            let mut result = BTreeMap::new();
            result.insert("cookies".to_string(), Json::Array(cookies));
            Ok(CdpMessage::ok_response(id, Json::Object(result)))
        }
        "Network.setCookie" => {
            // M70.4: set a cookie. Puppeteer `page.setCookie()`.
            // Params: name, value, domain, path. We can't mutate state here
            // (dispatch takes &PageState), so this is ack-only. Cookie setting
            // via CDP for the next navigate is a known limitation — use the
            // cookie persistence file or --cookie-file flag instead.
            let mut result = BTreeMap::new();
            result.insert("success".to_string(), Json::Bool(true));
            Ok(CdpMessage::ok_response(id, Json::Object(result)))
        }
        "Network.deleteCookies" => {
            // M70.4: ack-only (same &PageState constraint as setCookie).
            Ok(CdpMessage::ok_empty(id))
        }
        // M53: unknown methods → no-op ack (puppeteer sends many enable/disable)
        _ => Ok(CdpMessage::ok_empty(id)),
    }
}

/// M70.4: Convert a NetworkCookie to the CDP `Network.Cookie` JSON shape.
fn cookie_to_json(c: &NetworkCookie) -> Json {
    let mut m = BTreeMap::new();
    m.insert("name".to_string(), Json::String(c.name.clone()));
    m.insert("value".to_string(), Json::String(c.value.clone()));
    m.insert("domain".to_string(), Json::String(c.domain.clone()));
    m.insert("path".to_string(), Json::String(c.path.clone()));
    m.insert(
        "size".to_string(),
        Json::Number((c.name.len() + c.value.len()) as f64),
    );
    m.insert("httpOnly".to_string(), Json::Bool(c.http_only));
    m.insert("secure".to_string(), Json::Bool(c.secure));
    m.insert("session".to_string(), Json::Bool(true));
    Json::Object(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_state_with_cookie() -> PageState {
        let mut st = PageState::default();
        st.cookies.push(NetworkCookie {
            name: "session".into(),
            value: "abc123".into(),
            domain: "example.com".into(),
            path: "/".into(),
            secure: true,
            http_only: true,
        });
        st.raw_html = "<html><body>Hello</body></html>".to_string();
        st
    }

    #[test]
    fn enable_returns_ok_empty() {
        let st = PageState::default();
        let resp = dispatch(1, "Network.enable", None, &st).unwrap();
        assert_eq!(resp, r#"{"id":1,"result":{}}"#);
    }

    #[test]
    fn get_response_body_returns_html() {
        let st = make_state_with_cookie();
        // 兼容旧行为：无 requestId → 返回主文档体。
        let resp = dispatch(1, "Network.getResponseBody", None, &st).unwrap();
        assert!(
            resp.contains("\"body\":\"<html><body>Hello</body></html>\""),
            "got: {resp}"
        );
    }

    // ── M81(B6): requestId → network_bodies 查表 ──

    #[test]
    fn get_response_body_by_request_id_found() {
        let mut st = make_state_with_cookie();
        st.network_bodies.insert(
            "1".to_string(),
            "<html><body>Main</body></html>".to_string(),
        );
        st.network_bodies
            .insert("2".to_string(), "xhr-payload".to_string());
        let mut params = BTreeMap::new();
        params.insert("requestId".to_string(), Json::String("2".to_string()));
        let resp = dispatch(
            1,
            "Network.getResponseBody",
            Some(&Json::Object(params)),
            &st,
        )
        .unwrap();
        assert!(resp.contains("\"body\":\"xhr-payload\""), "got: {resp}");
        assert!(resp.contains("\"base64Encoded\":false"), "got: {resp}");
        assert!(resp.contains("\"id\":1"), "got: {resp}");
    }

    #[test]
    fn get_response_body_request_id_one_returns_main_doc() {
        let mut st = make_state_with_cookie();
        st.network_bodies.insert(
            "1".to_string(),
            "<html><body>Main</body></html>".to_string(),
        );
        let mut params = BTreeMap::new();
        params.insert("requestId".to_string(), Json::String("1".to_string()));
        let resp = dispatch(
            1,
            "Network.getResponseBody",
            Some(&Json::Object(params)),
            &st,
        )
        .unwrap();
        assert!(
            resp.contains("\"body\":\"<html><body>Main</body></html>\""),
            "got: {resp}"
        );
    }

    #[test]
    fn get_response_body_unknown_request_id_errors() {
        let st = make_state_with_cookie();
        let mut params = BTreeMap::new();
        params.insert("requestId".to_string(), Json::String("99".to_string()));
        let err = dispatch(
            1,
            "Network.getResponseBody",
            Some(&Json::Object(params)),
            &st,
        )
        .unwrap_err();
        // Chrome 标准错误文案。
        assert!(
            err.to_string()
                .contains("No resource with given identifier"),
            "got: {err}"
        );
    }

    #[test]
    fn get_response_body_empty_map_with_request_id_errors() {
        // 主文档还没记录（navigate 前）但带了 requestId → NotFound。
        let st = PageState::default();
        let mut params = BTreeMap::new();
        params.insert("requestId".to_string(), Json::String("1".to_string()));
        let err = dispatch(
            1,
            "Network.getResponseBody",
            Some(&Json::Object(params)),
            &st,
        );
        assert!(err.is_err(), "unknown id must error, got {err:?}");
    }

    #[test]
    fn get_cookies_returns_stored_cookies() {
        let st = make_state_with_cookie();
        let resp = dispatch(1, "Network.getCookies", None, &st).unwrap();
        assert!(resp.contains("\"name\":\"session\""), "got: {resp}");
        assert!(resp.contains("\"value\":\"abc123\""), "got: {resp}");
        assert!(resp.contains("\"secure\":true"), "got: {resp}");
        assert!(resp.contains("\"httpOnly\":true"), "got: {resp}");
    }

    #[test]
    fn get_cookies_empty_when_none() {
        let st = PageState::default();
        let resp = dispatch(1, "Network.getCookies", None, &st).unwrap();
        assert!(resp.contains("\"cookies\":[]"), "got: {resp}");
    }

    #[test]
    fn set_cookie_acks_success() {
        let st = PageState::default();
        let resp = dispatch(1, "Network.setCookie", None, &st).unwrap();
        assert!(resp.contains("\"success\":true"), "got: {resp}");
    }

    #[test]
    fn delete_cookies_acks_empty() {
        let st = PageState::default();
        let resp = dispatch(1, "Network.deleteCookies", None, &st).unwrap();
        assert_eq!(resp, r#"{"id":1,"result":{}}"#);
    }

    #[test]
    fn unknown_method_noop() {
        let st = make_state_with_cookie();
        let resp = dispatch(1, "Network.totallyFake", None, &st).unwrap();
        assert!(resp.contains(r#""result":{}"#), "got: {resp}");
    }
}
