//! M-cls.3: CSR 数据兜底 —— JS 跑空/失败时直接拉 SSR 数据页注入正文。
//!
//! ## 背景
//! cls.cn/telegraph 是 Next.js **CSR**：`__NEXT_DATA__` 只含 `{chooseNav}`，
//! 真正电报正文由 JS 运行时通过带签名的 XHR 拉 `get_roll_list`。签名算法
//! 混淆在 vendor bundle 里，逆向不稳定且 bundle 在 boa 里直接 OOM。
//!
//! 但 **m.cls.cn/telegraph**（移动版）是 SSR：HTML 里直接内嵌
//! `initialState: {... roll_data: [...]}`，每条含 `brief`/`ctime`/`level`，
//! 无需签名、无需 JS。本模块在 JS 渲染结果为空壳时拉这个移动版页面，
//! 提取 roll_data 注入 DOM `<body>`，再让上层布局+渲染输出真实电报。
//!
//! ## host→fetcher 注册表
//! `HOST_FETCHERS` 把"匹配的 host"映射到"数据 fetcher"。新增别的 CSR 站点
//! 只加一项，不动主逻辑。

use std::collections::BTreeMap;

use browser_dom::Tree;

use crate::bridge::{append_body_text, find_first_element, SharedTree};

/// 一条兜底数据项（爬虫友好的归一化结构）。
#[derive(Debug, Clone, PartialEq)]
pub struct FallbackItem {
    /// 时间戳（Unix 秒）。0 = 缺失。
    pub ctime: u64,
    /// 正文摘要（cls = brief 字段）。非空。
    pub brief: String,
    /// 等级（cls = level，如 "B"/"C"）。可空。
    pub level: String,
}

/// 尝试对当前树做 CSR 兜底。
///
/// 返回 `Ok(true)` = 命中并注入了数据；`Ok(false)` = 无需兜底（无匹配 host
/// fetcher，或 fetcher 没拿到数据）；`Err` = 基础设施错误（极少，调用方忽略）。
///
/// 触发条件：**base_url 的 host 在注册表里**（注册表本身就是"此站点需要
/// CSR 数据兜底"的显式声明）。不做 body 文本长度启发式 —— 因为静态壳可能
/// 含导航等长文本，启发式会误判"已渲染"而跳过兜底。fetcher 自己决定数据在不在。
///
/// 流程：①查 host fetcher ②fetch+解析 ③注入 DOM。任一环节失败都返回
/// `Ok(false)` —— 兜底是 best-effort，绝不能阻断渲染。
pub fn try_csr_fallback(tree: &SharedTree, base_url: &str) -> Result<bool, String> {
    if base_url.is_empty() || base_url == "about:blank" {
        return Ok(false);
    }
    let host = match url::Url::parse(base_url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
    {
        Some(h) => h,
        None => return Ok(false),
    };
    let fetcher = match HOST_FETCHERS.iter().find(|(matcher, _)| matcher(&host)) {
        Some((_, f)) => f,
        None => return Ok(false),
    };
    let items = match fetcher(base_url) {
        Ok(items) if !items.is_empty() => items,
        Ok(_) => return Ok(false),
        Err(e) => return Err(format!("fetcher for {host}: {e}")),
    };
    // 注入：每条作为独立文本节点 append 到 body，格式 `[YYYY-MM-DD HH:MM] 等级 正文`。
    let mut tree = tree.borrow_mut();
    let injected = inject_items(&mut tree, &items);
    Ok(injected)
}

/// 把数据项注入树的 `<body>`，每项一个文本节点。
/// 返回 true 表示至少注入了一条。会先插入一个分隔标题。
fn inject_items(tree: &mut Tree, items: &[FallbackItem]) -> bool {
    if items.is_empty() {
        return false;
    }
    if find_first_element(tree, "body").is_none() {
        return false;
    }
    // 分隔标题，让爬虫知道这是兜底注入的电报流。
    append_body_text(tree, "【电报（CSR 兜底渲染）】");
    for it in items {
        let line = format_item(it);
        append_body_text(tree, &line);
    }
    true
}

/// 格式化一条为爬虫友好的纯文本行。
fn format_item(it: &FallbackItem) -> String {
    let time = format_timestamp(it.ctime);
    let level = if it.level.is_empty() {
        String::new()
    } else {
        format!("[{}]", it.level)
    };
    if it.brief.is_empty() {
        return String::new();
    }
    format!("[{time}] {level} {brief}", brief = it.brief.trim())
}

/// Unix 秒 → "YYYY-MM-DD HH:MM"（UTC，爬虫场景够用；避免引入 chrono）。
fn format_timestamp(secs: u64) -> String {
    if secs == 0 {
        return String::from("--");
    }
    let days = secs / 86_400;
    let rem = secs % 86_400;
    let hour = rem / 3600;
    let minute = (rem % 3600) / 60;
    // 1970-01-01 起 + days 天，转 YYYY-MM-DD。
    let (y, m, d) = civil_from_days(days as i64);
    format!("{y:04}-{m:02}-{d:02} {hour:02}:{minute:02}")
}

/// days since 1970-01-01 → (year, month, day)。Howard Hinnant 算法。
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m as u32, d as u32)
}

// ============================== host→fetcher 注册表 ==============================

type HostMatcher = fn(&str) -> bool;
type Fetcher = fn(&str) -> Result<Vec<FallbackItem>, String>;

/// 已注册的 (host 匹配器, 数据 fetcher) 列表。顺序匹配，第一个命中即用。
static HOST_FETCHERS: &[(HostMatcher, Fetcher)] = &[(host_is_cls_cn, fetch_cls_cn_telegraph)];

fn host_is_cls_cn(host: &str) -> bool {
    host == "www.cls.cn" || host == "cls.cn" || host == "m.cls.cn"
}

/// cls.cn 电报 fetcher：拉移动版 SSR 页面，提取内嵌 roll_data JSON。
///
/// 移动版 `m.cls.cn/telegraph` 把 `initialState: {... roll_data: [...]}` 直接
/// SSR 进 HTML，每条含 `brief`/`ctime`/`level`，无需签名。这是比带签名
/// `get_roll_list` API 稳定得多的数据源。
fn fetch_cls_cn_telegraph(base_url: &str) -> Result<Vec<FallbackItem>, String> {
    // 无论桌面版还是移动版 URL，统一拉移动版页面（数据同源）。
    let mobile_url = "https://m.cls.cn/telegraph";
    let _ = base_url; // 仅用于日志/未来按路径分流
    let html = fetch_sync_text(mobile_url)?;
    let json = extract_roll_data_json(&html)?;
    let roll = parse_json(&json).map_err(|e| format!("roll_data parse: {e}"))?;
    let arr = match roll {
        Json::Array(a) => a,
        _ => return Err("roll_data not an array".into()),
    };
    let mut items = Vec::with_capacity(arr.len());
    for v in arr {
        let obj = match v {
            Json::Object(o) => o,
            _ => continue,
        };
        let brief = obj
            .get("brief")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        let content = obj.get("content").and_then(|x| x.as_str()).unwrap_or("");
        // brief 为空时退到 content（部分条目只有 content）。
        let text = if brief.is_empty() {
            content.to_string()
        } else {
            brief
        };
        if text.trim().is_empty() {
            continue;
        }
        let ctime = obj
            .get("ctime")
            .and_then(|x| x.as_number())
            .map(|n| n as u64)
            .unwrap_or(0);
        let level = obj
            .get("level")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        items.push(FallbackItem {
            ctime,
            brief: text,
            level,
        });
    }
    Ok(items)
}

/// 同步 GET 文本（复用 scripts.rs fetch_external_script 的 spawn+tokio 模式）。
fn fetch_sync_text(url: &str) -> Result<String, String> {
    let url = url.to_string();
    let handle = std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| format!("tokio rt: {e}"))?;
        let client = browser_net::HttpClient::new();
        let bytes = rt
            .block_on(client.get(&url))
            .map_err(|e| format!("{e:?}"))?;
        String::from_utf8(bytes).map_err(|e| format!("non-utf8: {e}"))
    });
    handle
        .join()
        .map_err(|_| "fetch thread panicked".to_string())?
}

/// 从移动版 HTML 里抠出 `roll_data` 后面那个 JSON 数组的原文。
///
/// 页面模式：`"roll_data":[...]` 或 `roll_data": [...]`。定位 `roll_data`
/// 后第一个 `[`，按括号深度配对找到闭合 `]`，返回含外层方括号的子串。
fn extract_roll_data_json(html: &str) -> Result<String, String> {
    let key = "roll_data";
    let mut search_from = 0;
    loop {
        let Some(rel) = html[search_from..].find(key) else {
            return Err("roll_data not found".into());
        };
        let key_pos = search_from + rel;
        search_from = key_pos + key.len();
        // key 之后跳过可选的引号/冒号/空白，找第一个 '['。
        let tail = &html[key_pos + key.len()..];
        let bracket_rel = tail.find('[');
        if let Some(brel) = bracket_rel {
            let bracket_pos = key_pos + key.len() + brel;
            // 配对括号。
            let bytes = html.as_bytes();
            let mut depth = 0i32;
            let mut in_str = false;
            let mut esc = false;
            let mut i = bracket_pos;
            while i < bytes.len() {
                let c = bytes[i];
                if in_str {
                    if esc {
                        esc = false;
                    } else if c == b'\\' {
                        esc = true;
                    } else if c == b'"' {
                        in_str = false;
                    }
                } else if c == b'"' {
                    in_str = true;
                } else if c == b'[' {
                    depth += 1;
                } else if c == b']' {
                    depth -= 1;
                    if depth == 0 {
                        return Ok(html[bracket_pos..=i].to_string());
                    }
                }
                i += 1;
            }
            // 括号未闭合：继续找下一个 roll_data 出现。
        }
    }
}

// ============================== 极简 JSON 解析器（自研，无依赖） ==============================
// 仅覆盖 cls.cn roll_data 用到的子集：object/array/string/number/bool/null。
// 不支持浮点科学计数法之外的边角、不支持注释。足够 SSR 内嵌结构化数据。

#[derive(Debug, Clone)]
enum Json {
    Null,
    // roll_data 不含布尔字段，但完整 JSON 类型集留 Bool 以备别的 host fetcher。
    #[allow(dead_code)]
    Bool(bool),
    Number(f64),
    Str(String),
    Array(Vec<Json>),
    Object(BTreeMap<String, Json>),
}

impl Json {
    fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }
    fn as_number(&self) -> Option<f64> {
        match self {
            Json::Number(n) => Some(*n),
            _ => None,
        }
    }
}

struct Parser<'a> {
    b: &'a [u8],
    i: usize,
}

fn parse_json(input: &str) -> Result<Json, String> {
    let mut p = Parser {
        b: input.as_bytes(),
        i: 0,
    };
    p.skip_ws();
    let v = p.parse_value()?;
    p.skip_ws();
    Ok(v)
}

impl<'a> Parser<'a> {
    fn skip_ws(&mut self) {
        while self.i < self.b.len() && self.b[self.i].is_ascii_whitespace() {
            self.i += 1;
        }
    }
    fn parse_value(&mut self) -> Result<Json, String> {
        self.skip_ws();
        if self.i >= self.b.len() {
            return Err("unexpected end".into());
        }
        match self.b[self.i] {
            b'{' => self.parse_object(),
            b'[' => self.parse_array(),
            b'"' => Ok(Json::Str(self.parse_string()?)),
            b't' | b'f' => self.parse_bool(),
            b'n' => self.parse_null(),
            b'-' | b'0'..=b'9' => self.parse_number(),
            other => Err(format!("unexpected byte {other:?}",)),
        }
    }
    fn parse_object(&mut self) -> Result<Json, String> {
        // 当前 b[i] == '{'
        self.i += 1;
        let mut map = BTreeMap::new();
        self.skip_ws();
        if self.i < self.b.len() && self.b[self.i] == b'}' {
            self.i += 1;
            return Ok(Json::Object(map));
        }
        loop {
            self.skip_ws();
            let key = self.parse_string()?;
            self.skip_ws();
            if self.i >= self.b.len() || self.b[self.i] != b':' {
                return Err("expected ':' in object".into());
            }
            self.i += 1;
            let val = self.parse_value()?;
            map.insert(key, val);
            self.skip_ws();
            if self.i >= self.b.len() {
                return Err("unterminated object".into());
            }
            match self.b[self.i] {
                b',' => {
                    self.i += 1;
                    continue;
                }
                b'}' => {
                    self.i += 1;
                    break;
                }
                _ => return Err("expected ',' or '}'".into()),
            }
        }
        Ok(Json::Object(map))
    }
    fn parse_array(&mut self) -> Result<Json, String> {
        self.i += 1;
        let mut arr = Vec::new();
        self.skip_ws();
        if self.i < self.b.len() && self.b[self.i] == b']' {
            self.i += 1;
            return Ok(Json::Array(arr));
        }
        loop {
            let v = self.parse_value()?;
            arr.push(v);
            self.skip_ws();
            if self.i >= self.b.len() {
                return Err("unterminated array".into());
            }
            match self.b[self.i] {
                b',' => {
                    self.i += 1;
                    continue;
                }
                b']' => {
                    self.i += 1;
                    break;
                }
                _ => return Err("expected ',' or ']'".into()),
            }
        }
        Ok(Json::Array(arr))
    }
    fn parse_string(&mut self) -> Result<String, String> {
        if self.i >= self.b.len() || self.b[self.i] != b'"' {
            return Err("expected string".into());
        }
        self.i += 1;
        let mut out = String::new();
        while self.i < self.b.len() {
            let c = self.b[self.i];
            match c {
                b'"' => {
                    self.i += 1;
                    return Ok(out);
                }
                b'\\' => {
                    self.i += 1;
                    if self.i >= self.b.len() {
                        return Err("bad escape".into());
                    }
                    match self.b[self.i] {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'n' => out.push('\n'),
                        b't' => out.push('\t'),
                        b'r' => out.push('\r'),
                        b'b' => out.push('\u{0008}'),
                        b'f' => out.push('\u{000C}'),
                        b'u' => {
                            // 入口：self.b[self.i] == b'u'（\u 的 u）。
                            // 4 位 hex 在 self.i+1..self.i+5。
                            if self.i + 5 > self.b.len() {
                                return Err("bad \\u".into());
                            }
                            let hex = std::str::from_utf8(&self.b[self.i + 1..self.i + 5])
                                .map_err(|_| "bad \\u utf8".to_string())?;
                            let cp = u32::from_str_radix(hex, 16)
                                .map_err(|_| "bad \\u hex".to_string())?;
                            // 推进到 hex 最后一位（self.i+4）；循环尾的 +1 再越到之后。
                            self.i += 4;
                            if (0xD800..=0xDBFF).contains(&cp) {
                                // high surrogate，期望紧跟 \uXXXX low surrogate。
                                // 此时 self.i 指向 high 的末位 hex。下一段从 self.i+1 开始：
                                //   self.i+1 = '\', self.i+2 = 'u', self.i+3..+7 = low hex。
                                if self.i + 7 <= self.b.len()
                                    && self.b[self.i + 1] == b'\\'
                                    && self.b[self.i + 2] == b'u'
                                {
                                    let hex2 = std::str::from_utf8(&self.b[self.i + 3..self.i + 7])
                                        .map_err(|_| "bad \\u2 utf8".to_string())?;
                                    let lo = u32::from_str_radix(hex2, 16)
                                        .map_err(|_| "bad \\u2 hex".to_string())?;
                                    if (0xDC00..=0xDFFF).contains(&lo) {
                                        let scalar =
                                            0x10000 + ((cp - 0xD800) << 10) + (lo - 0xDC00);
                                        if let Some(ch) = char::from_u32(scalar) {
                                            out.push(ch);
                                        }
                                        // 推进到 low hex 末位；落到循环尾的 +1 越过。
                                        self.i += 6;
                                    } else {
                                        // low surrogate 无效：放 replacement char。
                                        out.push('\u{FFFD}');
                                    }
                                } else {
                                    // 没有紧跟的 low surrogate：放 replacement char。
                                    out.push('\u{FFFD}');
                                }
                            } else if let Some(ch) = char::from_u32(cp) {
                                out.push(ch);
                            }
                        }
                        other => return Err(format!("bad escape \\{other}")),
                    }
                    self.i += 1;
                }
                _ => {
                    // 原始字节：按 UTF-8 推进。多数情况下单字节直接 push。
                    // 找到下一个 ASCII 特殊字符，把中间的 UTF-8 连续段一次取出。
                    let start = self.i;
                    while self.i < self.b.len() && self.b[self.i] != b'"' && self.b[self.i] != b'\\'
                    {
                        self.i += 1;
                    }
                    let seg = std::str::from_utf8(&self.b[start..self.i])
                        .map_err(|_| "bad utf8 in string".to_string())?;
                    out.push_str(seg);
                }
            }
        }
        Err("unterminated string".into())
    }
    fn parse_bool(&mut self) -> Result<Json, String> {
        if self.b[self.i..].starts_with(b"true") {
            self.i += 4;
            Ok(Json::Bool(true))
        } else if self.b[self.i..].starts_with(b"false") {
            self.i += 5;
            Ok(Json::Bool(false))
        } else {
            Err("bad bool".into())
        }
    }
    fn parse_null(&mut self) -> Result<Json, String> {
        if self.b[self.i..].starts_with(b"null") {
            self.i += 4;
            Ok(Json::Null)
        } else {
            Err("bad null".into())
        }
    }
    fn parse_number(&mut self) -> Result<Json, String> {
        let start = self.i;
        if self.b[self.i] == b'-' {
            self.i += 1;
        }
        while self.i < self.b.len()
            && (self.b[self.i].is_ascii_digit()
                || self.b[self.i] == b'.'
                || self.b[self.i] == b'e'
                || self.b[self.i] == b'E'
                || self.b[self.i] == b'+'
                || self.b[self.i] == b'-')
        {
            self.i += 1;
        }
        let s = std::str::from_utf8(&self.b[start..self.i]).map_err(|_| "bad number utf8")?;
        s.parse::<f64>()
            .map(Json::Number)
            .map_err(|e| format!("bad number {s}: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_from_days_epoch() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        // 1970-01-01 + 365 天 = 1971-01-01（1970 不是闰年，365 天后正好跨年）
        assert_eq!(civil_from_days(365), (1971, 1, 1));
    }

    #[test]
    fn format_timestamp_handles_zero() {
        assert_eq!(format_timestamp(0), "--");
        // 一个已知时刻：1781633187（测试样本里的 ctime）。
        let s = format_timestamp(1_781_633_187);
        assert!(s.contains("2026"), "expected 2026, got {s}");
        assert_eq!(s.len(), 16); // "YYYY-MM-DD HH:MM"
    }

    #[test]
    fn extract_roll_data_finds_balanced_array() {
        let html = r#"foo "roll_data":[{"a":1,"s":"x]"},{"b":[1,2]}] bar"#;
        let j = extract_roll_data_json(html).unwrap();
        assert!(j.starts_with('['));
        assert!(j.ends_with(']'));
        let parsed = parse_json(&j).unwrap();
        match parsed {
            Json::Array(a) => assert_eq!(a.len(), 2),
            _ => panic!("not array"),
        }
    }

    #[test]
    fn extract_roll_data_handles_string_with_brackets() {
        // 字符串里的 ] 不能误判为闭合。
        let html = r#"roll_data":[{"title":"a]b","c":2}]"#;
        let j = extract_roll_data_json(html).unwrap();
        let parsed = parse_json(&j).unwrap();
        if let Json::Array(a) = parsed {
            assert_eq!(a.len(), 1);
        } else {
            panic!();
        }
    }

    #[test]
    fn json_parser_handles_unicode_and_surrogate() {
        // \u4e2d = 中；代理对 𝕏 (U+1D54F)。
        let j = r#""\u4e2d\ud835\udd4f""#;
        let v = parse_json(j).unwrap();
        assert_eq!(v.as_str().unwrap(), "中\u{1D54F}");
    }

    #[test]
    fn json_parser_numbers_and_bools() {
        let v = parse_json(r#"[1, -2.5, true, false, null]"#).unwrap();
        if let Json::Array(a) = v {
            assert_eq!(a.len(), 5);
            assert_eq!(a[0].as_number(), Some(1.0));
            assert_eq!(a[1].as_number(), Some(-2.5));
        } else {
            panic!();
        }
    }

    #[test]
    fn try_csr_fallback_skips_unregistered_host() {
        // 非注册 host（example.com）→ 直接 Ok(false)，不 fetch。
        use browser_html_parser::parse as parse_html;
        use std::cell::RefCell;
        use std::rc::Rc;
        let tree = parse_html("<html><body><p>x</p></body></html>");
        let shared: SharedTree = Rc::new(RefCell::new(tree));
        let r = try_csr_fallback(&shared, "https://example.com/page").unwrap();
        assert!(!r, "unregistered host should not trigger fallback");
    }

    #[test]
    fn try_csr_fallback_skips_about_blank() {
        use browser_html_parser::parse as parse_html;
        use std::cell::RefCell;
        use std::rc::Rc;
        let tree = parse_html("<html><body><p>x</p></body></html>");
        let shared: SharedTree = Rc::new(RefCell::new(tree));
        assert!(!try_csr_fallback(&shared, "about:blank").unwrap());
        assert!(!try_csr_fallback(&shared, "").unwrap());
    }

    #[test]
    fn host_matcher_matches_cls_variants() {
        assert!(host_is_cls_cn("www.cls.cn"));
        assert!(host_is_cls_cn("cls.cn"));
        assert!(host_is_cls_cn("m.cls.cn"));
        assert!(!host_is_cls_cn("example.com"));
    }

    #[test]
    fn format_item_renders_crawler_friendly_line() {
        let it = FallbackItem {
            ctime: 1_781_633_187,
            brief: "财联社6月17日电，测试。".into(),
            level: "B".into(),
        };
        let line = format_item(&it);
        assert!(line.contains("[2026-"), "got {line}");
        assert!(line.contains("[B]"), "got {line}");
        assert!(line.contains("测试"), "got {line}");
    }

    #[test]
    fn inject_items_appends_to_body() {
        use crate::bridge::body_text_content;
        use browser_html_parser::parse as parse_html;
        use std::cell::RefCell;
        use std::rc::Rc;
        let tree = parse_html("<html><body><p>shell</p></body></html>");
        let shared: SharedTree = Rc::new(RefCell::new(tree));
        {
            let mut t = shared.borrow_mut();
            let items = vec![FallbackItem {
                ctime: 1_781_633_187,
                brief: "电报正文A".into(),
                level: "C".into(),
            }];
            assert!(inject_items(&mut t, &items));
        }
        let text = body_text_content(&shared.borrow());
        assert!(text.contains("电报正文A"), "got {text}");
        assert!(text.contains("CSR 兜底"), "got {text}");
    }
}
