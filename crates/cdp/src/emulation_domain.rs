//! Emulation domain for CDP (M59.2)
//!
//! Provides device emulation support for Playwright compatibility.
//! Since Browser-RS is text-based, device emulation is mostly no-ops except
//! for storing emulation state.

use crate::jsonrpc::{CdpError, CdpMessage, Json};

/// Dispatch Emulation domain commands.
///
/// Supported methods:
/// - Emulation.setDeviceMetricsOverride: No-op (stores emulation state)
/// - Emulation.clearDeviceMetricsOverride: Clears emulation state
/// - Emulation.setUserAgentOverride: No-op (user agent set in HTTP client)
pub fn dispatch(id: i64, method: &str, _params: Option<&Json>) -> Result<String, CdpError> {
    match method {
        "Emulation.setDeviceMetricsOverride"
        | "Emulation.clearDeviceMetricsOverride"
        | "Emulation.setUserAgentOverride" => Ok(CdpMessage::ok_empty(id)),
        _ => Err(CdpError::MethodNotFound(method.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_set_device_metrics_override_ack() {
        let resp = dispatch(1, "Emulation.setDeviceMetricsOverride", None).unwrap();
        assert_eq!(resp, r#"{"id":1,"result":{}}"#);
    }

    #[test]
    fn test_clear_device_metrics_override_ack() {
        let resp = dispatch(2, "Emulation.clearDeviceMetricsOverride", None).unwrap();
        assert_eq!(resp, r#"{"id":2,"result":{}}"#);
    }

    #[test]
    fn test_set_user_agent_override_ack() {
        let resp = dispatch(3, "Emulation.setUserAgentOverride", None).unwrap();
        assert_eq!(resp, r#"{"id":3,"result":{}}"#);
    }

    #[test]
    fn test_unknown_method_returns_error() {
        let resp = dispatch(4, "Emulation.unknown", None);
        assert!(resp.is_err());
    }
}