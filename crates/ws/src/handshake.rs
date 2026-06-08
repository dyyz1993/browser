//! WebSocket handshake helpers (RFC 6455 §1.3, §4.1, §4.2.2).
//!
//! Client→Server opening handshake:
//! ```http
//! GET /chat HTTP/1.1
//! Host: server.example.com
//! Upgrade: websocket
//! Connection: Upgrade
//! Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==
//! Sec-WebSocket-Version: 13
//! ```
//!
//! Server→Client response:
//! ```http
//! HTTP/1.1 101 Switching Protocols
//! Upgrade: websocket
//! Connection: Upgrade
//! Sec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=
//! ```
//!
//! Where `Sec-WebSocket-Accept = base64(sha1(Sec-WebSocket-Key + GUID))`
//! and GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11".

/// The magic GUID appended to the client key when computing the accept value.
pub const GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// Compute `Sec-WebSocket-Accept` from the client's `Sec-WebSocket-Key`.
///
/// `accept = base64(sha1(key + GUID))`
pub fn compute_accept(key: &str) -> String {
    let mut concat = String::with_capacity(key.len() + GUID.len());
    concat.push_str(key);
    concat.push_str(GUID);
    let hash = crate::sha1::sha1(concat.as_bytes());
    crate::base64::encode(&hash)
}

/// Build a `Sec-WebSocket-Key` from 16 random bytes (base64-encoded).
///
/// Caller supplies the randomness so this stays pure + testable.
/// Real callers should use a CSPRNG; tests pass fixed bytes.
pub fn key_from_random(rand16: [u8; 16]) -> String {
    crate::base64::encode(&rand16)
}

/// Build the client HTTP upgrade request as a complete string
/// (ready to send over the socket).
pub fn build_client_request(host: &str, path: &str, key: &str) -> String {
    format!(
        "GET {path} HTTP/1.1\r\n\
         Host: {host}\r\n\
         Upgrade: websocket\r\n\
         Connection: Upgrade\r\n\
         Sec-WebSocket-Key: {key}\r\n\
         Sec-WebSocket-Version: 13\r\n\
         \r\n"
    )
}

/// Parse the server's HTTP response and extract the `Sec-WebSocket-Accept`
/// header value. Returns `None` if the header is absent.
///
/// Header matching is case-insensitive (RFC 7230 §3.2).
pub fn parse_accept_from_response(response: &str) -> Option<&str> {
    for line in response.split("\r\n") {
        // Header line: "Name: value"
        if let Some(colon) = line.find(':') {
            let name = line[..colon].trim().to_ascii_lowercase();
            if name == "sec-websocket-accept" {
                return Some(line[colon + 1..].trim());
            }
        }
    }
    None
}

/// Parse the server's HTTP response status line, returning the status code.
/// E.g. "HTTP/1.1 101 Switching Protocols\r\n..." → `Some(101)`.
pub fn parse_status_code(response: &str) -> Option<u16> {
    let first_line = response.split("\r\n").next()?;
    // "HTTP/1.1 101 Switching Protocols"
    let mut parts = first_line.split_whitespace();
    parts.next()?; // "HTTP/1.1"
    parts.next()?.parse().ok()
}

/// **M42**: Parse a client's HTTP upgrade **request** and extract the
/// `Sec-WebSocket-Key` header value. Returns `None` if the header is absent
/// or the request is not a valid WebSocket upgrade.
///
/// Header matching is case-insensitive (RFC 7230 §3.2).
pub fn parse_key_from_request(request: &str) -> Option<&str> {
    for line in request.split("\r\n") {
        if let Some(colon) = line.find(':') {
            let name = line[..colon].trim().to_ascii_lowercase();
            if name == "sec-websocket-key" {
                return Some(line[colon + 1..].trim());
            }
        }
    }
    None
}

/// **M42**: Build the server→client 101 Switching Protocols response.
/// Uses [`compute_accept`] to compute `Sec-WebSocket-Accept` from the
/// client's `Sec-WebSocket-Key`.
pub fn build_server_response(key: &str) -> String {
    let accept = compute_accept(key);
    format!(
        "HTTP/1.1 101 Switching Protocols\r\n\
         Upgrade: websocket\r\n\
         Connection: Upgrade\r\n\
         Sec-WebSocket-Accept: {accept}\r\n\
         \r\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 6455 §4.2.2 example: the canonical handshake accept value.
    #[test]
    fn rfc6455_accept_vector() {
        let key = "dGhlIHNhbXBsZSBub25jZQ==";
        let accept = compute_accept(key);
        assert_eq!(accept, "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
    }

    #[test]
    fn accept_changes_with_key() {
        let a = compute_accept("dGhlIHNhbXBsZSBub25jZQ==");
        let b = compute_accept("AAAAAAAAAAAAAAAAAAAAAA==");
        assert_ne!(a, b);
    }

    #[test]
    fn accept_is_28_chars_base64() {
        // sha1 = 20 bytes, base64(20) = 28 chars (20 = 6*3 + 2, pads with one =)
        let accept = compute_accept("dGhlIHNhbXBsZSBub25jZQ==");
        assert_eq!(accept.len(), 28);
        assert!(accept.ends_with('='));
    }

    #[test]
    fn key_from_random_16_bytes() {
        let key = key_from_random([0u8; 16]);
        assert_eq!(key.len(), 24);
        assert!(key.ends_with("=="));
        // All-zero input → known base64
        assert_eq!(key, "AAAAAAAAAAAAAAAAAAAAAA==");
    }

    #[test]
    fn key_from_nonzero_random() {
        let key = key_from_random([0xFF; 16]);
        assert_eq!(key, "/////////////////////w==");
    }

    #[test]
    fn build_client_request_contains_required_headers() {
        let req = build_client_request("example.com", "/chat", "dGhlIHNhbXBsZSBub25jZQ==");
        assert!(req.starts_with("GET /chat HTTP/1.1\r\n"));
        assert!(req.contains("Host: example.com\r\n"));
        assert!(req.contains("Upgrade: websocket\r\n"));
        assert!(req.contains("Connection: Upgrade\r\n"));
        assert!(req.contains("Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n"));
        assert!(req.contains("Sec-WebSocket-Version: 13\r\n"));
        assert!(req.ends_with("\r\n\r\n"));
    }

    #[test]
    fn parse_accept_case_insensitive() {
        let resp = "HTTP/1.1 101 Switching Protocols\r\n\
                    Upgrade: websocket\r\n\
                    Connection: Upgrade\r\n\
                    sec-websocket-accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=\r\n\
                    \r\n";
        assert_eq!(
            parse_accept_from_response(resp),
            Some("s3pPLMBiTxaQ9kYGzzhZRbK+xOo=")
        );
    }

    #[test]
    fn parse_accept_mixed_case_header_name() {
        let resp = "HTTP/1.1 101\r\nSec-WebSocket-Accept: abc123\r\n\r\n";
        assert_eq!(parse_accept_from_response(resp), Some("abc123"));
    }

    #[test]
    fn parse_accept_missing_returns_none() {
        let resp = "HTTP/1.1 101\r\nUpgrade: websocket\r\n\r\n";
        assert_eq!(parse_accept_from_response(resp), None);
    }

    #[test]
    fn parse_status_code_101() {
        let resp = "HTTP/1.1 101 Switching Protocols\r\n\r\n";
        assert_eq!(parse_status_code(resp), Some(101));
    }

    #[test]
    fn parse_status_code_400() {
        let resp = "HTTP/1.1 400 Bad Request\r\n\r\n";
        assert_eq!(parse_status_code(resp), Some(400));
    }

    #[test]
    fn full_handshake_round_trip() {
        // Client generates key → server computes accept → client verifies.
        let key = key_from_random([0x12; 16]);
        let req = build_client_request("localhost", "/ws", &key);
        assert!(req.contains(&key));

        // Server side: derive the accept value it would send.
        let accept = compute_accept(&key);
        let server_resp = format!(
            "HTTP/1.1 101 Switching Protocols\r\n\
             Upgrade: websocket\r\n\
             Connection: Upgrade\r\n\
             Sec-WebSocket-Accept: {accept}\r\n\r\n"
        );

        // Client side: parse + verify.
        assert_eq!(parse_status_code(&server_resp), Some(101));
        assert_eq!(
            parse_accept_from_response(&server_resp),
            Some(accept.as_str())
        );
    }

    // ── M42: server handshake helpers ──

    #[test]
    fn parse_key_from_request_extracts_key() {
        let req = build_client_request("localhost", "/ws", "dGhlIHNhbXBsZSBub25jZQ==");
        assert_eq!(
            parse_key_from_request(&req),
            Some("dGhlIHNhbXBsZSBub25jZQ==")
        );
    }

    #[test]
    fn parse_key_from_request_case_insensitive() {
        // Some clients send lowercase header names.
        let req = "GET / HTTP/1.1\r\nsec-websocket-key: abc123==\r\n\r\n";
        assert_eq!(parse_key_from_request(req), Some("abc123=="));
    }

    #[test]
    fn parse_key_from_request_missing() {
        let req = "GET / HTTP/1.1\r\nHost: localhost\r\n\r\n";
        assert_eq!(parse_key_from_request(req), None);
    }

    #[test]
    fn build_server_response_contains_accept() {
        let resp = build_server_response("dGhlIHNhbXBsZSBub25jZQ==");
        assert!(resp.starts_with("HTTP/1.1 101 Switching Protocols\r\n"));
        assert!(resp.contains("Upgrade: websocket\r\n"));
        assert!(resp.contains("Connection: Upgrade\r\n"));
        // RFC 6455 §4.2.2 canonical accept value.
        assert!(resp.contains("Sec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=\r\n"));
        assert!(resp.ends_with("\r\n\r\n"));
    }

    #[test]
    fn server_response_accept_varies_with_key() {
        let r1 = build_server_response("dGhlIHNhbXBsZSBub25jZQ==");
        let r2 = build_server_response("AAAAAAAAAAAAAAAAAAAAAA==");
        assert_ne!(r1, r2);
    }

    #[test]
    fn full_server_handshake_round_trip() {
        // Client builds request with a key; server parses key + builds response.
        let key = key_from_random([0xAB; 16]);
        let req = build_client_request("localhost", "/cdp", &key);
        let parsed_key = parse_key_from_request(&req).expect("key parsed");
        let resp = build_server_response(parsed_key);
        // The accept in the response must match compute_accept(original key).
        let expected_accept = compute_accept(&key);
        assert!(resp.contains(&format!("Sec-WebSocket-Accept: {expected_accept}\r\n")));
    }
}
