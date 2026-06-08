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

use browser_ws::handshake::{build_server_response, parse_key_from_request};
use browser_ws::{decode_frame, encode_frame, Frame, OpCode};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::jsonrpc::{parse_message, CdpMessage};

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
}

/// A single CDP client session (one WebSocket connection).
///
/// M42 scope: handshake + frame I/O + dispatch loop. Unknown methods get a
/// `-32601 Method not found` error response. M44+ adds real domain handlers.
pub struct CdpSession {
    stream: TcpStream,
}

impl CdpSession {
    /// Handle a connection end-to-end: handshake → message loop.
    ///
    /// # Errors
    /// Returns an error string on handshake or I/O failure.
    pub async fn handle(stream: TcpStream) -> Result<(), String> {
        let mut session = CdpSession { stream };
        session.do_handshake().await?;
        session.message_loop().await
    }

    /// Read the HTTP upgrade request and send the 101 response.
    async fn do_handshake(&mut self) -> Result<(), String> {
        // Read until \r\n\r\n (end of HTTP headers). Cap at 8KB to avoid abuse.
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
        let request = String::from_utf8_lossy(&buf[..filled]).to_string();
        let key = parse_key_from_request(&request)
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
        let resp = match method {
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
                CdpMessage::ok_response(id, crate::jsonrpc::Json::Object(result))
            }
            _ => CdpMessage::error_response(id, -32601, "Method not found"),
        };
        self.send_text(&resp).await
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
}
