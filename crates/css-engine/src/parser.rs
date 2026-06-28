//! CSS parser — minimal hand-rolled tokenizer for the M2 subset.
//!
//! We deliberately don't use `cssparser` here. Its API is built for
//! full-spec compliance, which adds a lot of friction for the very
//! narrow M2 surface (tag/class/id selectors + flat declarations).
//! Hand-rolling keeps the parser small and easy to follow.

use crate::ast::{Declaration, Rule, Stylesheet};

/// Parse a CSS source string into a [`Stylesheet`].
///
/// Forgiving: malformed input simply truncates the current rule or
/// declaration; the parser keeps going.
///
/// # Example
/// ```
/// use browser_css_engine::parse;
///
/// let sheet = parse("h1 { color: red; }");
/// assert_eq!(sheet.rules.len(), 1);
/// ```
#[must_use]
pub fn parse(css: &str) -> Stylesheet {
    let mut p = Parser::new(css);
    let mut rules = Vec::new();
    while !p.is_eof() {
        p.skip_whitespace_and_comments();
        if p.is_eof() {
            break;
        }
        if let Some(rule) = parse_rule(&mut p) {
            rules.push(rule);
        } else {
            // Defensive: ensure forward progress.
            p.advance_one();
        }
    }
    Stylesheet { rules }
}

fn parse_rule(p: &mut Parser<'_>) -> Option<Rule> {
    // Read selectors up to `{`.
    let selectors = p.read_until_char('{');
    let selectors = selectors.trim().to_string();
    if !p.consume_char('{') {
        return None;
    }
    // Parse declarations until `}` or EOF.
    let mut declarations = Vec::new();
    while !p.is_eof() && !p.peek_char('}') {
        p.skip_whitespace_and_comments();
        if p.peek_char('}') {
            break;
        }
        if let Some(decl) = parse_declaration(p) {
            declarations.push(decl);
        } else {
            // Skip to next `;` or `}` so we don't get stuck.
            while !p.is_eof() && !p.peek_char(';') && !p.peek_char('}') {
                p.advance_one();
            }
            if p.peek_char(';') {
                p.advance_one();
            }
        }
    }
    let _ = p.consume_char('}');
    Some(Rule {
        selectors,
        declarations,
    })
}

fn parse_declaration(p: &mut Parser<'_>) -> Option<Declaration> {
    let property = p.read_ident().trim().to_string();
    if property.is_empty() {
        return None;
    }
    if !p.consume_char(':') {
        return None;
    }
    // Value = everything up to `;` or `}`, then strip `!important`.
    let raw_value = p.read_until_char_any(&[';', '}']);
    let (value, important) = strip_important(raw_value.trim());
    // Consume the trailing `;` if present (but NOT the `}`).
    p.consume_char(';');
    if value.is_empty() {
        return None;
    }
    Some(Declaration {
        property,
        value: value.to_string(),
        important,
    })
}

fn strip_important(value: &str) -> (&str, bool) {
    let lower = value.to_ascii_lowercase();
    if let Some(idx) = lower.rfind("!important") {
        let head = value[..idx].trim();
        (head, true)
    } else {
        (value.trim(), false)
    }
}

/// M70.1: Parse an inline `style="..."` attribute value into declarations.
///
/// The value is a `;`-separated list of `property: value` pairs (no selector,
/// no braces). This is the same `parse_declaration` logic used inside rules,
/// exposed so `compute_styles` can fold inline styles into the computed map.
///
/// # Example
/// ```
/// use browser_css_engine::parse_declaration_list;
///
/// let decls = parse_declaration_list("display: grid; color: red");
/// assert_eq!(decls.len(), 2);
/// assert_eq!(decls[0].property, "display");
/// assert_eq!(decls[0].value, "grid");
/// ```
#[must_use]
pub fn parse_declaration_list(style: &str) -> Vec<Declaration> {
    let mut p = Parser::new(style);
    let mut declarations = Vec::new();
    while !p.is_eof() {
        p.skip_whitespace_and_comments();
        if p.is_eof() {
            break;
        }
        if let Some(decl) = parse_declaration(&mut p) {
            declarations.push(decl);
        } else {
            // Skip to next `;` or EOF so we don't get stuck.
            while !p.is_eof() && !p.peek_char(';') {
                p.advance_one();
            }
            if p.peek_char(';') {
                p.advance_one();
            }
        }
    }
    declarations
}

// ---------------------------------------------------------------------------
// Tiny cursor helper
// ---------------------------------------------------------------------------

struct Parser<'a> {
    src: &'a str,
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(src: &'a str) -> Self {
        Self {
            src,
            bytes: src.as_bytes(),
            pos: 0,
        }
    }

    fn is_eof(&self) -> bool {
        self.pos >= self.bytes.len()
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn peek_char(&self, c: char) -> bool {
        self.peek() == Some(c as u8)
    }

    fn advance_one(&mut self) {
        if !self.is_eof() {
            self.pos += 1;
        }
    }

    fn consume_char(&mut self, c: char) -> bool {
        if self.peek_char(c) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn skip_whitespace_and_comments(&mut self) {
        loop {
            // Whitespace.
            while self
                .peek()
                .is_some_and(|b| b == b' ' || b == b'\t' || b == b'\n' || b == b'\r')
            {
                self.pos += 1;
            }
            // `/* ... */` comment.
            if self.bytes.get(self.pos..self.pos + 2) == Some(b"/*") {
                let start = self.pos + 2;
                if let Some(end_rel) = self.src[start..].find("*/") {
                    self.pos = start + end_rel + 2;
                    continue; // re-scan whitespace after comment
                } else {
                    // Unterminated comment — eat the rest.
                    self.pos = self.bytes.len();
                    return;
                }
            }
            break;
        }
    }

    /// Read characters until we see one of `stops`. The stopper is NOT
    /// consumed.
    fn read_until_char_any(&mut self, stops: &[char]) -> String {
        let start = self.pos;
        while let Some(b) = self.peek() {
            if stops.iter().any(|c| *c as u8 == b) {
                break;
            }
            self.pos += 1;
        }
        self.src[start..self.pos].to_string()
    }

    fn read_until_char(&mut self, stop: char) -> String {
        self.read_until_char_any(&[stop])
    }

    /// Read a CSS identifier: `[a-zA-Z_-][a-zA-Z0-9_-]*`.
    fn read_ident(&mut self) -> String {
        let start = self.pos;
        while let Some(b) = self.peek() {
            if b.is_ascii_alphanumeric() || b == b'-' || b == b'_' {
                self.pos += 1;
            } else {
                break;
            }
        }
        self.src[start..self.pos].to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_important_basic() {
        assert_eq!(strip_important("red"), ("red", false));
        assert_eq!(strip_important("red !important"), ("red", true));
        assert_eq!(strip_important("red !IMPORTANT"), ("red", true));
        assert_eq!(
            strip_important("  padding:0 !important  "),
            ("padding:0", true)
        );
    }

    #[test]
    fn parse_handles_comments() {
        let css = "/* header */ h1 { color: red; }";
        let sheet = parse(css);
        assert_eq!(sheet.rules.len(), 1);
        assert_eq!(sheet.rules[0].selectors, "h1");
    }
}
