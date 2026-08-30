//! CDP Emulation domain - device metrics and user agent override
//!
//! Implements:
//! - Emulation.setDeviceMetricsOverride: viewport, device scale factor, mobile mode
//! - Emulation.setUserAgentOverride: custom user agent string
//! - Emulation.clearDeviceMetricsOverride: reset to defaults
//!
//! M81(B2): `setDeviceMetricsOverride` 的 width/height 真正落到
//! [`crate::page::PageState::viewport`] 并触发 `render_from_tree` 重跑布局
//! （px → 格列 = `layout_columns_for_px`），让 Playwright 的
//! `page.setViewportSize()` / `viewport` 参数生效。

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
///
/// M81(B2): `page` 是会话共享的页面状态——视口覆盖写入后立即重跑布局。
pub fn dispatch(
    id: i64,
    method: &str,
    params: Option<&Json>,
    state: &mut EmulationState,
    page: &mut crate::page::PageState,
) -> Result<String, CdpError> {
    match method {
        "Emulation.setDeviceMetricsOverride" => set_device_metrics(id, params, state, page),
        "Emulation.setUserAgentOverride" => set_user_agent(id, params, state),
        "Emulation.clearDeviceMetricsOverride" => clear_device_metrics(id, state, page),
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
    page: &mut crate::page::PageState,
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

    // M81(B2): 视口落地到 PageState 并重跑布局。width/height 缺省按 0 处理；
    // width=0 是 puppeteer resetViewport 的撤销语义（等价 clear）。
    let w = state.width.unwrap_or(0);
    let h = state.height.unwrap_or(0);
    if w == 0 {
        page.clear_viewport();
    } else {
        page.set_viewport(w as usize, h as usize);
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
fn clear_device_metrics(
    id: i64,
    state: &mut EmulationState,
    page: &mut crate::page::PageState,
) -> Result<String, CdpError> {
    state.clear();
    // M81(B2): 视口覆盖同时撤销，布局宽度回默认列数。
    page.clear_viewport();
    Ok(CdpMessage::ok_empty(id))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn make_state() -> EmulationState {
        EmulationState::new()
    }

    /// M81(B2): dispatch 现在需要页面状态（视口落地 + 重布局）。
    fn make_page() -> crate::page::PageState {
        crate::page::PageState::default()
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
        let mut page = make_page();
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
            &mut page,
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
        let mut page = make_page();
        let mut params = BTreeMap::new();
        params.insert("width".to_string(), Json::Number(1024.0));

        let result = dispatch(
            2,
            "Emulation.setDeviceMetricsOverride",
            Some(&Json::Object(params)),
            &mut st,
            &mut page,
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
        let mut page = make_page();
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
            &mut page,
        );
        assert!(result.is_ok());
        assert_eq!(st.user_agent, Some("MyBot/1.0".to_string()));
    }

    #[test]
    fn test_clear_device_metrics() {
        let mut st = make_state();
        let mut page = make_page();
        st.width = Some(800);
        st.height = Some(600);
        st.mobile = true;

        let result = dispatch(
            4,
            "Emulation.clearDeviceMetricsOverride",
            None,
            &mut st,
            &mut page,
        );
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
        let mut page = make_page();
        let result = dispatch(5, "Emulation.unknownMethod", None, &mut st, &mut page);
        assert!(
            result.is_ok(),
            "unknown Emulation method should ack ok_empty"
        );
        // 状态不应被改动。
        assert!(st.width.is_none());
        assert!(st.height.is_none());
    }

    // ── M81(B2): 视口覆盖落地 PageState + 重布局 ──

    #[test]
    fn set_device_metrics_applies_viewport_and_relayout() {
        // 核心验收：width/height 写入 PageState.viewport，布局列数按 px 换算，
        // 页面内容重渲染（rendered_text/layout 更新）。
        let mut st = make_state();
        let mut page = make_page();
        page.render("<html><body><p>M81</p></body></html>", "test://m81", 80);
        assert_eq!(page.width, 80);

        let mut params = BTreeMap::new();
        params.insert("width".to_string(), Json::Number(800.0));
        params.insert("height".to_string(), Json::Number(600.0));
        let result = dispatch(
            6,
            "Emulation.setDeviceMetricsOverride",
            Some(&Json::Object(params)),
            &mut st,
            &mut page,
        );
        assert!(result.is_ok());
        assert_eq!(page.viewport, Some((800, 600)));
        assert_eq!(page.width, browser_render::layout_columns_for_px(800));
        assert!(
            page.rendered_text.contains("M81"),
            "content must survive relayout"
        );
        assert!(page.layout.is_some());
    }

    #[test]
    fn zero_metrics_clear_viewport_puppeteer_reset_semantics() {
        // puppeteer resetViewport 发 width:0,height:0 → 撤销覆盖回默认。
        let mut st = make_state();
        let mut page = make_page();
        page.render("<html><body><p>x</p></body></html>", "test://z", 80);
        page.set_viewport(800, 600);
        assert_ne!(page.width, 80);

        let mut params = BTreeMap::new();
        params.insert("width".to_string(), Json::Number(0.0));
        params.insert("height".to_string(), Json::Number(0.0));
        let result = dispatch(
            7,
            "Emulation.setDeviceMetricsOverride",
            Some(&Json::Object(params)),
            &mut st,
            &mut page,
        );
        assert!(result.is_ok());
        assert!(page.viewport.is_none(), "0×0 must clear the override");
        assert_eq!(page.width, crate::page::DEFAULT_RENDER_WIDTH);
    }

    #[test]
    fn clear_device_metrics_resets_page_viewport() {
        let mut st = make_state();
        let mut page = make_page();
        page.render("<html><body><p>x</p></body></html>", "test://c", 80);
        page.set_viewport(1024, 768);
        assert!(page.viewport.is_some());

        let result = dispatch(
            8,
            "Emulation.clearDeviceMetricsOverride",
            None,
            &mut st,
            &mut page,
        );
        assert!(result.is_ok());
        assert!(page.viewport.is_none());
        assert_eq!(page.width, 80);
        // 宽度回默认列数回换 px（80 列 × cell_w）。
        let (cell_w, _) = browser_render::cell_metrics();
        assert_eq!(
            page.client_viewport_px().0,
            (80.0_f32 * cell_w).round() as i64
        );
    }

    #[test]
    fn viewport_override_survives_relayout_helpers() {
        // set_viewport 后 capture_png_base64 应仍可用（新视口下的渲染产物）。
        let mut page = make_page();
        page.render("<html><body><p>shot</p></body></html>", "test://s", 80);
        page.set_viewport(800, 600);
        assert!(page.capture_png_base64().is_ok());
    }
}
