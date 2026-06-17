//! CDP Emulation domain - device metrics and user agent override
//!
//! Implements:
//! - Emulation.setDeviceMetricsOverride: viewport, device scale factor, mobile mode
//! - Emulation.setUserAgentOverride: custom user agent string
//! - Emulation.clearDeviceMetricsOverride: reset to defaults

use crate::jsonrpc::{CdpError, CdpMessage, Json};

/// Device metrics emulation state
#[derive(Debug, Clone, Default)]
pub struct EmulationState {
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub device_scale_factor: Option<f64>,
    pub mobile: bool,
    pub user_agent: Option<String>,
}

impl EmulationState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }
}

/// Dispatch Emulation domain commands
pub fn dispatch(
    id: i64,
    method: &str,
    params: Option<&Json>,
    state: &mut EmulationState,
) -> Result<String, CdpError> {
    match method {
        "Emulation.setDeviceMetricsOverride" => set_device_metrics(id, params, state),
        "Emulation.setUserAgentOverride" => set_user_agent(id, params, state),
        "Emulation.clearDeviceMetricsOverride" => clear_device_metrics(id, state),
        // M48: puppeteer 发很多 Emulation.set*EmulationEnabled / setCPUThrottlingRate 等。
        // 我们不真正模拟，但必须 ok_empty（-32601 会让 puppeteer 的 EmulationManager
        // 抛 ProtocolError，整个 newPage 失败）。
        _ => Ok(CdpMessage::ok_empty(id)),
    }
}

/// Emulation.setDeviceMetricsOverride
fn set_device_metrics(
    id: i64,
    params_obj: Option<&Json>,
    state: &mut EmulationState,
) -> Result<String, CdpError> {
    let params = params_obj.ok_or_else(|| CdpError::InvalidJson("params missing".to_string()))?;

    // Extract optional fields
    if let Some(Json::Number(w)) = params.get("width") {
        state.width = Some(*w as u32);
    }
    if let Some(Json::Number(h)) = params.get("height") {
        state.height = Some(*h as u32);
    }
    if let Some(Json::Number(dsf)) = params.get("deviceScaleFactor") {
        state.device_scale_factor = Some(*dsf);
    }
    if let Some(Json::Bool(m)) = params.get("mobile") {
        state.mobile = *m;
    }

    // Return minimal success response
    Ok(CdpMessage::ok_empty(id))
}

/// Emulation.setUserAgentOverride
fn set_user_agent(
    id: i64,
    params_obj: Option<&Json>,
    state: &mut EmulationState,
) -> Result<String, CdpError> {
    let params = params_obj.ok_or_else(|| CdpError::InvalidJson("params missing".to_string()))?;
    let ua = params.get("userAgent").and_then(|v| {
        if let Json::String(s) = v {
            Some(s.clone())
        } else {
            None
        }
    });
    state.user_agent = ua;
    Ok(CdpMessage::ok_empty(id))
}

/// Emulation.clearDeviceMetricsOverride
fn clear_device_metrics(id: i64, state: &mut EmulationState) -> Result<String, CdpError> {
    state.clear();
    Ok(CdpMessage::ok_empty(id))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn make_state() -> EmulationState {
        EmulationState::new()
    }

    #[test]
    fn test_emulation_state_default() {
        let st = EmulationState::default();
        assert!(st.width.is_none());
        assert!(st.height.is_none());
        assert!(st.device_scale_factor.is_none());
        assert!(!st.mobile);
        assert!(st.user_agent.is_none());
    }

    #[test]
    fn test_clear() {
        let mut st = make_state();
        st.width = Some(800);
        st.height = Some(600);
        st.mobile = true;
        st.user_agent = Some("test".to_string());

        st.clear();
        assert!(st.width.is_none());
        assert!(st.height.is_none());
        assert!(!st.mobile);
        assert!(st.user_agent.is_none());
    }

    #[test]
    fn test_set_device_metrics_full() {
        let mut st = make_state();
        let mut params = BTreeMap::new();
        params.insert("width".to_string(), Json::Number(1024.0));
        params.insert("height".to_string(), Json::Number(768.0));
        params.insert("deviceScaleFactor".to_string(), Json::Number(2.0));
        params.insert("mobile".to_string(), Json::Bool(true));

        let result = dispatch(
            1,
            "Emulation.setDeviceMetricsOverride",
            Some(&Json::Object(params)),
            &mut st,
        );
        assert!(result.is_ok());
        assert_eq!(st.width, Some(1024));
        assert_eq!(st.height, Some(768));
        assert_eq!(st.device_scale_factor, Some(2.0));
        assert!(st.mobile);
    }

    #[test]
    fn test_set_device_metrics_partial() {
        let mut st = make_state();
        let mut params = BTreeMap::new();
        params.insert("width".to_string(), Json::Number(1024.0));

        let result = dispatch(
            2,
            "Emulation.setDeviceMetricsOverride",
            Some(&Json::Object(params)),
            &mut st,
        );
        assert!(result.is_ok());
        assert_eq!(st.width, Some(1024));
        assert_eq!(st.height, None);
        assert_eq!(st.device_scale_factor, None);
        assert!(!st.mobile);
    }

    #[test]
    fn test_set_user_agent() {
        let mut st = make_state();
        let mut params = BTreeMap::new();
        params.insert(
            "userAgent".to_string(),
            Json::String("MyBot/1.0".to_string()),
        );

        let result = dispatch(
            3,
            "Emulation.setUserAgentOverride",
            Some(&Json::Object(params)),
            &mut st,
        );
        assert!(result.is_ok());
        assert_eq!(st.user_agent, Some("MyBot/1.0".to_string()));
    }

    #[test]
    fn test_clear_device_metrics() {
        let mut st = make_state();
        st.width = Some(800);
        st.height = Some(600);
        st.mobile = true;

        let result = dispatch(4, "Emulation.clearDeviceMetricsOverride", None, &mut st);
        assert!(result.is_ok());
        assert!(st.width.is_none());
        assert!(st.height.is_none());
        assert!(!st.mobile);
    }

    #[test]
    fn test_unknown_method() {
        // M48: 未知 Emulation 方法返回 ok_empty（而非 MethodNotFound）。
        // puppeteer 连接时会发一批 Emulation.set*（setDeviceMetricsOverride 等），
        // 其中很多我们没有真实实现，但必须 ack 否则 puppeteer 握手失败。
        let mut st = make_state();
        let result = dispatch(5, "Emulation.unknownMethod", None, &mut st);
        assert!(
            result.is_ok(),
            "unknown Emulation method should ack ok_empty"
        );
        // 状态不应被改动。
        assert!(st.width.is_none());
        assert!(st.height.is_none());
    }
}
