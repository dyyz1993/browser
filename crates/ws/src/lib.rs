//! `browser-ws` — RFC 6455 WebSocket frame codec (hand-written).
//!
//! M23.1 scope: pure frame encode/decode, no networking.
//! - [`OpCode`] — frame opcode (text/binary/close/ping/pong/continuation)
//! - [`Frame`] — a single WebSocket frame
//! - [`encode_frame`] — encode to wire bytes (client-side masked)
//! - [`decode_frame`] — decode from buffer (returns None if incomplete)
//! - [`apply_mask`] — XOR payload with 4-byte mask key (symmetric)
//!
//! Design: hand-written per GOALS.md "self-build first". No tungstenite.
//! `forbid(unsafe_code)` — masking uses safe loop (perf acceptable for MVP).

#![forbid(unsafe_code)]

pub mod base64;
pub mod client;
pub mod handshake;
pub mod manager;
pub mod sha1;

pub use client::{Message, WebSocket};
pub use manager::{WsEvent, WsManager};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum WsError {
    #[error("incomplete frame (need more bytes)")]
    IncompleteFrame,
    #[error("invalid opcode: 0x{0:x}")]
    InvalidOpcode(u8),
    #[error("invalid frame: {0}")]
    InvalidFrame(&'static str),
}

/// WebSocket opcode (RFC 6455 §5.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum OpCode {
    Continuation = 0x0,
    Text = 0x1,
    Binary = 0x2,
    Close = 0x8,
    Ping = 0x9,
    Pong = 0xA,
}

impl OpCode {
    /// Parse opcode from low nibble.
    pub fn from_u8(b: u8) -> Result<Self, WsError> {
        Ok(match b {
            0x0 => Self::Continuation,
            0x1 => Self::Text,
            0x2 => Self::Binary,
            0x8 => Self::Close,
            0x9 => Self::Ping,
            0xA => Self::Pong,
            other => return Err(WsError::InvalidOpcode(other)),
        })
    }
    /// Control frames (close/ping/pong) must be FIN and ≤125 bytes.
    pub fn is_control(self) -> bool {
        matches!(self, Self::Close | Self::Ping | Self::Pong)
    }
}

/// A single WebSocket frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub fin: bool,
    pub opcode: OpCode,
    pub payload: Vec<u8>,
}

impl Frame {
    /// Create a final Text frame.
    pub fn text(s: &str) -> Self {
        Self {
            fin: true,
            opcode: OpCode::Text,
            payload: s.as_bytes().to_vec(),
        }
    }
    /// Create a final Binary frame.
    pub fn binary(data: Vec<u8>) -> Self {
        Self {
            fin: true,
            opcode: OpCode::Binary,
            payload: data,
        }
    }
    /// Create a Close frame with status code + reason (UTF-8).
    pub fn close(code: u16, reason: &str) -> Self {
        let mut payload = code.to_be_bytes().to_vec();
        payload.extend_from_slice(reason.as_bytes());
        Self {
            fin: true,
            opcode: OpCode::Close,
            payload,
        }
    }
    /// Create a Ping frame.
    pub fn ping(data: Vec<u8>) -> Self {
        Self {
            fin: true,
            opcode: OpCode::Ping,
            payload: data,
        }
    }
    /// Create a Pong frame.
    pub fn pong(data: Vec<u8>) -> Self {
        Self {
            fin: true,
            opcode: OpCode::Pong,
            payload: data,
        }
    }
    /// Decode Close frame payload to `(code, reason)`. None if not Close/empty/invalid.
    pub fn close_code(&self) -> Option<(u16, &str)> {
        if self.opcode != OpCode::Close {
            return None;
        }
        if self.payload.is_empty() {
            return None;
        }
        if self.payload.len() == 1 {
            return None; // invalid: code needs 2 bytes
        }
        let code = u16::from_be_bytes([self.payload[0], self.payload[1]]);
        let reason = std::str::from_utf8(&self.payload[2..]).ok()?;
        Some((code, reason))
    }
}

/// Apply/remove XOR mask (symmetric operation). RFC 6455 §5.3.
/// `payload[i] ^= mask_key[i % 4]`.
pub fn apply_mask(payload: &mut [u8], mask_key: [u8; 4]) {
    for (i, b) in payload.iter_mut().enumerate() {
        *b ^= mask_key[i % 4];
    }
}

/// Encode a frame to wire bytes.
/// - `mask_key = Some(k)`: client→server (masked). `k` is the 4-byte mask key.
/// - `mask_key = None`: server→client (unmasked).
pub fn encode_frame(frame: &Frame, mask_key: Option<[u8; 4]>) -> Vec<u8> {
    let masked = mask_key.is_some();
    let mut buf = Vec::with_capacity(frame.payload.len() + 14);
    // Byte 0: FIN + RSV(000) + opcode
    let b0 = (if frame.fin { 0x80 } else { 0x00 }) | (frame.opcode as u8);
    buf.push(b0);
    // Byte 1: MASK + payload len
    let mask_bit = if masked { 0x80 } else { 0x00 };
    let len = frame.payload.len();
    if len < 126 {
        buf.push(mask_bit | len as u8);
    } else if len <= u16::MAX as usize {
        buf.push(mask_bit | 126);
        buf.extend_from_slice(&(len as u16).to_be_bytes());
    } else {
        buf.push(mask_bit | 127);
        buf.extend_from_slice(&(len as u64).to_be_bytes());
    }
    // Mask key (if masked)
    if let Some(k) = mask_key {
        buf.extend_from_slice(&k);
    }
    // Payload (masked copy if needed)
    if masked {
        let mut payload = frame.payload.clone();
        apply_mask(&mut payload, mask_key.unwrap());
        buf.extend_from_slice(&payload);
    } else {
        buf.extend_from_slice(&frame.payload);
    }
    buf
}

/// Decode a frame from buffer.
/// Returns `Ok(None)` if incomplete (caller should read more bytes).
/// Handles both masked (client) and unmasked (server) frames.
/// Returns `(Frame, bytes_consumed)` on success.
pub fn decode_frame(buf: &[u8]) -> Result<Option<(Frame, usize)>, WsError> {
    // Min header: 2 bytes
    if buf.len() < 2 {
        return Ok(None);
    }
    let b0 = buf[0];
    let b1 = buf[1];
    let fin = b0 & 0x80 != 0;
    let rsv = b0 & 0x70;
    if rsv != 0 {
        return Err(WsError::InvalidFrame("RSV bits must be zero"));
    }
    let opcode = OpCode::from_u8(b0 & 0x0F)?;
    let masked = b1 & 0x80 != 0;
    let len7 = (b1 & 0x7F) as usize;
    // Control frames must be FIN and ≤125 bytes
    if opcode.is_control() {
        if !fin {
            return Err(WsError::InvalidFrame(
                "control frame must not be fragmented",
            ));
        }
        if len7 > 125 {
            return Err(WsError::InvalidFrame("control frame payload > 125"));
        }
    }
    // Compute payload length + extended-length byte count
    let (payload_len, ext_len_bytes) = match len7 {
        0..=125 => (len7, 0usize),
        126 => {
            if buf.len() < 4 {
                return Ok(None);
            }
            let l = u16::from_be_bytes([buf[2], buf[3]]) as usize;
            if l < 126 {
                return Err(WsError::InvalidFrame(
                    "16-bit length must be >= 126 (canonical encoding)",
                ));
            }
            (l, 2)
        }
        127 => {
            if buf.len() < 10 {
                return Ok(None);
            }
            let raw = [
                buf[2], buf[3], buf[4], buf[5], buf[6], buf[7], buf[8], buf[9],
            ];
            let l = u64::from_be_bytes(raw) as usize;
            // Sanity: cap at 1 MiB for MVP (defense against malformed)
            if l > 1024 * 1024 {
                return Err(WsError::InvalidFrame("payload exceeds 1 MiB MVP limit"));
            }
            (l, 8)
        }
        _ => unreachable!(),
    };
    let mask_bytes = if masked { 4 } else { 0 };
    let header_len = 2 + ext_len_bytes + mask_bytes;
    if buf.len() < header_len {
        return Ok(None);
    }
    let total = header_len + payload_len;
    if buf.len() < total {
        return Ok(None); // need more bytes
    }
    // Extract mask key (if masked)
    let mask_key = if masked {
        let off = 2 + ext_len_bytes;
        Some([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]])
    } else {
        None
    };
    // Extract + unmask payload
    let mut payload = buf[header_len..total].to_vec();
    if let Some(k) = mask_key {
        apply_mask(&mut payload, k);
    }
    Ok(Some((
        Frame {
            fin,
            opcode,
            payload,
        },
        total,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ping() {
        assert_eq!(env!("CARGO_PKG_NAME"), "browser-ws");
    }

    // ---- OpCode ----
    #[test]
    fn opcode_round_trip() {
        for &op in &[
            OpCode::Continuation,
            OpCode::Text,
            OpCode::Binary,
            OpCode::Close,
            OpCode::Ping,
            OpCode::Pong,
        ] {
            assert_eq!(OpCode::from_u8(op as u8).unwrap(), op);
        }
        assert!(OpCode::from_u8(0xB).is_err());
    }

    #[test]
    fn opcode_is_control() {
        assert!(OpCode::Close.is_control());
        assert!(OpCode::Ping.is_control());
        assert!(OpCode::Pong.is_control());
        assert!(!OpCode::Text.is_control());
        assert!(!OpCode::Binary.is_control());
    }

    // ---- apply_mask ----
    #[test]
    fn mask_is_symmetric() {
        let key = [0x37, 0xfa, 0x21, 0x3d];
        let mut a = b"Hello".to_vec();
        apply_mask(&mut a, key);
        assert_ne!(a, b"Hello"); // changed
        apply_mask(&mut a, key); // unmask → original
        assert_eq!(a, b"Hello");
    }

    #[test]
    fn mask_key_wraps_mod_4() {
        let key = [1, 2, 3, 4];
        let mut data = vec![0u8; 10];
        apply_mask(&mut data, key);
        assert_eq!(data, vec![1, 2, 3, 4, 1, 2, 3, 4, 1, 2]);
    }

    // ---- encode ----
    #[test]
    fn encode_unmasked_text_short() {
        let f = Frame::text("Hi");
        let bytes = encode_frame(&f, None);
        // FIN|Text=0x81, len=2, 'H','i'
        assert_eq!(bytes, vec![0x81, 0x02, b'H', b'i']);
    }

    #[test]
    fn encode_masked_text_includes_mask_key() {
        let f = Frame::text("Hi");
        let key = [0u8; 4]; // zero mask = no change
        let bytes = encode_frame(&f, Some(key));
        // 0x81, 0x82 (masked, len 2), 00 00 00 00, 'H','i'
        assert_eq!(bytes, vec![0x81, 0x82, 0, 0, 0, 0, b'H', b'i']);
    }

    #[test]
    fn encode_masked_text_xors_payload() {
        let f = Frame::text("AAAA"); // 4 bytes
        let key = [0x01; 4];
        let bytes = encode_frame(&f, Some(key));
        assert_eq!(
            bytes,
            vec![
                0x81,
                0x84,
                1,
                1,
                1,
                1,
                b'A' ^ 1,
                b'A' ^ 1,
                b'A' ^ 1,
                b'A' ^ 1
            ]
        );
    }

    #[test]
    fn encode_16bit_length() {
        let payload = "x".repeat(200);
        let f = Frame::text(&payload);
        let bytes = encode_frame(&f, None);
        assert_eq!(bytes[0], 0x81);
        assert_eq!(bytes[1], 126); // 16-bit signal
        assert_eq!(u16::from_be_bytes([bytes[2], bytes[3]]), 200);
        assert_eq!(bytes.len(), 4 + 200);
    }

    #[test]
    fn encode_64bit_length() {
        let payload = vec![0u8; 70_000];
        let f = Frame::binary(payload.clone());
        let bytes = encode_frame(&f, None);
        assert_eq!(bytes[1], 127); // 64-bit signal
        let len = u64::from_be_bytes(bytes[2..10].try_into().unwrap()) as usize;
        assert_eq!(len, 70_000);
        assert_eq!(bytes.len(), 10 + 70_000);
    }

    #[test]
    fn encode_close_frame() {
        let f = Frame::close(1000, "bye");
        let bytes = encode_frame(&f, None);
        assert_eq!(bytes[0], 0x88); // FIN|Close
        assert_eq!(bytes[1], 5); // 2 bytes code + 3 bytes "bye"
        let code = u16::from_be_bytes([bytes[2], bytes[3]]);
        assert_eq!(code, 1000);
    }

    #[test]
    fn encode_ping_pong() {
        let ping = encode_frame(&Frame::ping(b"data".to_vec()), None);
        assert_eq!(ping[0], 0x89); // FIN|Ping
        assert_eq!(ping[1], 4);
        let pong = encode_frame(&Frame::pong(b"data".to_vec()), None);
        assert_eq!(pong[0], 0x8A); // FIN|Pong
    }

    #[test]
    fn encode_fragmented_frame() {
        let f = Frame {
            fin: false,
            opcode: OpCode::Text,
            payload: b"frag".to_vec(),
        };
        let bytes = encode_frame(&f, None);
        assert_eq!(bytes[0], 0x01); // NOT FIN | Text
    }

    // ---- decode ----
    #[test]
    fn decode_returns_none_when_incomplete() {
        assert_eq!(decode_frame(&[]).unwrap(), None);
        assert_eq!(decode_frame(&[0x81]).unwrap(), None); // only 1 byte
        assert_eq!(decode_frame(&[0x81, 0x05, b'H']).unwrap(), None); // need 5, have 1
    }

    #[test]
    fn decode_unmasked_text() {
        let bytes = vec![0x81, 0x02, b'H', b'i'];
        let (f, consumed) = decode_frame(&bytes).unwrap().unwrap();
        assert_eq!(consumed, 4);
        assert!(f.fin);
        assert_eq!(f.opcode, OpCode::Text);
        assert_eq!(f.payload, b"Hi");
    }

    #[test]
    fn decode_masked_text_unmasks_payload() {
        let key = [0x01; 4];
        let original = Frame::text("AAAA");
        let bytes = encode_frame(&original, Some(key));
        let (f, consumed) = decode_frame(&bytes).unwrap().unwrap();
        assert_eq!(consumed, bytes.len());
        assert_eq!(f.payload, b"AAAA"); // unmasked back
        assert_eq!(f.opcode, OpCode::Text);
    }

    #[test]
    fn decode_close_frame() {
        let f = Frame::close(1000, "bye");
        let bytes = encode_frame(&f, None);
        let (decoded, _) = decode_frame(&bytes).unwrap().unwrap();
        assert_eq!(decoded.opcode, OpCode::Close);
        assert_eq!(decoded.close_code(), Some((1000, "bye")));
    }

    #[test]
    fn decode_16bit_length() {
        let payload = "x".repeat(200);
        let f = Frame::text(&payload);
        let bytes = encode_frame(&f, None);
        let (decoded, consumed) = decode_frame(&bytes).unwrap().unwrap();
        assert_eq!(consumed, bytes.len());
        assert_eq!(decoded.payload.len(), 200);
        assert_eq!(std::str::from_utf8(&decoded.payload).unwrap(), payload);
    }

    #[test]
    fn decode_64bit_length() {
        let f = Frame::binary(vec![0xAB; 70_000]);
        let bytes = encode_frame(&f, None);
        let (decoded, consumed) = decode_frame(&bytes).unwrap().unwrap();
        assert_eq!(consumed, bytes.len());
        assert_eq!(decoded.payload.len(), 70_000);
        assert!(decoded.payload.iter().all(|&b| b == 0xAB));
    }

    #[test]
    fn decode_rejects_rsv_bits() {
        let bytes = vec![0xF1, 0x00]; // RSV1,2,3 set
        assert!(decode_frame(&bytes).is_err());
    }

    #[test]
    fn decode_rejects_invalid_opcode() {
        let bytes = vec![0x8B, 0x00]; // opcode 0xB
        assert!(decode_frame(&bytes).is_err());
    }

    #[test]
    fn decode_rejects_fragmented_control_frame() {
        // FIN=0, opcode=Close (must be FIN)
        let bytes = vec![0x08, 0x00];
        assert!(decode_frame(&bytes).is_err());
    }

    #[test]
    fn decode_rejects_oversized_control_payload() {
        // Close frame with 16-bit length signal (126) is invalid for control
        let bytes = vec![0x88, 126, 0x01, 0x00]; // claims 256 bytes
        assert!(decode_frame(&bytes).is_err());
    }

    // ---- round trip ----
    #[test]
    fn round_trip_text_masked_utf8() {
        let original = Frame::text("Hello, 世界! 🦀");
        let key = [0x12, 0x34, 0x56, 0x78];
        let bytes = encode_frame(&original, Some(key));
        let (decoded, _) = decode_frame(&bytes).unwrap().unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn round_trip_binary_unmasked() {
        let original = Frame::binary(vec![0, 1, 2, 3, 255, 128, 64]);
        let bytes = encode_frame(&original, None);
        let (decoded, _) = decode_frame(&bytes).unwrap().unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn decode_multiple_frames_from_buffer() {
        // Two frames concatenated
        let b1 = encode_frame(&Frame::text("AB"), None);
        let b2 = encode_frame(&Frame::ping(b"x".to_vec()), None);
        let mut buf = b1.clone();
        buf.extend_from_slice(&b2);
        let (f1, c1) = decode_frame(&buf).unwrap().unwrap();
        let (f2, c2) = decode_frame(&buf[c1..]).unwrap().unwrap();
        assert_eq!(f1, Frame::text("AB"));
        assert_eq!(f2.opcode, OpCode::Ping);
        assert_eq!(c1 + c2, buf.len());
    }
}
