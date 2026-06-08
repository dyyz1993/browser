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

use browser_ws::handshake::{build_server_response, parse_key_from_request};
use browser_ws::{decode_frame, encode_frame, Frame, OpCode};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::jsonrpc::{parse_message, CdpMessage, Json};
use std::collections::BTreeMap;

#[cfg(test)]
use std::time::Duration;

/// Default CDP port (Chrome standard).
pub const DEFAULT_CDP_PORT: u16 = 9222;

/// A running CDP server bound to a port.
///
/// Call [`CdpServer::listen`] to accept connections. The server processes
/// one session at a time (M42 scope — adequate for a single-Tab browser;
/// M43 adds multi-Tab + discovery endpoints).
pub struct CdpServer;

impl CdpServer {
    /// Bind to `port` and accept connections in a loop, handling each session
    /// to completion before the next. Runs until the listener errors.
    ///
    /// # Errors
    /// Returns an error if binding fails or the accept loop hits an
    /// unrecoverable I/O error.
    pub async fn listen(port: u16) -> std::io::Result<()> {
        let listener = TcpListener::bind(("127.0.0.1", port)).await?;
        eprintln!("[cdp] listening on http://127.0.0.1:{port}");
        loop {
            let (stream, addr) = listener.accept().await?;
            eprintln!("[cdp] connection from {addr}");
            if let Err(e) = CdpSession::handle(stream).await {
                eprintln!("[cdp] session ended: {e}");
            }
        }
    }

    /// Accept and handle exactly one connection, then return. Used by tests.
    #[cfg(test)]
    pub(crate) async fn accept_one(listener: TcpListener) -> std::io::Result<()> {
        let (stream, _) = listener.accept().await?;
        let _ = CdpSession::handle(stream).await;
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
/// `-32601 Method not found` error response. M44+ adds real domain handlers.
pub struct CdpSession {
    stream: TcpStream,
    /// M44: per-session page state (single-tab model).
    page: Arc<Mutex<crate::page::PageState>>,
}

impl CdpSession {
    /// Handle a connection end-to-end: handshake → message loop.
    ///
    /// # Errors
    /// Returns an error string on handshake or I/O failure.
    pub async fn handle(stream: TcpStream) -> Result<(), String> {
        let mut session = CdpSession {
            stream,
            page: Arc::new(Mutex::new(crate::page::PageState::default())),
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
        // M42: dispatch table is empty — everything is "not found" yet.
        // M44+ will route "Page.*" to PageHandler, "Runtime.*" to RuntimeHandler, etc.
        // M50: collect response + post-response events
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
            // ── M44+M50: Page domain (navigate, captureScreenshot + events) ──
            m if m.starts_with("Page.") => {
                match crate::page::dispatch(id, m, msg.params.as_ref(), self.page.clone()).await {
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
            m if m.starts_with("Runtime.") => {
                match crate::runtime_domain::dispatch(id, m, msg.params.as_ref()) {
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
            // ── M47: Network domain (getResponseBody, enable/disable) ──
            m if m.starts_with("Network.") => {
                let Ok(st) = self.page.lock() else {
                    return Ok(());
                };
                match crate::network_domain::dispatch(id, m, &st) {
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
                let resp = match crate::target_domain::dispatch(id, m) {
                    Ok(resp) => resp,
                    Err(crate::jsonrpc::CdpError::MethodNotFound(_)) => {
                        CdpMessage::error_response(id, -32601, "Method not found")
                    }
                    Err(e) => CdpMessage::error_response(id, -32000, &e.to_string()),
                };
                // M51: emit events at the right time
                let ws_host = self.ws_host();
                let events: Vec<String> = match m {
                    "Target.setDiscoverTargets" => {
                        // puppeteer registers listener before calling this
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
                    "Target.setAutoAttach" => {
                        // emit attachedToTarget so puppeteer creates a session
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
            _ => (
                CdpMessage::error_response(id, -32601, "Method not found"),
                vec![],
            ),
        };
        // M50: send pre-response events first (so puppeteer creates sessions before looking them up),
        // then response, then post-response events.
        for evt in &post_events {
            if let Err(e) = self.send_text(evt).await {
                eprintln!("[cdp] event send error: {e}");
            }
        }
        self.send_text(&resp).await?;
        Ok(())
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

#[cfg(test)]
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
            CdpSession::handle(stream).await
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
            CdpSession::handle(stream).await
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
        let server = tokio::spawn(CdpServer::accept_one(listener));

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
        let server = tokio::spawn(CdpServer::accept_one(listener));

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
        let server = tokio::spawn(CdpServer::accept_one(listener));

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
        let server = tokio::spawn(CdpServer::accept_one(listener));

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
