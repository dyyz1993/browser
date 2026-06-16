//! Input domain for CDP (M59.2)
//!
//! Provides basic mouse/keyboard input support for Playwright compatibility.
//! Since Browser-RS is text-based, input commands are mostly no-ops.

use crate::jsonrpc::{CdpError, CdpMessage, Json};

/// Dispatch Input domain commands.
///
/// Supported methods:
/// - Input.dispatchMouseEvent: No-op (text-based renderer has no mouse events)
/// - Input.dispatchKeyEvent: No-op (text-based renderer has no keyboard events)
/// - Input.setInterceptDrags: No-op acknowledgment
pub fn dispatch(id: i64, method: &str, _params: Option<&Json>) -> Result<String, CdpError> {
    match method {
        "Input.dispatchMouseEvent" | "Input.dispatchKeyEvent" | "Input.setInterceptDrags" => {
            Ok(CdpMessage::ok_empty(id))
        }
        _ => Err(CdpError::MethodNotFound(method.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dispatch_mouse_event_ack() {
        let resp = dispatch(1, "Input.dispatchMouseEvent", None).unwrap();
        assert_eq!(resp, r#"{"id":1,"result":{}}"#);
    }

    #[test]
    fn test_dispatch_key_event_ack() {
        let resp = dispatch(2, "Input.dispatchKeyEvent", None).unwrap();
        assert_eq!(resp, r#"{"id":2,"result":{}}"#);
    }

    #[test]
    fn test_set_intercept_drags_ack() {
        let resp = dispatch(3, "Input.setInterceptDrags", None).unwrap();
        assert_eq!(resp, r#"{"id":3,"result":{}}"#);
    }

    #[test]
    fn test_unknown_method_returns_error() {
        let resp = dispatch(4, "Input.unknown", None);
        assert!(resp.is_err());
    }
}
