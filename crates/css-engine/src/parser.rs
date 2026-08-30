//! CSS parser — minimal hand-rolled tokenizer for the M2 subset.
//!
//! We deliberately don't use `cssparser` here. Its API is built for
//! full-spec compliance, which adds a lot of friction for the very
//! narrow M2 surface (tag/class/id selectors + flat declarations).
//! Hand-rolling keeps the parser small and easy to follow.

use crate::ast::{Declaration, MediaQuery, Rule, Stylesheet};

/// Parse a CSS source string into a [`Stylesheet`].
///
/// Forgiving: malformed input simply truncates the current rule or
/// declaration; the parser keeps going.
///
/// M81: `@media` blocks are parsed — the prelude becomes a [`MediaQuery`]
/// attached to every inner rule; unparseable preludes drop the whole block.
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
        if p.peek_char('@') {
            match parse_at_media(&mut p) {
                Some(media_rules) => rules.extend(media_rules),
                None => {
                    // Unknown at-rule: skip its statement (`;`-terminated)
                    // or balanced `{...}` block so the next real rule
                    // parses cleanly.
                    p.skip_at_rule();
                }
            }
        } else if let Some(rule) = parse_rule(&mut p) {
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
        media: None,
    })
}

// ---------------------------------------------------------------------------
// M81: `@media` blocks
// ---------------------------------------------------------------------------

/// Parse an at-rule starting at `@` (cursor is positioned on it).
///
/// Returns the inner rules (each tagged with the parsed [`MediaQuery`]) for
/// `@media` blocks — an empty `Vec` when the prelude is unparseable (block
/// is consumed and dropped, matching pre-M81 "silently discard" behavior).
/// Returns `None` for non-media at-rules so the caller can skip them.
fn parse_at_media(p: &mut Parser<'_>) -> Option<Vec<Rule>> {
    p.advance_one(); // consume '@'
    let keyword = p.read_ident();
    if !keyword.eq_ignore_ascii_case("media") {
        return None;
    }
    // Prelude = everything up to the opening brace.
    let condition = p.read_until_char('{').trim().to_string();
    if !p.consume_char('{') {
        return None;
    }
    // Extract the balanced `{...}` body first — even a bad prelude must not
    // leak its inner rules into the stylesheet.
    let inner = p.read_balanced_braces().to_string();
    let Some(query) = parse_media_query(&condition) else {
        return Some(Vec::new());
    };
    let mut rules = Vec::new();
    for mut r in parse(&inner).rules {
        r.media = Some(match r.media.take() {
            // Nested `@media`: combine outer and inner conditions.
            Some(inner_q) => MediaQuery::All(vec![query.clone(), inner_q]),
            None => query.clone(),
        });
        rules.push(r);
    }
    Some(rules)
}

/// Parse an `@media` prelude (e.g. `"screen"`, `"(max-width: 768px)"`,
/// `"screen and (min-width: 900px)"`) into a [`MediaQuery`].
///
/// `None` for anything outside the supported subset (`not`, `,`-lists,
/// unknown media types/features, non-px lengths).
#[must_use]
pub fn parse_media_query(condition: &str) -> Option<MediaQuery> {
    let mut parts = Vec::new();
    for part in condition.split(" and ") {
        parts.push(parse_media_term(part.trim())?);
    }
    match parts.len() {
        0 => None, // empty prelude
        1 => parts.pop(),
        _ => Some(MediaQuery::All(parts)),
    }
}

/// Parse one `and`-separated term of a media prelude.
fn parse_media_term(term: &str) -> Option<MediaQuery> {
    // `only screen` / `not screen` prefixes: `only` is transparent, `not` is
    // unsupported (fails the parse → block dropped).
    let term = term.strip_prefix("only ").unwrap_or(term).trim();
    if term.starts_with("not ") {
        return None;
    }
    match term.to_ascii_lowercase().as_str() {
        "screen" => return Some(MediaQuery::Screen),
        "print" => return Some(MediaQuery::Print),
        _ => {}
    }
    // Feature test — strip the surrounding parens: `(max-width: 768px)`.
    let inner = term
        .strip_prefix('(')
        .and_then(|s| s.strip_suffix(')'))
        .map(str::trim)?;
    let (name, value) = inner.split_once(':')?;
    let name = name.trim().to_ascii_lowercase();
    let px = parse_px_length(value.trim())?;
    match name.as_str() {
        "max-width" => Some(MediaQuery::MaxWidth(px)),
        "min-width" => Some(MediaQuery::MinWidth(px)),
        _ => None,
    }
}

/// Parse a length that is either unitless or in `px` (e.g. `"768"`,
/// `"768px"`, `"768.5px"`) into whole pixels. Other units fail the parse.
fn parse_px_length(value: &str) -> Option<u32> {
    let value = value.strip_suffix("px").unwrap_or(value).trim();
    let n: f32 = value.parse().ok()?;
    if n.is_finite() && n >= 0.0 {
        Some(n as u32)
    } else {
        None
    }
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

    /// M81: from the current position (just past an opening `{`), scan to the
    /// *matching* closing `}` and return the inner source. The closing brace
    /// is consumed. At EOF inside the block the remainder is returned.
    ///
    /// Brace depth counting only (no string/comment awareness — consistent
    /// with the rest of this minimal parser).
    fn read_balanced_braces(&mut self) -> &'a str {
        let start = self.pos;
        let mut depth = 1usize;
        while let Some(b) = self.peek() {
            match b {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        let inner = &self.src[start..self.pos];
                        self.pos += 1; // consume the closing brace
                        return inner;
                    }
                }
                _ => {}
            }
            self.pos += 1;
        }
        &self.src[start..]
    }

    /// Skip an unknown at-rule: advance to and past its terminating `;`
    /// (statement form, e.g. `@import url(x);`) or consume its balanced
    /// `{...}` block (block form, e.g. `@font-face { ... }`). At EOF, stops.
    fn skip_at_rule(&mut self) {
        while let Some(b) = self.peek() {
            match b {
                b';' => {
                    self.pos += 1;
                    return;
                }
                b'{' => {
                    let _ = self.read_balanced_braces();
                    return;
                }
                _ => self.pos += 1,
            }
        }
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

    // ---- M81: CSS 变量（custom properties）----

    /// `--xxx: value` 自定义属性必须保留（property 原样、value 原样），
    /// 不能被当非法属性跳过。
    #[test]
    fn parse_keeps_custom_property_declarations() {
        let sheet = parse(":root { --main-color: #ff0000; --gap: 10px; color: red; }");
        assert_eq!(sheet.rules.len(), 1);
        let decls = &sheet.rules[0].declarations;
        assert_eq!(decls.len(), 3);
        assert_eq!(decls[0].property, "--main-color");
        assert_eq!(decls[0].value, "#ff0000");
        assert_eq!(decls[1].property, "--gap");
        assert_eq!(decls[1].value, "10px");
        assert_eq!(decls[2].property, "color");
    }

    /// 消费侧：`var(--x)` 出现在普通声明 value 中时保留原样（展开在
    /// computed.rs 的变量解析 pass 做，这里只锁定 parser 不破坏 token）。
    #[test]
    fn parse_keeps_var_function_in_value() {
        let sheet = parse("body { color: var(--main-color); }");
        assert_eq!(sheet.rules[0].declarations[0].value, "var(--main-color)");
    }

    /// inline style 声明列表同样保留 `--` 自定义属性。
    #[test]
    fn parse_declaration_list_keeps_custom_properties() {
        let decls = parse_declaration_list("--x: 9px; margin: var(--x)");
        assert_eq!(decls.len(), 2);
        assert_eq!(decls[0].property, "--x");
        assert_eq!(decls[0].value, "9px");
    }

    // ---- M81: @media 解析 ----

    #[test]
    fn parse_media_screen_block() {
        let sheet = parse("@media screen { .desktop { color: red; } }");
        assert_eq!(sheet.rules.len(), 1);
        assert_eq!(sheet.rules[0].selectors, ".desktop");
        assert_eq!(sheet.rules[0].media, Some(MediaQuery::Screen));
        assert_eq!(sheet.rules[0].declarations[0].property, "color");
        assert_eq!(sheet.rules[0].declarations[0].value, "red");
    }

    #[test]
    fn parse_media_print_block() {
        let sheet = parse("@media PRINT { .desktop { color: blue; } }");
        assert_eq!(sheet.rules.len(), 1);
        assert_eq!(sheet.rules[0].media, Some(MediaQuery::Print));
    }

    #[test]
    fn parse_media_width_breakpoints() {
        let sheet = parse(
            "@media (max-width: 1200px) { .a { margin: 8px; } } \
             @media (min-width: 900px) { .b { margin: 4px; } }",
        );
        assert_eq!(sheet.rules.len(), 2);
        assert_eq!(sheet.rules[0].media, Some(MediaQuery::MaxWidth(1200)));
        assert_eq!(sheet.rules[1].media, Some(MediaQuery::MinWidth(900)));
    }

    /// `and` 组合：`screen and (min-width: 900px)` → All([Screen, MinWidth])。
    #[test]
    fn parse_media_and_combination() {
        let sheet = parse("@media screen and (min-width: 900px) { .x { color: red; } }");
        assert_eq!(sheet.rules.len(), 1);
        assert_eq!(
            sheet.rules[0].media,
            Some(MediaQuery::All(vec![
                MediaQuery::Screen,
                MediaQuery::MinWidth(900)
            ]))
        );
    }

    /// 一个 @media 块内多条规则共享同一条件。
    #[test]
    fn parse_media_block_with_multiple_rules() {
        let sheet = parse("@media screen { .a { color: red; } .b { color: blue; } }");
        assert_eq!(sheet.rules.len(), 2);
        assert_eq!(sheet.rules[0].media, Some(MediaQuery::Screen));
        assert_eq!(sheet.rules[1].media, Some(MediaQuery::Screen));
    }

    /// 无条件规则不受影响（media = None），且 media 块之后的规则继续正常解析。
    #[test]
    fn parse_unconditional_rules_around_media_blocks() {
        let sheet = parse(
            "p { margin: 0; } \
             @media print { .a { color: blue; } } \
             h1 { font-size: 20px; }",
        );
        assert_eq!(sheet.rules.len(), 3);
        assert_eq!(sheet.rules[0].media, None);
        assert_eq!(sheet.rules[0].selectors, "p");
        assert_eq!(sheet.rules[1].media, Some(MediaQuery::Print));
        assert_eq!(sheet.rules[2].media, None);
        assert_eq!(sheet.rules[2].selectors, "h1");
        assert_eq!(sheet.rules[2].declarations[0].value, "20px");
    }

    /// 不支持的条件（print 专有 not / 逗号 or 列表 / 非宽度 feature）→
    /// 整块丢弃，内部规则不得泄漏进 stylesheet。
    #[test]
    fn parse_media_unsupported_condition_dropped() {
        for css in [
            "@media not print { .a { color: red; } }",
            "@media screen, print { .a { color: red; } }",
            "@media (orientation: landscape) { .a { color: red; } }",
            "@media { .a { color: red; } }",
        ] {
            let sheet = parse(css);
            assert!(sheet.rules.is_empty(), "should drop: {css}");
        }
    }

    /// 嵌套大括号（内层还有 @media）正确配平，外层条件与内层 AND 合并。
    #[test]
    fn parse_media_nested_blocks() {
        let sheet = parse("@media screen { @media (min-width: 600px) { .x { color: red; } } }");
        assert_eq!(sheet.rules.len(), 1);
        assert_eq!(
            sheet.rules[0].media,
            Some(MediaQuery::All(vec![
                MediaQuery::Screen,
                MediaQuery::MinWidth(600)
            ]))
        );
    }

    /// `@media` 块在 EOF 前未闭合 → 仍解析到内部规则（宽容语义）。
    #[test]
    fn parse_media_unclosed_block_tolerated() {
        let sheet = parse("@media screen { .x { color: red; }");
        assert_eq!(sheet.rules.len(), 1);
        assert_eq!(sheet.rules[0].media, Some(MediaQuery::Screen));
    }

    /// parse_media_query 直接单测：only 前缀透明、单位/负值处理。
    #[test]
    fn parse_media_query_terms() {
        assert_eq!(parse_media_query("only screen"), Some(MediaQuery::Screen));
        assert_eq!(
            parse_media_query("(max-width: 767.5px)"),
            Some(MediaQuery::MaxWidth(767))
        );
        assert_eq!(
            parse_media_query("(min-width: 48em)"),
            None,
            "non-px units unsupported"
        );
        assert_eq!(parse_media_query("not screen"), None);
    }

    /// 非 media 的 at-rule（如 @import / @charset）返回 None，外层跳过继续。
    #[test]
    fn parse_other_at_rules_skipped() {
        // 之后的第一条普通规则仍可解析。
        let sheet = parse("@charset \"utf-8\"; h1 { color: red; }");
        assert_eq!(sheet.rules.len(), 1);
        assert_eq!(sheet.rules[0].selectors, "h1");
        assert_eq!(sheet.rules[0].media, None);
    }
}
