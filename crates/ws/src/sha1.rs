//! Pure Rust SHA-1 (FIPS 180-4). Hand-written for learning + self-build.
//!
//! **NOT for cryptographic security** — SHA-1 is broken for signatures/certs.
//! Used here only for the WebSocket handshake accept value (RFC 6455 §1.3),
//! where it's a non-secret integrity check, not a security boundary.
//!
//! `forbid(unsafe_code)` — all rotates/wraps are safe.

/// Compute SHA-1 digest (20 bytes) of `input`.
#[allow(clippy::needless_range_loop)]
pub fn sha1(input: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];

    // Padding: append 0x80, then zeros until length ≡ 56 (mod 64),
    // then 8-byte big-endian bit length.
    let mut msg = input.to_vec();
    let bit_len = (input.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());

    // Process each 64-byte block.
    for chunk in msg.chunks(64) {
        let mut w = [0u32; 80];
        for (i, word_bytes) in chunk.chunks(4).enumerate() {
            w[i] = u32::from_be_bytes([word_bytes[0], word_bytes[1], word_bytes[2], word_bytes[3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }

        let (mut a, mut b, mut c, mut d, mut e) = (h[0], h[1], h[2], h[3], h[4]);

        for i in 0..80 {
            let (f, k): (u32, u32) = match i {
                0..=19 => ((b & c) | ((!b) & d), 0x5A827999),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6),
            };
            let temp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(w[i]);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }

        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    }

    let mut out = [0u8; 20];
    for (i, &v) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&v.to_be_bytes());
    }
    out
}

/// Format digest as lowercase hex string.
pub fn sha1_hex(input: &[u8]) -> String {
    let digest = sha1(input);
    let mut s = String::with_capacity(40);
    for b in digest {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hex string → byte comparison helper.
    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    // FIPS 180-4 / RFC 3174 test vectors.
    #[test]
    fn empty_string() {
        assert_eq!(sha1_hex(b""), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
    }

    #[test]
    fn abc() {
        assert_eq!(sha1_hex(b"abc"), "a9993e364706816aba3e25717850c26c9cd0d89d");
    }

    #[test]
    fn longer_than_one_block() {
        // 448 bits = 56 bytes (< 64), single block after padding
        let msg = b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq";
        assert_eq!(sha1_hex(msg), "84983e441c3bd26ebaae4aa1f95129e5e54670f1");
    }

    #[test]
    fn exactly_one_million_a() {
        // Classic stress test: 1,000,000 'a' → multi-block
        let msg = vec![b'a'; 1_000_000];
        assert_eq!(sha1_hex(&msg), "34aa973cd4c4daa4f61eeb2bdbad27316534016f");
    }

    #[test]
    fn crosses_block_boundary() {
        // 64 bytes exactly fills one block, forces second block for padding
        let msg = vec![b'a'; 64];
        assert_eq!(sha1_hex(&msg), "0098ba824b5c16427bd7a1122a5a442a25ec644d");
    }

    #[test]
    fn sha1_returns_20_bytes() {
        let digest = sha1(b"test");
        assert_eq!(digest.len(), 20);
        assert_eq!(hex("a94a8fe5ccb19ba61c4c0873d391e987982fbbd3"), digest);
    }

    #[test]
    fn rfc6455_handshake_concat() {
        // The exact concat used by WS handshake (Sec-WebSocket-Key + GUID).
        // SHA1("dGhlIHNhbXBsZSBub25jZQ==258EAFA5-E914-47DA-95CA-C5AB0DC85B11")
        let concat = b"dGhlIHNhbXBsZSBub25jZQ==258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
        // Expected: base64 of this hash = "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        // So raw hash hex = b37a4f2cc0624f1690f64606cf385945.... (derive below)
        let digest = sha1(concat);
        // Cross-check: base64-encode manually
        let b64 = crate::base64::encode(&digest);
        assert_eq!(b64, "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
    }
}
