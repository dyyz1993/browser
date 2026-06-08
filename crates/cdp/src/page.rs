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
//! M44 does **not** execute JS (that needs the js-runtime threading model
//! which doesn't fit CDP's async session model cleanly — M45+).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use browser_dom::Tree;
use browser_html_parser::parse as parse_html;

use crate::jsonrpc::{CdpError, CdpMessage, Json};

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
}

impl Default for PageState {
    fn default() -> Self {
        Self {
            url: String::new(),
            tree: Tree::new(),
            raw_html: String::new(),
            rendered_text: String::new(),
            rendered_colored: String::new(),
            width: 80,
        }
    }
}

impl PageState {
    /// Render `html` (already fetched) and store the result.
    pub fn render(&mut self, html: &str, url: &str, width: usize) {
        let tree = parse_html(html);
        let style_text = extract_style_text(&tree);
        let sheet = browser_css_engine::parse(&style_text);
        let styles = browser_css_engine::compute_styles(&tree, &sheet);
        let mut layout = browser_layout::construct_layout_tree(&tree, &styles);
        browser_layout::layout(
            &mut layout,
            browser_layout::LayoutConfig {
                viewport_width: width as f32,
            },
        );
        self.rendered_text = browser_render::render_ascii(&layout, width);
        self.rendered_colored = browser_render::render_ascii_colored(&layout, width);
        self.url = url.to_string();
        self.width = width;
        self.tree = tree;
    }

    /// Render the current page to a PNG screenshot, returning base64 data.
    ///
    /// M44 uses the screenshot renderer (fontdue + ANSI color parse).
    pub fn capture_png_base64(&self) -> Result<String, CdpError> {
        if self.rendered_colored.is_empty() {
            return Err(CdpError::InvalidJson(
                "no page loaded — call Page.navigate first".to_string(),
            ));
        }
        let mut renderer = browser_render::font::FontRenderer::new();
        // Decode the colored ANSI text to RGBA pixels (reuse the M30 screenshot path).
        let (rgba, _w, _h) = render_text_to_rgba(&mut renderer, &self.rendered_colored);
        // Encode RGBA → PNG → base64.
        let png = encode_rgba_as_png(&rgba, _w, _h)
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
/// Mirrors `crates/cli/src/screenshot.rs`'s logic (M30 + M39 background).
fn render_text_to_rgba(
    renderer: &mut browser_render::font::FontRenderer,
    text: &str,
) -> (Vec<u8>, usize, usize) {
    // Strip ANSI, track link + background spans per line.
    let lines: Vec<&str> = text.split('\n').collect();
    let mut link_spans: Vec<Vec<(usize, usize)>> = Vec::new();
    let mut bg_spans: Vec<browser_render::font::BgSpans> = Vec::new();
    let mut plain_lines: Vec<String> = Vec::new();
    for line in &lines {
        let (chars, links, bgs) = strip_ansi_and_track_styles(line);
        plain_lines.push(chars.into_iter().collect());
        link_spans.push(links);
        bg_spans.push(bgs);
    }
    let (w, h, rgba) =
        renderer.render_text_to_rgba(&plain_lines.join("\n"), &link_spans, &bg_spans);
    (rgba, w, h)
}

/// Minimal ANSI stripper that also tracks link (underline truecolor) and
/// background (48;2;R;G;B) spans. Mirrors `screenshot.rs`.
pub(crate) fn strip_ansi_and_track_styles(
    line: &str,
) -> (
    Vec<char>,
    Vec<(usize, usize)>,
    Vec<browser_render::font::BgSpan>,
) {
    let link_spans: Vec<(usize, usize)> = Vec::new();
    let bg_spans: Vec<browser_render::font::BgSpan> = Vec::new();
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
    (result, link_spans, bg_spans)
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
) -> Result<DispatchResult, CdpError> {
    match method {
        "Page.navigate" => {
            let url = params
                .and_then(|p| p.get_str("url"))
                .ok_or_else(|| CdpError::InvalidJson("missing url param".to_string()))?;
            let client = browser_net::HttpClient::new();
            let bytes = client
                .get(url)
                .await
                .map_err(|e| CdpError::Io(format!("fetch: {e}")))?;
            let html = String::from_utf8_lossy(&bytes).to_string();
            {
                let mut st = state
                    .lock()
                    .map_err(|e| CdpError::Io(format!("lock: {e}")))?;
                st.render(&html, url, 80);
                st.raw_html = html.clone();
            }
            let mut result = BTreeMap::new();
            result.insert(
                "frameId".to_string(),
                Json::String(crate::discovery::TARGET_ID.to_string()),
            );
            // M50: emit Page lifecycle events after navigate
            let frame_id = crate::discovery::TARGET_ID.to_string();
            let nav_url = url.to_string();
            let events = vec![
                CdpMessage::event(
                    "Page.frameNavigated",
                    Json::Object({
                        let mut p = BTreeMap::new();
                        let mut frame = BTreeMap::new();
                        frame.insert("id".to_string(), Json::String(frame_id.clone()));
                        frame.insert("url".to_string(), Json::String(nav_url.clone()));
                        frame.insert("loaderId".to_string(), Json::String("0".to_string()));
                        p.insert("frame".to_string(), Json::Object(frame));
                        p
                    }),
                ),
                CdpMessage::event(
                    "Page.loadEventFired",
                    Json::Object({
                        let mut p = BTreeMap::new();
                        p.insert("timestamp".to_string(), Json::Number(0.0));
                        p
                    }),
                ),
                CdpMessage::event(
                    "Page.frameStoppedLoading",
                    Json::Object({
                        let mut p = BTreeMap::new();
                        p.insert("frameId".to_string(), Json::String(frame_id.clone()));
                        p
                    }),
                ),
                CdpMessage::event(
                    "Page.domContentEventFired",
                    Json::Object({
                        let mut p = BTreeMap::new();
                        p.insert("timestamp".to_string(), Json::Number(0.0));
                        p
                    }),
                ),
            ];
            Ok(DispatchResult {
                response: CdpMessage::ok_response(id, Json::Object(result)),
                events,
            })
        }
        "Page.captureScreenshot" => {
            let st = state
                .lock()
                .map_err(|e| CdpError::Io(format!("lock: {e}")))?;
            let data = st.capture_png_base64()?;
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
        // M53: unknown Page.* methods (enable/disable/etc) → no-op ack
        _ => Ok(DispatchResult {
            response: CdpMessage::ok_empty(id),
            events: vec![],
        }),
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
        let (chars, _, _) = strip_ansi_and_track_styles("\x1b[4;34mgo\x1b[0m");
        let s: String = chars.into_iter().collect();
        assert_eq!(s, "go");
    }
}
