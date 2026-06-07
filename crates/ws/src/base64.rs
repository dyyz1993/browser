//! Pure Rust Base64 (RFC 4648) encoder. Hand-written for self-build.
//!
//! Encode only — WebSocket handshake never needs to decode base64
//! (client generates key, server computes accept; both are encode paths).
//!
//! `forbid(unsafe_code)`.

const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Base64-encode `input`. Pads with '=' per RFC 4648.
pub fn encode(input: &[u8]) -> String {
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    let mut i = 0;
    while i < input.len() {
        let b0 = input[i];
        let b1 = input.get(i + 1).copied().unwrap_or(0);
        let b2 = input.get(i + 2).copied().unwrap_or(0);

        out.push(TABLE[(b0 >> 2) as usize] as char);
        out.push(TABLE[(((b0 & 0x03) << 4) | (b1 >> 4)) as usize] as char);

        if i + 1 < input.len() {
            out.push(TABLE[(((b1 & 0x0f) << 2) | (b2 >> 6)) as usize] as char);
        } else {
            out.push('=');
        }
        if i + 2 < input.len() {
            out.push(TABLE[(b2 & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
        i += 3;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // RFC 4648 §10 test vectors.
    #[test]
    fn empty() {
        assert_eq!(encode(b""), "");
    }

    #[test]
    fn one_byte() {
        assert_eq!(encode(b"f"), "Zg==");
    }

    #[test]
    fn two_bytes() {
        assert_eq!(encode(b"fo"), "Zm8=");
    }

    #[test]
    fn three_bytes() {
        assert_eq!(encode(b"foo"), "Zm9v");
    }

    #[test]
    fn four_bytes() {
        assert_eq!(encode(b"foob"), "Zm9vYg==");
    }

    #[test]
    fn five_bytes() {
        assert_eq!(encode(b"fooba"), "Zm9vYmE=");
    }

    #[test]
    fn six_bytes() {
        assert_eq!(encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn ws_key_16_bytes_is_24_chars() {
        // 16 random bytes → base64 → 24 chars (16 = 5*3 + 1, last group pads ==)
        let key = [0u8; 16];
        let encoded = encode(&key);
        assert_eq!(encoded.len(), 24);
        assert!(encoded.ends_with("=="));
    }

    #[test]
    fn known_ws_key_vector() {
        // "the sample nonce" base64 = "dGhlIHNhbXBsZSBub25jZQ=="
        let nonce = b"the sample nonce";
        assert_eq!(encode(nonce), "dGhlIHNhbXBsZSBub25jZQ==");
    }
}
