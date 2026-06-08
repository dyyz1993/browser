//! M42.2: Hand-rolled JSON-RPC message parsing/serialization for CDP.
//!
//! **No serde** (manual-implementation-first principle). CDP messages have
//! a fixed shape, so a minimal recursive-descent JSON parser suffices.
//!
//! ## CDP message shapes
//!
//! ```json
//! // Request (client → server)
//! {"id":1,"method":"Page.navigate","params":{"url":"https://example.com"}}
//! // Response success (server → client)
//! {"id":1,"result":{}}
//! // Response error
//! {"id":1,"error":{"code":-32601,"message":"Method not found"}}
//! // Event (server → client, no id)
//! {"method":"Runtime.consoleAPICalled","params":{...}}
//! ```

use std::collections::BTreeMap;

/// A parsed JSON value (recursive). Minimal — only what CDP needs.
#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    /// f64 — CDP ids are small integers, f64 is lossless for them.
    Number(f64),
    String(String),
    Array(Vec<Json>),
    Object(BTreeMap<String, Json>),
}

impl Json {
    /// Serialize to a JSON string. Numbers that are integral print without
    /// a fractional part (CDP ids must be `1` not `1.0`).
    #[must_use]
    pub fn to_json_string(&self) -> String {
        match self {
            Json::Null => "null".to_string(),
            Json::Bool(b) => b.to_string(),
            Json::Number(n) => {
                // CDP ids are small integers; print without fractional part
                // so ids appear as `1` not `1.0`.
                if n.fract() == 0.0 && n.is_finite() && n.abs() < 1e15 {
                    format!("{}", *n as i64)
                } else {
                    format!("{n}")
                }
            }
            Json::String(s) => json_escape_string(s),
            Json::Array(items) => {
                let parts: Vec<String> = items.iter().map(|v| v.to_json_string()).collect();
                format!("[{}]", parts.join(","))
            }
            Json::Object(map) => {
                let parts: Vec<String> = map
                    .iter()
                    .map(|(k, v)| format!("{}:{}", json_escape_string(k), v.to_json_string()))
                    .collect();
                format!("{{{}}}", parts.join(","))
            }
        }
    }

    /// Get an object field, returning `None` for non-objects / missing keys.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Json> {
        if let Json::Object(map) = self {
            map.get(key)
        } else {
            None
        }
    }

    /// Get a field as a string, unwrapping nested `Json::String`.
    #[must_use]
    pub fn get_str(&self, key: &str) -> Option<&str> {
        match self.get(key) {
            Some(Json::String(s)) => Some(s.as_str()),
            _ => None,
        }
    }

    /// Get a field as an integer id (CDP ids are positive integers).
    #[must_use]
    pub fn get_id(&self, key: &str) -> Option<i64> {
        match self.get(key) {
            Some(Json::Number(n)) => Some(*n as i64),
            _ => None,
        }
    }
}

/// Quote + escape a string per JSON spec (RFC 8259 §7).
fn json_escape_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A parse error for the JSON decoder.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum CdpError {
    #[error("invalid JSON: {0}")]
    InvalidJson(String),
    #[error("invalid WebSocket frame: {0}")]
    InvalidFrame(String),
    #[error("I/O error: {0}")]
    Io(String),
    #[error("handshake failed: {0}")]
    Handshake(String),
    #[error("method not found: {0}")]
    MethodNotFound(String),
}

/// A parsed CDP request/response/event envelope.
#[derive(Debug, Clone, PartialEq)]
pub struct CdpMessage {
    /// `id` — present for requests/responses, absent for events.
    pub id: Option<i64>,
    /// `method` — e.g. "Page.navigate". Present for requests and events.
    pub method: Option<String>,
    /// `params` — opaque JSON for requests/events.
    pub params: Option<Json>,
    /// `result` — opaque JSON for success responses.
    pub result: Option<Json>,
    /// `error` — object `{code, message}` for error responses.
    pub error: Option<Json>,
    /// `sessionId` — M53: flatten session mode. Present when message is
    /// routed through a specific CDP session.
    pub session_id: Option<String>,
}

impl CdpMessage {
    /// Build a success response: `{"id":..,"result":..}`.
    #[must_use]
    pub fn ok_response(id: i64, result: Json) -> String {
        let mut map = BTreeMap::new();
        map.insert("id".to_string(), Json::Number(id as f64));
        map.insert("result".to_string(), result);
        Json::Object(map).to_json_string()
    }

    /// Build an empty-result success response: `{"id":..,"result":{}}`.
    #[must_use]
    pub fn ok_empty(id: i64) -> String {
        Self::ok_response(id, Json::Object(BTreeMap::new()))
    }

    /// Build an error response: `{"id":..,"error":{"code":..,"message":..}}`.
    #[must_use]
    pub fn error_response(id: i64, code: i64, message: &str) -> String {
        let mut err = BTreeMap::new();
        err.insert("code".to_string(), Json::Number(code as f64));
        err.insert("message".to_string(), Json::String(message.to_string()));
        let mut map = BTreeMap::new();
        map.insert("id".to_string(), Json::Number(id as f64));
        map.insert("error".to_string(), Json::Object(err));
        Json::Object(map).to_json_string()
    }

    /// Build an event: `{"method":..,"params":..}` (no id).
    #[must_use]
    pub fn event(method: &str, params: Json) -> String {
        let mut map = BTreeMap::new();
        map.insert("method".to_string(), Json::String(method.to_string()));
        map.insert("params".to_string(), params);
        Json::Object(map).to_json_string()
    }
}

/// A decoded CDP request (used by domain handlers in M44+).
#[derive(Debug, Clone, PartialEq)]
pub struct CdpRequest {
    pub id: i64,
    pub method: String,
    pub params: Option<Json>,
}

/// A decoded CDP response (for completeness; server side rarely parses these).
#[derive(Debug, Clone, PartialEq)]
pub struct CdpResponse {
    pub id: i64,
    pub result: Option<Json>,
    pub error: Option<Json>,
}

/// Parse a CDP message from a JSON text frame.
///
/// # Errors
/// Returns [`CdpError::InvalidJson`] if the text is not valid JSON or not a
/// JSON object.
pub fn parse_message(text: &str) -> Result<CdpMessage, CdpError> {
    let value = parse_json(text)?;
    let Json::Object(map) = value else {
        return Err(CdpError::InvalidJson("not an object".to_string()));
    };
    Ok(CdpMessage {
        id: map.get("id").and_then(|v| {
            if let Json::Number(n) = v {
                Some(*n as i64)
            } else {
                None
            }
        }),
        method: map.get("method").and_then(|v| {
            if let Json::String(s) = v {
                Some(s.clone())
            } else {
                None
            }
        }),
        params: map.get("params").cloned(),
        result: map.get("result").cloned(),
        error: map.get("error").cloned(),
        session_id: map.get("sessionId").and_then(|v| {
            if let Json::String(s) = v {
                Some(s.clone())
            } else {
                None
            }
        }),
    })
}

/// Parse JSON text into [`Json`]. Minimal recursive-descent parser.
///
/// # Errors
/// Returns [`CdpError::InvalidJson`] on malformed input.
pub fn parse_json(text: &str) -> Result<Json, CdpError> {
    let mut p = Parser {
        chars: text.chars().collect(),
        pos: 0,
    };
    p.skip_ws();
    let v = p.parse_value()?;
    p.skip_ws();
    if p.pos != p.chars.len() {
        return Err(CdpError::InvalidJson(format!(
            "trailing characters at pos {}",
            p.pos
        )));
    }
    Ok(v)
}

struct Parser {
    chars: Vec<char>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn next(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += 1;
        Some(c)
    }

    fn skip_ws(&mut self) {
        while let Some(c) = self.peek() {
            if c.is_whitespace() {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    fn parse_value(&mut self) -> Result<Json, CdpError> {
        self.skip_ws();
        match self.peek() {
            Some('{') => self.parse_object(),
            Some('[') => self.parse_array(),
            Some('"') => self.parse_string().map(Json::String),
            Some('t') | Some('f') => self.parse_bool(),
            Some('n') => self.parse_null(),
            Some(c) if c == '-' || c.is_ascii_digit() => self.parse_number(),
            _ => Err(CdpError::InvalidJson(format!(
                "unexpected char at pos {}",
                self.pos
            ))),
        }
    }

    fn parse_object(&mut self) -> Result<Json, CdpError> {
        self.next(); // consume '{'
        let mut map = BTreeMap::new();
        self.skip_ws();
        if self.peek() == Some('}') {
            self.next();
            return Ok(Json::Object(map));
        }
        loop {
            self.skip_ws();
            if self.peek() != Some('"') {
                return Err(CdpError::InvalidJson("expected string key".to_string()));
            }
            let key = self.parse_string()?;
            self.skip_ws();
            if self.next() != Some(':') {
                return Err(CdpError::InvalidJson("expected ':'".to_string()));
            }
            let value = self.parse_value()?;
            map.insert(key, value);
            self.skip_ws();
            match self.next() {
                Some(',') => continue,
                Some('}') => break,
                _ => return Err(CdpError::InvalidJson("expected ',' or '}'".to_string())),
            }
        }
        Ok(Json::Object(map))
    }

    fn parse_array(&mut self) -> Result<Json, CdpError> {
        self.next(); // consume '['
        let mut items = Vec::new();
        self.skip_ws();
        if self.peek() == Some(']') {
            self.next();
            return Ok(Json::Array(items));
        }
        loop {
            let value = self.parse_value()?;
            items.push(value);
            self.skip_ws();
            match self.next() {
                Some(',') => continue,
                Some(']') => break,
                _ => return Err(CdpError::InvalidJson("expected ',' or ']'".to_string())),
            }
        }
        Ok(Json::Array(items))
    }

    fn parse_string(&mut self) -> Result<String, CdpError> {
        if self.next() != Some('"') {
            return Err(CdpError::InvalidJson("expected '\"'".to_string()));
        }
        let mut s = String::new();
        loop {
            match self.next() {
                Some('"') => break,
                Some('\\') => {
                    let c = self
                        .next()
                        .ok_or_else(|| CdpError::InvalidJson("unterminated escape".to_string()))?;
                    match c {
                        '"' => s.push('"'),
                        '\\' => s.push('\\'),
                        '/' => s.push('/'),
                        'n' => s.push('\n'),
                        'r' => s.push('\r'),
                        't' => s.push('\t'),
                        'b' => s.push('\u{08}'),
                        'f' => s.push('\u{0c}'),
                        'u' => {
                            let mut code = 0u32;
                            for _ in 0..4 {
                                let h = self.next().ok_or_else(|| {
                                    CdpError::InvalidJson("incomplete unicode escape".to_string())
                                })?;
                                code = code * 16
                                    + h.to_digit(16).ok_or_else(|| {
                                        CdpError::InvalidJson("bad hex digit".to_string())
                                    })?;
                            }
                            if let Some(ch) = char::from_u32(code) {
                                s.push(ch);
                            }
                        }
                        _ => return Err(CdpError::InvalidJson(format!("bad escape \\{c}"))),
                    }
                }
                Some(c) => s.push(c),
                None => return Err(CdpError::InvalidJson("unterminated string".to_string())),
            }
        }
        Ok(s)
    }

    fn parse_number(&mut self) -> Result<Json, CdpError> {
        let start = self.pos;
        if self.peek() == Some('-') {
            self.next();
        }
        while let Some(c) = self.peek() {
            if c.is_ascii_digit() || c == '.' || c == 'e' || c == 'E' || c == '+' || c == '-' {
                self.pos += 1;
            } else {
                break;
            }
        }
        let text: String = self.chars[start..self.pos].iter().collect();
        text.parse::<f64>()
            .map(Json::Number)
            .map_err(|_| CdpError::InvalidJson(format!("bad number '{text}'")))
    }

    fn parse_bool(&mut self) -> Result<Json, CdpError> {
        if self.match_literal("true") {
            Ok(Json::Bool(true))
        } else if self.match_literal("false") {
            Ok(Json::Bool(false))
        } else {
            Err(CdpError::InvalidJson("bad literal".to_string()))
        }
    }

    fn parse_null(&mut self) -> Result<Json, CdpError> {
        if self.match_literal("null") {
            Ok(Json::Null)
        } else {
            Err(CdpError::InvalidJson("bad literal".to_string()))
        }
    }

    fn match_literal(&mut self, lit: &str) -> bool {
        let lit_chars: Vec<char> = lit.chars().collect();
        if self.pos + lit_chars.len() > self.chars.len() {
            return false;
        }
        for (i, c) in lit_chars.iter().enumerate() {
            if self.chars[self.pos + i] != *c {
                return false;
            }
        }
        self.pos += lit_chars.len();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_object() {
        let j = parse_json(r#"{"id":1,"method":"Page.navigate"}"#).unwrap();
        assert_eq!(j.get_id("id"), Some(1));
        assert_eq!(j.get_str("method"), Some("Page.navigate"));
    }

    #[test]
    fn parse_nested_params() {
        let j = parse_json(r#"{"id":1,"method":"X","params":{"url":"https://a.com"}}"#).unwrap();
        let params = j.get("params").unwrap();
        assert_eq!(params.get_str("url"), Some("https://a.com"));
    }

    #[test]
    fn parse_array_and_bool_and_null() {
        let j = parse_json(r#"{"a":[1,2,true,null],"b":false}"#).unwrap();
        let arr = j.get("a").unwrap();
        assert_eq!(arr.get_id("0"), None); // arrays use indexing not keys
        if let Json::Array(items) = arr {
            assert_eq!(items.len(), 4);
        } else {
            panic!("not array");
        }
        let b = j.get("b").unwrap();
        assert_eq!(*b, Json::Bool(false));
    }

    #[test]
    fn parse_escaped_string() {
        let j = parse_json(r#"{"s":"line\nbreak\"quote"}"#).unwrap();
        assert_eq!(j.get_str("s"), Some("line\nbreak\"quote"));
    }

    #[test]
    fn serialize_object_integral_id() {
        let mut map = BTreeMap::new();
        map.insert("id".to_string(), Json::Number(1.0));
        map.insert("result".to_string(), Json::Object(BTreeMap::new()));
        let s = Json::Object(map).to_json_string();
        assert!(s.contains("\"id\":1"), "got: {s}");
        assert!(!s.contains("1.0"), "got: {s}");
        assert!(s.contains("\"result\":{}"), "got: {s}");
    }

    #[test]
    fn message_ok_empty_format() {
        let s = CdpMessage::ok_empty(1);
        assert_eq!(s, r#"{"id":1,"result":{}}"#);
    }

    #[test]
    fn message_error_response_format() {
        let s = CdpMessage::error_response(1, -32601, "Method not found");
        assert_eq!(
            s,
            r#"{"error":{"code":-32601,"message":"Method not found"},"id":1}"#
        );
    }

    #[test]
    fn message_event_format() {
        let mut params = BTreeMap::new();
        params.insert("frameId".to_string(), Json::String("A".to_string()));
        let s = CdpMessage::event("Page.frameNavigated", Json::Object(params));
        assert_eq!(
            s,
            r#"{"method":"Page.frameNavigated","params":{"frameId":"A"}}"#
        );
    }

    #[test]
    fn parse_message_request() {
        let m = parse_message(r#"{"id":5,"method":"Page.navigate","params":{"url":"x"}}"#).unwrap();
        assert_eq!(m.id, Some(5));
        assert_eq!(m.method.as_deref(), Some("Page.navigate"));
        assert!(m.params.is_some());
        assert!(m.result.is_none());
        assert!(m.error.is_none());
    }

    #[test]
    fn parse_message_event_no_id() {
        let m = parse_message(r#"{"method":"X.event","params":{}}"#).unwrap();
        assert_eq!(m.id, None);
        assert_eq!(m.method.as_deref(), Some("X.event"));
    }

    #[test]
    fn parse_message_invalid() {
        assert!(parse_message("not json").is_err());
        assert!(parse_message("[1,2]").is_err()); // not an object
    }

    #[test]
    fn round_trip_request() {
        let text = r#"{"id":1,"method":"Page.navigate","params":{"url":"https://example.com"}}"#;
        let m = parse_message(text).unwrap();
        // Rebuild a response.
        let resp = CdpMessage::ok_empty(m.id.unwrap());
        assert_eq!(resp, r#"{"id":1,"result":{}}"#);
    }

    #[test]
    fn parse_unicode_escape() {
        let j = parse_json(r#"{"s":"\u4e2d\u6587"}"#).unwrap();
        assert_eq!(j.get_str("s"), Some("中文"));
    }

    #[test]
    fn parse_negative_number() {
        let j = parse_json(r#"{"x":-42.5}"#).unwrap();
        match j.get("x") {
            Some(Json::Number(n)) => assert!((n + 42.5).abs() < 1e-9),
            _ => panic!(),
        }
    }
}
