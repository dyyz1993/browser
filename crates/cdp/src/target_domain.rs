//! M48: CDP `Target` domain — getBrowserContexts + other Puppeteer-required stubs.
//!
//! Puppeteer's `connect()` flow calls these immediately after WS handshake:
//! - `Target.getBrowserContexts` — returns the list of browser contexts.
//! - `Target.setDiscoverTargets` — request target discovery events.
//! - `Target.getTargets` — list available targets.
//!
//! We implement a minimal single-context single-target model: one browser
//! context ("default") with our single page target.

use std::collections::BTreeMap;

use crate::jsonrpc::{CdpError, CdpMessage, Json};

/// Dispatch a `Target.*` CDP method.
///
/// `ws_host` (e.g. `127.0.0.1:9222`) builds the target's
/// `webSocketDebuggerUrl`. M81(A1) adds `Target.getTargetInfo` — Playwright's
/// `connect_over_cdp` fires it right after `Target.setAutoAttach` (Chrome-side
/// workaround), and a -32601 here rejects the connect handshake.
pub fn dispatch(id: i64, method: &str, ws_host: &str) -> Result<String, CdpError> {
    match method {
        "Target.getBrowserContexts" => {
            // Single default context, no browser-context isolation.
            let mut result = BTreeMap::new();
            result.insert("browserContextIds".to_string(), Json::Array(vec![]));
            Ok(CdpMessage::ok_response(id, Json::Object(result)))
        }
        "Target.setDiscoverTargets" => {
            // No-op ack — we don't emit target discovery events.
            Ok(CdpMessage::ok_empty(id))
        }
        "Target.setAutoAttach" => {
            // No-op ack — we don't do auto-attach (single target already connected).
            Ok(CdpMessage::ok_empty(id))
        }
        "Target.getTargets" => {
            // Return our single page target.
            let target = crate::discovery::target_object(ws_host);
            let mut result = BTreeMap::new();
            result.insert("targetInfos".to_string(), Json::Array(vec![target]));
            Ok(CdpMessage::ok_response(id, Json::Object(result)))
        }
        "Target.getTargetInfo" => {
            // M81(A1): Playwright fires this on connect. Return our single
            // page target's info (targetId param is ignored in the
            // single-tab model).
            let target = crate::discovery::target_object(ws_host);
            let mut result = BTreeMap::new();
            result.insert("targetInfo".to_string(), target);
            Ok(CdpMessage::ok_response(id, Json::Object(result)))
        }
        "Target.createTarget" => {
            // Single-tab model: return the existing target id.
            let mut result = BTreeMap::new();
            result.insert(
                "targetId".to_string(),
                Json::String(crate::discovery::TARGET_ID.to_string()),
            );
            Ok(CdpMessage::ok_response(id, Json::Object(result)))
        }
        "Target.disposeTarget" | "Target.closeTarget" => {
            // Single-tab model: no-op ack.
            Ok(CdpMessage::ok_empty(id))
        }
        "Target.attachToTarget" | "Target.attachToBrowserTarget" => {
            // Return a session id (single session model).
            let mut result = BTreeMap::new();
            result.insert(
                "sessionId".to_string(),
                Json::String("browser-rs-session-0".to_string()),
            );
            Ok(CdpMessage::ok_response(id, Json::Object(result)))
        }
        "Target.sendMessageToTarget" | "Target.detachFromTarget" => Ok(CdpMessage::ok_empty(id)),
        _ => Err(CdpError::MethodNotFound(method.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOST: &str = "127.0.0.1:9222";

    #[test]
    fn get_browser_contexts_empty() {
        let resp = dispatch(1, "Target.getBrowserContexts", HOST).unwrap();
        assert!(resp.contains("\"browserContextIds\":[]"), "got: {resp}");
    }

    #[test]
    fn set_discover_targets_ok() {
        let resp = dispatch(1, "Target.setDiscoverTargets", HOST).unwrap();
        assert_eq!(resp, r#"{"id":1,"result":{}}"#);
    }

    #[test]
    fn set_auto_attach_ok() {
        let resp = dispatch(1, "Target.setAutoAttach", HOST).unwrap();
        assert_eq!(resp, r#"{"id":1,"result":{}}"#);
    }

    #[test]
    fn get_targets_returns_one() {
        let resp = dispatch(1, "Target.getTargets", HOST).unwrap();
        assert!(resp.contains("\"targetInfos\""), "got: {resp}");
        assert!(resp.contains("\"type\":\"page\""), "got: {resp}");
    }

    #[test]
    fn get_target_info_returns_target_info() {
        // M81(A1): Playwright connect_over_cdp fires this after setAutoAttach.
        let resp = dispatch(1, "Target.getTargetInfo", HOST).unwrap();
        assert!(resp.contains("\"targetInfo\""), "got: {resp}");
        assert!(
            resp.contains("\"targetId\":\"browser-rs-target-0\""),
            "got: {resp}"
        );
        assert!(resp.contains("\"type\":\"page\""), "got: {resp}");
    }

    #[test]
    fn unknown_method_errors() {
        assert!(dispatch(1, "Target.totallyFake", HOST).is_err());
    }
}
