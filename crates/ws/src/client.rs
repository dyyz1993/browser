//! Async WebSocket client over tokio TCP (RFC 6455).
//!
//! M23.3 scope: `ws://` (plaintext) connect + handshake + frame read/write.
//! `wss://` (TLS) is a known limitation (network env can't e2e verify real HTTPS).
//!
//! Design:
//! - Client→server frames MUST be masked (RFC 6455 §5.3).
//! - Mask key from xorshift64 PRNG seeded by system time (WS key is not a
//!   security credential — the accept-check is the integrity boundary).
//! - All I/O via tokio `TcpStream` + `AsyncReadExt`/`AsyncWriteExt`.

use std::time::{SystemTime, UNIX_EPOCH};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use url::Url;

use crate::handshake::{
    build_client_request, compute_accept, key_from_random, parse_accept_from_response,
    parse_status_code,
};
use crate::{decode_frame, encode_frame, Frame, OpCode, WsError};

/// M31: 抽象 stream 类型——plaintext (TcpStream) 和 TLS (TlsStream) 共享同一接口。
/// Box<dyn ...> 动态分发，避免泛型污染整个 WebSocket 结构。
type BoxStream = Box<dyn AsyncReadWrite + Unpin + Send>;

/// xorshift64 PRNG (Marsaglia). Non-crypto, fine for WS mask keys + client key.
/// Seeded from system nanos so each run differs.
struct XorShift64(u64);

impl XorShift64 {
    fn from_time() -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0xdead_beef_cafe_babe);
        // Avoid the all-zero state (xorshift would stuck at 0).
        Self(nanos | 1)
    }
    /// Next 64-bit (advances state).
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    /// Fill 16 bytes (for Sec-WebSocket-Key).
    fn fill16(&mut self) -> [u8; 16] {
        let a = self.next_u64().to_le_bytes();
        let b = self.next_u64().to_le_bytes();
        let mut out = [0u8; 16];
        out[..8].copy_from_slice(&a);
        out[8..].copy_from_slice(&b);
        out
    }
    /// 4-byte mask key.
    fn mask4(&mut self) -> [u8; 4] {
        let v = self.next_u64();
        [v as u8, (v >> 8) as u8, (v >> 16) as u8, (v >> 24) as u8]
    }
}

/// M31: AsyncRead + AsyncWrite 合一 trait（用于 trait object）。
pub trait AsyncReadWrite: tokio::io::AsyncRead + tokio::io::AsyncWrite {}

// Blanket impl: 所有同时实现 AsyncRead + AsyncWrite 的类型自动实现本 trait。
impl<T> AsyncReadWrite for T where T: tokio::io::AsyncRead + tokio::io::AsyncWrite {}

/// An open WebSocket connection.
pub struct WebSocket {
    // M31: plaintext TcpStream 或 TlsStream<TcpStream>，统一为 trait object。
    socket: BoxStream,
    rng: XorShift64,
    // M23.5: TCP 是字节流，一次 read 可能读到多帧（或多帧的一部分）。
    // recv_buf 跨调用保留未消费字节，避免丢失后续帧。
    recv_buf: Vec<u8>,
}

/// Message received from the server (after reassembly of continuation frames).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    Text(String),
    Binary(Vec<u8>),
    Close(Option<u16>, String),
    Ping(Vec<u8>),
    Pong(Vec<u8>),
}

impl WebSocket {
    /// Connect to a `ws://host:port/path` URL and complete the handshake.
    ///
    /// Returns Err on: unsupported scheme, TCP connect failure, bad HTTP
    /// response, or Sec-WebSocket-Accept mismatch.
    pub async fn connect(url_str: &str) -> Result<Self, WsError> {
        let url = Url::parse(url_str).map_err(|_| WsError::InvalidFrame("invalid url"))?;
        let scheme = url.scheme();
        if scheme != "ws" && scheme != "wss" {
            return Err(WsError::InvalidFrame(
                "unsupported scheme (only ws:// and wss://)",
            ));
        }
        let is_tls = scheme == "wss";
        let host = url
            .host_str()
            .ok_or(WsError::InvalidFrame("missing host"))?;
        let default_port = if is_tls { 443 } else { 80 };
        let port = url.port().unwrap_or(default_port);
        let path = if url.path().is_empty() {
            "/"
        } else {
            url.path()
        };
        // Query string appended to path (e.g. /ws?token=abc).
        let path_with_query = match url.query() {
            Some(q) => format!("{path}?{q}"),
            None => path.to_string(),
        };

        // 1. TCP connect.
        let addr = format!("{host}:{port}");
        let tcp_socket = TcpStream::connect(&addr)
            .await
            .map_err(|_| WsError::InvalidFrame("tcp connect failed"))?;

        // M31/M96.20: wss:// 需要 TLS handshake——rustls（与 net 主栈一致；
        // TLS 后端全线去 openssl，避免与 boring 共存的 Linux 堆损坏）。
        let mut socket: BoxStream = if is_tls {
            let roots = rustls::RootCertStore {
                roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
            };
            let config = rustls::ClientConfig::builder()
                .with_root_certificates(roots)
                .with_no_client_auth();
            let connector = tokio_rustls::TlsConnector::from(std::sync::Arc::new(config));
            let server_name = rustls_pki_types::ServerName::try_from(host.to_string())
                .map_err(|_| WsError::InvalidFrame("invalid server name"))?
                .to_owned();
            let tls_socket = connector
                .connect(server_name, tcp_socket)
                .await
                .map_err(|_| WsError::InvalidFrame("tls handshake failed"))?;
            Box::new(tls_socket)
        } else {
            Box::new(tcp_socket)
        };

        // 2. Generate client key + send upgrade request.
        let mut rng = XorShift64::from_time();
        let rand16 = rng.fill16();
        let key = key_from_random(rand16);
        let req = build_client_request(&format!("{host}:{port}"), &path_with_query, &key);
        socket
            .write_all(req.as_bytes())
            .await
            .map_err(|_| WsError::InvalidFrame("write handshake failed"))?;

        // 3. Read HTTP response (until \r\n\r\n).
        let mut buf = vec![0u8; 8192];
        let mut total = 0usize;
        loop {
            if total >= buf.len() {
                return Err(WsError::InvalidFrame("handshake response too large"));
            }
            let n = socket
                .read(&mut buf[total..])
                .await
                .map_err(|_| WsError::InvalidFrame("read handshake failed"))?;
            if n == 0 {
                return Err(WsError::InvalidFrame("connection closed during handshake"));
            }
            total += n;
            if buf[..total].windows(4).any(|w| w == b"\r\n\r\n") {
                break;
            }
        }
        let response = std::str::from_utf8(&buf[..total])
            .map_err(|_| WsError::InvalidFrame("non-utf8 handshake response"))?;

        // 4. Validate status + Sec-WebSocket-Accept.
        if parse_status_code(response) != Some(101) {
            return Err(WsError::InvalidFrame(
                "expected HTTP 101 Switching Protocols",
            ));
        }
        let expected_accept = compute_accept(&key);
        match parse_accept_from_response(response) {
            Some(actual) if actual == expected_accept.as_str() => { /* ok */ }
            Some(other) => {
                eprintln!("[ws] accept mismatch: expected={expected_accept} got={other}");
                return Err(WsError::InvalidFrame("sec-websocket-accept mismatch"));
            }
            None => return Err(WsError::InvalidFrame("missing sec-websocket-accept")),
        }

        Ok(Self {
            socket,
            rng,
            recv_buf: Vec::new(),
        })
    }

    /// Send a Text message (masked, single frame).
    pub async fn send_text(&mut self, text: &str) -> Result<(), WsError> {
        let frame = Frame::text(text);
        self.send_frame(&frame).await
    }

    /// Send a Binary message (masked, single frame).
    pub async fn send_binary(&mut self, data: &[u8]) -> Result<(), WsError> {
        let frame = Frame::binary(data.to_vec());
        self.send_frame(&frame).await
    }

    /// Send a raw frame (masked).
    pub async fn send_frame(&mut self, frame: &Frame) -> Result<(), WsError> {
        let mask = self.rng.mask4();
        let bytes = encode_frame(frame, Some(mask));
        self.socket
            .write_all(&bytes)
            .await
            .map_err(|_| WsError::InvalidFrame("send frame failed"))?;
        Ok(())
    }

    /// Read the next complete message, auto-responding to Ping with Pong.
    /// Reassembles fragmented (continuation) frames per RFC 6455 §5.4.
    pub async fn recv_message(&mut self) -> Result<Message, WsError> {
        // Accumulator for fragmented messages.
        let mut acc: Option<(OpCode, Vec<u8>)> = None;
        loop {
            // Try to decode a frame from the recv buffer.
            let frame_res = decode_frame(&self.recv_buf);
            let frame_and_consumed: Option<(Frame, usize)> = match frame_res {
                Ok(Some(x)) => Some(x),
                Ok(None) => {
                    // Need more bytes.
                    let mut tmp = [0u8; 8192];
                    let n = self
                        .socket
                        .read(&mut tmp)
                        .await
                        .map_err(|_| WsError::InvalidFrame("read frame failed"))?;
                    if n == 0 {
                        return Err(WsError::InvalidFrame("connection closed"));
                    }
                    self.recv_buf.extend_from_slice(&tmp[..n]);
                    continue;
                }
                Err(e) => return Err(e),
            };
            let (frame, consumed) = frame_and_consumed.unwrap();
            self.recv_buf.drain(..consumed);

            match frame.opcode {
                OpCode::Ping => {
                    // Auto-pong with same payload (masked).
                    let pong = Frame::pong(frame.payload.clone());
                    self.send_frame(&pong).await?;
                    return Ok(Message::Ping(frame.payload));
                }
                OpCode::Pong => {
                    return Ok(Message::Pong(frame.payload));
                }
                OpCode::Close => {
                    let (code, reason) = frame
                        .close_code()
                        .map(|(c, r)| (Some(c), r.to_string()))
                        .unwrap_or((None, String::new()));
                    return Ok(Message::Close(code, reason));
                }
                OpCode::Text | OpCode::Binary if frame.fin => {
                    // Unfragmented message.
                    return Ok(Self::data_message(frame.opcode, frame.payload));
                }
                OpCode::Text | OpCode::Binary => {
                    // First fragment.
                    acc = Some((frame.opcode, frame.payload));
                }
                OpCode::Continuation => {
                    let acc_ref = acc
                        .as_mut()
                        .ok_or(WsError::InvalidFrame("continuation without start"))?;
                    acc_ref.1.extend_from_slice(&frame.payload);
                    if frame.fin {
                        let (op, data) = acc.take().unwrap();
                        return Ok(Self::data_message(op, data));
                    }
                }
            }
        }
    }

    fn data_message(op: OpCode, payload: Vec<u8>) -> Message {
        match op {
            OpCode::Text => Message::Text(String::from_utf8_lossy(&payload).into_owned()),
            _ => Message::Binary(payload),
        }
    }

    /// Send a Close frame (status 1000, empty reason) and stop.
    pub async fn close(&mut self) -> Result<(), WsError> {
        let frame = Frame::close(1000, "");
        self.send_frame(&frame).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xorshift_never_zero_state() {
        let mut rng = XorShift64::from_time();
        for _ in 0..1000 {
            assert_ne!(rng.next_u64(), 0);
        }
    }

    #[test]
    fn xorshift_fill16_is_16_bytes() {
        let mut rng = XorShift64::from_time();
        let a = rng.fill16();
        let b = rng.fill16();
        assert_eq!(a.len(), 16);
        assert_eq!(b.len(), 16);
        assert_ne!(a, b); // extremely likely to differ
    }

    #[test]
    fn xorshift_mask4_is_4_bytes() {
        let mut rng = XorShift64::from_time();
        let m = rng.mask4();
        assert_eq!(m.len(), 4);
    }

    #[test]
    fn message_text_construction() {
        let m = WebSocket::data_message(OpCode::Text, b"hello".to_vec());
        assert_eq!(m, Message::Text("hello".to_string()));
    }

    #[test]
    fn message_binary_construction() {
        let m = WebSocket::data_message(OpCode::Binary, vec![0, 1, 2]);
        assert_eq!(m, Message::Binary(vec![0, 1, 2]));
    }

    #[test]
    fn message_binary_for_continuation_op() {
        // data_message only differentiates Text vs non-Text; Binary op → Binary.
        let m = WebSocket::data_message(OpCode::Binary, vec![0xAB]);
        assert!(matches!(m, Message::Binary(_)));
    }
}
