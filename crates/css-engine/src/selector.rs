//! Selector parsing + matching for the M2 subset.
//!
//! Supported selector forms:
//! - `*`                  universal
//! - `tag`                type (e.g. `p`, `h1`)
//! - `.cls`               class
//! - `#id`                identifier
//! - `tag.cls#id`         compound (any combination)
//! - `A B`                descendant (whitespace combinator)
//! - `A + B`              adjacent sibling combinator
//! - `A, B`               selector list
//!
//! Explicitly out of scope:
//! pseudo-classes beyond the M78 subset, `>`, `~`.

use std::fmt;

use browser_dom::{NodeData, NodeId, Tree};

/// A selector list (the right-hand side of a CSS rule).
#[derive(Debug, Clone)]
pub struct Selector {
    /// The original source string, for debugging and Display.
    pub source: String,
    /// Comma-separated list of compound+combinator chains.
    pub selectors: Vec<SelectorChain>,
}

/// A chain of compound selectors connected by combinators,
/// e.g. `body p.title + a.link`.
#[derive(Debug, Clone)]
pub struct SelectorChain {
    pub parts: Vec<CompoundSelector>,
    /// `combinators[i]` 描述 `parts[i]` 与 `parts[i+1]` 之间的关系，
    /// 长度恒为 `parts.len() - 1`（单 compound 的链为空）。
    pub combinators: Vec<Combinator>,
}

/// 链上相邻两个复合选择器之间的组合器。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Combinator {
    /// `A B`（空白）—— B 是 A 的后代（任意深度）。
    Descendant,
    /// `A + B` —— B 是 A 紧邻的后一个元素兄弟（中间无其他元素节点，
    /// 文本等非元素节点不破坏相邻性，CSS Selectors L4 §13.2）。
    Adjacent,
    /// `A ~ B` —— B 是 A 之后的任意元素兄弟。
    LaterSibling,
    /// `A > B` —— B 是 A 的直接子元素。
    Child,
}

/// A single compound selector with no whitespace, e.g. `p.title#main`.
#[derive(Debug, Clone, Default)]
pub struct CompoundSelector {
    pub tag: Option<String>, // None = universal / not specified
    pub classes: Vec<String>,
    pub id: Option<String>,
    /// M78: pseudo-classes（:lang/:dir/:nth-child 子集）。
    pub pseudos: Vec<Pseudo>,
    /// M78: attribute selectors（[lang]、[lang="es"]、[lang|="es"]）。
    pub attrs: Vec<AttrSelector>,
}

/// M78: 属性选择器（CSS Selectors L3 子集）。
#[derive(Debug, Clone, PartialEq)]
pub enum AttrSelector {
    /// `[attr]` — 属性存在。
    Exists(String),
    /// `[attr="value"]` — 精确相等（属性名大小写不敏感，值区分大小写）。
    Equals(String, String),
    /// `[attr|="value"]` — dash-match：等于 value 或以 `value-` 开头（lang 经典用法）。
    DashMatch(String, String),
}

/// M78: 支持的伪类子集（CSS Selectors L4 里爬虫/测试最高频的三个）。
#[derive(Debug, Clone)]
pub enum Pseudo {
    /// `:lang(en, fr-*)` — BCP47 语言范围列表（RFC4647 basic filtering）。
    Lang(Vec<String>),
    /// `:dir(ltr|rtl)` — 方向（从最近带 dir 属性的祖先继承）。
    Dir(String),
    /// `:nth-child(n)` — 仅整数形式。
    NthChild(u32),
    /// `:not(compound)` — 复合选择器取反。
    Not(Box<CompoundSelector>),
}

impl fmt::Display for CompoundSelector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.tag {
            Some(t) => write!(f, "{t}")?,
            None => write!(f, "*")?,
        }
        for c in &self.classes {
            write!(f, ".{c}")?;
        }
        if let Some(id) = &self.id {
            write!(f, "#{id}")?;
        }
        for p in &self.pseudos {
            match p {
                Pseudo::Lang(ranges) => {
                    let joined: Vec<String> = ranges.iter().map(|r| format!("\"{r}\"")).collect();
                    write!(f, ":lang({})", joined.join(", "))?;
                }
                Pseudo::Dir(d) => write!(f, ":dir({d})")?,
                Pseudo::NthChild(n) => write!(f, ":nth-child({n})")?,
                Pseudo::Not(inner) => write!(f, ":not({inner})")?,
            }
        }
        for a in &self.attrs {
            match a {
                AttrSelector::Exists(name) => write!(f, "[{name}]")?,
                AttrSelector::Equals(name, value) => write!(f, "[{name}=\"{value}\"]")?,
                AttrSelector::DashMatch(name, value) => write!(f, "[{name}|=\"{value}\"]")?,
            }
        }
        Ok(())
    }
}

impl Selector {
    /// Parse a selector list like `"h1, .title, body p"`.
    ///
    /// # Errors
    /// Returns an error string for malformed input.
    pub fn parse(input: &str) -> Result<Self, String> {
        let mut selectors = Vec::new();
        for chunk in input.split(',') {
            let chain = parse_chain(chunk.trim())?;
            selectors.push(chain);
        }
        Ok(Self {
            source: input.to_string(),
            selectors,
        })
    }

    /// Whether this selector list matches the element at `element_id`
    /// in `tree`. `element_id` must point to an Element node.
    #[must_use]
    pub fn matches(&self, tree: &Tree, element_id: NodeId) -> bool {
        if !matches!(tree.data(element_id), NodeData::Element { .. }) {
            return false;
        }
        self.selectors
            .iter()
            .any(|chain| chain_matches(tree, element_id, chain))
    }
}

fn parse_chain(input: &str) -> Result<SelectorChain, String> {
    // 顶层（depth 0）扫描：空白是 Descendant 组合器，'+' 是 Adjacent（两侧空格可省略）。
    // 方括号/圆括号内的空白与 '+' 属于 compound 自身（如 `[title="a + b"]`、`:lang(en, fr)`）。
    let mut parts: Vec<CompoundSelector> = Vec::new();
    let mut combinators: Vec<Combinator> = Vec::new();
    let mut chunk = String::new();
    let mut depth: usize = 0;
    // 已见到但尚未落到 parts 之间的组合器；空白先预设 Descendant，
    // 其后出现 '+' 则升级为 Adjacent（`div + p` / `div+p` 等价）。
    let mut pending: Option<Combinator> = None;

    for c in input.chars() {
        if depth == 0 && c.is_whitespace() {
            if !chunk.is_empty() {
                parts.push(parse_compound(&chunk)?);
                chunk.clear();
                pending = Some(Combinator::Descendant);
            }
            continue;
        }
        if depth == 0 && c == '+' {
            if !chunk.is_empty() {
                parts.push(parse_compound(&chunk)?);
                chunk.clear();
            }
            if parts.is_empty() {
                return Err(format!("selector chain starts with '+' in {input:?}"));
            }
            if pending == Some(Combinator::Adjacent) {
                return Err(format!("doubled '+' combinator in {input:?}"));
            }
            pending = Some(Combinator::Adjacent);
            continue;
        }
        if depth == 0 && c == '>' {
            if !chunk.is_empty() {
                parts.push(parse_compound(&chunk)?);
                chunk.clear();
            }
            if parts.is_empty() {
                return Err(format!("selector chain starts with '>' in {input:?}"));
            }
            pending = Some(Combinator::Child);
            continue;
        }
        if depth == 0 && c == '~' {
            if !chunk.is_empty() {
                parts.push(parse_compound(&chunk)?);
                chunk.clear();
            }
            if parts.is_empty() {
                return Err(format!("selector chain starts with '~' in {input:?}"));
            }
            pending = Some(Combinator::LaterSibling);
            continue;
        }
        if matches!(c, '[' | '(') {
            depth += 1;
        } else if matches!(c, ']' | ')') {
            depth = depth.saturating_sub(1);
        }
        // 新 compound 的第一个字符：落定它与上一个 part 之间的组合器。
        if chunk.is_empty() {
            if let Some(comb) = pending.take() {
                combinators.push(comb);
            }
        }
        chunk.push(c);
    }
    if !chunk.is_empty() {
        parts.push(parse_compound(&chunk)?);
    }
    if parts.is_empty() {
        return Err(format!("empty selector chain: {input:?}"));
    }
    // 末尾悬空的 '+'（后面没有 compound）非法；悬空的空白无所谓。
    if pending == Some(Combinator::Adjacent) {
        return Err(format!("trailing '+' combinator in {input:?}"));
    }
    if pending == Some(Combinator::LaterSibling) {
        return Err(format!("trailing '~' combinator in {input:?}"));
    }
    if pending == Some(Combinator::Child) {
        return Err(format!("trailing '>' combinator in {input:?}"));
    }
    debug_assert_eq!(combinators.len(), parts.len() - 1);
    Ok(SelectorChain { parts, combinators })
}

fn parse_compound(input: &str) -> Result<CompoundSelector, String> {
    let mut out = CompoundSelector::default();
    let mut chars = input.chars().peekable();
    // Optional tag at the start.
    let mut tag = String::new();
    while let Some(&c) = chars.peek() {
        if c == '.' || c == '#' || c == ':' || c == '[' {
            break;
        }
        tag.push(c);
        chars.next();
    }
    if !tag.is_empty() && tag != "*" {
        out.tag = Some(tag);
    }
    // Class / id parts.
    while let Some(&c) = chars.peek() {
        match c {
            '.' => {
                chars.next();
                let mut name = String::new();
                while let Some(&c) = chars.peek() {
                    if c == '.' || c == '#' || c == ':' || c == '[' {
                        break;
                    }
                    name.push(c);
                    chars.next();
                }
                if name.is_empty() {
                    return Err(format!("empty class name in {input:?}"));
                }
                out.classes.push(name);
            }
            '#' => {
                chars.next();
                let mut name = String::new();
                while let Some(&c) = chars.peek() {
                    if c == '.' || c == '#' || c == ':' || c == '[' {
                        break;
                    }
                    name.push(c);
                    chars.next();
                }
                if name.is_empty() {
                    return Err(format!("empty id in {input:?}"));
                }
                out.id = Some(name);
            }
            ':' => {
                chars.next();
                let pseudo = parse_pseudo(&mut chars, input)?;
                out.pseudos.push(pseudo);
            }
            '[' => {
                chars.next();
                let attr = parse_attr_selector(&mut chars, input)?;
                out.attrs.push(attr);
            }
            _ => {
                return Err(format!(
                    "unexpected char {c:?} in compound selector {input:?}"
                ));
            }
        }
    }
    Ok(out)
}

/// M78: 解析属性选择器 `[attr]` / `[attr="v"]` / `[attr|="v"]`（读入时已消费 '['）。
fn parse_attr_selector(
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
    input: &str,
) -> Result<AttrSelector, String> {
    while matches!(chars.peek(), Some(c) if c.is_whitespace()) {
        chars.next();
    }
    let mut name = String::new();
    while let Some(&c) = chars.peek() {
        if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
            name.push(c);
            chars.next();
        } else {
            break;
        }
    }
    if name.is_empty() {
        return Err(format!("empty attribute name in {input:?}"));
    }
    while matches!(chars.peek(), Some(c) if c.is_whitespace()) {
        chars.next();
    }
    // 无操作符 → [attr]
    if matches!(chars.peek(), Some(']')) {
        chars.next();
        return Ok(AttrSelector::Exists(name));
    }
    // 操作符：= 或 |=
    let dash_match = if matches!(chars.peek(), Some('|')) {
        chars.next();
        true
    } else {
        false
    };
    if !matches!(chars.peek(), Some('=')) {
        return Err(format!("unsupported attribute operator in {input:?}"));
    }
    chars.next();
    while matches!(chars.peek(), Some(c) if c.is_whitespace()) {
        chars.next();
    }
    let mut value = String::new();
    match chars.peek() {
        Some(&q) if q == '"' || q == '\'' => {
            chars.next();
            while let Some(&c) = chars.peek() {
                if c == q {
                    chars.next();
                    break;
                }
                value.push(c);
                chars.next();
            }
        }
        _ => {
            while let Some(&c) = chars.peek() {
                if c == ']' || c.is_whitespace() {
                    break;
                }
                value.push(c);
                chars.next();
            }
        }
    }
    while matches!(chars.peek(), Some(c) if c.is_whitespace()) {
        chars.next();
    }
    if !matches!(chars.peek(), Some(']')) {
        return Err(format!("unterminated attribute selector in {input:?}"));
    }
    chars.next();
    Ok(if dash_match {
        AttrSelector::DashMatch(name, value)
    } else {
        AttrSelector::Equals(name, value)
    })
}

/// M78: 解析伪类 `:lang(...)` / `:dir(...)` / `:nth-child(n)`。
/// 其他伪类（:hover 等）返回 Err —— 与 M2 以来"未知符号即整条规则作废"一致。
fn parse_pseudo(
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
    input: &str,
) -> Result<Pseudo, String> {
    let mut name = String::new();
    while let Some(&c) = chars.peek() {
        if c.is_ascii_alphanumeric() || c == '-' {
            name.push(c);
            chars.next();
        } else {
            break;
        }
    }
    let has_paren = matches!(chars.peek(), Some('('));
    if !has_paren {
        return Err(format!(
            "pseudo-class :{name} requires arguments in {input:?}"
        ));
    }
    chars.next(); // consume '('
    let mut args = String::new();
    let mut depth = 1;
    while let Some(&c) = chars.peek() {
        if c == '(' {
            depth += 1;
        } else if c == ')' {
            depth -= 1;
            if depth == 0 {
                chars.next();
                break;
            }
        }
        args.push(c);
        chars.next();
    }
    if depth != 0 {
        return Err(format!("unbalanced parentheses in pseudo-class {input:?}"));
    }
    match name.as_str() {
        "lang" => {
            let mut ranges = Vec::new();
            for part in args.split(',') {
                let p = part.trim().trim_matches(|c| c == '"' || c == '\'');
                if p.is_empty() {
                    return Err(format!("empty :lang() argument in {input:?}"));
                }
                ranges.push(p.to_string());
            }
            Ok(Pseudo::Lang(ranges))
        }
        "dir" => {
            let d = args.trim();
            if d.is_empty() || d.contains(',') || d.contains('"') || d.contains('\'') {
                return Err(format!(
                    "':dir' requires exactly one ident (ltr|rtl) in {input:?}"
                ));
            }
            Ok(Pseudo::Dir(d.to_string()))
        }
        "not" => {
            let inner = parse_compound(args.trim())?;
            Ok(Pseudo::Not(Box::new(inner)))
        }
        "nth-child" => {
            let n: u32 = args.trim().parse().map_err(|_| {
                format!(
                    "unsupported :nth-child argument {:?} in {input:?}",
                    args.trim()
                )
            })?;
            if n == 0 {
                return Err(format!("':nth-child' is 1-based in {input:?}"));
            }
            Ok(Pseudo::NthChild(n))
        }
        _ => Err(format!("unsupported pseudo-class :{name} in {input:?}")),
    }
}

/// RFC 4647 range 匹配：`en` 匹配 en / en-US（subtag 边界必须是 '-'）；
/// `en-*` 匹配 en 及 en-任意 subtag；`*` 匹配任何非空 tag。
/// `enm` 是不同的主标签，`en`/`en-*` 都不匹配它。
fn lang_range_matches(range: &str, tag: &str) -> bool {
    let r = range.trim().to_lowercase();
    let t = tag.trim().to_lowercase();
    if t.is_empty() {
        return false;
    }
    let prefix = r.strip_suffix('*').unwrap_or(&r);
    let prefix = prefix.strip_suffix('-').unwrap_or(prefix);
    if prefix.is_empty() {
        return true; // 纯 "*" 通配
    }
    t.starts_with(prefix) && (t.len() == prefix.len() || t.as_bytes()[prefix.len()] == b'-')
}

/// 元素的语言：从自身向上找最近的 lang / xml:lang 属性（HTML 语义）。
fn element_lang(tree: &Tree, id: NodeId) -> Option<String> {
    // HTML 文档语义：:lang 只看 lang 属性（xml:lang 不参与——WPT 有专项测试）。
    let mut cur = Some(id);
    while let Some(nid) = cur {
        if let NodeData::Element { attrs, .. } = tree.data(nid) {
            if let Some((_, v)) = attrs.iter().find(|(k, _)| k.eq_ignore_ascii_case("lang")) {
                return Some(v.clone());
            }
        }
        cur = tree.get(nid).parent;
    }
    None
}

/// M78.78: dir="auto" 的内容方向探测——首个强方向字符
/// （简化 bidi：ASCII 字母→ltr，阿拉伯/希伯来→rtl，其他→ltr）。
fn dir_auto_content_direction(tree: &Tree, id: NodeId) -> String {
    let text = collect_text_for_dir(tree, id);
    for ch in text.chars() {
        let c = ch as u32;
        // 阿拉伯文 U+0600-U+06FF / 希伯来 U+0590-U+05FF → rtl
        if (0x0600..=0x06FF).contains(&c) || (0x0590..=0x05FF).contains(&c) {
            return "rtl".to_string();
        }
        // ASCII 字母 → ltr
        if ch.is_ascii_alphabetic() {
            return "ltr".to_string();
        }
    }
    "ltr".to_string()
}

fn collect_text_for_dir(tree: &Tree, id: NodeId) -> String {
    let mut out = String::new();
    collect_text_dir_inner(tree, id, &mut out);
    out
}

fn collect_text_dir_inner(tree: &Tree, id: NodeId, out: &mut String) {
    for &child in tree.children_of(id) {
        match tree.data(child) {
            NodeData::Text(s) => out.push_str(s),
            NodeData::Element { .. } => collect_text_dir_inner(tree, child, out),
            _ => {}
        }
    }
}

/// 元素的方向：从自身向上找最近的 dir="ltr|rtl"（auto 不参与匹配，近似）。
/// HTML 规范：无任何 dir 祖先时默认方向性为 ltr。
fn element_dir(tree: &Tree, id: NodeId) -> Option<String> {
    let mut cur = Some(id);
    while let Some(nid) = cur {
        if let NodeData::Element { attrs, .. } = tree.data(nid) {
            if let Some((_, v)) = attrs.iter().find(|(k, _)| k.eq_ignore_ascii_case("dir")) {
                let d = v.trim().to_lowercase();
                if d == "ltr" || d == "rtl" {
                    return Some(d);
                }
                if d == "auto" {
                    // M78.78: dir="auto" 探测内容方向。
                    return Some(dir_auto_content_direction(tree, nid));
                }
            }
        }
        cur = tree.get(nid).parent;
    }
    Some("ltr".to_string())
}

/// 元素在父节点的元素子节点中的 1-based 位置。
fn element_child_index(tree: &Tree, id: NodeId) -> u32 {
    let parent = tree.get(id).parent;
    let Some(pid) = parent else {
        return 1;
    };
    let mut idx = 0;
    for &child in tree.children_of(pid) {
        if matches!(tree.data(child), NodeData::Element { .. }) {
            idx += 1;
            if child == id {
                return idx;
            }
        }
    }
    1
}

fn compound_matches(tree: &Tree, id: NodeId, sel: &CompoundSelector) -> bool {
    let data = tree.data(id);
    let (tag, attrs): (&str, &[(String, String)]) = match data {
        NodeData::Element { tag, attrs } => (tag.as_str(), attrs.as_slice()),
        _ => return false,
    };
    if let Some(s) = &sel.tag {
        if s != tag {
            return false;
        }
    }
    if let Some(want_id) = &sel.id {
        let has = attrs.iter().find(|(k, _)| k == "id");
        if !matches!(has, Some((_, v)) if v == want_id) {
            return false;
        }
    }
    if !sel.classes.is_empty() {
        let class_attr = attrs
            .iter()
            .find(|(k, _)| k == "class")
            .map(|(_, v)| v.as_str())
            .unwrap_or("");
        let present: std::collections::HashSet<&str> = class_attr.split_whitespace().collect();
        for c in &sel.classes {
            if !present.contains(c.as_str()) {
                return false;
            }
        }
    }
    for a in &sel.attrs {
        let name = match a {
            AttrSelector::Exists(n)
            | AttrSelector::Equals(n, _)
            | AttrSelector::DashMatch(n, _) => n.as_str(),
        };
        let found = attrs.iter().find(|(k, _)| k.eq_ignore_ascii_case(name));
        // HTML 的 lang / xml:lang 属性值比较大小写不敏感（CSS Selectors 4 §4.2）。
        let ci = name.eq_ignore_ascii_case("lang") || name.eq_ignore_ascii_case("xml:lang");
        let ok = match (a, found) {
            (AttrSelector::Exists(_), Some(_)) => true,
            (AttrSelector::Exists(_), None) => false,
            (AttrSelector::Equals(_, want), Some((_, v))) => {
                if ci {
                    v.eq_ignore_ascii_case(want)
                } else {
                    v == want
                }
            }
            (AttrSelector::Equals(_, _), None) => false,
            (AttrSelector::DashMatch(_, want), Some((_, v))) => {
                let direct = if ci {
                    v.eq_ignore_ascii_case(want)
                } else {
                    v == want
                };
                if direct {
                    true
                } else {
                    let vl = v.to_lowercase();
                    let wl = want.to_lowercase();
                    vl.starts_with(&wl) && vl.as_bytes().get(wl.len()) == Some(&b'-')
                }
            }
            (AttrSelector::DashMatch(_, _), None) => false,
        };
        if !ok {
            return false;
        }
    }
    for p in &sel.pseudos {
        let ok = match p {
            Pseudo::Lang(ranges) => match element_lang(tree, id) {
                Some(lang) => ranges.iter().any(|r| lang_range_matches(r, &lang)),
                None => false,
            },
            Pseudo::Dir(d) => match element_dir(tree, id) {
                Some(dir) => dir.eq_ignore_ascii_case(d),
                None => false,
            },
            Pseudo::NthChild(n) => element_child_index(tree, id) == *n,
            Pseudo::Not(inner) => !compound_matches(tree, id, inner),
        };
        if !ok {
            return false;
        }
    }
    true
}

/// 紧邻的前一个元素兄弟：遍历父节点的 children，跳过 text 等非元素节点
/// （CSS `+` 只看元素兄弟；html 的 parent 是 Document 节点时同样照常处理）。
fn prev_element_sibling(tree: &Tree, id: NodeId) -> Option<NodeId> {
    let parent = tree.get(id).parent?;
    let mut prev: Option<NodeId> = None;
    for &child in tree.children_of(parent) {
        if child == id {
            return prev;
        }
        if matches!(tree.data(child), NodeData::Element { .. }) {
            prev = Some(child);
        }
    }
    None
}

fn chain_matches(tree: &Tree, id: NodeId, chain: &SelectorChain) -> bool {
    // The last part matches `id`; 从右往左按组合器归约：
    // Descendant 沿祖先链找任意匹配祖先，Adjacent 只看紧邻的前一个元素兄弟。
    let parts = &chain.parts;
    let combinators = &chain.combinators;
    let last_idx = parts.len() - 1;
    if !compound_matches(tree, id, &parts[last_idx]) {
        return false;
    }
    let mut current_id = id;
    for i in (0..last_idx).rev() {
        match combinators[i] {
            Combinator::Adjacent => match prev_element_sibling(tree, current_id) {
                Some(prev) if compound_matches(tree, prev, &parts[i]) => current_id = prev,
                _ => return false,
            },
            // M78.33: `A ~ B` —— 沿前向兄弟链找任意匹配 A 的兄弟。
            Combinator::LaterSibling => {
                let mut cur = current_id;
                let mut found = false;
                while let Some(prev) = prev_element_sibling(tree, cur) {
                    if compound_matches(tree, prev, &parts[i]) {
                        current_id = prev;
                        found = true;
                        break;
                    }
                    cur = prev;
                }
                if !found {
                    return false;
                }
            }
            // M78.34: `A > B` —— 直接父必须匹配 A。
            Combinator::Child => match tree.get(current_id).parent {
                Some(parent) if compound_matches(tree, parent, &parts[i]) => {
                    current_id = parent;
                }
                _ => return false,
            },
            Combinator::Descendant => {
                // Walk ancestors until we find one that matches `part`.
                let mut ancestor = tree.get(current_id).parent;
                let mut found = false;
                while let Some(aid) = ancestor {
                    if compound_matches(tree, aid, &parts[i]) {
                        current_id = aid;
                        found = true;
                        break;
                    }
                    ancestor = tree.get(aid).parent;
                }
                if !found {
                    return false;
                }
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use browser_dom::{NodeData, Tree};

    /// Build a tiny fixture:
    /// ```text
    /// Document
    ///   └─ html
    ///      └─ body
    ///         ├─ div.container
    ///         │  └─ p.text#main  ("hello")
    ///         └─ a.link          ("more")
    /// ```
    fn fixture() -> Tree {
        let mut t = Tree::with_root(NodeData::Document);
        let root = t.root();
        let html = t.insert(
            Some(root),
            NodeData::Element {
                tag: "html".into(),
                attrs: vec![],
            },
        );
        let body = t.insert(
            Some(html),
            NodeData::Element {
                tag: "body".into(),
                attrs: vec![],
            },
        );
        let div = t.insert(
            Some(body),
            NodeData::Element {
                tag: "div".into(),
                attrs: vec![("class".into(), "container".into())],
            },
        );
        let p = t.insert(
            Some(div),
            NodeData::Element {
                tag: "p".into(),
                attrs: vec![
                    ("class".into(), "text greeting".into()),
                    ("id".into(), "main".into()),
                ],
            },
        );
        let _ = t.insert(Some(p), NodeData::Text("hello".into()));
        let a = t.insert(
            Some(body),
            NodeData::Element {
                tag: "a".into(),
                attrs: vec![("class".into(), "link".into())],
            },
        );
        let _ = t.insert(Some(a), NodeData::Text("more".into()));
        t
    }

    #[test]
    fn parse_universal_matches_everything() {
        let tree = fixture();
        let sel = Selector::parse("*").unwrap();
        // Element ids: html=1, body=2, div=3, p=4, a=6.
        for id in [1, 2, 3, 4, 6] {
            assert!(sel.matches(&tree, id), "expected * to match id={id}");
        }
        // Text nodes must NOT match.
        assert!(!sel.matches(&tree, 5));
    }

    #[test]
    fn parse_type_selector_matches_tag_only() {
        let tree = fixture();
        let sel = Selector::parse("p").unwrap();
        // <p> lives at id=4; other element ids must not match.
        assert!(sel.matches(&tree, 4));
        assert!(!sel.matches(&tree, 1));
        assert!(!sel.matches(&tree, 2));
        assert!(!sel.matches(&tree, 3));
        assert!(!sel.matches(&tree, 6));
    }

    #[test]
    fn parse_class_selector() {
        let tree = fixture();
        let sel = Selector::parse(".container").unwrap();
        assert!(sel.matches(&tree, 3)); // div.container
        assert!(!sel.matches(&tree, 4)); // p.text#main
    }

    #[test]
    fn parse_class_match_when_class_attr_has_multiple() {
        let tree = fixture();
        let sel = Selector::parse(".text").unwrap();
        assert!(sel.matches(&tree, 4));
        let sel2 = Selector::parse(".greeting").unwrap();
        assert!(sel2.matches(&tree, 4));
    }

    #[test]
    fn parse_id_selector() {
        let tree = fixture();
        let sel = Selector::parse("#main").unwrap();
        assert!(sel.matches(&tree, 4));
        assert!(!sel.matches(&tree, 3));
    }

    #[test]
    fn parse_compound_tag_class_id() {
        let tree = fixture();
        let sel = Selector::parse("p.text#main").unwrap();
        assert!(sel.matches(&tree, 4));
        let sel2 = Selector::parse("p.other").unwrap();
        assert!(!sel2.matches(&tree, 4));
    }

    #[test]
    fn parse_list_of_selectors() {
        let tree = fixture();
        let sel = Selector::parse("a, .container").unwrap();
        assert!(sel.matches(&tree, 3)); // div.container
        assert!(sel.matches(&tree, 6)); // a (id=6, not 5)
        assert!(!sel.matches(&tree, 4)); // p
    }

    #[test]
    fn parse_descendant_combinator() {
        let tree = fixture();
        // body p — matches the <p> inside body (via div).
        let sel = Selector::parse("body p").unwrap();
        assert!(sel.matches(&tree, 4));
        // body a — matches the <a> directly under body.
        let sel2 = Selector::parse("body a").unwrap();
        assert!(sel2.matches(&tree, 6));
        // div a — does NOT match <a> (a is sibling of div, not descendant).
        let sel3 = Selector::parse("div a").unwrap();
        assert!(!sel3.matches(&tree, 6));
    }

    #[test]
    fn parse_invalid_selector_returns_err() {
        assert!(Selector::parse(".").is_err());
        assert!(Selector::parse("#").is_err());
    }

    #[test]
    fn parse_universal_with_classes_matches_correctly() {
        let tree = fixture();
        let sel = Selector::parse("*.link").unwrap();
        assert!(sel.matches(&tree, 6));
        assert!(!sel.matches(&tree, 4));
    }

    // ---- M78: :lang / :dir / :nth-child ----

    /// html[lang=en] > body > div[lang=es] > p#in + p#out
    fn lang_fixture() -> (Tree, NodeId, NodeId) {
        let mut t = Tree::with_root(NodeData::Document);
        let root = t.root();
        let html = t.insert(
            Some(root),
            NodeData::Element {
                tag: "html".into(),
                attrs: vec![("lang".into(), "en".into())],
            },
        );
        let body = t.insert(
            Some(html),
            NodeData::Element {
                tag: "body".into(),
                attrs: vec![],
            },
        );
        let div = t.insert(
            Some(body),
            NodeData::Element {
                tag: "div".into(),
                attrs: vec![("lang".into(), "es".into())],
            },
        );
        let p_in = t.insert(
            Some(div),
            NodeData::Element {
                tag: "p".into(),
                attrs: vec![("id".into(), "in".into())],
            },
        );
        let p_out = t.insert(
            Some(div),
            NodeData::Element {
                tag: "p".into(),
                attrs: vec![("id".into(), "out".into())],
            },
        );
        (t, p_in, p_out)
    }

    #[test]
    fn lang_matches_own_attribute() {
        let (tree, p_in, _) = lang_fixture();
        assert!(Selector::parse("p:lang(es)").unwrap().matches(&tree, p_in));
    }

    #[test]
    fn lang_inherits_from_ancestor() {
        // html[lang=en] 的孙子元素 p:out 没有 lang 属性，但继承 en。
        let (tree, _, p_out) = lang_fixture();
        // 注意：p_in/p_out 都在 div[lang=es] 下；真正的“继承 en”测试需要 div 外的元素。
        // 这里用 body 侧验证：body 无 lang → 继承 html 的 en。
        let _ = p_out;
        let body = tree.get(tree.root()).children[0]; // html
        let body = tree.get(body).children[0]; // body
        assert!(Selector::parse("body:lang(en)")
            .unwrap()
            .matches(&tree, body));
        assert!(!Selector::parse("body:lang(es)")
            .unwrap()
            .matches(&tree, body));
    }

    #[test]
    fn lang_range_and_region_semantics() {
        // RFC4647：en 匹配 en-US 但不匹配 enm；en-* 匹配 en-US 不匹配 enm；* 全匹配。
        let mut t = Tree::with_root(NodeData::Document);
        let root = t.root();
        let p_en = t.insert(
            Some(root),
            NodeData::Element {
                tag: "p".into(),
                attrs: vec![("lang".into(), "en-US".into())],
            },
        );
        let p_enm = t.insert(
            Some(root),
            NodeData::Element {
                tag: "p".into(),
                attrs: vec![("lang".into(), "enm".into())],
            },
        );
        assert!(Selector::parse(":lang(en)").unwrap().matches(&t, p_en));
        assert!(!Selector::parse(":lang(en)").unwrap().matches(&t, p_enm));
        assert!(!Selector::parse(":lang(fr-*)").unwrap().matches(&t, p_en));
        assert!(Selector::parse(":lang(en-*)").unwrap().matches(&t, p_en));
        assert!(!Selector::parse(":lang(en-*)").unwrap().matches(&t, p_enm));
        assert!(Selector::parse(":lang(*)").unwrap().matches(&t, p_enm));
    }

    #[test]
    fn dir_matches_inherited_direction() {
        let mut t = Tree::with_root(NodeData::Document);
        let root = t.root();
        let div = t.insert(
            Some(root),
            NodeData::Element {
                tag: "div".into(),
                attrs: vec![("dir".into(), "rtl".into())],
            },
        );
        let p = t.insert(
            Some(div),
            NodeData::Element {
                tag: "p".into(),
                attrs: vec![],
            },
        );
        assert!(Selector::parse("p:dir(rtl)").unwrap().matches(&t, p));
        assert!(!Selector::parse("p:dir(ltr)").unwrap().matches(&t, p));
    }

    #[test]
    fn nth_child_counts_element_siblings() {
        let (tree, p_in, p_out) = lang_fixture();
        assert!(Selector::parse("p:nth-child(1)")
            .unwrap()
            .matches(&tree, p_in));
        assert!(Selector::parse("p:nth-child(2)")
            .unwrap()
            .matches(&tree, p_out));
        assert!(!Selector::parse("p:nth-child(1)")
            .unwrap()
            .matches(&tree, p_out));
    }

    #[test]
    fn id_plus_pseudo_compound() {
        // `#box:lang(es)` —— id 名必须在 ':' 处断开，伪类独立解析。
        let (tree, p_in, _) = lang_fixture();
        assert!(Selector::parse("#in:lang(es)")
            .unwrap()
            .matches(&tree, p_in));
        assert!(!Selector::parse("#in:lang(fr)")
            .unwrap()
            .matches(&tree, p_in));
    }

    #[test]
    fn attr_selectors_parse_and_match() {
        let (tree, p_in, p_out) = lang_fixture();
        // p_in 无 lang（继承 div[lang=es]）→ 属性选择器只看自身属性，不继承。
        // fixture 里 p#in 无 lang 属性；给 [lang|=es] 换个带属性的元素测：
        // div[lang=es] 的 id 无关，直接对 div 匹配。
        let body = tree.get(p_in).parent.unwrap(); // div
        assert!(Selector::parse("[lang]").unwrap().matches(&tree, body));
        assert!(Selector::parse("[lang=\"es\"]")
            .unwrap()
            .matches(&tree, body));
        assert!(Selector::parse("[lang|=\"es\"]")
            .unwrap()
            .matches(&tree, body));
        assert!(!Selector::parse("[lang|=\"e\"]")
            .unwrap()
            .matches(&tree, body));
        // p#in 自身无 lang 属性 → [lang] 不匹配
        assert!(!Selector::parse("[lang]").unwrap().matches(&tree, p_in));
        // id 属性
        assert!(Selector::parse("p[id=\"in\"]")
            .unwrap()
            .matches(&tree, p_in));
        assert!(!Selector::parse("p[id=\"out\"]")
            .unwrap()
            .matches(&tree, p_in));
        let _ = p_out;
    }

    #[test]
    fn pseudo_parse_errors() {
        assert!(Selector::parse(":dir()").is_err());
        assert!(Selector::parse(":dir(ltr, rtl)").is_err());
        assert!(Selector::parse(":dir('ltr')").is_err());
        assert!(Selector::parse(":lang()").is_err());
        assert!(Selector::parse(":hover").is_err());
    }

    // ---- 相邻兄弟组合器 `+` ----

    /// 相邻兄弟 fixture：
    /// ```text
    /// Document
    ///   └─ html
    ///      └─ body
    ///         ├─ div.container
    ///         │  ├─ p#first    ("one")   ┐ p#first 的紧邻元素兄弟
    ///         │  └─ span#inner ("in")    ┘ （div 内部）
    ///         ├─ p#second      ("two")   ← div 的紧邻元素兄弟
    ///         ├─ span#tail     ("three") ← p#second 的紧邻元素兄弟
    ///         └─ section
    ///            ├─ b          ("bold")
    ///            ├─ Text " sep "       ← 文本不破坏相邻性
    ///            └─ i          ("italic")
    /// ```
    fn sibling_fixture() -> (Tree, NodeId, NodeId, NodeId, NodeId, NodeId, NodeId) {
        let el = |tag: &str, attrs: Vec<(&str, &str)>| NodeData::Element {
            tag: tag.into(),
            attrs: attrs
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        };
        let mut t = Tree::with_root(NodeData::Document);
        let root = t.root();
        let html = t.insert(Some(root), el("html", vec![]));
        let body = t.insert(Some(html), el("body", vec![]));
        let div = t.insert(Some(body), el("div", vec![("class", "container")]));
        let p_first = t.insert(Some(div), el("p", vec![("id", "first")]));
        let _ = t.insert(Some(p_first), NodeData::Text("one".into()));
        let span_inner = t.insert(Some(div), el("span", vec![("id", "inner")]));
        let _ = t.insert(Some(span_inner), NodeData::Text("in".into()));
        let p_second = t.insert(Some(body), el("p", vec![("id", "second")]));
        let _ = t.insert(Some(p_second), NodeData::Text("two".into()));
        let span = t.insert(Some(body), el("span", vec![("id", "tail")]));
        let _ = t.insert(Some(span), NodeData::Text("three".into()));
        let section = t.insert(Some(body), el("section", vec![]));
        let b = t.insert(Some(section), el("b", vec![]));
        let _ = t.insert(Some(b), NodeData::Text("bold".into()));
        let _ = t.insert(Some(section), NodeData::Text(" sep ".into()));
        let i = t.insert(Some(section), el("i", vec![]));
        let _ = t.insert(Some(i), NodeData::Text("italic".into()));
        (t, p_first, span_inner, p_second, span, b, i)
    }

    #[test]
    fn adjacent_matches_immediate_predecessor_only() {
        let (tree, p_first, _, p_second, span, _, _) = sibling_fixture();
        // div + p：p#second 紧跟 div（同父 body，中间无其他元素）→ 命中。
        let sel = Selector::parse("div + p").unwrap();
        assert!(sel.matches(&tree, p_second));
        // p#first 在 div 内部（div 是它的父亲不是兄弟）→ 不命中。
        assert!(!sel.matches(&tree, p_first));
        // span#tail 被 p#second 隔开 → 不命中。
        assert!(!sel.matches(&tree, span));
        // p + span：span#tail 的前一元素兄弟正是 p#second → 命中。
        assert!(Selector::parse("p + span").unwrap().matches(&tree, span));
    }

    #[test]
    fn adjacent_skips_text_nodes() {
        // b 与 i 之间有 Text 节点；CSS 相邻兄弟只看元素兄弟 → 仍命中。
        let (tree, _, _, _, _, b, i) = sibling_fixture();
        assert!(Selector::parse("b + i").unwrap().matches(&tree, i));
        // 顺序反过来不成立（b 在 i 前面，i 不是 b 的前兄弟）。
        assert!(!Selector::parse("i + b").unwrap().matches(&tree, b));
    }

    #[test]
    fn adjacent_no_space_and_mixed_spacing() {
        // '+' 两侧空格可省略：div+p / div+ p / div +p 与 div + p 等价。
        let (tree, _, _, p_second, _, _, _) = sibling_fixture();
        for src in ["div+p", "div+ p", "div +p", "div   +   p"] {
            let sel = Selector::parse(src).unwrap();
            assert!(sel.matches(&tree, p_second), "expected {src:?} to match");
        }
    }

    #[test]
    fn adjacent_parses_into_chain_structure() {
        // 源顺序：parts[0] combinators[0] parts[1] combinators[1] parts[2]。
        let sel = Selector::parse("div p + span").unwrap();
        let chain = &sel.selectors[0];
        let tags: Vec<Option<&str>> = chain.parts.iter().map(|p| p.tag.as_deref()).collect();
        assert_eq!(tags, vec![Some("div"), Some("p"), Some("span")]);
        assert_eq!(
            chain.combinators,
            vec![Combinator::Descendant, Combinator::Adjacent]
        );
        // 单 compound：无组合器。
        let single = Selector::parse("p").unwrap();
        assert!(single.selectors[0].combinators.is_empty());
    }

    #[test]
    fn adjacent_mixed_chain_descendant_then_adjacent() {
        // div p + span：目标的紧邻前元素兄弟匹配 p，且该 p 的祖先链上有 div。
        let (tree, _, span_inner, p_second, span_tail, _, _) = sibling_fixture();
        assert!(Selector::parse("div p + span")
            .unwrap()
            .matches(&tree, span_inner));
        // 换成 html（p#first 的祖先）也应命中 —— Descendant 部分照常走祖先链。
        assert!(Selector::parse("html p + span")
            .unwrap()
            .matches(&tree, span_inner));
        // 祖先没有 section → 不命中。
        assert!(!Selector::parse("section p + span")
            .unwrap()
            .matches(&tree, span_inner));
        // span#tail 的前兄弟是 p#second，其祖先链无 div → 归约锚定紧邻兄弟，不命中。
        assert!(!Selector::parse("div p + span")
            .unwrap()
            .matches(&tree, span_tail));
        // div span + p：p#second 的前一元素兄弟是 div 不是 span → 不命中。
        assert!(!Selector::parse("div span + p")
            .unwrap()
            .matches(&tree, p_second));
    }

    #[test]
    fn adjacent_universal_and_no_prev_sibling() {
        let (tree, p_first, _, p_second, _, _, _) = sibling_fixture();
        // * + p：任意前元素兄弟（div）都行 → 命中。
        assert!(Selector::parse("* + p").unwrap().matches(&tree, p_second));
        // p#first 是 div 的第一个元素子节点，无前元素兄弟 → 任何 X + p 都不命中。
        assert!(!Selector::parse("* + p").unwrap().matches(&tree, p_first));
    }

    #[test]
    fn adjacent_malformed_chains_return_err() {
        assert!(Selector::parse("div +").is_err()); // 尾部悬空 '+'
        assert!(Selector::parse("+ p").is_err()); // 开头 '+'
        assert!(Selector::parse("div + + p").is_err()); // 连续 '+'
    }

    #[test]
    fn adjacent_plus_inside_brackets_is_not_combinator() {
        // '+' 在属性选择器内部不是组合器（depth 跟踪）。
        let mut t = Tree::with_root(NodeData::Document);
        let root = t.root();
        let p = t.insert(
            Some(root),
            NodeData::Element {
                tag: "p".into(),
                attrs: vec![("title".into(), "a + b".into())],
            },
        );
        assert!(Selector::parse(r#"p[title="a + b"]"#)
            .unwrap()
            .matches(&t, p));
    }
}
