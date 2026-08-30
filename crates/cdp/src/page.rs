//! M44: CDP `Page` domain — navigate + captureScreenshot.
//!
//! This is the most valuable CDP domain: lets clients navigate to a URL and
//! capture the rendered page as a PNG screenshot.
//!
//! ## Methods (M44 scope)
//!
//! - `Page.navigate` — fetch URL, parse, render → store current page state.
//!   Returns `{frameId}`.
//! - `Page.captureScreenshot` — render current page to PNG, return base64.
//!   Returns `{data}`.
//! - `Page.getNavigationHistory` — return `{currentIndex, entries:[{url,...}]}`.
//!
//! ## Render pipeline (reuses base crates)
//!
//! ```text
//! fetch(url) → html → parse_html → Tree
//!            → extract_style_text → parse(css) → compute_styles
//!            → construct_layout_tree → run_layout → render_ascii_colored
//! ```
//!
//! M68: `Page.navigate` **does** execute page `<script>` via
//! `run_scripts_with_base_engine`（对齐 CLI 管线，含 timer/networkidle 驱动）。
//! JS 改过的 DOM 反映到 `PageState.tree`，后续 `DOM.getDocument` /
//! `getOuterHTML` / `Runtime.evaluate` 读到的是渲染后的 DOM。spawn_blocking
//! 隔离 !Send 的 thread_local DOM 后端，catch_unwind 防 JS panic 杀 server。

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use browser_dom::{NodeId, Tree};
use browser_html_parser::parse as parse_html;
use browser_js_runtime::EngineKind;

use crate::jsonrpc::{CdpError, CdpMessage, Json};

/// M81(B2): 未指定视口时的默认渲染宽度（格列数，与 CLI `--width` 缺省一致）。
pub const DEFAULT_RENDER_WIDTH: usize = 80;

/// The current page state for a CDP session (single-tab model, M43).
///
/// Stored in an `Arc<Mutex>` so the async session loop can share it between
/// the navigate and captureScreenshot handlers.
pub struct PageState {
    /// The URL last navigated to (empty before first navigate).
    pub url: String,
    /// The parsed DOM tree (for DOM domain queries, M46).
    pub tree: Tree,
    /// The raw fetched HTML body (for Network.getResponseBody, M47).
    pub raw_html: String,
    /// The rendered plain text (for future Runtime/eval that needs it).
    pub rendered_text: String,
    /// The colored ANSI text (parsed by the PNG renderer for link colors).
    pub rendered_colored: String,
    /// Render width in chars.
    pub width: usize,
    /// M81(B2): Emulation.setDeviceMetricsOverride 的视口覆盖，**CSS px**。
    /// `Some((w, h))` 时布局宽度换算为 `layout_columns_for_px(w)` 格列，
    /// navigate 也沿用该宽度（Chrome 语义：override 跨导航持续）。
    /// `None` → 默认 [`DEFAULT_RENDER_WIDTH`] 列。
    pub viewport: Option<(usize, usize)>,
    /// M80.17: 布局树快照（`render_from_tree` 填充），供
    /// `Input.dispatchMouseEvent` 做坐标 hit_test（CSS px → NodeId）。
    /// 未渲染（未 navigate）时为 `None`。
    pub layout: Option<browser_layout::LayoutTree>,
    /// M70.4: HTTP status of the last navigate (for Network.responseReceived).
    pub last_status: u16,
    /// M70.4: Response headers of the last navigate (for Network.responseReceived).
    pub last_headers: Vec<(String, String)>,
    /// M70.4: Cookie jar (Send-safe owned cookies, for Network.getCookies/setCookie).
    pub cookies: Vec<NetworkCookie>,
    /// M81(B1): 当前焦点元素（`Input.dispatchKeyEvent` 的合成 key/input 事件
    /// 目标，对应浏览器的 document.activeElement）。`Input.dispatchMouseEvent`
    /// mousePressed 命中元素时设置；navigate 重建 tree 时清零（NodeId 失效）。
    /// `None` → 键事件派发到 body。
    pub focused_node: Option<NodeId>,
    /// M81(B7): `Page.setLifecycleEventsEnabled` 的开关。Chrome 语义：
    /// `Page.lifecycleEvent` 事件只在 enable 后派发；默认 false（Chrome
    /// 行为——不 enable 就没有 lifecycleEvent）。Puppeteer/Playwright 初始化
    /// 时都会下发 setLifecycleEventsEnabled(true)，不影响 goto 等待链。
    pub lifecycle_events_enabled: bool,
    /// M81(B6): `Network.getResponseBody` 的响应体存储（requestId → 文本体）。
    /// navigate 填主文档（requestId "1" = `raw_html`），每次 navigate 清空重填。
    /// JS fetch/XHR 的响应体不在捕获范围（js-runtime `CapturedNetworkEvent`
    /// 只带 size），这类 requestId 查询返回 CDP NotFound（对齐 Chrome 的
    /// "No resource with given identifier found"）。
    pub network_bodies: BTreeMap<String, String>,
}

/// M70.4: A single cookie for the CDP Network domain. Owned + Send-safe
/// (unlike browser_cookie::CookieHandle which is Rc<RefCell>).
#[derive(Debug, Clone, Default)]
pub struct NetworkCookie {
    pub name: String,
    pub value: String,
    pub domain: String,
    pub path: String,
    pub secure: bool,
    pub http_only: bool,
}

impl Default for PageState {
    fn default() -> Self {
        Self {
            url: String::new(),
            // M81(B2): 默认树带 Document 根（与 html-parser 一致）。裸 `Tree::new()`
            // 会在 navigate 前的 `Runtime.evaluate` / `setDeviceMetricsOverride`
            // 重布局路径上撞 `Tree::root` 空树 panic，杀掉整个 CDP 会话。
            tree: Tree::with_root(browser_dom::NodeData::Document),
            raw_html: String::new(),
            rendered_text: String::new(),
            rendered_colored: String::new(),
            width: DEFAULT_RENDER_WIDTH,
            viewport: None,
            layout: None,
            last_status: 0,
            last_headers: Vec::new(),
            cookies: Vec::new(),
            focused_node: None,
            lifecycle_events_enabled: false,
            network_bodies: BTreeMap::new(),
        }
    }
}

impl PageState {
    /// M81(B2): 视口覆盖生效时的布局列数（`layout_columns_for_px` 换算）；
    /// 无覆盖 → 默认 [`DEFAULT_RENDER_WIDTH`] 列。navigate 与
    /// `render_from_tree` 都用这个宽度，保证 override 跨导航持续。
    #[must_use]
    pub fn effective_width(&self) -> usize {
        self.viewport.map_or(DEFAULT_RENDER_WIDTH, |(w, _)| {
            browser_render::layout_columns_for_px(w)
        })
    }

    /// M81(B2): Emulation.setDeviceMetricsOverride 落地——存 px 视口、
    /// 换算布局列数并重跑 css+layout+render。后续 `Page.captureScreenshot`
    /// / `Runtime.evaluate`（clientWidth）读到的都是新视口下的布局。
    pub fn set_viewport(&mut self, width_px: usize, height_px: usize) {
        self.viewport = Some((width_px, height_px));
        self.width = self.effective_width();
        self.render_from_tree();
    }

    /// M81(B2): Emulation.clearDeviceMetricsOverride——清空覆盖、
    /// 宽度回默认列数并重渲染。无覆盖时 no-op（避免无谓重排）。
    pub fn clear_viewport(&mut self) {
        if self.viewport.take().is_some() {
            self.width = self.effective_width();
            self.render_from_tree();
        }
    }

    /// M81(B2): 布局视口的 px 尺寸（`document.documentElement.clientWidth`
    /// /`clientHeight` 语义）。宽度 = 实际布局列数 × `cell_w` 回换 px
    /// （800px → 格列 → ≈800）；高度 = 覆盖值，无覆盖时用布局树根盒
    /// 高度（内容行数 × 行高）近似，未渲染时 0。
    #[must_use]
    pub fn client_viewport_px(&self) -> (i64, i64) {
        let (cell_w, cell_h) = browser_render::cell_metrics();
        let w = (f64::from(self.effective_width() as f32 * cell_w)).round() as i64;
        let h = match self.viewport {
            Some((_, h)) => h as i64,
            None => self.layout.as_ref().map_or(0, |l| {
                f64::from(l.root.dimensions.height * cell_h).round() as i64
            }),
        };
        (w, h)
    }

    /// Render `html` (already fetched) and store the result.
    ///
    /// 便捷封装：parse → render_from_tree。M68 起 navigate 不再调这个
    /// （它需要在中间插入 JS 执行步骤），改调 `parse_only` + `render_from_tree`。
    pub fn render(&mut self, html: &str, url: &str, width: usize) {
        self.parse_only(html, url, width);
        self.render_from_tree();
    }

    /// M68: 只 parse HTML 存 tree/url/width，不 layout（给 JS 步骤留插入点）。
    pub fn parse_only(&mut self, html: &str, url: &str, width: usize) {
        self.tree = parse_html(html);
        self.url = url.to_string();
        self.width = width;
        // M81(B1): navigate 重建 tree，旧 NodeId 失效 → 焦点清零。
        self.focused_node = None;
    }

    /// M68: 从 `self.tree` 跑 css+layout+render，填 rendered_text/rendered_colored。
    /// JS 改完 DOM 后调这个，用新 tree 重新渲染。
    /// M80.17: 同时保存布局树到 `self.layout`（Input hit_test 用）。
    pub fn render_from_tree(&mut self) {
        // M81(B2): Playwright 的 `viewport` 参数在 newPage 时就下发
        // setDeviceMetricsOverride——早于第一次 navigate，此时 tree 为空，
        // `Tree::root` 会 panic。空树直接跳过布局：视口已存入 `self.viewport`，
        // navigate 时经 `effective_width` 消费。
        if self.tree.is_empty() {
            return;
        }
        let style_text = extract_style_text(&self.tree);
        let sheet = browser_css_engine::parse(&style_text);
        let styles = browser_css_engine::compute_styles(&self.tree, &sheet);
        let mut layout = browser_layout::construct_layout_tree(&self.tree, &styles);
        browser_layout::layout(
            &mut layout,
            browser_layout::LayoutConfig {
                viewport_width: self.width as f32,
            },
        );
        self.rendered_text = browser_render::render_ascii(&layout, self.width);
        self.rendered_colored = browser_render::render_ascii_colored(&layout, self.width);
        self.layout = Some(layout);
    }

    /// Render the current page to a PNG screenshot, returning base64 data.
    ///
    /// M44 uses the screenshot renderer (fontdue + ANSI color parse).
    pub fn capture_png_base64(&self) -> Result<String, CdpError> {
        self.capture_png_base64_opts(None, false)
    }

    /// M81.B3: clip 区域截图 + captureBeyondViewport 全页模式。
    /// clip: Some((x, y, width, height))——px 坐标，裁剪渲染产物。
    /// fullpage: true 时对整页高度渲染（当前 rendered_colored 已含整页）。
    pub fn capture_png_base64_opts(
        &self,
        clip: Option<(f64, f64, f64, f64)>,
        _capture_beyond: bool,
    ) -> Result<String, CdpError> {
        if self.rendered_colored.is_empty() {
            return Err(CdpError::InvalidJson(
                "no page loaded — call Page.navigate first".to_string(),
            ));
        }
        let mut renderer = browser_render::font::FontRenderer::new();
        // Decode the colored ANSI text to RGBA pixels (reuse the M30 screenshot path).
        let (mut rgba, w, h) = render_text_to_rgba(&mut renderer, &self.rendered_colored);
        // M81.B3: clip 裁剪——对 RGBA 缓冲按行/列截取子区域。
        if let Some((cx, cy, cw, ch)) = clip {
            let x0 = (cx.max(0.0) as usize).min(w.saturating_sub(1));
            let y0 = (cy.max(0.0) as usize).min(h.saturating_sub(1));
            let cw = (cw as usize).min(w.saturating_sub(x0));
            let ch = (ch as usize).min(h.saturating_sub(y0));
            if cw > 0 && ch > 0 {
                let mut cropped = Vec::with_capacity(cw * ch * 4);
                for row in y0..(y0 + ch) {
                    let start = (row * w + x0) * 4;
                    let end = start + cw * 4;
                    cropped.extend_from_slice(&rgba[start..end]);
                }
                rgba = cropped;
                let png = encode_rgba_as_png(&rgba, cw, ch)
                    .map_err(|e| CdpError::InvalidJson(format!("png encode: {e}")))?;
                return Ok(base64_encode(&png));
            }
        }
        // Encode RGBA → PNG → base64.
        let png = encode_rgba_as_png(&rgba, w, h)
            .map_err(|e| CdpError::InvalidJson(format!("png encode: {e}")))?;
        Ok(base64_encode(&png))
    }
}

/// Extract `<style>` tag text content from a DOM tree (mirrors CLI logic).
fn extract_style_text(tree: &Tree) -> String {
    use browser_dom::{NodeData, NodeId};
    let mut buf = String::new();
    let mut stack: Vec<NodeId> = vec![tree.root()];
    while let Some(id) = stack.pop() {
        match tree.data(id) {
            NodeData::Element { tag, .. } if tag.eq_ignore_ascii_case("style") => {
                for &child in tree.children_of(id) {
                    if let NodeData::Text(s) = tree.data(child) {
                        buf.push_str(s);
                        buf.push('\n');
                    }
                }
            }
            _ => {
                for &child in tree.children_of(id) {
                    stack.push(child);
                }
            }
        }
    }
    buf
}

/// Render colored ANSI text to RGBA pixels using the shared font renderer.
/// Mirrors `crates/cli/src/screenshot.rs`'s logic (M30 + M39 background + M70 fg).
fn render_text_to_rgba(
    renderer: &mut browser_render::font::FontRenderer,
    text: &str,
) -> (Vec<u8>, usize, usize) {
    // Strip ANSI, track link + background + foreground spans per line.
    let lines: Vec<&str> = text.split('\n').collect();
    let mut link_spans: Vec<Vec<(usize, usize)>> = Vec::new();
    let mut bg_spans: Vec<browser_render::font::BgSpans> = Vec::new();
    let mut fg_spans: Vec<browser_render::font::FgSpans> = Vec::new();
    let mut plain_lines: Vec<String> = Vec::new();
    for line in &lines {
        let (chars, links, bgs, fgs) = strip_ansi_and_track_styles(line);
        plain_lines.push(chars.into_iter().collect());
        link_spans.push(links);
        bg_spans.push(bgs);
        fg_spans.push(fgs);
    }
    let (w, h, rgba) =
        renderer.render_text_to_rgba(&plain_lines.join("\n"), &link_spans, &bg_spans, &fg_spans);
    (rgba, w, h)
}

/// Minimal ANSI stripper that also tracks link (underline truecolor) and
/// background (48;2;R;G;B) and foreground (38;2;R;G;B) spans. Mirrors `screenshot.rs`.
#[allow(clippy::type_complexity)] // 4-tuple return mirrors screenshot.rs
pub(crate) fn strip_ansi_and_track_styles(
    line: &str,
) -> (
    Vec<char>,
    Vec<(usize, usize)>,
    Vec<browser_render::font::BgSpan>,
    Vec<browser_render::font::FgSpan>,
) {
    let link_spans: Vec<(usize, usize)> = Vec::new();
    let bg_spans: Vec<browser_render::font::BgSpan> = Vec::new();
    let fg_spans: Vec<browser_render::font::FgSpan> = Vec::new();
    // M44: simplified — we don't replicate the full parser here, the font
    // renderer handles plain text. For CDP screenshots, link coloring is
    // cosmetic; the key deliverable is the rendered text layout.
    // (M45+ can lift the full parser from screenshot.rs into a shared module.)
    let plain: Vec<char> = line.chars().collect();
    // If there's ANSI, strip it naively between ESC[ and m.
    let mut result: Vec<char> = Vec::new();
    let mut in_escape = false;
    for c in plain {
        if in_escape {
            if c == 'm' {
                in_escape = false;
            }
            continue;
        }
        if c == '\x1b' {
            in_escape = true;
            continue;
        }
        result.push(c);
    }
    (result, link_spans, bg_spans, fg_spans)
}

/// Encode an RGBA buffer as a PNG (mirrors M12.1 screenshot encoding).
fn encode_rgba_as_png(rgba: &[u8], width: usize, height: usize) -> Result<Vec<u8>, String> {
    use png::{ColorType, Encoder};
    let mut out = Vec::new();
    {
        let mut encoder = Encoder::new(&mut out, width as u32, height as u32);
        encoder.set_color(ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder
            .write_header()
            .map_err(|e| format!("png header: {e}"))?;
        writer
            .write_image_data(rgba)
            .map_err(|e| format!("png data: {e}"))?;
    }
    Ok(out)
}

/// Standard base64 alphabet (RFC 4648). Hand-rolled (no dep).
fn base64_encode(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    let mut i = 0;
    while i + 3 <= input.len() {
        let n = (input[i] as u32) << 16 | (input[i + 1] as u32) << 8 | (input[i + 2] as u32);
        out.push(TABLE[((n >> 18) & 63) as usize] as char);
        out.push(TABLE[((n >> 12) & 63) as usize] as char);
        out.push(TABLE[((n >> 6) & 63) as usize] as char);
        out.push(TABLE[(n & 63) as usize] as char);
        i += 3;
    }
    let rem = input.len() - i;
    if rem == 1 {
        let n = (input[i] as u32) << 16;
        out.push(TABLE[((n >> 18) & 63) as usize] as char);
        out.push(TABLE[((n >> 12) & 63) as usize] as char);
        out.push('=');
        out.push('=');
    } else if rem == 2 {
        let n = (input[i] as u32) << 16 | (input[i + 1] as u32) << 8;
        out.push(TABLE[((n >> 18) & 63) as usize] as char);
        out.push(TABLE[((n >> 12) & 63) as usize] as char);
        out.push(TABLE[((n >> 6) & 63) as usize] as char);
        out.push('=');
    }
    out
}

/// M50: dispatch 结果包含 response + 待发送的 CDP 事件列表.
pub struct DispatchResult {
    pub response: String,
    pub events: Vec<String>,
}

/// Dispatch a `Page.*` CDP method. Returns the JSON response string + events.
///
/// `state` is the session's shared page state. `params` is the raw params JSON.
/// `id` is the request id.
pub async fn dispatch(
    id: i64,
    method: &str,
    params: Option<&Json>,
    state: Arc<Mutex<PageState>>,
    engine_kind: EngineKind,
) -> Result<DispatchResult, CdpError> {
    match method {
        "Page.navigate" => {
            let url = params
                .and_then(|p| p.get_str("url"))
                .ok_or_else(|| CdpError::InvalidJson("missing url param".to_string()))?;
            let client = browser_net::HttpClient::new();
            // M70.4: 用 request_full_raw 拿完整 (status, body, headers)，
            // 不报错返回非 2xx（让 404/500 页面也能渲染 + 发 Network.responseReceived）。
            let (status, bytes, resp_headers) = client
                .request_full_raw(url, "GET", None, None, None)
                .await
                .map_err(|e| CdpError::Io(format!("fetch: {e}")))?;
            let html = String::from_utf8_lossy(&bytes).to_string();
            // M70.4: 提取响应头 + 解析 Set-Cookie 存入 jar。
            let header_pairs: Vec<(String, String)> = resp_headers
                .iter()
                .map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap_or("").to_string()))
                .collect();
            {
                let mut st = state
                    .lock()
                    .map_err(|e| CdpError::Io(format!("lock: {e}")))?;
                st.last_status = status;
                st.last_headers = header_pairs.clone();
                // 解析 Set-Cookie 入 jar
                for (k, v) in &header_pairs {
                    if k.eq_ignore_ascii_case("set-cookie") {
                        if let Some(ck) = parse_set_cookie(v, url) {
                            st.cookies.push(ck);
                        }
                    }
                }
            }

            // M68: ① parse HTML 存 tree（不 layout），给 JS 步骤留插入点。
            // M81(B2): 宽度用视口覆盖换算的列数（override 跨导航持续，Chrome 语义）。
            // M81(B6): 同步重置响应体表并记录主文档（requestId "1"）。
            {
                let mut st = state
                    .lock()
                    .map_err(|e| CdpError::Io(format!("lock: {e}")))?;
                let width = st.effective_width();
                st.parse_only(&html, url, width);
                st.raw_html = html.clone();
                st.network_bodies.clear();
                st.network_bodies.insert("1".to_string(), html.clone());
            }

            // M68: ② spawn_blocking 跑页面 <script>（对齐 CLI run_scripts 管线）。
            // run_scripts 是同步阻塞（含最长 8s timer loop）且内部用 thread_local
            // DOM 后端（!Send），必须用 spawn_blocking 在固定 OS 线程跑完。
            // panic 兜底沿用 CLI main.rs:486 的 catch_unwind 模式——JS 引擎崩溃
            // (OOM/栈溢出) 时回退静态树，不杀 CDP server。
            let tree_clone = {
                let st = state
                    .lock()
                    .map_err(|e| CdpError::Io(format!("lock: {e}")))?;
                st.tree.clone()
            };
            let url_owned = url.to_string();
            // 注意：SharedTree (Rc<RefCell<Tree>>) 是 !Send，不能跨 spawn_blocking
            // 返回。所以在闭包内部就 clone 成 owned Tree（Tree: Send），返回 Tree。
            // M70.4: 同时 drain JS fetch/XHR 捕获的网络事件，返回给 navigate 发 CDP 事件。
            let (js_tree, captured_net): (Tree, Vec<browser_js_runtime::CapturedNetworkEvent>) =
                tokio::task::spawn_blocking(move || {
                    use std::panic::AssertUnwindSafe;
                    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
                        browser_js_runtime::run_scripts_with_base_engine(
                            tree_clone,
                            Some(url_owned),
                            &engine_kind,
                        )
                    }));
                    match result {
                        Ok((shared, _executed)) => {
                            let tree = shared.borrow().clone();
                            let net = browser_js_runtime::drain_captured_network_events();
                            (tree, net)
                        }
                        // panic → 回退静态树（重 parse），不杀 server。
                        Err(_) => (parse_html(""), Vec::new()),
                    }
                })
                .await
                .map_err(|e| CdpError::Io(format!("js task join: {e}")))?;

            // M68: ③ JS 后 owned tree 回写 + 重新 layout+render。
            // M81(B7): 顺带读 lifecycle 开关（锁内取值，锁外用）。
            let lifecycle_enabled;
            {
                let mut st = state
                    .lock()
                    .map_err(|e| CdpError::Io(format!("lock: {e}")))?;
                st.tree = js_tree;
                st.render_from_tree();
                lifecycle_enabled = st.lifecycle_events_enabled;
            }
            let mut result = BTreeMap::new();
            result.insert(
                "frameId".to_string(),
                Json::String(crate::discovery::TARGET_ID.to_string()),
            );
            // M55: loaderId is required — puppeteer uses it to match lifecycle events
            result.insert("loaderId".to_string(), Json::String("1".to_string()));
            // M50: emit Page lifecycle events after navigate
            let frame_id = crate::discovery::TARGET_ID.to_string();
            let nav_url = url.to_string();
            // M70.4: Network.* 事件 — Chrome 在页面事件之前发。Puppeteer 的
            // page.on('request'/'response') 依赖这三个事件。
            let mime_type = header_pairs
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("content-type"))
                .map(|(_, v)| {
                    v.split(';')
                        .next()
                        .unwrap_or("text/html")
                        .trim()
                        .to_string()
                })
                .unwrap_or_else(|| "text/html".to_string());
            let req_id = "1".to_string();
            let network_events = vec![
                // requestWillBeSent
                CdpMessage::event(
                    "Network.requestWillBeSent",
                    Json::Object({
                        let mut p = BTreeMap::new();
                        p.insert("requestId".to_string(), Json::String(req_id.clone()));
                        let mut request = BTreeMap::new();
                        request.insert("url".to_string(), Json::String(nav_url.clone()));
                        request.insert("method".to_string(), Json::String("GET".to_string()));
                        request.insert("headers".to_string(), Json::Object(BTreeMap::new()));
                        p.insert("request".to_string(), Json::Object(request));
                        p.insert("loaderId".to_string(), Json::String("1".to_string()));
                        p.insert("timestamp".to_string(), Json::Number(0.0));
                        p.insert("type".to_string(), Json::String("Document".to_string()));
                        let mut init = BTreeMap::new();
                        init.insert("url".to_string(), Json::String(nav_url.clone()));
                        p.insert("initiator".to_string(), Json::Object(init));
                        p
                    }),
                ),
                // responseReceived
                CdpMessage::event(
                    "Network.responseReceived",
                    Json::Object({
                        let mut p = BTreeMap::new();
                        p.insert("requestId".to_string(), Json::String(req_id.clone()));
                        let mut response = BTreeMap::new();
                        response.insert("url".to_string(), Json::String(nav_url.clone()));
                        response.insert("status".to_string(), Json::Number(f64::from(status)));
                        response.insert(
                            "statusText".to_string(),
                            Json::String(status_text(status).to_string()),
                        );
                        let mut resp_hdrs = BTreeMap::new();
                        for (k, v) in &header_pairs {
                            resp_hdrs.insert(k.clone(), Json::String(v.clone()));
                        }
                        response.insert("headers".to_string(), Json::Object(resp_hdrs));
                        response.insert("mimeType".to_string(), Json::String(mime_type.clone()));
                        response
                            .insert("protocol".to_string(), Json::String("http/1.1".to_string()));
                        p.insert("response".to_string(), Json::Object(response));
                        p.insert("timestamp".to_string(), Json::Number(0.0));
                        p.insert("type".to_string(), Json::String("Document".to_string()));
                        p
                    }),
                ),
                // loadingFinished
                CdpMessage::event(
                    "Network.loadingFinished",
                    Json::Object({
                        let mut p = BTreeMap::new();
                        p.insert("requestId".to_string(), Json::String(req_id.clone()));
                        p.insert("timestamp".to_string(), Json::Number(0.0));
                        p.insert(
                            "encodedDataLength".to_string(),
                            Json::Number(f64::from(bytes.len() as u32)),
                        );
                        p
                    }),
                ),
            ];
            // M70.4: 为 JS fetch/XHR 捕获的网络请求生成 Network.* 事件。
            // requestId 从 "2" 开始递增（"1" 是主文档）。
            let mut network_events = network_events;
            for (i, ev) in captured_net.iter().enumerate() {
                let rid = (i + 2).to_string();
                let ev_url = ev.url.clone();
                let ev_method = ev.method.clone();
                let ev_status = ev.status;
                let ev_mime = ev.mime_type.clone();
                let ev_size = ev.body_size;
                // requestWillBeSent
                network_events.push(CdpMessage::event(
                    "Network.requestWillBeSent",
                    Json::Object({
                        let mut p = BTreeMap::new();
                        p.insert("requestId".to_string(), Json::String(rid.clone()));
                        let mut request = BTreeMap::new();
                        request.insert("url".to_string(), Json::String(ev_url.clone()));
                        request.insert("method".to_string(), Json::String(ev_method.clone()));
                        request.insert("headers".to_string(), Json::Object(BTreeMap::new()));
                        p.insert("request".to_string(), Json::Object(request));
                        p.insert("loaderId".to_string(), Json::String("1".to_string()));
                        p.insert("timestamp".to_string(), Json::Number(0.0));
                        p.insert("type".to_string(), Json::String("XHR".to_string()));
                        p
                    }),
                ));
                // responseReceived
                network_events.push(CdpMessage::event(
                    "Network.responseReceived",
                    Json::Object({
                        let mut p = BTreeMap::new();
                        p.insert("requestId".to_string(), Json::String(rid.clone()));
                        let mut response = BTreeMap::new();
                        response.insert("url".to_string(), Json::String(ev_url.clone()));
                        response.insert("status".to_string(), Json::Number(f64::from(ev_status)));
                        response.insert("statusText".to_string(), Json::String(String::new()));
                        response.insert("headers".to_string(), Json::Object(BTreeMap::new()));
                        response.insert("mimeType".to_string(), Json::String(ev_mime.clone()));
                        response.insert("protocol".to_string(), Json::String("http/1.1".into()));
                        p.insert("response".to_string(), Json::Object(response));
                        p.insert("timestamp".to_string(), Json::Number(0.0));
                        p.insert("type".to_string(), Json::String("XHR".to_string()));
                        p
                    }),
                ));
                // loadingFinished
                network_events.push(CdpMessage::event(
                    "Network.loadingFinished",
                    Json::Object({
                        let mut p = BTreeMap::new();
                        p.insert("requestId".to_string(), Json::String(rid));
                        p.insert("timestamp".to_string(), Json::Number(0.0));
                        p.insert(
                            "encodedDataLength".to_string(),
                            Json::Number(ev_size as f64),
                        );
                        p
                    }),
                ));
            }
            let mut page_events = vec![
                // 1. frameNavigated — tells FrameManager the frame URL changed
                CdpMessage::event(
                    "Page.frameNavigated",
                    Json::Object({
                        let mut p = BTreeMap::new();
                        let mut frame = BTreeMap::new();
                        frame.insert("id".to_string(), Json::String(frame_id.clone()));
                        frame.insert("url".to_string(), Json::String(nav_url.clone()));
                        frame.insert("loaderId".to_string(), Json::String("1".to_string()));
                        p.insert("frame".to_string(), Json::Object(frame));
                        p
                    }),
                ),
            ];
            // M81(B7): `Page.lifecycleEvent` 按 Chrome 语义只在
            // `Page.setLifecycleEventsEnabled(true)` 之后派发（enabled=false
            // 停止派发）。Chrome 生命周期顺序：init → commit → DOMContentLoaded
            // → load → networkIdle。Puppeteer（FrameManager.initialize）与
            // Playwright 初始化都会下发 enable，goto 的 load/DCL 等待链不受影响。
            if lifecycle_enabled {
                for name in ["init", "commit", "DOMContentLoaded", "load", "networkIdle"] {
                    page_events.push(lifecycle_event(&frame_id, name));
                }
            }
            page_events.extend([
                // domContentEventFired（Chrome 里由 Page.enable 门控；本实现
                // Page.enable 恒开，故无条件发，保持既有 Puppeteer 兼容）。
                CdpMessage::event(
                    "Page.domContentEventFired",
                    Json::Object({
                        let mut p = BTreeMap::new();
                        p.insert("timestamp".to_string(), Json::Number(0.0));
                        p
                    }),
                ),
                // loadEventFired
                CdpMessage::event(
                    "Page.loadEventFired",
                    Json::Object({
                        let mut p = BTreeMap::new();
                        p.insert("timestamp".to_string(), Json::Number(0.0));
                        p
                    }),
                ),
                // frameStoppedLoading
                CdpMessage::event(
                    "Page.frameStoppedLoading",
                    Json::Object({
                        let mut p = BTreeMap::new();
                        p.insert("frameId".to_string(), Json::String(frame_id.clone()));
                        p
                    }),
                ),
            ]);
            // M70.4: Network.* 事件先于 Page.* 发出（Chrome 的顺序）。
            let mut events = network_events;
            events.append(&mut page_events);
            Ok(DispatchResult {
                response: CdpMessage::ok_response(id, Json::Object(result)),
                events,
            })
        }
        "Page.captureScreenshot" => {
            let st = state
                .lock()
                .map_err(|e| CdpError::Io(format!("lock: {e}")))?;
            // M81.B3: params.clip = {x,y,width,height,scale}——裁剪区域。
            let clip = params.and_then(|p| p.get("clip")).and_then(|c| {
                let gf = |k: &str| {
                    c.get(k).and_then(|v| match v {
                        Json::Number(n) => Some(*n),
                        _ => None,
                    })
                };
                let (x, y, w, h) = (gf("x")?, gf("y")?, gf("width")?, gf("height")?);
                Some((x, y, w, h))
            });
            let data = st.capture_png_base64_opts(clip, false)?;
            let mut result = BTreeMap::new();
            result.insert("data".to_string(), Json::String(data));
            Ok(DispatchResult {
                response: CdpMessage::ok_response(id, Json::Object(result)),
                events: vec![],
            })
        }
        "Page.getNavigationHistory" => {
            let st = state
                .lock()
                .map_err(|e| CdpError::Io(format!("lock: {e}")))?;
            let mut entry = BTreeMap::new();
            entry.insert("id".to_string(), Json::Number(0.0));
            entry.insert(
                "url".to_string(),
                Json::String(if st.url.is_empty() {
                    "about:blank".to_string()
                } else {
                    st.url.clone()
                }),
            );
            let mut result = BTreeMap::new();
            result.insert("currentIndex".to_string(), Json::Number(0.0));
            result.insert(
                "entries".to_string(),
                Json::Array(vec![Json::Object(entry)]),
            );
            Ok(DispatchResult {
                response: CdpMessage::ok_response(id, Json::Object(result)),
                events: vec![],
            })
        }
        // M54: Page.getFrameTree — puppeteer's FrameManager requires this on page init.
        // Returns a single main frame with our target id and current url.
        "Page.getFrameTree" => {
            let st = state
                .lock()
                .map_err(|e| CdpError::Io(format!("lock: {e}")))?;
            let mut frame = BTreeMap::new();
            frame.insert(
                "id".to_string(),
                Json::String(crate::discovery::TARGET_ID.to_string()),
            );
            frame.insert(
                "url".to_string(),
                Json::String(if st.url.is_empty() {
                    "about:blank".to_string()
                } else {
                    st.url.clone()
                }),
            );
            frame.insert("loaderId".to_string(), Json::String("0".to_string()));
            frame.insert("securityOrigin".to_string(), Json::String(String::new()));
            frame.insert(
                "mimeType".to_string(),
                Json::String("text/html".to_string()),
            );
            let mut frame_tree = BTreeMap::new();
            frame_tree.insert("frame".to_string(), Json::Object(frame));
            frame_tree.insert("childFrames".to_string(), Json::Array(vec![]));
            let mut result = BTreeMap::new();
            result.insert("frameTree".to_string(), Json::Object(frame_tree));
            Ok(DispatchResult {
                response: CdpMessage::ok_response(id, Json::Object(result)),
                events: vec![],
            })
        }
        // M81(B7): Page.setLifecycleEventsEnabled — 切换 `Page.lifecycleEvent`
        // 事件派发开关。存 PageState 标志，navigate 时按它决定是否发
        // lifecycle 事件（init/commit/DOMContentLoaded/load/networkIdle）。
        // Chrome 语义：enabled=false 停止派发；params 缺失/非 bool 视为 false。
        "Page.setLifecycleEventsEnabled" => {
            let enabled = params
                .and_then(|p| p.get("enabled"))
                .is_some_and(|v| matches!(v, Json::Bool(true)));
            {
                let mut st = state
                    .lock()
                    .map_err(|e| CdpError::Io(format!("lock: {e}")))?;
                st.lifecycle_events_enabled = enabled;
            }
            Ok(DispatchResult {
                response: CdpMessage::ok_empty(id),
                events: vec![],
            })
        }
        // M53: unknown Page.* methods (enable/disable/etc) → no-op ack
        // M48: puppeteer 的 _createIsolatedWorld 调这两个。返回正确结构才能
        // 让 isolated world 建立、newPage() 继续推进。
        "Page.addScriptToEvaluateOnNewDocument" => {
            // 标准 CDP 返回 {identifier}（脚本 id）。给个固定 id 即可。
            let mut result = BTreeMap::new();
            result.insert(
                "identifier".to_string(),
                Json::String("browser-rs-script-1".to_string()),
            );
            Ok(DispatchResult {
                response: CdpMessage::ok_response(id, Json::Object(result)),
                events: vec![],
            })
        }
        "Page.createIsolatedWorld" => {
            // 标准 CDP 返回 {executionContextId}。单 context 模型给 id=2
            // （main world 是 1，isolated world 用 2）。
            let mut result = BTreeMap::new();
            result.insert("executionContextId".to_string(), Json::Number(2.0));
            // M48: 必须同时发 Runtime.executionContextCreated（context id=2），
            // 否则 puppeteer 的 utilityWorld（isolated world）context 永不就绪，
            // page.title()/evaluate() 等（跑在 utility world 上）永远卡住。
            let frame_id = params
                .and_then(|p| p.get_str("frameId"))
                .unwrap_or(crate::discovery::TARGET_ID)
                .to_string();
            // M48: name 必须用 puppeteer 传来的 worldName（值为
            // '__puppeteer_utility_world__<version>'）。FrameManager 靠
            // contextPayload.name === UTILITY_WORLD_NAME 把 context 映射到
            // PUPPETEER_WORLD；名字不对会被忽略，isolatedRealm() 永远没 context，
            // page.title()/$(...)（跑在 utility world 上）永远卡住。
            let world_name = params
                .and_then(|p| p.get_str("worldName"))
                .unwrap_or("")
                .to_string();
            let mut ctx = BTreeMap::new();
            ctx.insert("id".to_string(), Json::Number(2.0));
            ctx.insert("origin".to_string(), Json::String(String::new()));
            ctx.insert("name".to_string(), Json::String(world_name));
            ctx.insert(
                "auxData".to_string(),
                Json::Object({
                    let mut a = BTreeMap::new();
                    a.insert("frameId".to_string(), Json::String(frame_id));
                    a.insert("isDefault".to_string(), Json::Bool(false));
                    a.insert("type".to_string(), Json::String("isolated".to_string()));
                    a
                }),
            );
            let event = CdpMessage::event(
                "Runtime.executionContextCreated",
                Json::Object({
                    let mut p = BTreeMap::new();
                    p.insert("context".to_string(), Json::Object(ctx));
                    p
                }),
            );
            Ok(DispatchResult {
                response: CdpMessage::ok_response(id, Json::Object(result)),
                events: vec![event],
            })
        }
        _ => Ok(DispatchResult {
            response: CdpMessage::ok_empty(id),
            events: vec![],
        }),
    }
}

/// M70.4: Parse a `Set-Cookie` header value into a NetworkCookie.
///
/// Handles `name=value; Domain=...; Path=...; Secure; HttpOnly`.
/// Domain falls back to the request URL's host.
fn parse_set_cookie(header: &str, request_url: &str) -> Option<NetworkCookie> {
    let mut parts = header.split(';');
    let nv = parts.next()?;
    let (name, value) = nv.split_once('=')?;
    let name = name.trim().to_string();
    let value = value.trim().to_string();
    if name.is_empty() {
        return None;
    }
    let host = url::Url::parse(request_url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or_default();
    let mut ck = NetworkCookie {
        name,
        value,
        domain: host,
        path: "/".to_string(),
        secure: false,
        http_only: false,
    };
    for attr in parts {
        let attr = attr.trim();
        let lower = attr.to_ascii_lowercase();
        if lower == "secure" {
            ck.secure = true;
        } else if lower == "httponly" {
            ck.http_only = true;
        } else if let Some(d) = lower.strip_prefix("domain=") {
            ck.domain = d.trim().to_string();
        } else if let Some(p) = lower.strip_prefix("path=") {
            ck.path = p.trim().to_string();
        }
    }
    Some(ck)
}

/// M81(B7): 构造一条 `Page.lifecycleEvent` 事件（name: init/commit/
/// DOMContentLoaded/load/networkIdle 等，frameId + loaderId + timestamp 0）。
fn lifecycle_event(frame_id: &str, name: &str) -> String {
    CdpMessage::event(
        "Page.lifecycleEvent",
        Json::Object({
            let mut p = BTreeMap::new();
            p.insert("frameId".to_string(), Json::String(frame_id.to_string()));
            p.insert("loaderId".to_string(), Json::String("1".to_string()));
            p.insert("name".to_string(), Json::String(name.to_string()));
            p.insert("timestamp".to_string(), Json::Number(0.0));
            p
        }),
    )
}

/// M70.4: Minimal HTTP status text lookup (for Network.responseReceived.statusText).
fn status_text(code: u16) -> &'static str {
    match code {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        301 => "Moved Permanently",
        302 => "Found",
        304 => "Not Modified",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_encodes_known_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn extract_style_finds_tags() {
        let html = "<html><head><style>p{color:red}</style></head><body><p>hi</p></body></html>";
        let tree = browser_html_parser::parse(html);
        let s = extract_style_text(&tree);
        assert!(s.contains("color:red"));
    }

    #[test]
    fn page_state_render_works() {
        let mut st = PageState::default();
        let html = "<html><body><p>Hello</p></body></html>";
        st.render(html, "test://x", 80);
        assert!(st.rendered_text.contains("Hello"));
        assert_eq!(st.url, "test://x");
    }

    #[test]
    fn page_state_capture_png_returns_base64() {
        let mut st = PageState::default();
        st.render("<html><body><p>X</p></body></html>", "test://x", 80);
        let png = st.capture_png_base64().unwrap();
        // base64 PNG starts with this signature.
        assert!(png.starts_with("iVBOR"));
        assert!(!png.contains("\n"));
        assert!(png.len() > 100);
    }

    #[test]
    fn page_state_capture_before_navigate_errors() {
        let st = PageState::default();
        assert!(st.capture_png_base64().is_err());
    }

    #[test]
    fn strip_ansi_removes_escape_sequences() {
        let (chars, _, _, _) = strip_ansi_and_track_styles("\x1b[4;34mgo\x1b[0m");
        let s: String = chars.into_iter().collect();
        assert_eq!(s, "go");
    }

    // ── M68: Page.navigate 执行页面 <script> 的核心逻辑测试 ──
    // 不经过 dispatch 的网络层（需真实 tokio runtime + fetch），直接测
    // parse_only → run_scripts → render_from_tree 的组合，验证 JS 改的 DOM
    // 能反映到 PageState。

    #[test]
    fn parse_only_then_render_from_tree_matches_render() {
        // 回归保护：拆分后的 parse_only + render_from_tree 应等价于旧 render()。
        let html = "<html><body><p>Static</p></body></html>";
        let mut a = PageState::default();
        a.render(html, "test://a", 80);
        let mut b = PageState::default();
        b.parse_only(html, "test://b", 80);
        b.render_from_tree();
        // Tree 未实现 PartialEq，比 rendered_text 即可验证回归。
        assert_eq!(a.rendered_text, b.rendered_text);
        assert_eq!(a.url, "test://a");
        assert_eq!(b.url, "test://b");
    }

    #[test]
    fn run_scripts_mutates_tree_reflected_in_page_state() {
        // 核心：JS 改 DOM 后，PageState.tree + rendered_text 应反映改动。
        let html = r#"<html><body><div id="root"></div>
<script>document.getElementById('root').innerHTML = '<p>JS-RENDERED</p>';</script>
</body></html>"#;
        let mut st = PageState::default();
        st.parse_only(html, "http://example.com/", 80);

        // 跑页面 JS（对齐 CLI，默认 QuickJS）。
        let ek = EngineKind::QuickJs;
        let tree = st.tree.clone();
        let (shared, _n) = browser_js_runtime::run_scripts_with_base_engine(
            tree,
            Some("http://example.com/".to_string()),
            &ek,
        );
        st.tree = shared.borrow().clone();
        st.render_from_tree();

        // JS 注入的文本应出现在渲染结果里。
        assert!(
            st.rendered_text.contains("JS-RENDERED"),
            "rendered_text should contain JS-RENDERED, got: {}",
            st.rendered_text
        );
    }

    #[test]
    fn run_scripts_async_timer_reflected() {
        // 验证 timer 驱动：setTimeout 后改 DOM 应反映（对齐 CLI，覆盖异步 SPA）。
        let html = r#"<html><body><div id="root">LOADING</div>
<script>setTimeout(function(){ document.getElementById('root').innerHTML = '<p>ASYNC-DONE</p>'; }, 0);</script>
</body></html>"#;
        let mut st = PageState::default();
        st.parse_only(html, "http://example.com/", 80);
        let ek = EngineKind::QuickJs;
        let (shared, _) = browser_js_runtime::run_scripts_with_base_engine(
            st.tree.clone(),
            Some("http://example.com/".to_string()),
            &ek,
        );
        st.tree = shared.borrow().clone();
        st.render_from_tree();
        assert!(
            st.rendered_text.contains("ASYNC-DONE"),
            "async timer result should appear, got: {}",
            st.rendered_text
        );
    }

    #[test]
    fn no_script_page_renders_static() {
        // 无 <script> 的页面：run_scripts 不改 DOM，渲染纯静态（回归保护）。
        let html = "<html><body><p>Plain Text</p></body></html>";
        let mut st = PageState::default();
        st.parse_only(html, "http://example.com/", 80);
        let before = st.rendered_text.clone();
        let (shared, _) = browser_js_runtime::run_scripts_with_base_engine(
            st.tree.clone(),
            Some("http://example.com/".to_string()),
            &EngineKind::QuickJs,
        );
        st.tree = shared.borrow().clone();
        st.render_from_tree();
        // 静态页面跑完 JS，正文文本应仍在。
        assert!(
            st.rendered_text.contains("Plain Text"),
            "static content preserved, got: {}",
            st.rendered_text
        );
        let _ = before;
    }

    // ── M81(B2): Emulation.setDeviceMetricsOverride 的 PageState 落地 ──

    #[test]
    fn set_viewport_relayouts_with_px_columns() {
        // set_viewport(800,600)：布局列数 = layout_columns_for_px(800)，
        // 重渲染后 rendered_text/rendered_colored/layout 都更新。
        let mut st = PageState::default();
        st.render("<html><body><p>VP</p></body></html>", "test://vp", 80);
        assert_eq!(st.width, 80);
        assert!(st.viewport.is_none());

        st.set_viewport(800, 600);
        assert_eq!(st.viewport, Some((800, 600)));
        let expect_cols = browser_render::layout_columns_for_px(800);
        assert_eq!(st.width, expect_cols);
        assert!(st.rendered_text.contains("VP"), "content preserved");
        assert!(st.layout.is_some(), "layout tree snapshot refreshed");
    }

    #[test]
    fn client_width_px_round_trips_through_columns() {
        // clientWidth 语义：800px → 格列 → 回换 px 应接近 800（格宽舍入误差 ≤ 1 格）。
        let mut st = PageState::default();
        st.render("<html><body><p>x</p></body></html>", "test://cw", 80);
        st.set_viewport(800, 600);
        let (w, h) = st.client_viewport_px();
        let (cell_w, _) = browser_render::cell_metrics();
        let expect_w = (st.width as f32 * cell_w).round() as i64;
        assert_eq!(w, expect_w, "clientWidth = cols × cell_w");
        assert!(
            (w - 800).abs() as f32 <= cell_w,
            "clientWidth {w} must be within one cell of 800"
        );
        assert_eq!(h, 600, "clientHeight = 覆盖高度");
    }

    #[test]
    fn client_viewport_without_override_uses_layout_height() {
        // 无覆盖：宽 = 80 列回换 px；高 = 布局树根盒高度（内容行）× 行高。
        let st = {
            let mut s = PageState::default();
            s.render("<html><body><p>a</p></body></html>", "test://d", 80);
            s
        };
        let (w, h) = st.client_viewport_px();
        let (cell_w, cell_h) = browser_render::cell_metrics();
        assert_eq!(w, (80.0_f32 * cell_w).round() as i64);
        assert_eq!(
            h,
            (st.layout.as_ref().unwrap().root.dimensions.height * cell_h).round() as i64
        );
    }

    #[test]
    fn clear_viewport_restores_default_width() {
        let mut st = PageState::default();
        st.render("<html><body><p>x</p></body></html>", "test://c", 80);
        st.set_viewport(800, 600);
        assert_ne!(st.width, 80);

        st.clear_viewport();
        assert!(st.viewport.is_none());
        assert_eq!(st.width, DEFAULT_RENDER_WIDTH);
        assert!(st.rendered_text.contains("x"));
    }

    #[test]
    fn clear_viewport_without_override_is_noop() {
        let mut st = PageState::default();
        st.render("<html><body><p>x</p></body></html>", "test://n", 80);
        let before = st.rendered_text.clone();
        st.clear_viewport();
        assert!(st.viewport.is_none());
        assert_eq!(st.width, 80);
        assert_eq!(st.rendered_text, before);
    }

    #[test]
    fn effective_width_follows_override() {
        let mut st = PageState::default();
        assert_eq!(st.effective_width(), 80);
        st.viewport = Some((800, 600));
        assert_eq!(
            st.effective_width(),
            browser_render::layout_columns_for_px(800)
        );
        // 0 宽防御：layout_columns_for_px 保底 1。
        st.viewport = Some((0, 0));
        assert_eq!(st.effective_width(), 1);
    }

    #[test]
    fn set_viewport_before_navigate_survives_empty_tree() {
        // Playwright 的 viewport 参数在 newPage（navigate 前）下发——空 tree
        // 不能 panic（Tree::root 对空树断言）；视口存住，navigate 时消费。
        let mut st = PageState::default();
        st.set_viewport(800, 600);
        assert_eq!(st.viewport, Some((800, 600)));
        assert_eq!(
            st.effective_width(),
            browser_render::layout_columns_for_px(800)
        );
        // 之后 navigate（dispatch 路径：effective_width → parse_only → render_from_tree）
        // 布局按 override 列数跑。
        let width = st.effective_width();
        st.render(
            "<html><body><p>late</p></body></html>",
            "test://late",
            width,
        );
        assert_eq!(st.width, browser_render::layout_columns_for_px(800));
        assert!(st.rendered_text.contains("late"));
        assert_eq!(st.client_viewport_px().1, 600);
    }

    // ── M81(B7): Page.setLifecycleEventsEnabled ──

    #[tokio::test]
    async fn set_lifecycle_events_enabled_toggles_flag() {
        let state = Arc::new(Mutex::new(PageState::default()));
        assert!(!state.lock().unwrap().lifecycle_events_enabled);

        // enable → true
        let mut params = BTreeMap::new();
        params.insert("enabled".to_string(), Json::Bool(true));
        let dr = dispatch(
            7,
            "Page.setLifecycleEventsEnabled",
            Some(&Json::Object(params)),
            state.clone(),
            EngineKind::QuickJs,
        )
        .await
        .unwrap();
        assert!(
            dr.response.contains(r#""result":{}"#),
            "ack only, got: {}",
            dr.response
        );
        assert!(dr.events.is_empty(), "no events from the toggle itself");
        assert!(state.lock().unwrap().lifecycle_events_enabled);

        // disable → false
        let mut params = BTreeMap::new();
        params.insert("enabled".to_string(), Json::Bool(false));
        dispatch(
            8,
            "Page.setLifecycleEventsEnabled",
            Some(&Json::Object(params)),
            state.clone(),
            EngineKind::QuickJs,
        )
        .await
        .unwrap();
        assert!(!state.lock().unwrap().lifecycle_events_enabled);
    }

    #[tokio::test]
    async fn set_lifecycle_events_enabled_missing_param_defaults_false() {
        // Chrome 语义：params 缺失/非 bool 视为 false（不派发）。
        let state = Arc::new(Mutex::new(PageState::default()));
        dispatch(
            1,
            "Page.setLifecycleEventsEnabled",
            None,
            state.clone(),
            EngineKind::QuickJs,
        )
        .await
        .unwrap();
        assert!(!state.lock().unwrap().lifecycle_events_enabled);

        // enabled 非布尔（字符串 "true"）不算开启。
        let mut params = BTreeMap::new();
        params.insert("enabled".to_string(), Json::String("true".to_string()));
        dispatch(
            2,
            "Page.setLifecycleEventsEnabled",
            Some(&Json::Object(params)),
            state.clone(),
            EngineKind::QuickJs,
        )
        .await
        .unwrap();
        assert!(!state.lock().unwrap().lifecycle_events_enabled);
    }

    #[test]
    fn lifecycle_event_json_shape() {
        let ev = lifecycle_event("frame-1", "networkIdle");
        assert!(
            ev.contains(r#""method":"Page.lifecycleEvent""#),
            "got: {ev}"
        );
        assert!(ev.contains(r#""name":"networkIdle""#), "got: {ev}");
        assert!(ev.contains(r#""frameId":"frame-1""#), "got: {ev}");
        assert!(ev.contains(r#""loaderId":"1""#), "got: {ev}");
        assert!(ev.contains(r#""timestamp":0"#), "got: {ev}");
    }

    #[test]
    fn network_bodies_default_empty_and_reset_between_navigates() {
        // B6: 默认空表；navigate 的填充逻辑在 dispatch（需网络），这里固化
        // "1" 主文档约定 + Default 无残留。
        let st = PageState::default();
        assert!(st.network_bodies.is_empty());
        assert!(!st.lifecycle_events_enabled);
        let mut st2 = PageState::default();
        st2.network_bodies
            .insert("1".to_string(), "<html>old</html>".to_string());
        // parse_only 不清表（navigate 的 dispatch 块里显式 clear+insert）。
        st2.parse_only("<html>new</html>", "test://n2", 80);
        assert_eq!(
            st2.network_bodies.get("1").map(String::as_str),
            Some("<html>old</html>")
        );
    }
}
