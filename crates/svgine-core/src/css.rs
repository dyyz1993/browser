//! CSS 子集（车道P 扩展）：`<style>` 规则解析 + 选择器链匹配 + 级联排序。
//!
//! 支持的选择器（CSS2 子集，styling-css-02/03/04 口径）：
//!
//! | 形式 | specificity 贡献 | 例 |
//! |------|------------------|-----|
//! | `tag` | tag=1 | `rect { fill: red }` |
//! | `*` | 0 | `* { stroke: red }` |
//! | `.class` | class=10 | `.accent { fill: green }` |
//! | `#id` | id=100 | `#hero { fill: gold }` |
//! | `[attr]` / `[attr=v]` / `[attr~=v]` / `[attr|=v]` | 10 | `[cursor="help"]` |
//! | `:first-child` | 10 | `rect:first-child` |
//! | `:hover` / `:active` / `:focus` | 10 | `rect:hover`（车道GG） |
//! | 组合器：后代空格 / `>` 子 / `+` 相邻 | 0 | `#a > rect`、`.a + .b` |
//!
//! `~`（通用兄弟）与其余伪类不支持——含它们的整条规则静默丢弃
//! （Chrome 语义：非法规则不阻断后续规则）。
//!
//! 状态伪类（车道GG，2026-09-12）：`:hover`/`:active`/`:focus` 解析进
//! [`Compound`] 的状态位，匹配时查 [`NodeSource::state_flags`]（宿主
//! 注入的交互状态；缺省恒 0 = 规则永不匹配 = 既有静态语义逐字节不变）。
//! 状态语义（CSS Selectors L4 + Blink 实证）：
//! - `:hover`：指针 designated 元素**及其祖先链**都匹配（容器 hover——
//!   下拉菜单 `li:hover ul` 语义的地基）；
//! - `:active`：按压（mousedown~mouseup）元素及其祖先链；
//! - `:focus`：聚焦元素自身（**不**向祖先传播；`:focus-within` 不做）。
//!
//! specificity 各计类级（10），与 CSS 伪类约定一致。
//!
//! 级联三明治（SVG 语义，由 builder 的应用顺序实现）：
//! **presentation attribute < CSS 规则（specificity 升序，同分源序）<
//! `style=""` 内联**。链匹配需要树导航（祖先/前兄弟），由
//! [`NodeSource`] 抽象——builder 桥接 Tree/ShapeAttrs，本模块可独立测。

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
// 车道CI：预算挂死测试用 std 计时（no_std crate 测试侧前例）
#[cfg(test)]
extern crate std;

/// 一条规则：选择器链 + 声明块。
#[derive(Clone, Debug, PartialEq)]
pub struct Rule {
    pub sel: Selector,
    /// specificity：Σ(id=100 / class·attr·伪类=10 / tag=1，`*`=0)。
    pub specificity: u32,
    /// 源序（同分后定义胜）。
    pub order: u32,
    /// 声明列表（属性名已小写化；同块内同名属性后者覆盖前者）。
    pub decls: Vec<(String, String)>,
}

/// 属性选择器算子。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AttrOp {
    /// `[attr]` 存在即可。
    Exists,
    /// `[attr="v"]` 全等。
    Equals,
    /// `[attr~="v"]` 空白分隔词表包含。
    Includes,
    /// `[attr|="v"]` 全等或 `v-` 前缀（语言码风格）。
    DashMatch,
}

/// 单个属性选择器。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttrSel {
    pub name: String,
    pub op: AttrOp,
    pub value: String,
}

/// 复合选择器：一段内（无组合器）的 simple selector 序列。
/// `tag = None` 表示 `*` 或未写 tag（匹配任意标签）。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Compound {
    pub tag: Option<String>,
    pub id: Option<String>,
    pub classes: Vec<String>,
    pub attrs: Vec<AttrSel>,
    pub first_child: bool,
    /// `:hover`（车道GG）：宿主交互状态位（见 [`NodeSource::state_flags`]）。
    pub hover: bool,
    /// `:active`：按压态。
    pub active: bool,
    /// `:focus`：聚焦态。
    pub focus: bool,
}

/// [`NodeSource::state_flags`] 返回的交互状态位（车道GG）。
pub const STATE_HOVER: u8 = 1;
/// 按压态位（mousedown~mouseup）。
pub const STATE_ACTIVE: u8 = 2;
/// 聚焦态位。
pub const STATE_FOCUS: u8 = 4;

/// 组合器。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Combinator {
    /// 后代（空白）。
    Descendant,
    /// `>` 直接子。
    Child,
    /// `+` 相邻兄弟。
    Adjacent,
}

/// 选择器链：`first` 最左；`rest[i].0` 连接其左邻。
/// `A > B C` = first:A, rest:[(Child, B), (Descendant, C)]。
/// 注意 `A * B` 的 `*` 匹配**真实中间元素**（CSS 语义，非零层通配）。
#[derive(Clone, Debug, PartialEq)]
pub struct Selector {
    pub first: Compound,
    pub rest: Vec<(Combinator, Compound)>,
}

/// `<style>` 元素文本解析出的规则表（按源序存放）。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StyleSheet {
    pub rules: Vec<Rule>,
}

/// 解析 `<style>` 文本：先剥 `/* ... */` 注释，再按 `}` 分块，
/// 每块 `selector { decl; decl }`。无法理解的选择器整条静默丢弃
/// （Chrome 语义：非法规则不阻断后续规则）。
pub fn parse(text: &str) -> StyleSheet {
    let cleaned = strip_comments(text);
    let mut rules = Vec::new();
    let mut order = 0u32;
    let mut rest = cleaned.as_str();
    while let Some(brace) = rest.find('{') {
        let (sel_text, after) = rest.split_at(brace);
        let body = match after.find('}') {
            Some(end) => &after[1..end],
            None => &after[1..], // 未闭合：尽力取到结尾
        };
        rest = match after.find('}') {
            Some(end) => &after[end + 1..],
            None => "",
        };
        // 逗号分组：`#one, #two { … }` = 两条同声明规则（各持 specificity，
        // 源序连续递增——同组内书写顺序即级联顺序）
        for part in split_top_commas(sel_text) {
            if let Some(sel) = parse_selector(part) {
                let specificity = sel.specificity();
                rules.push(Rule {
                    sel,
                    specificity,
                    order,
                    decls: parse_decls(body),
                });
                order += 1;
            }
        }
    }
    StyleSheet { rules }
}

/// 按顶层逗号分段（`[...]`/引号内的逗号不分）。
fn split_top_commas(text: &str) -> alloc::vec::Vec<&str> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut depth = 0i32;
    let mut quote = 0u8;
    let mut start = 0;
    for (i, &b) in bytes.iter().enumerate() {
        if quote != 0 {
            if b == quote {
                quote = 0;
            }
        } else {
            match b {
                b'"' | b'\'' => quote = b,
                b'[' => depth += 1,
                b']' => depth -= 1,
                b',' if depth <= 0 => {
                    out.push(&text[start..i]);
                    start = i + 1;
                }
                _ => {}
            }
        }
    }
    out.push(&text[start..]);
    out
}

/// 声明块解析（也复用于 `style=""` 内联属性）：按 `;` 分段，每段
/// `name: value`（首个 `:`）；属性名 trim + 小写化，值 trim；空段跳过。
pub fn parse_decls(block: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for part in block.split(';') {
        let Some((name, value)) = part.split_once(':') else {
            continue;
        };
        let name = name.trim();
        if name.is_empty() {
            continue;
        }
        out.push((name.to_ascii_lowercase(), String::from(value.trim())));
    }
    out
}

/// 样式表内属性选择器引用的属性名去重表（builder 在解析期只暂存这些
/// 原文属性——选择器匹配需要，ShapeAttrs 类型化字段不保留原文）。
pub fn attr_names(sheet: &StyleSheet) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for r in &sheet.rules {
        for comp in core::iter::once(&r.sel.first).chain(r.sel.rest.iter().map(|(_, c)| c)) {
            for a in &comp.attrs {
                if !out.iter().any(|n| n == &a.name) {
                    out.push(a.name.clone());
                }
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// 选择器解析

/// 选择器链解析（车道P）：compound 序列 + 三种组合器。
/// 不支持（整条丢弃）：`~` 通用兄弟、`:first-child` 外的伪类、
/// 命名空间竖线、CSS 转义。
fn parse_selector(text: &str) -> Option<Selector> {
    let mut first: Option<Compound> = None;
    let mut rest: Vec<(Combinator, Compound)> = Vec::new();
    // pending_comb：已见而未落定的组合器（None=尚无，Some(None)=隐式后代）
    let mut pending: Option<Option<Combinator>> = None;
    let bytes = text.as_bytes();
    let mut i = 0;
    loop {
        // 跳过空白（记录「见过空白」）
        let ws_start = i;
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let had_ws = i > ws_start;
        if i >= bytes.len() {
            break;
        }
        // 显式组合器
        let explicit = match bytes[i] {
            b'>' => Some(Combinator::Child),
            b'+' => Some(Combinator::Adjacent),
            b'~' => return None, // 通用兄弟不支持
            _ => None,
        };
        if let Some(c) = explicit {
            if first.is_none() || pending.is_some() {
                return None; // 组合器开头/连续（如 `> a`、`a > > b`）
            }
            pending = Some(Some(c));
            i += 1;
            // 组合器后的空白跳过
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            if i >= bytes.len() {
                return None; // 以组合器结尾
            }
        } else if had_ws && first.is_some() {
            // 隐式后代（compound 之间有空白）
            if pending.is_some() {
                return None; // 空白跟在显式组合器后已在上面消化，异常态
            }
            pending = Some(Some(Combinator::Descendant));
        }
        // 解析一个 compound
        let comp = match parse_compound(text, &mut i) {
            Some(c) => c,
            None => {
                // 无 compound 可解析：first 已有且无悬挂组合器 = 选择器
                // 正常结束（残余字符如收尾 ']' 停驻——compound 的 break
                // 臂不消费它）
                if first.is_some() && pending.is_none() {
                    break;
                }
                return None;
            }
        };
        match pending.take() {
            None => {
                if first.is_some() {
                    return None; // 两个 compound 无组合器相邻
                }
                first = Some(comp);
            }
            Some(c) => {
                let c = c?;
                rest.push((c, comp));
            }
        }
    }
    let first = first?;
    Some(Selector { first, rest })
}

/// 从 `i` 起解析一个 compound，推进 `i` 越过其文本。
fn parse_compound(text: &str, i: &mut usize) -> Option<Compound> {
    let bytes = text.as_bytes();
    let mut comp = Compound::default();
    let mut any = false;
    while *i < bytes.len() {
        match bytes[*i] {
            b'*' => {
                any = true; // 通配：tag 保持 None（specificity 0）
                *i += 1;
            }
            b'#' => {
                *i += 1; // 越过 '#'
                comp.id = Some(ident_at(text, i)?);
                any = true;
            }
            b'.' => {
                *i += 1; // 越过 '.'
                let name = ident_at(text, i)?;
                comp.classes.push(name);
                any = true;
            }
            b'[' => {
                let a = parse_attr_sel(text, i)?;
                comp.attrs.push(a);
                any = true; // parse_attr_sel 返回时 i 停在 ']'
            }
            b']' => break,
            b':' => {
                // :first-child + 状态伪类 :hover/:active/:focus（车道GG）；
                // :: 伪元素与其余伪类不支持（整条丢弃）
                if bytes.get(*i + 1) == Some(&b':') {
                    return None;
                }
                *i += 1; // 越过 ':'
                let name = ident_at(text, i)?;
                if name.eq_ignore_ascii_case("first-child") {
                    comp.first_child = true;
                } else if name.eq_ignore_ascii_case("hover") {
                    comp.hover = true;
                } else if name.eq_ignore_ascii_case("active") {
                    comp.active = true;
                } else if name.eq_ignore_ascii_case("focus") {
                    comp.focus = true;
                } else {
                    return None;
                }
                any = true;
            }
            c if c.is_ascii_alphanumeric() || c == b'_' || c == b'-' => {
                if comp.tag.is_some() {
                    return None; // tag 只能出现一次（打头）
                }
                comp.tag = Some(ident_at(text, i)?);
                any = true;
            }
            _ => break, // compound 结束（空白/组合器/结尾）
        }
    }
    if any {
        Some(comp)
    } else {
        None
    }
}

/// 从 `*i` 起读 ident（字母数字 `-` `_`），推进游标；空 ident → None。
fn ident_at(text: &str, i: &mut usize) -> Option<String> {
    let bytes = text.as_bytes();
    let s = *i;
    let mut e = s;
    while e < bytes.len()
        && (bytes[e].is_ascii_alphanumeric() || bytes[e] == b'-' || bytes[e] == b'_')
    {
        e += 1;
    }
    if e == s {
        return None;
    }
    *i = e;
    Some(String::from(&text[s..e]))
}

/// `[...]` 属性选择器解析：调用时 `i` 指向 '['；返回时 `i` 落在 ']' 上。
fn parse_attr_sel(text: &str, i: &mut usize) -> Option<AttrSel> {
    *i += 1; // 越过 '['
    let bytes = text.as_bytes();
    while *i < bytes.len() && bytes[*i].is_ascii_whitespace() {
        *i += 1;
    }
    let name_start = *i;
    while *i < bytes.len()
        && !bytes[*i].is_ascii_whitespace()
        && bytes[*i] != b']'
        && bytes[*i] != b'='
        && bytes[*i] != b'~'
        && bytes[*i] != b'|'
    {
        *i += 1;
    }
    if *i == name_start {
        return None;
    }
    let name = String::from(&text[name_start..*i]).to_ascii_lowercase();
    // 算子
    let op = match bytes.get(*i) {
        Some(b']') => AttrOp::Exists,
        Some(b'=') => {
            *i += 1;
            AttrOp::Equals
        }
        Some(b'~') if bytes.get(*i + 1) == Some(&b'=') => {
            *i += 2;
            AttrOp::Includes
        }
        Some(b'|') if bytes.get(*i + 1) == Some(&b'=') => {
            *i += 2;
            AttrOp::DashMatch
        }
        _ => return None,
    };
    // 值（可带引号；两侧空白容忍）
    while *i < bytes.len() && bytes[*i].is_ascii_whitespace() {
        *i += 1;
    }
    let value = match bytes.get(*i) {
        Some(q @ (b'"' | b'\'')) => {
            *i += 1;
            let v_start = *i;
            while *i < bytes.len() && bytes[*i] != *q {
                *i += 1;
            }
            if *i >= bytes.len() {
                return None;
            }
            let v = String::from(&text[v_start..*i]);
            *i += 1; // 越过闭引号
            v
        }
        _ => {
            let v_start = *i;
            while *i < bytes.len() && !bytes[*i].is_ascii_whitespace() && bytes[*i] != b']' {
                *i += 1;
            }
            String::from(&text[v_start..*i])
        }
    };
    while *i < bytes.len() && bytes[*i].is_ascii_whitespace() {
        *i += 1;
    }
    if bytes.get(*i) != Some(&b']') {
        return None;
    }
    Some(AttrSel { name, op, value })
}

impl Selector {
    /// specificity：Σ(id=100 / class·attr·伪类=10 / tag=1，`*`=0)。
    pub fn specificity(&self) -> u32 {
        let mut s = 0u32;
        for comp in core::iter::once(&self.first).chain(self.rest.iter().map(|(_, c)| c)) {
            if comp.id.is_some() {
                s += 100;
            }
            s += 10
                * ((comp.classes.len()
                    + comp.attrs.len()
                    + comp.first_child as usize
                    + comp.hover as usize
                    + comp.active as usize
                    + comp.focus as usize) as u32);
            if comp.tag.is_some() {
                s += 1;
            }
        }
        s
    }
}

// ---------------------------------------------------------------------------
// 匹配

/// 单规则匹配的 compound 试探总预算（车道CI parser-fuzz：后代组合器的
/// 回溯在「深树 × 长链 × 链头不命中」形态下组合爆炸——实测 6 链×40 层
/// 456ms、8 链分钟级挂死）。超预算按不匹配降级（宽容语义）；真实语料
/// 链 ≤6 compound × 树深 <30，耗用远低于预算线。
const MATCH_BUDGET: u32 = 100_000;

/// 选择器匹配所需的节点查询（builder 侧桥接 Tree/ShapeAttrs/属性暂存）。
pub trait NodeSource {
    /// (tag 名，class 属性原文，id 属性值)。tag 不在可匹配集 → None。
    fn tag_class_id(&self, id: u32) -> (Option<&str>, Option<&str>, Option<&str>);
    /// 属性选择器用的原文属性值（只查样式表引用过的名字）。
    fn attr_value(&self, id: u32, name: &str) -> Option<&str>;
    /// 父节点（无 → None）。
    fn parent(&self, id: u32) -> Option<u32>;
    /// 前一个兄弟（无 → None）。
    fn prev_sibling(&self, id: u32) -> Option<u32>;
    /// 交互状态位（[`STATE_HOVER`]/[`STATE_ACTIVE`]/[`STATE_FOCUS`] 之
    /// 或；车道GG）。缺省恒 0 = 无任何状态 = 状态伪类规则永不匹配
    /// （静态渲染与无状态注入逐字节同——零回归缺省）。
    fn state_flags(&self, _id: u32) -> u8 {
        0
    }
}

/// 单 compound 对节点匹配。
fn compound_matches(comp: &Compound, src: &dyn NodeSource, node: u32) -> bool {
    let (tag, class_attr, id_attr) = src.tag_class_id(node);
    if let Some(want) = &comp.tag {
        if tag != Some(want.as_str()) {
            return false;
        }
    }
    if let Some(want) = &comp.id {
        if id_attr != Some(want.as_str()) {
            return false;
        }
    }
    if !comp.classes.is_empty() {
        let Some(classes) = class_attr else {
            return false;
        };
        for want in &comp.classes {
            if !classes.split_whitespace().any(|c| c == want.as_str()) {
                return false;
            }
        }
    }
    for a in &comp.attrs {
        let Some(v) = src.attr_value(node, &a.name) else {
            return false;
        };
        let ok = match a.op {
            AttrOp::Exists => true,
            AttrOp::Equals => v.trim() == a.value,
            AttrOp::Includes => v.split_ascii_whitespace().any(|w| w == a.value),
            AttrOp::DashMatch => {
                v == a.value
                    || (v.len() > a.value.len()
                        && v.starts_with(&a.value)
                        && v.as_bytes()[a.value.len()] == b'-')
            }
        };
        if !ok {
            return false;
        }
    }
    if comp.first_child {
        // 有父且是第一个**元素**子（无前兄弟即首元素）
        if src.parent(node).is_none() || src.prev_sibling(node).is_some() {
            return false;
        }
    }
    // 状态伪类（车道GG）：要求的状态位必须全部在场。
    let flags = src.state_flags(node);
    if comp.hover && flags & STATE_HOVER == 0 {
        return false;
    }
    if comp.active && flags & STATE_ACTIVE == 0 {
        return false;
    }
    if comp.focus && flags & STATE_FOCUS == 0 {
        return false;
    }
    true
}

/// 链回溯匹配：`parts[idx]` 已匹配 `node`，向左消化。
/// `budget`：compound 试探总预算（见 `MATCH_BUDGET`），耗尽按不匹配
/// 返回（挂死防护，车道CI）。
fn match_up(
    parts: &[&Compound],
    combs: &[Option<Combinator>],
    src: &dyn NodeSource,
    idx: usize,
    node: u32,
    budget: &mut u32,
) -> bool {
    if idx == 0 {
        return true;
    }
    // combs[idx-1] 连接 parts[idx-1]（左）与 parts[idx]（右）
    let Some(comb) = combs[idx - 1] else {
        return false;
    };
    let left = parts[idx - 1];
    match comb {
        Combinator::Child => match src.parent(node) {
            Some(p) => {
                spend(budget)
                    && compound_matches(left, src, p)
                    && match_up(parts, combs, src, idx - 1, p, budget)
            }
            None => false,
        },
        Combinator::Adjacent => match src.prev_sibling(node) {
            Some(p) => {
                spend(budget)
                    && compound_matches(left, src, p)
                    && match_up(parts, combs, src, idx - 1, p, budget)
            }
            None => false,
        },
        Combinator::Descendant => {
            let mut cur = node;
            while let Some(anc) = src.parent(cur) {
                if !spend(budget) {
                    return false;
                }
                if compound_matches(left, src, anc)
                    && match_up(parts, combs, src, idx - 1, anc, budget)
                {
                    return true;
                }
                cur = anc;
            }
            false
        }
    }
}

/// 预算扣减：耗尽（0）返回 false 并不再扣（防 u32 下溢）。
fn spend(budget: &mut u32) -> bool {
    if *budget == 0 {
        return false;
    }
    *budget -= 1;
    true
}

impl Selector {
    /// 完整链匹配 `node`（右端 compound 对节点自身）。
    pub fn matches(&self, src: &dyn NodeSource, node: u32) -> bool {
        let mut parts: Vec<&Compound> = vec![&self.first];
        let mut combs: Vec<Option<Combinator>> = Vec::new();
        for (c, comp) in &self.rest {
            combs.push(Some(*c));
            parts.push(comp);
        }
        let last = parts.len() - 1;
        let mut budget = MATCH_BUDGET;
        spend(&mut budget)
            && compound_matches(parts[last], src, node)
            && match_up(&parts, &combs, src, last, node, &mut budget)
    }
}

/// 收集元素匹配的规则，按级联升序（specificity，再源序）排列——
/// 应用时顺序覆盖，最后应用的即胜者。返回引用避免整条克隆。
pub fn matching_rules<'a>(sheet: &'a StyleSheet, src: &dyn NodeSource, node: u32) -> Vec<&'a Rule> {
    let mut matched: Vec<&Rule> = sheet
        .rules
        .iter()
        .filter(|r| r.sel.matches(src, node))
        .collect();
    matched.sort_by_key(|r| (r.specificity, r.order));
    matched
}

/// 剥除 `/* ... */` 注释（未闭合注释尽力取到结尾）。
fn strip_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("/*") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        match after.find("*/") {
            Some(end) => rest = &after[end + 2..],
            None => {
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

/// 从 `<style>` 文本中剥除嵌套花括号的 at-rule 块（`@media` / `@supports`，
/// 大小写不敏感；车道CJ）。
///
/// 动机（同族于 `anim::strip_keyframes`）：[`parse`] 朴素按首个 `}` 分块，
/// 嵌套块的内层 `}` 提前闭合外层 → **外层 `}` 混进下一规则的 selector**
/// （`} .next {`）而把块后第一条规则整条丢弃。内层规则本就不评估（引擎
/// 无媒体/支持查询语境，能力位降级 = 整块忽略），但不该殃及块后规则。
/// `@font-face` 等扁平 at-rule 无嵌套花括号，不受此影响（selector 解析
/// 失败单条丢弃，无连带）。
pub fn strip_nested_at(text: &str) -> String {
    let cleaned = strip_comments(text);
    let mut out = String::with_capacity(cleaned.len());
    let mut rest = cleaned.as_str();
    while let Some(at) = find_nested_at(rest) {
        out.push_str(&rest[..at]);
        let after = &rest[at..];
        // 块从首个 '{' 起，配对 '}' 结束；未闭合 → 尽力剥到结尾
        rest = match after.find('{') {
            Some(brace) => match matching_brace(&after[brace..]) {
                Some(e) if brace + e + 1 < after.len() => &after[brace + e + 1..],
                _ => "",
            },
            None => "",
        };
    }
    out.push_str(rest);
    out
}

/// 定位下一个 `@media` / `@supports` 的起始下标（词边界防 `@mediax`）。
fn find_nested_at(s: &str) -> Option<usize> {
    const MEDIA: &[u8] = b"@media";
    const SUPPORTS: &[u8] = b"@supports";
    let bytes = s.as_bytes();
    let ident = |b: u8| b.is_ascii_alphanumeric() || b == b'-' || b == b'_';
    for i in 0..bytes.len() {
        for pat in [MEDIA, SUPPORTS] {
            if i + pat.len() <= bytes.len()
                && bytes[i..i + pat.len()].eq_ignore_ascii_case(pat)
                && !bytes.get(i + pat.len()).is_some_and(|b| ident(*b))
            {
                return Some(i);
            }
        }
    }
    None
}

/// `s[0] == '{'` 时找配对的 '}'（嵌套 +1），返回其下标；未闭合 → None。
fn matching_brace(s: &str) -> Option<usize> {
    let mut depth = 0i32;
    for (i, b) in s.bytes().enumerate() {
        match b {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::format;
    use alloc::vec;
    use alloc::vec::Vec;

    /// strip_nested_at（车道CJ）：嵌套 at-rule 整块剥除、块后规则存活。
    #[test]
    fn strip_nested_at_removes_media_and_supports() {
        let css =
            "@media (min-width:10px) { .a { fill: red } .b { fill: blue } } .c { fill: green }";
        let out = strip_nested_at(css);
        assert!(!out.contains(".a"), "内层规则剥除：{out}");
        assert!(
            out.trim_start().starts_with(".c"),
            "块后规则存活且无残骸：{out}"
        );
        // @supports 同款；大小写不敏感
        let css = "@SUPPORTS (display: grid) { .x { y: z } } #d { e: f }";
        let out = strip_nested_at(css);
        assert!(!out.contains(".x"), "@supports 剥除：{out}");
        assert!(out.contains("#d"), "块后规则存活：{out}");
        // 词边界：@mediax 不是 @media
        let css = "@mediax .a { fill: red }";
        assert_eq!(strip_nested_at(css), css, "非嵌套 at-rule 不动");
        // 未闭合：剥到结尾
        let out = strip_nested_at("@media screen { .a { fill: red }");
        assert_eq!(out, "");
    }

    /// 测试源节点：(tag, class, id属性, 父, 前兄弟, 属性KV)
    type MockNode = (
        &'static str,
        &'static str,
        &'static str,
        Option<u32>,
        Option<u32>,
        Vec<(&'static str, &'static str)>,
    );

    /// 测试源：id → MockNode
    struct Mock {
        nodes: Vec<MockNode>,
    }
    impl NodeSource for Mock {
        fn tag_class_id(&self, id: u32) -> (Option<&str>, Option<&str>, Option<&str>) {
            let n = &self.nodes[id as usize];
            (
                Some(n.0),
                if n.1.is_empty() { None } else { Some(n.1) },
                if n.2.is_empty() { None } else { Some(n.2) },
            )
        }
        fn attr_value(&self, id: u32, name: &str) -> Option<&str> {
            let n = &self.nodes[id as usize];
            n.5.iter().find(|(k, _)| *k == name).map(|(_, v)| *v)
        }
        fn parent(&self, id: u32) -> Option<u32> {
            self.nodes[id as usize].3
        }
        fn prev_sibling(&self, id: u32) -> Option<u32> {
            self.nodes[id as usize].4
        }
    }

    /// 基础四类 + specificity。
    #[test]
    fn parses_basic_selector_kinds() {
        let sheet = parse(
            "rect { fill: #ff0000 } .accent { fill: green } #hero { fill: gold } rect.stripe { fill: blue }",
        );
        assert_eq!(sheet.rules.len(), 4);
        assert_eq!(sheet.rules[0].specificity, 1);
        assert_eq!(sheet.rules[1].specificity, 10);
        assert_eq!(sheet.rules[2].specificity, 100);
        assert_eq!(sheet.rules[3].specificity, 11);
        assert_eq!(
            sheet.rules.iter().map(|r| r.order).collect::<Vec<_>>(),
            vec![0, 1, 2, 3]
        );
        // 逗号分组：#one/#two 两条同声明规则 + rect 第三条
        let sheet2 = parse("#one, #two { fill: red } rect { fill: b }");
        assert_eq!(sheet2.rules.len(), 3);
        assert_eq!(sheet2.rules[0].specificity, 100);
        assert_eq!(sheet2.rules[1].specificity, 100);
        assert_eq!(sheet2.rules[2].specificity, 1);
    }

    /// 声明块变体：注释、无尾分号、大小写属性名、多空格。
    #[test]
    fn decl_block_variants() {
        let sheet = parse("/* c1 */ rect { FILL: #123456 ; stroke-width: 2 }");
        assert_eq!(sheet.rules.len(), 1);
        assert_eq!(
            sheet.rules[0].decls,
            vec![
                (String::from("fill"), String::from("#123456")),
                (String::from("stroke-width"), String::from("2")),
            ]
        );
        assert_eq!(
            parse_decls(" fill:#8E44AD "),
            vec![(String::from("fill"), String::from("#8E44AD"))]
        );
        assert!(parse_decls("fill").is_empty(), "无冒号段跳过");
    }

    /// 不支持的伪类/通用兄弟整条丢弃，不阻断后续。
    /// 车道GG：`:hover` 已支持（解析保留），`:nth-child`/`~` 仍丢弃。
    #[test]
    fn unsupported_selectors_skipped() {
        let sheet = parse(
            "rect:hover { fill: red } a ~ b { fill: pink } rect:nth-child(2) { fill: tan } rect { fill: gray }",
        );
        assert_eq!(sheet.rules.len(), 2, ":hover 解析保留 + rect 兜底");
        assert_eq!(sheet.rules[0].specificity, 11, "tag(1) + :hover(10)");
        assert_eq!(sheet.rules[1].decls[0].1, "gray");
    }

    /// 车道GG：状态伪类解析 + specificity + 组合；不支持形态仍丢弃。
    #[test]
    fn pseudo_class_parsing_and_specificity() {
        let sheet = parse(
            "rect:hover { fill: r } .b:ACTIVE { fill: a } #i:focus { fill: f } g:hover > circle:hover { fill: c }",
        );
        assert_eq!(sheet.rules.len(), 4, "大小写不敏感 + 组合均解析");
        assert_eq!(sheet.rules[0].specificity, 11);
        assert_eq!(sheet.rules[1].specificity, 20, "class + :active");
        assert_eq!(sheet.rules[2].specificity, 110, "id + :focus");
        assert_eq!(
            sheet.rules[3].specificity, 22,
            "g(1)+:hover(10) > circle(1)+:hover(10)"
        );
        assert!(sheet.rules[0].sel.first.hover);
        assert!(sheet.rules[1].sel.first.active);
        assert!(!sheet.rules[1].sel.first.hover, "active 不连带 hover 位");
        assert!(sheet.rules[2].sel.first.focus);
        assert!(sheet.rules[3].sel.rest[0].1.hover);
        // 组合 :first-child:hover 与未知伪类/伪元素
        let sheet2 = parse("rect:first-child:hover { fill: x } rect::before { fill: y } rect:hover:focus { fill: z }");
        assert_eq!(sheet2.rules.len(), 2, "::before 丢弃，双伪类组合保留");
        assert!(sheet2.rules[0].sel.first.first_child && sheet2.rules[0].sel.first.hover);
        let both = &sheet2.rules[1].sel.first;
        assert!(both.hover && both.focus);
        assert_eq!(
            sheet2.rules[1].specificity, 21,
            "tag(1) + :hover(10) + :focus(10)"
        );
    }

    /// 车道GG：状态伪类匹配——state_flags 位驱动；无状态缺省恒不匹配
    /// （静态零回归缺省）；祖先链命中使 `g:hover circle`（下拉形态）
    /// 生效；兄弟节点不受连锁。
    #[test]
    fn pseudo_class_matching_with_state() {
        // 树：0 svg > 1 g#menu > 2 rect.a（item） 3 rect.b（兄弟）
        struct StMock {
            base: Mock,
            flags: Vec<u8>,
        }
        impl NodeSource for StMock {
            fn tag_class_id(&self, id: u32) -> (Option<&str>, Option<&str>, Option<&str>) {
                self.base.tag_class_id(id)
            }
            fn attr_value(&self, id: u32, name: &str) -> Option<&str> {
                self.base.attr_value(id, name)
            }
            fn parent(&self, id: u32) -> Option<u32> {
                self.base.parent(id)
            }
            fn prev_sibling(&self, id: u32) -> Option<u32> {
                self.base.prev_sibling(id)
            }
            fn state_flags(&self, id: u32) -> u8 {
                self.flags.get(id as usize).copied().unwrap_or(0)
            }
        }
        let base = Mock {
            nodes: vec![
                ("svg", "", "", None, None, vec![]),
                ("g", "", "menu", Some(0), None, vec![]),
                ("rect", "a", "", Some(1), None, vec![]),
                ("rect", "b", "", Some(1), Some(2), vec![]),
            ],
        };
        // hover 落在 rect.a（node 2）：链 = {2, 1, 0}（元素 + 祖先）
        let src = StMock {
            base,
            flags: vec![STATE_HOVER, STATE_HOVER, STATE_HOVER, 0],
        };
        let sheet = parse("rect:hover { fill: red }");
        assert_eq!(matching_rules(&sheet, &src, 2).len(), 1, "命中元素自身");
        assert_eq!(matching_rules(&sheet, &src, 3).len(), 0, "兄弟不连锁");
        let sheet = parse("g:hover circle { fill: red }");
        assert_eq!(matching_rules(&sheet, &src, 2).len(), 0, "非 circle 不命中");
        let sheet = parse("#menu:hover rect { fill: red }");
        assert_eq!(
            matching_rules(&sheet, &src, 2).len(),
            1,
            "祖先 hover（容器形态） + 后代主语"
        );
        assert_eq!(
            matching_rules(&sheet, &src, 3).len(),
            1,
            "g 下两个 rect 都是后代"
        );
        // 无状态缺省（state_flags 默认 0）：全不匹配
        let plain = Mock {
            nodes: vec![
                ("svg", "", "", None, None, vec![]),
                ("g", "", "menu", Some(0), None, vec![]),
                ("rect", "a", "", Some(1), None, vec![]),
                ("rect", "b", "", Some(1), Some(2), vec![]),
            ],
        };
        let sheet = parse("rect:hover { fill: red } g:hover rect { fill: blue }");
        assert_eq!(
            matching_rules(&sheet, &plain, 2).len(),
            0,
            "缺省无状态 = 不匹配"
        );
        // focus 不传播：只有被聚焦节点匹配 :focus
        let src = StMock {
            base: Mock {
                nodes: vec![
                    ("svg", "", "", None, None, vec![]),
                    ("g", "", "", Some(0), None, vec![]),
                    ("rect", "", "", Some(1), None, vec![]),
                ],
            },
            flags: vec![STATE_FOCUS, 0, STATE_FOCUS],
        };
        let sheet = parse("rect:focus { fill: red }");
        assert_eq!(matching_rules(&sheet, &src, 2).len(), 1, "聚焦元素");
        assert_eq!(matching_rules(&sheet, &src, 1).len(), 0, "非 rect 标签");
        // active 位独立：hover 规则不吃 active 位
        let src = StMock {
            base: Mock {
                nodes: vec![
                    ("svg", "", "", None, None, vec![]),
                    ("rect", "", "", Some(0), None, vec![]),
                ],
            },
            flags: vec![0, STATE_ACTIVE],
        };
        let sheet = parse("rect:active { fill: red } rect:hover { fill: blue }");
        assert_eq!(matching_rules(&sheet, &src, 1).len(), 1);
        assert_eq!(matching_rules(&sheet, &src, 1)[0].decls[0].1, "red");
    }

    /// 匹配 + 级联排序：tag < class < tag.class < #id，同分后定义胜。
    #[test]
    fn matching_rules_cascade_order() {
        // 元素：0 svg > 1 rect(class="s z", id=h)  2 circle
        let src = Mock {
            nodes: vec![
                ("svg", "", "", None, None, vec![]),
                ("rect", "s z", "h", Some(0), None, vec![]),
                ("circle", "", "", Some(0), Some(1), vec![]),
            ],
        };
        let sheet = parse(
            "rect { fill: a } .a { fill: b } rect.s { fill: c } #h { fill: d } .z { fill: late } rect { fill: second }",
        );
        // (rect,"s z",h)：命中 rect/.a✗/rect.s/#h/.z → 1,1,10,11,100
        let ordered = matching_rules(&sheet, &src, 1);
        let specifics: Vec<u32> = ordered.iter().map(|r| r.specificity).collect();
        assert_eq!(specifics, vec![1, 1, 10, 11, 100]);
        assert_eq!(ordered[0].decls[0].1, "a");
        assert_eq!(ordered[1].decls[0].1, "second");
        // circle 无 tag/class 命中 → 空
        assert!(matching_rules(&sheet, &src, 2).is_empty());
    }

    /// 组合器：后代 / 子 / 相邻 / `*` 需真实中间元素 / :first-child。
    #[test]
    fn combinator_matching() {
        // 树：0 svg > 1 g#alpha > 2 g#mid > 3 rect.a ; 4 rect.b (alpha 直子)
        let src = Mock {
            nodes: vec![
                ("svg", "", "", None, None, vec![]),
                ("g", "", "alpha", Some(0), None, vec![]),
                ("g", "", "mid", Some(1), None, vec![]),
                ("rect", "a", "", Some(2), None, vec![]),
                ("rect", "b", "", Some(1), Some(2), vec![]),
                ("rect", "c", "", Some(1), Some(3), vec![]),
            ],
        };
        let sheet = parse("#alpha rect { fill: green }");
        assert!(matching_rules(&sheet, &src, 3).len() == 1, "深层后代命中");
        assert!(matching_rules(&sheet, &src, 4).len() == 1, "直接子也是后代");
        // 子组合器：#alpha > rect 只中直子
        let sheet = parse("#alpha > rect { fill: orange }");
        assert_eq!(matching_rules(&sheet, &src, 4).len(), 1);
        assert_eq!(matching_rules(&sheet, &src, 3).len(), 0, "非直子");
        // `*` 需真实中间元素：#alpha * rect 中 3（经 mid）不中 4
        let sheet = parse("#alpha * rect { fill: red }");
        assert_eq!(matching_rules(&sheet, &src, 3).len(), 1);
        assert_eq!(matching_rules(&sheet, &src, 4).len(), 0);
        // 相邻：rect.a + rect 只中 5（前兄弟 = 3 = rect.a）；4 的前兄弟
        // 是 2=g 不中
        let sheet = parse("rect.a + rect { fill: purple }");
        assert_eq!(matching_rules(&sheet, &src, 5).len(), 1);
        assert_eq!(matching_rules(&sheet, &src, 4).len(), 0);
        // :first-child
        let sheet = parse("rect:first-child { fill: red }");
        assert_eq!(matching_rules(&sheet, &src, 3).len(), 1, "mid 首子");
        assert_eq!(matching_rules(&sheet, &src, 4).len(), 0, "非首子");
    }

    /// 属性选择器：存在/等值/词表/破折号 + 特异性压过普通 tag 规则。
    #[test]
    fn attr_selector_matching() {
        let src = Mock {
            nodes: vec![
                ("svg", "", "", None, None, vec![]),
                ("g", "", "zeta", Some(0), None, vec![]),
                ("rect", "", "A3", Some(1), None, vec![("cursor", "default")]),
                ("rect", "", "B3", Some(1), Some(2), vec![("cursor", "help")]),
                ("rect", "", "C3", Some(1), Some(3), vec![("rx", "3E-6")]),
            ],
        };
        let sheet = parse("#zeta [cursor] { fill: blue }");
        assert_eq!(matching_rules(&sheet, &src, 2).len(), 1);
        assert_eq!(matching_rules(&sheet, &src, 4).len(), 0, "无 cursor");
        let sheet = parse("g#zeta [cursor=\"help\"] { fill: green }");
        assert_eq!(matching_rules(&sheet, &src, 3).len(), 1);
        assert_eq!(matching_rules(&sheet, &src, 2).len(), 0);
        // ~= 词表：值含 '-' 整词才等（"3E-6" 的词不等于 "3E"）
        let sheet = parse("#zeta [rx~=\"3E\"] { fill: orange }");
        assert_eq!(matching_rules(&sheet, &src, 4).len(), 0);
        // |= 破折号：等值或 v- 前缀
        let src2 = Mock {
            nodes: vec![
                ("svg", "", "", None, None, vec![]),
                ("rect", "", "", Some(0), None, vec![("lang", "en-US")]),
            ],
        };
        let sheet = parse("[lang|=\"en\"] { fill: gold }");
        assert_eq!(matching_rules(&sheet, &src2, 1).len(), 1);
        let sheet = parse("[lang|=\"en-US\"] { fill: gold }");
        assert_eq!(matching_rules(&sheet, &src2, 1).len(), 1);
        let sheet = parse("[lang|=\"fr\"] { fill: gold }");
        assert_eq!(matching_rules(&sheet, &src2, 1).len(), 0);
    }

    /// styling-css-04 口径：specificity 竞争——103 红压 102 紫。
    #[test]
    fn specificity_race_deep_chain() {
        // gamma > g1 > g2 > g3 > g4 > F1
        let src = Mock {
            nodes: vec![
                ("svg", "", "", None, None, vec![]),
                ("g", "", "gamma", Some(0), None, vec![]),
                ("g", "", "", Some(1), None, vec![]),
                ("g", "", "", Some(2), None, vec![]),
                ("g", "", "", Some(3), None, vec![]),
                ("g", "", "", Some(4), None, vec![]),
                ("rect", "", "", Some(5), None, vec![]),
            ],
        };
        let sheet = parse("g#gamma * * rect { fill: purple } g#gamma * g * * rect { fill: red }");
        let ordered = matching_rules(&sheet, &src, 6);
        assert_eq!(ordered.len(), 2);
        assert_eq!(ordered[0].specificity, 102);
        assert_eq!(ordered[1].specificity, 103);
        let last = ordered.last().expect("specificity 竞争应有命中");
        assert_eq!(last.decls[0].1, "red", "103 胜出");
    }

    /// 车道CI（parser-fuzz）：深树 × 长后代链（链头不命中）在回溯匹配
    /// 上组合爆炸（修前 6 链×40 层 456ms、8 链分钟级挂死）。预算内必须
    /// 有限时间返回、且按不匹配降级；预算内的正常匹配不受影响
    /// （matching_rules_cascade_order / specificity_race_deep_chain 钉）。
    #[test]
    fn match_budget_bounds_pathological_chain() {
        let mut nodes: Vec<MockNode> = vec![("svg", "", "", None, None, vec![])];
        for i in 1..=3000usize {
            nodes.push(("g", "", "", Some((i - 1) as u32), None, vec![]));
        }
        nodes.push(("rect", "", "", Some(3000), None, vec![]));
        let src = Mock { nodes };
        let sheet = parse(&format!("x {}rect {{ fill: red }}", "g ".repeat(200)));
        let t = std::time::Instant::now();
        let rules = matching_rules(&sheet, &src, 3001);
        assert!(rules.is_empty(), "链头不命中 → 预算内按不匹配降级");
        assert!(
            t.elapsed().as_millis() < 2_000,
            "组合爆炸必须被预算截断，took {:?}",
            t.elapsed()
        );
        // 预算内的正常后代匹配照常
        let sheet2 = parse("#deep rect { fill: red }");
        let nodes2: Vec<MockNode> = vec![
            ("svg", "", "", None, None, vec![]),
            ("g", "", "deep", Some(0), None, vec![]),
            ("rect", "", "", Some(1), None, vec![]),
        ];
        assert_eq!(matching_rules(&sheet2, &Mock { nodes: nodes2 }, 2).len(), 1);
    }

    /// 属性名收集（builder 解析期只暂存这些原文属性）。
    #[test]
    fn collects_attr_names() {
        let sheet = parse(
            "[stroke-width=\"1.0001\"] { fill: blue } g#delta rect[SW=\"x\"] { fill: green }",
        );
        let names = attr_names(&sheet);
        assert_eq!(names.len(), 2);
        assert!(names.iter().any(|n| n == "stroke-width"));
        assert!(names.iter().any(|n| n == "sw"));
    }
}
