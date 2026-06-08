//! M47: CDP `Network` domain — getResponseBody + enable/disable.
//!
//! Lets clients inspect the raw response body of network requests. M47
//! scope: return the main document's HTML (fetched during Page.navigate).
//! Full request interception (XHR/fetch capture) is M48+.

use std::collections::BTreeMap;

use crate::jsonrpc::{CdpError, CdpMessage, Json};
use crate::page::PageState;

/// Dispatch a `Network.*` CDP method.
pub fn dispatch(id: i64, method: &str, state: &PageState) -> Result<String, CdpError> {
    match method {
        "Network.enable" | "Network.disable" => {
            // No-op ack — we don't emit network events yet.
            Ok(CdpMessage::ok_empty(id))
        }
        "Network.getResponseBody" => {
            // Return the main document body (requestId ignored in single-doc model).
            let mut result = BTreeMap::new();
            result.insert("body".to_string(), Json::String(state.raw_html.clone()));
            result.insert("base64Encoded".to_string(), Json::Bool(false));
            Ok(CdpMessage::ok_response(id, Json::Object(result)))
        }
        // M53: unknown methods → no-op ack (puppeteer sends many enable/disable)
        _ => Ok(CdpMessage::ok_empty(id)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_state() -> PageState {
        PageState {
            raw_html: "<html><body>Hello</body></html>".to_string(),
            ..PageState::default()
        }
    }

    #[test]
    fn enable_returns_ok_empty() {
        let st = make_state();
        let resp = dispatch(1, "Network.enable", &st).unwrap();
        assert_eq!(resp, r#"{"id":1,"result":{}}"#);
    }

    #[test]
    fn get_response_body_returns_html() {
        let st = make_state();
        let resp = dispatch(1, "Network.getResponseBody", &st).unwrap();
        assert!(
            resp.contains("\"body\":\"<html><body>Hello</body></html>\""),
            "got: {resp}"
        );
        assert!(resp.contains("\"base64Encoded\":false"), "got: {resp}");
    }

    #[test]
    fn get_response_body_empty_when_not_loaded() {
        let st = PageState::default();
        let resp = dispatch(1, "Network.getResponseBody", &st).unwrap();
        assert!(resp.contains("\"body\":\"\""), "got: {resp}");
    }

    #[test]
    fn unknown_method_noop() {
        let st = PageState {
            raw_html: "<html><body>Hello</body></html>".to_string(),
            ..PageState::default()
        };
        // M53: unknown methods return no-op ack
        let resp = dispatch(1, "Network.totallyFake", &st).unwrap();
        assert!(resp.contains(r#""result":{}"#), "got: {resp}");
    }
}
