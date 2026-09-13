//! M42.3: CDP WebSocket server + session handler.
//!
//! ## Flow
//!
//! ```text
//! CdpServer::listen(port)
//!   └─ TcpListener::accept loop (tokio)
//!        └─ CdpSession::handle(stream)
//!             ├─ read HTTP upgrade request (\r\n\rn-terminated)
//!             ├─ parse Sec-WebSocket-Key
//!             ├─ send 101 Switching Protocols (Sec-WebSocket-Accept)
//!             └─ message loop:
//!                  ├─ read frame (masked, client→server)
//!                  ├─ parse CDP JSON-RPC
//!                  └─ dispatch (M44+: domain handlers; M42: echo error)
//! ```
//!
//! ## RFC 6455 server obligations
//!
//! - Client→server frames **must** be masked; server→client frames **must not**
//!   be masked (RFC 6455 §5.3).
//! - Server must validate the mask bit and unmask the payload.

use std::sync::{Arc, Mutex};

use browser_js_runtime::EngineKind;
use browser_ws::handshake::{build_server_response, parse_key_from_request};
use browser_ws::{decode_frame, encode_frame, Frame, OpCode};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::jsonrpc::{parse_message, CdpMessage, Json};
use std::collections::BTreeMap;

#[cfg(all(test, feature = "boa"))]
use std::time::Duration;

/// Default CDP port (Chrome standard).
pub const DEFAULT_CDP_PORT: u16 = 9222;

/// M81(A1): browser-level state shared by **all** concurrent CDP connections.
///
/// Playwright's `connect_over_cdp()` opens a browser-level WebSocket and a
/// page-level WebSocket concurrently (and Puppeteer may open follow-up
/// connections too). Every connection is its own `CdpSession` task, but they
/// all share this single-tab state so a `Page.navigate` on one connection is
/// immediately visible to `DOM.*` / `Runtime.*` on the other.
#[derive(Clone)]
pub(crate) struct SharedBrowserState {
    /// Shared page state (single-tab model, M44).
    pub(crate) page: Arc<Mutex<crate::page::PageState>>,
    /// M49: emulation state (device metrics, user agent).
    pub(crate) emulation: Arc<Mutex<crate::emulation_domain::EmulationState>>,
}

impl SharedBrowserState {
    pub(crate) fn new() -> Self {
        Self {
            page: Arc::new(Mutex::new(crate::page::PageState::default())),
            emulation: Arc::new(Mutex::new(crate::emulation_domain::EmulationState::new())),
        }
    }

    /// Test-only: build shared state pre-seeded with a page (no network).
    #[cfg(test)]
    pub(crate) fn with_page(page: crate::page::PageState) -> Self {
        Self {
            page: Arc::new(Mutex::new(page)),
            emulation: Arc::new(Mutex::new(crate::emulation_domain::EmulationState::new())),
        }
    }
}

/// A running CDP server bound to a port.
///
/// Call [`CdpServer::listen`] to accept connections. M81(A1): each accepted
/// connection is handled **concurrently** in its own task (Playwright
/// dual-connection model); connections share [`SharedBrowserState`].
pub struct CdpServer;

impl CdpServer {
    /// Bind to `port` and accept connections in a loop, spawning a concurrent
    /// session task per connection (M81(A1)). Runs until the listener errors.
    ///
    /// # Errors
    /// Returns an error if binding fails or the accept loop hits an
    /// unrecoverable I/O error.
    pub async fn listen(port: u16, engine_kind: EngineKind) -> std::io::Result<()> {
        let listener = TcpListener::bind(("127.0.0.1", port)).await?;
        eprintln!(
            "[cdp] listening on http://127.0.0.1:{port} (engine: {})",
            match engine_kind {
                EngineKind::QuickJs => "quickjs",
                #[cfg(feature = "boa")]
                EngineKind::Boa => "boa",
                #[cfg(feature = "v8")]
                EngineKind::V8 => "v8",
            }
        );
        // M81(A1): one SharedBrowserState for the whole server — every
        // connection (browser-level + page-level) sees the same page.
        let shared = SharedBrowserState::new();
        loop {
            let (stream, addr) = listener.accept().await?;
            eprintln!("[cdp] connection from {addr}");
            let state = shared.clone();
            // Handle connections concurrently instead of serially: the second
            // (page-level) connection must not be blocked by the first.
            tokio::spawn(async move {
                if let Err(e) = CdpSession::handle(stream, engine_kind, state).await {
                    eprintln!("[cdp] session ended: {e}");
                }
            });
        }
    }

    /// Accept and handle exactly one connection, then return. Used by tests.
    #[cfg(test)]
    pub(crate) async fn accept_one(
        listener: TcpListener,
        engine_kind: EngineKind,
        shared: SharedBrowserState,
    ) -> std::io::Result<()> {
        let (stream, _) = listener.accept().await?;
        let _ = CdpSession::handle(stream, engine_kind, shared).await;
        Ok(())
    }
}

/// Parse the request path from an HTTP request line.
/// e.g. `GET /json/version HTTP/1.1\r\n...` → `/json/version`.
fn parse_request_path(request: &str) -> String {
    request
        .split("\r\n")
        .next() // request line
        .and_then(|line| line.split_whitespace().nth(1)) // method PATH version
        .unwrap_or("/")
        .to_string()
}

/// A single CDP client session (one WebSocket connection).
///
/// M42 scope: handshake + frame I/O + dispatch loop. Unknown methods get a
/// `-32601 Method not found` error response.
///
/// M81(A1): multiple sessions run concurrently and share one
/// [`SharedBrowserState`] (single-tab page + emulation), so Playwright's
/// browser-level and page-level connections observe the same page.
pub struct CdpSession {
    stream: TcpStream,
    /// M44: shared page state (single-tab model; M81: shared across sessions).
    page: Arc<Mutex<crate::page::PageState>>,
    /// M49: emulation state (device metrics, user agent; M81: shared).
    emulation: Arc<Mutex<crate::emulation_domain::EmulationState>>,
    /// M67: JS engine backend for `Runtime.evaluate` / `callFunctionOn`.
    /// Default QuickJs (aligns with CLI); Boa fallback via `--js-engine boa`.
    engine_kind: EngineKind,
}

impl CdpSession {
    /// Handle a connection end-to-end: handshake → message loop.
    ///
    /// `shared` is the browser-level state this session operates on (M81(A1):
    /// all concurrent connections share the same instance).
    ///
    /// # Errors
    /// Returns an error string on handshake or I/O failure.
    pub(crate) async fn handle(
        stream: TcpStream,
        engine_kind: EngineKind,
        shared: SharedBrowserState,
    ) -> Result<(), String> {
        let mut session = CdpSession {
            stream,
            page: shared.page,
            emulation: shared.emulation,
            engine_kind,
        };
        session.route().await
    }

    /// Read the initial HTTP request and route it: either HTTP discovery
    /// (GET /json*) or WebSocket upgrade (everything else).
    async fn route(&mut self) -> Result<(), String> {
        let request = self.read_http_headers().await?;
        let path = parse_request_path(&request);
        let is_ws_upgrade = request.to_ascii_lowercase().contains("upgrade: websocket");
        // M43: /json* paths are HTTP discovery (no WS upgrade).
        if path.starts_with("/json") && !is_ws_upgrade {
            self.serve_discovery(&path).await?;
            return Ok(()); // discovery is a one-shot HTTP response
        }
        // Non-JSON plain HTTP (no WS upgrade) → 404 instead of a broken
        // WS handshake that drops the connection.
        if !is_ws_upgrade {
            let resp = crate::discovery::http_404();
            self.stream
                .write_all(resp.as_bytes())
                .await
                .map_err(|e| format!("write 404: {e}"))?;
            return Ok(());
        }
        // Otherwise: treat as WebSocket upgrade.
        self.do_ws_handshake(&request).await?;
        // M51: Target events moved to per-method dispatch (setDiscoverTargets/setAutoAttach)
        self.message_loop().await
    }

    /// Serve an HTTP discovery response then close.
    async fn serve_discovery(&mut self, path: &str) -> Result<(), String> {
        let ws_host = self.ws_host();
        let resp = crate::discovery::handle_discovery(path, &ws_host);
        self.stream
            .write_all(resp.as_bytes())
            .await
            .map_err(|e| format!("write discovery: {e}"))
    }

    /// Get the local address as "host:port" for constructing URLs.
    fn ws_host(&self) -> String {
        self.stream
            .local_addr()
            .map(|a| a.to_string())
            .unwrap_or_else(|_| "127.0.0.1:9222".to_string())
    }

    /// Read until \r\n\r\n (end of HTTP headers). Cap at 8KB to avoid abuse.
    async fn read_http_headers(&mut self) -> Result<String, String> {
        let mut buf = vec![0u8; 8192];
        let mut filled = 0;
        loop {
            if filled >= buf.len() {
                return Err("handshake request too large".to_string());
            }
            let n = self
                .stream
                .read(&mut buf[filled..])
                .await
                .map_err(|e| format!("read handshake: {e}"))?;
            if n == 0 {
                return Err("connection closed during handshake".to_string());
            }
            filled += n;
            let window = &buf[..filled];
            if window.windows(4).any(|w| w == b"\r\n\r\n") {
                break;
            }
        }
        Ok(String::from_utf8_lossy(&buf[..filled]).to_string())
    }

    /// Parse the request path from an HTTP request line (e.g.
    /// `GET /json/version HTTP/1.1`).
    fn _placeholder_do_ws_handshake_unused(&self) {}

    /// Do the WebSocket upgrade handshake using an already-read request.
    async fn do_ws_handshake(&mut self, request: &str) -> Result<(), String> {
        let key = parse_key_from_request(request)
            .ok_or_else(|| "no Sec-WebSocket-Key header".to_string())?;
        let response = build_server_response(key);
        self.stream
            .write_all(response.as_bytes())
            .await
            .map_err(|e| format!("write handshake: {e}"))?;
        Ok(())
    }

    /// Main message loop: read frames, parse CDP messages, dispatch.
    async fn message_loop(&mut self) -> Result<(), String> {
        let mut buf = Vec::with_capacity(4096);
        loop {
            // Read more bytes and try to decode a frame.
            let mut chunk = [0u8; 4096];
            let n = self
                .stream
                .read(&mut chunk)
                .await
                .map_err(|e| format!("read: {e}"))?;
            if n == 0 {
                return Ok(()); // client closed
            }
            buf.extend_from_slice(&chunk[..n]);

            // Drain all complete frames from the buffer.
            loop {
                match decode_frame(&buf) {
                    Ok(Some((frame, consumed))) => {
                        buf.drain(..consumed);
                        if let Err(e) = self.handle_frame(frame).await {
                            eprintln!("[cdp] frame error: {e}");
                        }
                    }
                    Ok(None) => break, // incomplete, need more bytes
                    Err(e) => return Err(format!("bad frame: {e}")),
                }
            }
        }
    }

    /// Process one decoded WebSocket frame.
    async fn handle_frame(&mut self, frame: Frame) -> Result<(), String> {
        match frame.opcode {
            OpCode::Text => {
                let text = String::from_utf8_lossy(&frame.payload);
                self.handle_cdp_message(&text).await?;
            }
            OpCode::Close => {
                // Echo a close frame back and end.
                let close = Frame::close(1000, "");
                let bytes = encode_frame(&close, None);
                let _ = self.stream.write_all(&bytes).await;
                return Err("client closed".to_string());
            }
            OpCode::Ping => {
                let pong = Frame::pong(frame.payload);
                let bytes = encode_frame(&pong, None);
                self.stream
                    .write_all(&bytes)
                    .await
                    .map_err(|e| format!("pong: {e}"))?;
            }
            OpCode::Pong => { /* ignore */ }
            OpCode::Binary => { /* CDP uses text frames only */ }
            OpCode::Continuation => { /* M42: no fragmentation support yet */ }
        }
        Ok(())
    }

    /// Parse a CDP JSON-RPC message and dispatch it.
    ///
    /// M42: unknown methods return `-32601 Method not found`. M44+ adds
    /// real domain handlers (Page/Runtime/DOM/Network).
    async fn handle_cdp_message(&mut self, text: &str) -> Result<(), String> {
        let msg = match parse_message(text) {
            Ok(m) => m,
            Err(e) => {
                // Malformed JSON — send a generic error (no id to echo).
                eprintln!("[cdp] bad json: {e}");
                return Ok(());
            }
        };
        // Only requests (with id + method) get a response. Events from client
        // are ignored (CDP clients rarely send events).
        let (Some(id), Some(method)) = (msg.id, msg.method.as_deref()) else {
            return Ok(());
        };
        let session_id = msg.session_id.clone(); // M53: flatten session routing
                                                 // M44+ routes "Page.*" / "Runtime.*" / etc. to domain handlers.
                                                 // M50: collect response + post-response events.
        let (resp, post_events): (String, Vec<String>) = match method {
            // ── M42 builtin: Discovery/Target probing ──
            // Puppeteer/Playwright probe these on connect; answering them early
            // avoids a flood of -32601 noise in logs.
            "Browser.getVersion" => {
                let mut result = std::collections::BTreeMap::new();
                result.insert(
                    "protocolVersion".to_string(),
                    crate::jsonrpc::Json::String("1.3".to_string()),
                );
                result.insert(
                    "product".to_string(),
                    crate::jsonrpc::Json::String("browser-rs/0.0.1".to_string()),
                );
                result.insert(
                    "userAgent".to_string(),
                    crate::jsonrpc::Json::String(
                        "Mozilla/5.0 (compatible; browser-rs/0.0.1)".to_string(),
                    ),
                );
                result.insert(
                    "jsVersion".to_string(),
                    crate::jsonrpc::Json::String("boa".to_string()),
                );
                (
                    CdpMessage::ok_response(id, crate::jsonrpc::Json::Object(result)),
                    vec![],
                )
            }
            // ── M81(A1): Browser.* fallback — Playwright's default browser
            // context init sends Browser.setDownloadBehavior; a -32601 here
            // rejects `CRBrowserContext._initialize()` and kills the whole
            // connect_over_cdp handshake. Ack the rest no-op.
            m if m.starts_with("Browser.") => (CdpMessage::ok_empty(id), vec![]),
            // ── M44+M50: Page domain (navigate, captureScreenshot + events) ──
            m if m.starts_with("Page.") => {
                match crate::page::dispatch(
                    id,
                    m,
                    msg.params.as_ref(),
                    self.page.clone(),
                    self.engine_kind,
                )
                .await
                {
                    Ok(dr) => (dr.response, dr.events),
                    Err(crate::jsonrpc::CdpError::MethodNotFound(_)) => (
                        CdpMessage::error_response(id, -32601, "Method not found"),
                        vec![],
                    ),
                    Err(e) => (
                        CdpMessage::error_response(id, -32000, &e.to_string()),
                        vec![],
                    ),
                }
            }
            // ── M46: DOM domain (getDocument, getOuterHTML, querySelector) ──
            // Synchronous: lock the page state and dispatch directly.
            m if m.starts_with("DOM.") => {
                let Ok(st) = self.page.lock() else {
                    return Ok(()); // lock poisoned — drop silently
                };
                match crate::dom_domain::dispatch(id, m, msg.params.as_ref(), &st) {
                    Ok(resp) => (resp, vec![]),
                    Err(crate::jsonrpc::CdpError::MethodNotFound(_)) => (
                        CdpMessage::error_response(id, -32601, "Method not found"),
                        vec![],
                    ),
                    Err(e) => (
                        CdpMessage::error_response(id, -32000, &e.to_string()),
                        vec![],
                    ),
                }
            }
            // ── M45: Runtime domain (evaluate JS, enable/disable) ──
            // M48: evaluate/callFunctionOn 现在跑在绑定到当前页面 DOM tree 的
            // shimmed ctx 上，所以这里 lock page 取 tree + url 传进去。dispatch
            // 是同步的，整段在这个 lock 作用域内完成（无 await）。
            m if m.starts_with("Runtime.") => {
                let Ok(mut st) = self.page.lock() else {
                    return Ok(()); // lock poisoned
                };
                let tree = &st.tree;
                let url = st.url.as_str();
                // M81(B2): 布局视口 px（clientWidth/clientHeight 注入用）——
                // setDeviceMetricsOverride 后 evaluate 读到新视口。
                let client_vp = st.client_viewport_px();
                // M48: Runtime.enable 时，标准浏览器会为每个 execution context
                // 发 Runtime.executionContextCreated 事件。puppeteer 的 FrameManager
                // 靠它把 context 绑到 frame（否则 mainWorld/isolatedWorld 永远没就绪，
                // _createIsolatedWorld 等不到 frame 的 context → newPage() 卡死）。
                // 单页单 context 模型：发一个 main-world context（id=1, auxData 指向
                // 主 frame，isDefault=true）。
                // M81(A1): 不再要求 session_id —— Playwright 的 page 级直连
                // （无 sessionId 的连接）同样需要这个事件来绑定 main world。
                if m == "Runtime.enable" {
                    let mut ctx = BTreeMap::new();
                    ctx.insert("id".to_string(), Json::Number(1.0));
                    ctx.insert("origin".to_string(), Json::String(String::new()));
                    ctx.insert("name".to_string(), Json::String(String::new()));
                    ctx.insert(
                        "auxData".to_string(),
                        Json::Object({
                            let mut a = BTreeMap::new();
                            a.insert(
                                "frameId".to_string(),
                                Json::String(crate::discovery::TARGET_ID.to_string()),
                            );
                            a.insert("isDefault".to_string(), Json::Bool(true));
                            a.insert("type".to_string(), Json::String("default".to_string()));
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
                    match crate::runtime_domain::dispatch(
                        id,
                        m,
                        msg.params.as_ref(),
                        tree,
                        url,
                        &self.engine_kind,
                        Some(client_vp),
                    ) {
                        Ok(resp) => {
                            // M81(B1): JS 侧 focus() 上报 → 回写 PageState.focused_node
                            // （dispatchKeyEvent 的 activeElement 同步）。
                            if let Some(nid) = browser_js_runtime::take_focus_node() {
                                st.focused_node = Some(nid);
                            }
                            (resp, vec![event])
                        }
                        Err(crate::jsonrpc::CdpError::MethodNotFound(_)) => (
                            CdpMessage::error_response(id, -32601, "Method not found"),
                            vec![event],
                        ),
                        Err(e) => (
                            CdpMessage::error_response(id, -32000, &e.to_string()),
                            vec![event],
                        ),
                    }
                } else {
                    match crate::runtime_domain::dispatch(
                        id,
                        m,
                        msg.params.as_ref(),
                        tree,
                        url,
                        &self.engine_kind,
                        Some(client_vp),
                    ) {
                        Ok(resp) => {
                            // M81(B1): JS 侧 focus() 上报 → 回写 focused_node。
                            if let Some(nid) = browser_js_runtime::take_focus_node() {
                                st.focused_node = Some(nid);
                            }
                            (resp, vec![])
                        }
                        Err(crate::jsonrpc::CdpError::MethodNotFound(_)) => (
                            CdpMessage::error_response(id, -32601, "Method not found"),
                            vec![],
                        ),
                        Err(e) => (
                            CdpMessage::error_response(id, -32000, &e.to_string()),
                            vec![],
                        ),
                    }
                }
            }
            // ── M47: Network domain (getResponseBody, enable/disable) ──
            // M81(B6): getResponseBody 需要 params.requestId 查响应体表。
            m if m.starts_with("Network.") => {
                let Ok(st) = self.page.lock() else {
                    return Ok(());
                };
                match crate::network_domain::dispatch(id, m, msg.params.as_ref(), &st) {
                    Ok(resp) => (resp, vec![]),
                    Err(crate::jsonrpc::CdpError::MethodNotFound(_)) => (
                        CdpMessage::error_response(id, -32601, "Method not found"),
                        vec![],
                    ),
                    Err(e) => (
                        CdpMessage::error_response(id, -32000, &e.to_string()),
                        vec![],
                    ),
                }
            }
            // ── M49: Emulation domain (device metrics, user agent) ──
            // M81(B2): setDeviceMetricsOverride 的视口要写入 PageState 并重跑
            // 布局——页锁 + 仿真锁同持。锁序固定 page → emulation（其余分支
            // 至多持其一，无反向嵌套，无死锁风险）。
            m if m.starts_with("Emulation.") => {
                let mut page = self
                    .page
                    .lock()
                    .map_err(|_| CdpMessage::error_response(id, -32000, "Lock poisoned"))?;
                let mut st = self
                    .emulation
                    .lock()
                    .map_err(|_| CdpMessage::error_response(id, -32000, "Lock poisoned"))?;
                match crate::emulation_domain::dispatch(
                    id,
                    m,
                    msg.params.as_ref(),
                    &mut st,
                    &mut page,
                ) {
                    Ok(resp) => (resp, vec![]),
                    Err(crate::jsonrpc::CdpError::MethodNotFound(_)) => (
                        CdpMessage::error_response(id, -32601, "Method not found"),
                        vec![],
                    ),
                    Err(e) => (
                        CdpMessage::error_response(id, -32000, &e.to_string()),
                        vec![],
                    ),
                }
            }
            // ── M80.17: Input domain（dispatchMouseEvent 真实点击）──
            // mousePressed(left) → hit_test + 同引擎会话合成点击（async，内部
            // 自行加锁 page——此分支不能预持锁）。其余事件 ack。
            m if m.starts_with("Input.") => {
                match crate::input_domain::dispatch(
                    id,
                    m,
                    msg.params.as_ref(),
                    self.page.clone(),
                    self.engine_kind,
                )
                .await
                {
                    Ok(resp) => (resp, vec![]),
                    Err(crate::jsonrpc::CdpError::MethodNotFound(_)) => (
                        CdpMessage::error_response(id, -32601, "Method not found"),
                        vec![],
                    ),
                    Err(e) => (
                        CdpMessage::error_response(id, -32000, &e.to_string()),
                        vec![],
                    ),
                }
            }
            // ── M48+M51: Target domain (Puppeteer connect flow + events) ──
            m if m.starts_with("Target.") => {
                let ws_host = self.ws_host();
                let resp = match crate::target_domain::dispatch(id, m, &ws_host) {
                    Ok(resp) => resp,
                    Err(crate::jsonrpc::CdpError::MethodNotFound(_)) => {
                        CdpMessage::error_response(id, -32601, "Method not found")
                    }
                    Err(e) => CdpMessage::error_response(id, -32000, &e.to_string()),
                };
                // M53: flatten session mode — emit events at the right time
                let events: Vec<String> = match m {
                    // setDiscoverTargets → emit targetCreated so puppeteer discovers our page target
                    "Target.setDiscoverTargets" => {
                        vec![CdpMessage::event(
                            "Target.targetCreated",
                            Json::Object({
                                let mut p = BTreeMap::new();
                                p.insert(
                                    "targetInfo".to_string(),
                                    crate::discovery::target_object(&ws_host),
                                );
                                p
                            }),
                        )]
                    }
                    // M53: setAutoAttach → emit attachedToTarget so puppeteer creates a session.
                    // Only emit for connection-level (no sessionId), not for session-level
                    // (which would cause infinite recursion).
                    // waitingForDebugger must be true because puppeteer sends waitForDebuggerOnStart: true.
                    // After this, puppeteer sends Runtime.runIfWaitingForDebugger to continue.
                    "Target.setAutoAttach" if session_id.is_none() => {
                        vec![CdpMessage::event(
                            "Target.attachedToTarget",
                            Json::Object({
                                let mut p = BTreeMap::new();
                                p.insert(
                                    "sessionId".to_string(),
                                    Json::String("browser-rs-session-0".to_string()),
                                );
                                p.insert(
                                    "targetInfo".to_string(),
                                    crate::discovery::target_object(&ws_host),
                                );
                                p.insert("waitingForDebugger".to_string(), Json::Bool(true));
                                p
                            }),
                        )]
                    }
                    "Target.setAutoAttach" => vec![], // session-level: no-op
                    // M48: createTarget = puppeteer 的 browser.newPage()。puppeteer 在
                    // createTarget 返回 {targetId} 后 waitForTarget(t => t._targetId===targetId)，
                    // 等一个 Target.targetCreated 事件命中。
                    // 注意：**不发** attachedToTarget——连接级 setAutoAttach(id=3) 已经发过
                    // attachedToTarget 并建了 session(browser-rs-session-0)。这里再发会让
                    // puppeteer 重复建同名 session、覆盖 map，导致 enable 命令的 callback
                    // 漂到被丢弃的旧 session 上 → newPage() 卡死。只发 targetCreated。
                    "Target.createTarget" => {
                        let ti = crate::discovery::target_object(&ws_host);
                        vec![CdpMessage::event(
                            "Target.targetCreated",
                            Json::Object({
                                let mut p = BTreeMap::new();
                                p.insert("targetInfo".to_string(), ti);
                                p
                            }),
                        )]
                    }
                    "Target.attachToTarget" | "Target.attachToBrowserTarget" => {
                        vec![CdpMessage::event(
                            "Target.attachedToTarget",
                            Json::Object({
                                let mut p = BTreeMap::new();
                                p.insert(
                                    "sessionId".to_string(),
                                    Json::String("browser-rs-session-0".to_string()),
                                );
                                p.insert(
                                    "targetInfo".to_string(),
                                    crate::discovery::target_object(&ws_host),
                                );
                                p.insert("waitingForDebugger".to_string(), Json::Bool(false));
                                p
                            }),
                        )]
                    }
                    _ => vec![],
                };
                (resp, events)
            }
            // M53: puppeteer sends many "enable/disable" style methods during
            // page initialization. For known CDP domains, return no-op ack instead
            // of error. Only truly unknown domains get -32601.
            m if m.starts_with("Fetch.")
                || m.starts_with("Emulation.")
                || m.starts_with("CSS.")
                || m.starts_with("DOMSnapshot.")
                || m.starts_with("Log.")
                || m.starts_with("Security.")
                || m.starts_with("Performance.")
                || m.starts_with("Inspector.")
                || m.starts_with("Accessibility.")
                || m.starts_with("Audits.")
                || m.starts_with("WebMCP.")
                || m.starts_with("BackgroundService.")
                || m.starts_with("Media.")
                || m.starts_with("ServiceWorker.")
                || m.starts_with("HeapProfiler.")
                || m.starts_with("Profiler.")
                || m.starts_with("DeviceOrientation.")
                || m.starts_with("Storage.")
                || m.starts_with("SystemInfo.")
                || m.starts_with("Autofill.")
                || m.starts_with("WebAuthn.")
                || m.starts_with("Permissions.")
                || m.starts_with("Cast.")
                || m.starts_with("Tethering.")
                || m.starts_with("Tracing.")
                || m.starts_with("FileSystem.") =>
            {
                // M48: puppeteer 初始化时会发一大批 *.enable，任何 -32601 都可能
                // 让它的批量 await 卡住。对未知/未实现的域统一 no-op ack。
                (CdpMessage::ok_empty(id), vec![])
            }
            _ => (
                CdpMessage::error_response(id, -32601, "Method not found"),
                vec![],
            ),
        };
        // M53: if request had sessionId, inject it into response and events
        let resp = if let Some(ref sid) = session_id {
            // Insert sessionId into the JSON response
            Self::inject_session_id(&resp, sid)
        } else {
            resp
        };
        let post_events: Vec<String> = post_events
            .into_iter()
            .map(|evt| {
                if let Some(ref sid) = session_id {
                    Self::inject_session_id(&evt, sid)
                } else {
                    evt
                }
            })
            .collect();
        // M55: Target events (session creation) before response;
        // Page lifecycle events after response (puppeteer's LifecycleWatcher
        // is created after receiving the navigate response).
        for evt in &post_events {
            if evt.contains("\"Target.") {
                if let Err(e) = self.send_text(evt).await {
                    eprintln!("[cdp] event send error: {e}");
                }
            }
        }
        self.send_text(&resp).await?;
        // M55: delay before sending lifecycle events.
        // Puppeteer's LifecycleWatcher is created in the response callback,
        // but events in the same TCP batch arrive in the same event-loop tick.
        // A tiny delay ensures puppeteer processes the response first.
        if post_events.iter().any(|e| !e.contains("\"Target.")) {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        for evt in &post_events {
            if !evt.contains("\"Target.") {
                if let Err(e) = self.send_text(evt).await {
                    eprintln!("[cdp] event send error: {e}");
                }
            }
        }
        Ok(())
    }

    /// M53: inject `sessionId` field into a JSON message string.
    /// This is needed for flatten session mode where responses and events
    /// must carry the session ID they belong to.
    fn inject_session_id(json: &str, session_id: &str) -> String {
        // Quick approach: find the opening { and insert after it.
        // All CDP messages start with `{`.
        if let Some(rest) = json.strip_prefix('{') {
            format!("{{\"sessionId\":\"{}\",{}", session_id, rest)
        } else {
            json.to_string()
        }
    }

    /// Send a text message as a single (unmasked, server→client) frame.
    async fn send_text(&mut self, text: &str) -> Result<(), String> {
        let frame = Frame::text(text);
        let bytes = encode_frame(&frame, None);
        self.stream
            .write_all(&bytes)
            .await
            .map_err(|e| format!("write: {e}"))?;
        Ok(())
    }
}

#[cfg(all(test, feature = "boa"))]
mod tests {
    use super::*;

    /// Integration test: spin up the CDP server, connect a raw TCP client,
    /// do the WS handshake, send a CDP message, and check the response.
    #[tokio::test]
    async fn end_to_end_handshake_and_get_version() {
        use browser_ws::handshake::{build_client_request, parse_accept_from_response};

        // Bind the server on an ephemeral port.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        // Server task: accept and handle one session.
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            CdpSession::handle(stream, EngineKind::Boa, SharedBrowserState::new()).await
        });

        // Client: connect, do WS handshake.
        let mut client = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        let key = browser_ws::handshake::key_from_random([0x42; 16]);
        let req = build_client_request("127.0.0.1", "/", &key);
        client.write_all(req.as_bytes()).await.unwrap();

        // Read the 101 response.
        let mut buf = vec![0u8; 1024];
        let n = client.read(&mut buf).await.unwrap();
        let resp = String::from_utf8_lossy(&buf[..n]).to_string();
        assert!(resp.starts_with("HTTP/1.1 101"));
        let accept = parse_accept_from_response(&resp).expect("accept header");
        assert_eq!(accept, browser_ws::handshake::compute_accept(&key));

        // Send a CDP message (masked, client→server).
        let msg = r#"{"id":1,"method":"Browser.getVersion"}"#;
        let mask = [0x11, 0x22, 0x33, 0x44];
        let frame = Frame::text(msg);
        let bytes = encode_frame(&frame, Some(mask));
        client.write_all(&bytes).await.unwrap();

        // Read the response frame (unmasked, server→client).
        let mut rbuf = vec![0u8; 4096];
        let rn = tokio::time::timeout(Duration::from_secs(2), client.read(&mut rbuf))
            .await
            .expect("timed out")
            .expect("read failed");
        let (resp_frame, _) = decode_frame(&rbuf[..rn]).unwrap().unwrap();
        assert_eq!(resp_frame.opcode, OpCode::Text);
        let text = String::from_utf8_lossy(&resp_frame.payload);
        assert!(text.contains("\"id\":1"), "got: {text}");
        assert!(text.contains("browser-rs"), "got: {text}");

        // Let the server finish.
        drop(client);
        let _ = server.await;
    }

    /// Unknown method returns -32601.
    #[tokio::test]
    async fn unknown_method_returns_error() {
        use browser_ws::handshake::build_client_request;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            CdpSession::handle(stream, EngineKind::Boa, SharedBrowserState::new()).await
        });
        let mut client = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        let key = browser_ws::handshake::key_from_random([0x01; 16]);
        let req = build_client_request("127.0.0.1", "/", &key);
        client.write_all(req.as_bytes()).await.unwrap();
        let mut buf = vec![0u8; 1024];
        let _ = client.read(&mut buf).await.unwrap();

        let msg = r#"{"id":2,"method":"Totally.MadeUp"}"#;
        let bytes = encode_frame(&Frame::text(msg), Some([1, 2, 3, 4]));
        client.write_all(&bytes).await.unwrap();

        let mut rbuf = vec![0u8; 4096];
        let rn = tokio::time::timeout(Duration::from_secs(2), client.read(&mut rbuf))
            .await
            .unwrap()
            .unwrap();
        let (resp_frame, _) = decode_frame(&rbuf[..rn]).unwrap().unwrap();
        let text = String::from_utf8_lossy(&resp_frame.payload);
        assert!(text.contains("-32601"), "got: {text}");
        assert!(text.contains("Method not found"), "got: {text}");

        drop(client);
        let _ = server.await;
    }

    /// M43: HTTP discovery endpoint /json/version works end-to-end.
    #[tokio::test]
    async fn http_discovery_version_endpoint() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(CdpServer::accept_one(
            listener,
            EngineKind::Boa,
            SharedBrowserState::new(),
        ));

        // Plain HTTP GET /json/version (no WebSocket upgrade).
        let mut client = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        client
            .write_all(b"GET /json/version HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")
            .await
            .unwrap();

        let mut buf = vec![0u8; 4096];
        let n = tokio::time::timeout(Duration::from_secs(2), client.read(&mut buf))
            .await
            .unwrap()
            .unwrap();
        let resp = String::from_utf8_lossy(&buf[..n]);
        assert!(resp.starts_with("HTTP/1.1 200 OK"), "got: {resp}");
        assert!(resp.contains("application/json"), "got: {resp}");
        assert!(
            resp.contains("\"Browser\":\"browser-rs/0.0.1\""),
            "got: {resp}"
        );
        assert!(resp.contains("webSocketDebuggerUrl"), "got: {resp}");
        // Must include the dynamically-detected host:port.
        assert!(resp.contains(&format!("127.0.0.1:{port}")), "got: {resp}");

        drop(client);
        let _ = server.await;
    }

    /// M43: /json/list returns an array.
    #[tokio::test]
    async fn http_discovery_list_endpoint() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(CdpServer::accept_one(
            listener,
            EngineKind::Boa,
            SharedBrowserState::new(),
        ));

        let mut client = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        client
            .write_all(b"GET /json/list HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")
            .await
            .unwrap();

        let mut buf = vec![0u8; 4096];
        let n = client.read(&mut buf).await.unwrap();
        let resp = String::from_utf8_lossy(&buf[..n]);
        assert!(resp.starts_with("HTTP/1.1 200 OK"));
        // Body should contain a JSON array with one target.
        let body_start = resp.find("\r\n\r\n").unwrap() + 4;
        let body = &resp[body_start..];
        assert!(body.starts_with('['), "got: {body}");
        assert!(
            body.contains("\"id\":\"browser-rs-target-0\""),
            "got: {body}"
        );

        drop(client);
        let _ = server.await;
    }

    /// M43: WebSocket upgrade still works when path is NOT /json*.
    /// Ensures the routing doesn't break existing WS clients.
    #[tokio::test]
    async fn routing_ws_still_works_alongside_discovery() {
        use browser_ws::handshake::build_client_request;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(CdpServer::accept_one(
            listener,
            EngineKind::Boa,
            SharedBrowserState::new(),
        ));

        let mut client = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        let key = browser_ws::handshake::key_from_random([0x99; 16]);
        let req = build_client_request("127.0.0.1", "/devtools/page/0", &key);
        client.write_all(req.as_bytes()).await.unwrap();

        let mut buf = vec![0u8; 1024];
        let n = client.read(&mut buf).await.unwrap();
        let resp = String::from_utf8_lossy(&buf[..n]);
        assert!(
            resp.starts_with("HTTP/1.1 101"),
            "ws upgrade broken: {resp}"
        );

        drop(client);
        let _ = server.await;
    }

    /// M43: Non-JSON plain HTTP returns 404 (not a broken WS drop).
    #[tokio::test]
    async fn non_json_plain_http_returns_404() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(CdpServer::accept_one(
            listener,
            EngineKind::Boa,
            SharedBrowserState::new(),
        ));

        let mut client = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        client
            .write_all(b"GET /random/path HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")
            .await
            .unwrap();

        let mut buf = vec![0u8; 1024];
        let n = client.read(&mut buf).await.unwrap();
        let resp = String::from_utf8_lossy(&buf[..n]);
        assert!(resp.starts_with("HTTP/1.1 404"), "got: {resp}");

        drop(client);
        let _ = server.await;
    }
}

/// M81(A1): multi-session tests (Playwright dual-connection model).
///
/// Not boa-gated: these flows never touch the JS engine (Browser.getVersion /
/// Page.getNavigationHistory are pure state reads), so they run under the
/// default feature set.
#[cfg(test)]
mod multi_session_tests {
    use super::*;

    use browser_ws::handshake::{build_client_request, parse_accept_from_response};

    /// Do a WS handshake on `client` and assert the 101 response.
    async fn ws_handshake(client: &mut TcpStream, seed: [u8; 16], path: &str) {
        let key = browser_ws::handshake::key_from_random(seed);
        let req = build_client_request("127.0.0.1", path, &key);
        client.write_all(req.as_bytes()).await.unwrap();
        let mut buf = vec![0u8; 1024];
        let n = tokio::time::timeout(std::time::Duration::from_secs(2), client.read(&mut buf))
            .await
            .expect("handshake timed out")
            .expect("handshake read failed");
        let resp = String::from_utf8_lossy(&buf[..n]).to_string();
        assert!(resp.starts_with("HTTP/1.1 101"), "not a 101: {resp}");
        let accept = parse_accept_from_response(&resp).expect("accept header");
        assert_eq!(accept, browser_ws::handshake::compute_accept(&key));
    }

    /// Send one CDP request (masked frame) and read one response frame.
    async fn cdp_roundtrip(client: &mut TcpStream, id: i64, method: &str) -> String {
        let msg = format!(r#"{{"id":{id},"method":"{method}"}}"#);
        let bytes = encode_frame(&Frame::text(&msg), Some([0x0A, 0x0B, 0x0C, 0x0D]));
        client.write_all(&bytes).await.unwrap();
        let mut rbuf = vec![0u8; 4096];
        let n = tokio::time::timeout(std::time::Duration::from_secs(2), client.read(&mut rbuf))
            .await
            .expect("response timed out")
            .expect("response read failed");
        let (frame, _) = decode_frame(&rbuf[..n]).unwrap().unwrap();
        String::from_utf8_lossy(&frame.payload).to_string()
    }

    /// Connect with a short retry — the server binds asynchronously.
    async fn connect_with_retry(port: u16) -> TcpStream {
        for _ in 0..50 {
            if let Ok(c) = TcpStream::connect(("127.0.0.1", port)).await {
                return c;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("server never came up on port {port}");
    }

    /// Two WebSocket connections must be served **concurrently**: both
    /// handshakes complete while neither connection has closed. Under the old
    /// serial accept loop the second connection was never even handshaked
    /// until the first one closed.
    #[tokio::test]
    async fn two_connections_served_concurrently() {
        // Reserve a free port, release it, then let CdpServer::listen bind it.
        let port = {
            let probe = TcpListener::bind("127.0.0.1:0").await.unwrap();
            probe.local_addr().unwrap().port()
        };
        let server = tokio::spawn(CdpServer::listen(port, EngineKind::QuickJs));

        // Open BOTH connections and complete BOTH handshakes first.
        let mut conn_a = connect_with_retry(port).await;
        let mut conn_b = connect_with_retry(port).await;
        ws_handshake(
            &mut conn_a,
            [0x01; 16],
            "/devtools/browser/browser-rs-target-0",
        )
        .await;
        ws_handshake(
            &mut conn_b,
            [0x02; 16],
            "/devtools/page/browser-rs-target-0",
        )
        .await;

        // Both must answer while both stay open (serial server would hang
        // on the second connection's handshake above).
        let resp_b = cdp_roundtrip(&mut conn_b, 10, "Browser.getVersion").await;
        assert!(resp_b.contains("\"id\":10"), "got: {resp_b}");
        assert!(resp_b.contains("browser-rs"), "got: {resp_b}");
        let resp_a = cdp_roundtrip(&mut conn_a, 11, "Browser.getVersion").await;
        assert!(resp_a.contains("\"id\":11"), "got: {resp_a}");

        drop(conn_a);
        drop(conn_b);
        server.abort();
        let _ = server.await;
    }

    /// Concurrent connections must share one page state: a URL seeded into
    /// the shared state is visible from BOTH connections (M81(A1) — the
    /// page-level connection reads what the browser-level connection did).
    #[tokio::test]
    async fn connections_share_page_state() {
        let page = crate::page::PageState {
            url: "http://shared.test/doc".to_string(),
            ..Default::default()
        };
        let shared = SharedBrowserState::with_page(page);

        let listener = std::sync::Arc::new(TcpListener::bind("127.0.0.1:0").await.unwrap());
        let port = listener.local_addr().unwrap().port();
        // Two sessions, same shared state (mirrors listen()).
        let sa = shared.clone();
        let l1 = listener.clone();
        let server_a = tokio::spawn(async move {
            let (stream, _) = l1.accept().await.unwrap();
            CdpSession::handle(stream, EngineKind::QuickJs, sa).await
        });
        let sb = shared.clone();
        let l2 = listener.clone();
        let server_b = tokio::spawn(async move {
            let (stream, _) = l2.accept().await.unwrap();
            CdpSession::handle(stream, EngineKind::QuickJs, sb).await
        });

        let mut conn_a = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        let mut conn_b = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        ws_handshake(&mut conn_a, [0x11; 16], "/devtools/browser/x").await;
        ws_handshake(&mut conn_b, [0x12; 16], "/devtools/page/x").await;

        // Both connections must observe the SAME (seeded) page URL.
        for (conn, id) in [(&mut conn_a, 20), (&mut conn_b, 21)] {
            let resp = cdp_roundtrip(conn, id, "Page.getNavigationHistory").await;
            assert!(
                resp.contains("http://shared.test/doc"),
                "conn id={id} missing shared url, got: {resp}"
            );
        }

        drop(conn_a);
        drop(conn_b);
        let _ = server_a.await;
        let _ = server_b.await;
    }

    /// M81(A1) Playwright connect-blocker: `Target.getTargetInfo` (fired by
    /// connect_over_cdp after setAutoAttach) must return targetInfo, and
    /// `Browser.setDownloadBehavior` (default-context init) must be acked —
    /// a -32601 on either rejects Playwright's connect handshake.
    #[tokio::test]
    async fn playwright_connect_blockers_answered() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(CdpServer::accept_one(
            listener,
            EngineKind::QuickJs,
            SharedBrowserState::new(),
        ));

        let mut conn = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        ws_handshake(
            &mut conn,
            [0x21; 16],
            "/devtools/browser/browser-rs-target-0",
        )
        .await;

        let info = cdp_roundtrip(&mut conn, 30, "Target.getTargetInfo").await;
        assert!(info.contains("\"targetInfo\""), "got: {info}");
        assert!(
            info.contains("\"browserContextId\""),
            "Playwright asserts browserContextId, got: {info}"
        );

        let dl = cdp_roundtrip(&mut conn, 31, "Browser.setDownloadBehavior").await;
        assert!(dl.contains(r#""result":{}"#), "got: {dl}");
        assert!(!dl.contains("error"), "must not error, got: {dl}");

        drop(conn);
        let _ = server.await;
    }
}
