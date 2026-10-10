//! XML pull parser（namespace 感知，零拷贝）。
//!
//! 设计约束：
//! - **零拷贝**：事件携带指向输入字节的 `&'a str` 切片，解析器本身零分配。
//! - **DOCTYPE / 内部子集**：跳过并计数（[`Parser::doctype_skips`]）。老
//!   Adobe SVG 常见的 DTD 实体声明不解析，预定义实体照常工作。
//! - **实体**：仅预定义五种（`&lt; &gt; &amp; &apos; &quot;`）+ 数字字符
//!   引用（`&#NN;` / `&#xHH;`）；未定义实体按字面保留（宽容）。
//!   解码用 [`decode_entities`]，由树构建层按需调用。
//! - **CDATA / 注释 / PI / XML 声明 / BOM**：正确跳过（CDATA 产出 Text 事件）。
//! - **良构性**：标签/闭合名不匹配不报错（由树构建层计数告警）；
//!   截断等硬错误返回 [`Event::Error`] 一次，随后 [`Event::Eof`]。

use alloc::string::String;
use alloc::vec::Vec;

/// 解析事件。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event<'a> {
    /// 元素开始。`name` 为带前缀的限定名（namespace 归一化在树构建层做）。
    /// 属性紧随其后通过 [`Parser::attrs_into`] 获取。
    StartElement {
        name: &'a str,
    },
    /// 元素结束。自闭合元素在 `StartElement` 后的**下一次**调用返回。
    EndElement {
        name: &'a str,
    },
    /// 字符数据（原文切片，实体未解码；CDATA 也归入此事件）。
    Text(&'a str),
    /// 硬错误（截断/非法结构）。之后调用返回 [`Event::Eof`]。
    Error {
        message: &'static str,
    },
    Eof,
}

/// 属性（原文值：引号已剥、实体未解码）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Attr<'a> {
    pub name: &'a str,
    pub value: &'a str,
}

/// 渐进式解析器游标。
pub struct Parser<'a> {
    input: &'a str,
    bytes: &'a [u8],
    pos: usize,
    /// 自闭合标签的待发结束事件。
    pending_end: Option<&'a str>,
    /// 最近一次 StartElement 的属性字节区间（起点在名字结束之后），
    /// 供 `attrs_into` 重扫。
    attr_region: Option<(usize, usize)>,
    doctype_skips: u32,
}

impl<'a> Parser<'a> {
    pub fn new(input: &'a str) -> Self {
        // UTF-8 BOM 容忍
        let pos = if input.starts_with('\u{FEFF}') { 3 } else { 0 };
        Self {
            input,
            bytes: input.as_bytes(),
            pos,
            pending_end: None,
            attr_region: None,
            doctype_skips: 0,
        }
    }

    /// 被跳过的 DOCTYPE 数（进文档 warnings）。
    pub fn doctype_skips(&self) -> u32 {
        self.doctype_skips
    }

    pub fn next_event(&mut self) -> Event<'a> {
        loop {
            self.attr_region = None;
            if let Some(name) = self.pending_end.take() {
                return Event::EndElement { name };
            }
            if self.pos >= self.bytes.len() {
                return Event::Eof;
            }
            if self.bytes[self.pos] != b'<' {
                // 文本段：直到下一个 '<'（进入分支时当前字节必不是 '<'，故非空）
                let end = match self.input[self.pos..].find('<') {
                    Some(off) => self.pos + off,
                    None => self.bytes.len(),
                };
                let text = &self.input[self.pos..end];
                self.pos = end;
                return Event::Text(text);
            }
            if self.starts_with_at(self.pos, "<?") {
                let Some(off) = self.input[self.pos..].find("?>") else {
                    return self.hard_error("未闭合的处理指令");
                };
                self.pos += off + 2;
                continue;
            }
            if self.starts_with_at(self.pos, "<!--") {
                let Some(off) = self.input[self.pos..].find("-->") else {
                    return self.hard_error("未闭合的注释");
                };
                self.pos += off + 3;
                continue;
            }
            if self.starts_with_at(self.pos, "<![CDATA[") {
                let content_start = self.pos + 9;
                let Some(off) = self.input[content_start..].find("]]>") else {
                    return self.hard_error("未闭合的 CDATA 段");
                };
                let end = content_start + off;
                let text = &self.input[content_start..end];
                self.pos = end + 3;
                return Event::Text(text);
            }
            if self.starts_with_at(self.pos, "<!") {
                // DOCTYPE（含内部子集 [..]；引号内的 '>' ']' 不计）
                if self.skip_doctype() {
                    self.doctype_skips += 1;
                    continue;
                }
                return self.hard_error("未闭合的 DOCTYPE");
            }
            if self.starts_with_at(self.pos, "</") {
                let Some(off) = self.input[self.pos..].find('>') else {
                    return self.hard_error("未闭合的结束标签");
                };
                let name = self.input[self.pos + 2..self.pos + off].trim();
                self.pos += off + 1;
                return Event::EndElement { name };
            }
            // 开始标签：'<' 之后必须是名字起始字符。
            // 车道CI（parser-fuzz）：输入以孤立 '<' 结尾（pos+1 == len）
            // 时旧代码 `bytes[pos+1]` 越界 panic——按截断硬错误处理。
            if self.pos + 1 >= self.bytes.len() {
                return self.hard_error("截断的开始标签");
            }
            let first = self.bytes[self.pos + 1];
            if !(first.is_ascii_alphabetic() || first == b'_' || first == b':') {
                return self.hard_error("非法的标签起始字符");
            }
            return self.parse_start_tag();
        }
    }

    /// 仅在 `next_event` 返回 `StartElement` 之后、下一次 `next_event` 之前
    /// 调用有效：把当前开始标签的属性解析进调用方提供的缓冲（零库内分配）。
    pub fn attrs_into(&self, out: &mut Vec<Attr<'a>>) {
        out.clear();
        let Some((s, e)) = self.attr_region else {
            return;
        };
        let mut i = s;
        while i < e {
            while i < e && self.bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            if i >= e {
                break;
            }
            let name_start = i;
            while i < e && !self.bytes[i].is_ascii_whitespace() && self.bytes[i] != b'=' {
                i += 1;
            }
            let name = &self.input[name_start..i];
            while i < e && self.bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            if i >= e || self.bytes[i] != b'=' {
                continue; // 无值属性（非良构 XML）：宽容跳过
            }
            i += 1;
            while i < e && self.bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            if i < e && (self.bytes[i] == b'"' || self.bytes[i] == b'\'') {
                let quote = self.bytes[i];
                let vstart = i + 1;
                let mut vend = vstart;
                while vend < e && self.bytes[vend] != quote {
                    vend += 1;
                }
                if vend >= e {
                    break; // 引号跨到标签外（截断）：停止
                }
                out.push(Attr {
                    name,
                    value: &self.input[vstart..vend],
                });
                i = vend + 1;
            } else {
                // 未加引号的值（非良构）：读到空白为止，宽容接受
                let vstart = i;
                while i < e && !self.bytes[i].is_ascii_whitespace() {
                    i += 1;
                }
                out.push(Attr {
                    name,
                    value: &self.input[vstart..i],
                });
            }
        }
    }

    fn parse_start_tag(&mut self) -> Event<'a> {
        // 名字：到空白 / '/' / '>' 为止
        let mut name_end = self.pos + 1;
        while name_end < self.bytes.len()
            && !self.bytes[name_end].is_ascii_whitespace()
            && self.bytes[name_end] != b'/'
            && self.bytes[name_end] != b'>'
        {
            name_end += 1;
        }
        if name_end >= self.bytes.len() {
            return self.hard_error("截断的开始标签");
        }
        let name = &self.input[self.pos + 1..name_end];
        // 扫描到引号感知的 '>'；记录最后一个非空白字符以判定自闭合
        let mut i = name_end;
        let mut quote = 0u8;
        while i < self.bytes.len() {
            let b = self.bytes[i];
            if quote != 0 {
                if b == quote {
                    quote = 0;
                }
            } else if b == b'"' || b == b'\'' {
                quote = b;
            } else if b == b'>' {
                break;
            }
            i += 1;
        }
        if i >= self.bytes.len() {
            return self.hard_error("截断的开始标签");
        }
        let gt = i;
        let mut j = gt;
        while j > name_end && self.bytes[j - 1].is_ascii_whitespace() {
            j -= 1;
        }
        let self_closing = j > name_end && self.bytes[j - 1] == b'/';
        let region_end = if self_closing { j - 1 } else { gt };
        self.attr_region = Some((name_end, region_end));
        self.pos = gt + 1;
        if self_closing {
            self.pending_end = Some(name);
        }
        Event::StartElement { name }
    }

    fn skip_doctype(&mut self) -> bool {
        let mut i = self.pos + 2;
        let mut quote = 0u8;
        let mut depth = 0i32;
        while i < self.bytes.len() {
            let b = self.bytes[i];
            if quote != 0 {
                if b == quote {
                    quote = 0;
                }
            } else {
                match b {
                    b'"' | b'\'' => quote = b,
                    b'[' => depth += 1,
                    b']' => depth -= 1,
                    b'>' if depth <= 0 => {
                        self.pos = i + 1;
                        return true;
                    }
                    _ => {}
                }
            }
            i += 1;
        }
        false
    }

    fn starts_with_at(&self, pos: usize, prefix: &str) -> bool {
        self.bytes[pos..].starts_with(prefix.as_bytes())
    }

    /// 硬错误：停在输入末尾，后续调用只产出 Eof。
    fn hard_error(&mut self, message: &'static str) -> Event<'a> {
        self.pos = self.bytes.len();
        Event::Error { message }
    }
}

/// 展开输出上限（防御实体风暴内存炸弹：`&e;` 3 字节引用 × 长值 =
/// 输出按输入平方级膨胀——28KB 输入实测展开出 65MB。超限即停止替换、
/// 余下引用按字面保留（宽容降级）；真实语料（coords-viewattr 族）
/// 展开输出 <2× 输入，远低于此线）。
const EXPAND_OUTPUT_CAP: usize = 4 << 20;

/// DOCTYPE 内部子集一般实体展开：把 `<!ENTITY name "value">` 声明收进表，
/// 再将**子集结束之后**文档体里的 `&name;` 引用替换为声明值（值可含标记，
/// 展开后整体重走解析器）。无 `<!ENTITY` 声明时返回 None（零分配快路径）。
/// 限制（登记）：不做递归展开（值内实体引用按原样保留）；CDATA 段内的
/// `&name;` 理论上不该展开，此处未特判（W3C 语料无此形态）。
pub fn expand_dtd_entities(src: &str) -> Option<String> {
    // 1) 定位 DOCTYPE 的内部子集 [ .. ]（引号感知；DOCTYPE 在文档最前，
    //    从头找第一个 '<!DOCTYPE' 即可）
    let dt = src.find("<!DOCTYPE")?;
    let bytes = src.as_bytes();
    let mut i = dt;
    let mut quote = 0u8;
    let mut subset_start: Option<usize> = None;
    let mut subset_end: Option<usize> = None;
    while i < bytes.len() {
        let b = bytes[i];
        if quote != 0 {
            if b == quote {
                quote = 0;
            }
        } else {
            match b {
                b'"' | b'\'' => quote = b,
                b'[' if subset_start.is_none() => subset_start = Some(i + 1),
                b']' if subset_start.is_some() => {
                    subset_end = Some(i);
                    break;
                }
                b'>' if subset_start.is_none() => break, // 无内部子集的 DOCTYPE
                _ => {}
            }
        }
        i += 1;
    }
    let (ss, se) = (subset_start?, subset_end?);
    let body_start = se + 1;
    if body_start > bytes.len() {
        return None;
    }
    let sub = &src[ss..se];
    if !sub.contains("<!ENTITY") {
        return None;
    }
    // 2) 收集声明：<!ENTITY name "value">（%参数实体跳过；引号两种）
    let mut map: Vec<(&str, &str)> = Vec::new();
    let mut j = 0;
    while let Some(off) = sub[j..].find("<!ENTITY") {
        let mut k = j + off + 8;
        // 跳过空白
        while k < sub.len() && sub.as_bytes()[k].is_ascii_whitespace() {
            k += 1;
        }
        if sub.as_bytes().get(k) == Some(&b'%') {
            // 参数实体：整条声明跳过
            if let Some(end) = sub[k..].find('>') {
                j = k + end + 1;
                continue;
            }
            break;
        }
        let name_start = k;
        while k < sub.len() && !sub.as_bytes()[k].is_ascii_whitespace() {
            k += 1;
        }
        let name = &sub[name_start..k];
        // 空白到引号
        while k < sub.len() && sub.as_bytes()[k].is_ascii_whitespace() {
            k += 1;
        }
        let q = *sub.as_bytes().get(k)?;
        if q != b'"' && q != b'\'' {
            break; // SYSTEM/PUBLIC 等形态：不支持，停止收集（宽容）
        }
        k += 1;
        let vstart = k;
        while k < sub.len() && sub.as_bytes()[k] != q {
            k += 1;
        }
        if k >= sub.len() {
            break;
        }
        let value = &sub[vstart..k];
        if !name.is_empty() {
            map.push((name, value));
        }
        // 到声明收尾 '>'
        if let Some(end) = sub[k..].find('>') {
            j = k + end + 1;
        } else {
            break;
        }
    }
    if map.is_empty() {
        return None;
    }
    // 3) 展开：文档体逐字符扫描，`&name;` 命中表则替换（一次Round；
    //    名字窗口对齐 decode_entities 的 10 字节上限不适用——实体名取到 ';'）
    //    展开上限随输入缩放（MCU 堆保护：输入平方级膨胀封顶在
    //    4×输入+64KB，绝对上限 4MB）。
    let expand_cap = (src.len().saturating_mul(4))
        .saturating_add(64 * 1024)
        .min(EXPAND_OUTPUT_CAP);
    let mut out = String::with_capacity(src.len() + 256);
    out.push_str(&src[..body_start]);
    let body = &src[body_start..];
    let bbytes = body.as_bytes();
    let mut plain = 0;
    let mut i = 0;
    while i < bbytes.len() {
        if bbytes[i] != b'&' {
            i += 1;
            continue;
        }
        let Some(rel) = bbytes[i + 1..].iter().position(|&b| b == b';') else {
            i += 1;
            continue;
        };
        let name = &body[i + 1..i + 1 + rel];
        if let Some((_, value)) = map.iter().find(|(n, _)| *n == name) {
            // 内存上限（车道CI）：达到上限后不再替换（余下引用按字面
            // 保留），展开输出规模恒 ≤ min(4×输入+64KB, 4MB)。
            if out.len() + value.len() > expand_cap {
                break;
            }
            out.push_str(&body[plain..i]);
            out.push_str(value);
            plain = i + 1 + rel + 1;
        }
        i += 1 + rel + 1;
    }
    out.push_str(&body[plain..]);
    Some(out)
}

/// 清空 `out` 后写入 `raw` 的实体解码结果。
/// 未定义实体按字面保留（如 `&foo;` 原样输出）。
pub fn decode_entities(raw: &str, out: &mut String) {
    out.clear();
    let bytes = raw.as_bytes();
    let mut i = 0;
    let mut plain = 0;
    while i < bytes.len() {
        if bytes[i] != b'&' {
            i += 1;
            continue;
        }
        // 实体名窗口：& 后 1..=10 字节内找 ';'，超窗视为字面 '&'
        let window_end = (i + 11).min(bytes.len());
        let Some(rel) = bytes[i + 1..window_end].iter().position(|&b| b == b';') else {
            i += 1;
            continue;
        };
        let semi = i + 1 + rel;
        let name = &raw[i + 1..semi];
        if let Some(c) = resolve_entity(name) {
            out.push_str(&raw[plain..i]);
            out.push(c);
            plain = semi + 1;
        }
        i = semi + 1;
    }
    out.push_str(&raw[plain..]);
}

fn resolve_entity(name: &str) -> Option<char> {
    match name {
        "lt" => Some('<'),
        "gt" => Some('>'),
        "amp" => Some('&'),
        "apos" => Some('\''),
        "quot" => Some('"'),
        _ => {
            let num = name.strip_prefix('#')?;
            let v = if let Some(hex) = num.strip_prefix('x').or_else(|| num.strip_prefix('X')) {
                u32::from_str_radix(hex, 16).ok()?
            } else {
                num.parse::<u32>().ok()?
            };
            char::from_u32(v)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::format;
    use alloc::vec;
    use alloc::vec::Vec;

    #[test]
    fn elements_attrs_and_self_closing() {
        let mut p = Parser::new(r#"<svg width="8"><rect x='1'/><g></g>hi</svg>"#);
        assert_eq!(p.next_event(), Event::StartElement { name: "svg" });
        let mut attrs: Vec<Attr> = Vec::new();
        p.attrs_into(&mut attrs);
        assert_eq!(
            attrs,
            vec![Attr {
                name: "width",
                value: "8"
            }]
        );

        assert_eq!(p.next_event(), Event::StartElement { name: "rect" });
        p.attrs_into(&mut attrs);
        assert_eq!(
            attrs,
            vec![Attr {
                name: "x",
                value: "1"
            }]
        );
        // 自闭合：下一次事件是同名结束
        assert_eq!(p.next_event(), Event::EndElement { name: "rect" });

        assert_eq!(p.next_event(), Event::StartElement { name: "g" });
        assert_eq!(p.next_event(), Event::EndElement { name: "g" });
        assert_eq!(p.next_event(), Event::Text("hi"));
        assert_eq!(p.next_event(), Event::EndElement { name: "svg" });
        assert_eq!(p.next_event(), Event::Eof);
    }

    #[test]
    fn skips_prolog_comment_and_doctype() {
        let src = "<?xml version=\"1.0\"?>\
                   <!DOCTYPE svg PUBLIC \"-//W3C//DTD SVG 1.1//EN\" \"http://e.dtd\" [<!ENTITY n \"v\">]>\
                   <!-- c --><svg/>";
        let mut p = Parser::new(src);
        assert_eq!(p.next_event(), Event::StartElement { name: "svg" });
        assert_eq!(p.doctype_skips(), 1);
        assert_eq!(p.next_event(), Event::EndElement { name: "svg" });
        assert_eq!(p.next_event(), Event::Eof);
    }

    #[test]
    fn cdata_is_text() {
        let mut p = Parser::new("<a><![CDATA[x<y & z]]></a>");
        assert_eq!(p.next_event(), Event::StartElement { name: "a" });
        assert_eq!(p.next_event(), Event::Text("x<y & z"));
        assert_eq!(p.next_event(), Event::EndElement { name: "a" });
    }

    #[test]
    fn attr_value_may_contain_gt_and_slash() {
        let mut p = Parser::new(r#"<a data="x>y/" b='q/r'/>"#);
        assert_eq!(p.next_event(), Event::StartElement { name: "a" });
        let mut attrs: Vec<Attr> = Vec::new();
        p.attrs_into(&mut attrs);
        assert_eq!(
            attrs,
            vec![
                Attr {
                    name: "data",
                    value: "x>y/"
                },
                Attr {
                    name: "b",
                    value: "q/r"
                },
            ]
        );
        assert_eq!(p.next_event(), Event::EndElement { name: "a" });
    }

    #[test]
    fn truncated_input_is_error_then_eof() {
        let mut p = Parser::new("<abc");
        assert!(matches!(p.next_event(), Event::Error { .. }));
        assert_eq!(p.next_event(), Event::Eof);
    }

    /// 车道CI（parser-fuzz）：孤立 `<` 收尾（pos+1 == len）旧代码
    /// `bytes[pos+1]` 越界 panic——按截断硬错误降级。
    #[test]
    fn lone_trailing_lt_is_hard_error() {
        for src in ["<", "a<", "<g></g><"] {
            let mut p = Parser::new(src);
            let mut guard = 0;
            loop {
                guard += 1;
                assert!(guard < 100, "事件流不终止: {src:?}");
                match p.next_event() {
                    Event::Eof => break,
                    Event::Error { .. } => {}
                    _ => {}
                }
            }
        }
    }

    /// 车道CI（parser-fuzz）：实体风暴输出封顶（修前 28KB 输入展开出
    /// 65MB——3 字节引用 × 长值 = 输入平方级膨胀）。上限 =
    /// min(4×输入+64KB, 4MB)+输入尾；真实语料形态不受影响。
    #[test]
    fn expand_entities_output_capped() {
        let val = "x".repeat(4096);
        let src = format!(
            "<!DOCTYPE svg [<!ENTITY e \"{v}\">]><svg>{r}</svg>",
            v = val,
            r = "&e;".repeat(20_000)
        );
        let out = expand_dtd_entities(&src).expect("有实体声明应产出展开串");
        let cap = src.len() * 4 + 64 * 1024;
        assert!(
            out.len() <= cap + src.len(),
            "展开输出 {} 超 cap {cap}",
            out.len()
        );
        // 前缀展开语义：封顶前已替换的部分仍在
        assert!(out.contains(&val));
    }

    #[test]
    fn decode_predefined_and_numeric() {
        let mut s = String::new();
        decode_entities("&#35;A&amp;&#x42;", &mut s);
        assert_eq!(s, "#A&B");
        // 未定义实体按字面保留
        decode_entities("&foo;&lt;", &mut s);
        assert_eq!(s, "&foo;<");
        // 无 ';' 的孤立 & 原样保留
        decode_entities("a & b", &mut s);
        assert_eq!(s, "a & b");
    }

    /// 车道P：DOCTYPE 内部子集实体展开——值含标记与单引号
    /// （coords-viewattr-01/02、render-elems-03 语料形态）。
    #[test]
    fn expands_doctype_internal_entities() {
        let src = "<?xml version=\"1.0\"?>\
                   <!DOCTYPE svg PUBLIC \"-//W3C//DTD SVG 1.1//EN\" \"http://e.dtd\" [\
                   <!ENTITY shape \"<path d='M0,0 l1,1'/>\">\
                   <!ENTITY vp \"<rect width='2'/>\">\
                   ]><svg>&shape;&vp;&shape;</svg>";
        let out = expand_dtd_entities(src).expect("应产出展开串");
        assert_eq!(
            out,
            "<?xml version=\"1.0\"?>\
             <!DOCTYPE svg PUBLIC \"-//W3C//DTD SVG 1.1//EN\" \"http://e.dtd\" [\
             <!ENTITY shape \"<path d='M0,0 l1,1'/>\">\
             <!ENTITY vp \"<rect width='2'/>\">\
             ]><svg><path d='M0,0 l1,1'/><rect width='2'/><path d='M0,0 l1,1'/></svg>"
        );
        // 无实体声明：None（零分配快路径）
        assert!(expand_dtd_entities("<!DOCTYPE svg [<!ELEMENT x ANY>]><svg/>").is_none());
        assert!(expand_dtd_entities("<svg/>").is_none());
        // 参数实体跳过、未定义引用保留
        let src2 = "<!DOCTYPE svg [<!ENTITY % p \"x\"><!ENTITY a \"A\">]><svg>&a;&b;</svg>";
        let expanded = expand_dtd_entities(src2).expect("有实体声明应产出展开串");
        assert_eq!(
            expanded,
            "<!DOCTYPE svg [<!ENTITY % p \"x\"><!ENTITY a \"A\">]><svg>A&b;</svg>"
        );
    }
}
