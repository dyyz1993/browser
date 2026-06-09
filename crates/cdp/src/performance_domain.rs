//! Performance domain for CDP (M59.2)
//!
//! Provides basic performance metrics support for Playwright compatibility.
//! Real performance metrics are not available in Browser-RS; we return stub values.

use std::collections::BTreeMap;

use crate::jsonrpc::{CdpError, CdpMessage, Json};

/// Dispatch Performance domain commands.
///
/// Supported methods:
/// - Performance.getMetrics: Returns mock metrics (stubbed)
/// - Performance.enable: No-op acknowledgment
/// - Performance.disable: No-op acknowledgment
pub fn dispatch(id: i64, method: &str, _params: Option<&Json>) -> Result<String, CdpError> {
    match method {
        "Performance.getMetrics" => Ok(get_metrics(id)),
        "Performance.enable" | "Performance.disable" => Ok(CdpMessage::ok_empty(id)),
        _ => Err(CdpError::MethodNotFound(method.to_string())),
    }
}

/// Performance.getMetrics: Returns mock performance metrics.
///
/// Playwright expects this method to exist during connection initialization.
/// We return stub metrics since Browser-RS doesn't track real performance data.
fn get_metrics(id: i64) -> String {
    let metrics = vec![
        ("Timestamp", 0.0f64),
        ("Documents", 1.0),
        ("Frames", 1.0),
        ("JSEventListeners", 0.0),
        ("Nodes", 0.0),
        ("LayoutCount", 0.0),
        ("RecalcStyleCount", 0.0),
        ("LayoutDuration", 0.0),
        ("RecalcStyleDuration", 0.0),
        ("ScriptDuration", 0.0),
        ("TaskDuration", 0.0),
        ("JSHeapUsedSize", 0.0),
        ("JSHeapTotalSize", 0.0),
    ];

    let mut metrics_array = vec![];
    for (name, value) in &metrics {
        let mut m = BTreeMap::new();
        m.insert("name".to_string(), Json::String(name.to_string()));
        m.insert("value".to_string(), Json::Number(*value));
        metrics_array.push(Json::Object(m));
    }

    let mut result = BTreeMap::new();
    result.insert("metrics".to_string(), Json::Array(metrics_array));

    let mut output = String::new();
    output.push('{');
    output.push_str(&format!("\"id\":{},\"result\":{{\"metrics\":[", id));
    for (i, metric) in metrics.iter().enumerate() {
        if i > 0 {
            output.push(',');
        }
        output.push_str(&format!(
            r#"{{"name":"{}","value":{}}}"#,
            metric.0,
            metric.1
        ));
    }
    output.push_str("]}}}\n");

    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_metrics_returns_stubs() {
        let resp = dispatch(1, "Performance.getMetrics", None).unwrap();
        assert!(resp.contains("\"name\":\"Timestamp\""));
        assert!(resp.contains("\"name\":\"Documents\""));
        assert!(resp.contains("\"metrics\":["));
    }

    #[test]
    fn test_enable_returns_ok_empty() {
        let resp = dispatch(2, "Performance.enable", None).unwrap();
        assert_eq!(resp, r#"{"id":2,"result":{}}"#);
    }

    #[test]
    fn test_disable_returns_ok_empty() {
        let resp = dispatch(3, "Performance.disable", None).unwrap();
        assert_eq!(resp, r#"{"id":3,"result":{}}"#);
    }

    #[test]
    fn test_unknown_method_returns_error() {
        let resp = dispatch(4, "Performance.unknown", None);
        assert!(resp.is_err());
    }
}