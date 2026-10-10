//! 样式与级联（M3 落地）。
//!
//! 属性解析管线是**固定多阶段管道**，顺序不可调换——这是 Blink
//! 三明治（sandwich）语义的落地结构，也是 Chrome 差分错误最高发的区域：
//!
//! ```text
//! presentation attribute -> CSS 级联 -> CSS 动画/transition 层 -> SMIL 层
//! ```
//!
//! 每一阶段保留中间结果，供单测逐段断言。

/// CSS 颜色值（`fill`/`stroke` 的解析目标之一）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Color {
    Rgb8 {
        r: u8,
        g: u8,
        b: u8,
    },
    /// 带 alpha 的颜色（车道NG，2026-09-14）：`rgba()`/`hsla()`、
    /// `rgb()/hsl()` 斜杠 alpha、`#rrggbbaa`/`#rgba`、`transparent`。
    /// alpha 语义 = 与消费点的 `-opacity` 属性**相乘**（Chrome 探针
    /// cg11/cg12 实证：0.6×0.5 → a8=77；stop 0.5×0.5 → a8=64）。
    Rgba {
        r: u8,
        g: u8,
        b: u8,
        a: u8,
    },
    /// `currentColor`：解析期回填宿主上下文的 color 值。
    CurrentColor,
    Inherit,
}

/// 解析 CSS/SVG 颜色：`#rgb`、`#rrggbb`、`#rgba`、`#rrggbbaa`（车道NG）、
/// `currentColor`、`inherit`、`transparent`（车道NG：= rgba(0,0,0,0)——
/// 旧注记「alpha 语义需乘透明度」随 `Color::Rgba` 变体落地作废）、
/// `rgb()/rgba()/hsl()/hsla()` 函数色（legacy 逗号与 modern 空白/斜杠
/// 两种语法；alpha 参数见各解析函数）、命名色（147 个 CSS 扩展关键字）。
pub fn parse_color(s: &str) -> Option<Color> {
    let s = s.trim();
    if s.eq_ignore_ascii_case("currentcolor") {
        return Some(Color::CurrentColor);
    }
    if s.eq_ignore_ascii_case("inherit") {
        return Some(Color::Inherit);
    }
    if s.eq_ignore_ascii_case("transparent") {
        return Some(Color::Rgba {
            r: 0,
            g: 0,
            b: 0,
            a: 0,
        });
    }
    if let Some(hex) = s.strip_prefix('#') {
        let rgb = match hex.len() {
            3 => Some([
                nibble(hex, 0)? * 17,
                nibble(hex, 1)? * 17,
                nibble(hex, 2)? * 17,
                255,
            ]),
            4 => Some([
                nibble(hex, 0)? * 17,
                nibble(hex, 1)? * 17,
                nibble(hex, 2)? * 17,
                nibble(hex, 3)? * 17,
            ]),
            6 => Some([
                byte_pair(hex, 0)?,
                byte_pair(hex, 2)?,
                byte_pair(hex, 4)?,
                255,
            ]),
            8 => Some([
                byte_pair(hex, 0)?,
                byte_pair(hex, 2)?,
                byte_pair(hex, 4)?,
                byte_pair(hex, 6)?,
            ]),
            _ => return None,
        }?;
        let [r, g, b, a] = rgb;
        return if a == 255 {
            Some(Color::Rgb8 { r, g, b })
        } else {
            Some(Color::Rgba { r, g, b, a })
        };
    }
    if let Some(c) = parse_rgb_fn(s) {
        return Some(c);
    }
    if let Some(c) = parse_hsl_fn(s) {
        return Some(c);
    }
    named_color(s)
}

/// `rgb(r,g,b)` / `rgba(r,g,b,a)` / modern `rgb(r g b / a)` 函数色
/// （车道NG 扩展）。`rgb` 与 `rgba` 前缀全别名（Chrome cg5 实证 3 参
/// rgba 合法）；legacy = 逗号分隔 3/4 参；modern = 空白分隔 3 参 +
/// 可选 `/ alpha`。每分量整数（0-255 钳位，浮点取整）、百分比
/// （0-100% × 255）或 `none`（CSS Color 4 缺失分量 = 0，Chrome cg15
/// 实证）；alpha 数值/百分比钳 [0,1] → u8。3 参产出 `Color::Rgb8`
/// （既有行为逐位不变），4 参产出 [`Color::Rgba`]。
///
/// 车道CI（parser-fuzz）：前缀/函数体切片字节级判定（旧式 `s[..3]`/
/// `s[4..len-1]` 在 `rgb(é)`、`é😀…` 上 char boundary panic）。
fn parse_rgb_fn(s: &str) -> Option<Color> {
    let b = s.as_bytes();
    // 字节级前缀判定（车道CI 纪律）：`rgb` 后可选 `a`，再 `(`。
    let ascii_rgb =
        b.len() >= 3 && (b[0] | 0x20) == b'r' && (b[1] | 0x20) == b'g' && (b[2] | 0x20) == b'b';
    if !ascii_rgb {
        return None;
    }
    let open = if b.len() > 3 && (b[3] | 0x20) == b'a' {
        4
    } else {
        3
    };
    if b.len() < open + 3 || b.get(open) != Some(&b'(') || b[b.len() - 1] != b')' {
        return None;
    }
    let body = s.get(open + 1..s.len() - 1)?;
    let parts = split_fn_body(body)?;
    let r = parse_chan(parts[0]?.trim())?;
    let g = parse_chan(parts[1]?.trim())?;
    let b2 = parse_chan(parts[2]?.trim())?;
    match parts[3] {
        None => Some(Color::Rgb8 { r, g, b: b2 }),
        Some(t) => Some(Color::Rgba {
            r,
            g,
            b: b2,
            a: parse_alpha(t.trim())?,
        }),
    }
}

/// 函数体切分：legacy 逗号形态（3/4 段，无 alpha 尾参）或 modern 空白
/// 形态（3 段 + 可选 `/ alpha`）。返回 [c0, c1, c2, alpha?]，未用槽
/// None；段未 trim，消费方自行 trim。
fn split_fn_body(body: &str) -> Option<[Option<&str>; 4]> {
    let mut out: [Option<&str>; 4] = [None; 4];
    if body.contains(',') {
        // legacy：逗号分隔；4 段 = 尾段 alpha
        for (i, p) in body.split(',').enumerate() {
            if i >= 4 {
                return None;
            }
            out[i] = Some(p);
        }
        if out[0].is_none() || out[1].is_none() || out[2].is_none() {
            return None;
        }
    } else {
        // modern：斜杠切 alpha；段内空白切通道
        let mut it = body.split('/');
        let chans = it.next()?;
        let mut n = 0;
        for p in chans.split_whitespace() {
            if n >= 3 {
                return None;
            }
            out[n] = Some(p);
            n += 1;
        }
        let alpha = it.next();
        if n != 3 || it.next().is_some() {
            return None;
        }
        out[3] = alpha;
    }
    Some(out)
}

/// 通道分量：整数（0-255 钳位）、百分比（×255）或 `none`（=0）。
fn parse_chan(t: &str) -> Option<u8> {
    if t.eq_ignore_ascii_case("none") {
        return Some(0);
    }
    let (n, pct) = match t.strip_suffix('%') {
        Some(rest) => (crate::fnum::parse_f32(rest.trim())?, true),
        None => (crate::fnum::parse_f32(t)?, false),
    };
    if !n.is_finite() {
        return None;
    }
    let v = if pct { n / 100.0 * 255.0 } else { n };
    Some(libm::roundf(v.clamp(0.0, 255.0)) as u8)
}

/// alpha 分量：数值/百分比钳 [0,1] → u8（`none` = 0）。
fn parse_alpha(t: &str) -> Option<u8> {
    if t.eq_ignore_ascii_case("none") {
        return Some(0);
    }
    let n = match t.strip_suffix('%') {
        Some(rest) => crate::fnum::parse_f32(rest.trim())? / 100.0,
        None => crate::fnum::parse_f32(t)?,
    };
    if !n.is_finite() {
        return None;
    }
    Some(libm::roundf(n.clamp(0.0, 1.0) * 255.0) as u8)
}

/// `hsl(h,s%,l%)` / `hsla(h,s%,l%,a)` / modern `hsl(h s l / a)` 函数色
/// （车道NB 落地、车道NG 扩展）：CSS Color 4 HSL→RGB。逗号（legacy）
/// 或空白（modern）分隔；`hsl`/`hsla` 前缀全别名（同 rgb/rgba）。
/// hue：数值 + 角度单位 `deg`/`turn`/`rad`/`grad`（缺省 deg；负值/超
/// 360 按 360 取模——`rem_euclid` 语义手写）；`none` = 0。s/l 百分比
/// 或无单位小数（CSS4 number = 百分比/100），钳位 [0,1]。4 参/slash
/// alpha → [`Color::Rgba`]（与 rgb 同口径）；3 参 `Color::Rgb8`。
fn parse_hsl_fn(s: &str) -> Option<Color> {
    let b = s.as_bytes();
    // 字节级前缀判定（同 parse_rgb_fn 的车道CI 纪律）
    let ascii_hsl =
        b.len() >= 3 && (b[0] | 0x20) == b'h' && (b[1] | 0x20) == b's' && (b[2] | 0x20) == b'l';
    if !ascii_hsl {
        return None;
    }
    let open = if b.len() > 3 && (b[3] | 0x20) == b'a' {
        4
    } else {
        3
    };
    if b.len() < open + 3 || b.get(open) != Some(&b'(') || b[b.len() - 1] != b')' {
        return None;
    }
    let body = s.get(open + 1..s.len() - 1)?;
    let parts = split_fn_body(body)?;
    let hue = parse_hue(parts[0]?.trim())?;
    let l01 = sat_comp(parts[2]?.trim())?;
    let s01 = sat_comp(parts[1]?.trim())?;
    let (r, g, b2) = hsl_to_rgb8(hue, s01, l01);
    match parts[3] {
        None => Some(Color::Rgb8 { r, g, b: b2 }),
        Some(t) => Some(Color::Rgba {
            r,
            g,
            b: b2,
            a: parse_alpha(t.trim())?,
        }),
    }
}

/// hue 分量：数值 + 角度单位（车道NG 扩展 turn/rad/grad；单位切分同
/// gradient parse_len 的「首字母断字」式）。`none` = 0。
fn parse_hue(t: &str) -> Option<f32> {
    if t.eq_ignore_ascii_case("none") {
        return Some(0.0);
    }
    let lower = t.to_ascii_lowercase();
    let (num, unit) = match lower.char_indices().find(|(_, c)| c.is_ascii_alphabetic()) {
        Some((i, _)) => lower.split_at(i),
        None => (lower.as_str(), ""),
    };
    let n = crate::fnum::parse_f32(num.trim())?;
    if !n.is_finite() {
        return None;
    }
    let deg = match unit {
        "" | "deg" => n,
        "turn" => n * 360.0,
        "grad" => n * 0.9,
        "rad" => n * 57.295_78_f32,
        _ => return None,
    };
    Some(deg)
}

/// s/l 分量：百分比或无单位小数（CSS4 number = 百分比/100），钳位
/// [0,1]；`none` = 0。
fn sat_comp(t: &str) -> Option<f32> {
    if t.eq_ignore_ascii_case("none") {
        return Some(0.0);
    }
    let n = match t.strip_suffix('%') {
        Some(rest) => crate::fnum::parse_f32(rest.trim())? / 100.0,
        None => crate::fnum::parse_f32(t)?,
    };
    if n.is_finite() {
        Some(n.clamp(0.0, 1.0))
    } else {
        None
    }
}

/// 车道NE（stress2）：CSS 声明位 rgba()/斜杠 alpha 形态识别——pygal 等
/// 真实图表工具链全量用 `fill:rgba(249,249,249,1)`，旧 parse_color 只认
/// `rgb()` → 整条声明丢弃、背景矩形回落默认黑（stress pygal 24/24 全灭
/// 根因之二，探针 ne_probe4 三格判别：实体子组合器已修而 rgba 仍黑）。
/// 返回 `Some((重构 rgb() 片段, Some(alpha)))`；非 alpha 形态返回 None
/// （调用方走原 parse_color 路径）。合法形态：legacy 逗号
/// `rgba(r,g,b,a)`、modern 斜杠 `rgba(r g b / a)` 与 `rgb(r g b / a)`。
/// 登记近似：alpha 并入 fill-opacity 链（CSS 语义=色α×fill-opacity），
/// 与同块后续独立 `fill-opacity` 声明的级联次序不复刻（现实语料无混用）。
pub fn split_css_alpha(value: &str) -> Option<(alloc::string::String, Option<f32>)> {
    let t = value.trim();
    let b = t.as_bytes();
    // 字节级前缀判定（车道CI 纪律：多字节安全，不切片到字符中间）
    if b.len() < 5
        || (b[0] | 0x20) != b'r'
        || (b[1] | 0x20) != b'g'
        || (b[2] | 0x20) != b'b'
        || b[b.len() - 1] != b')'
    {
        return None;
    }
    let has_a_suffix = (b[3] | 0x20) == b'a' && b[4] == b'(';
    let is_rgb_slash = b[3] == b'(' && t.contains('/');
    if !has_a_suffix && !is_rgb_slash {
        return None;
    }
    let open = if has_a_suffix { 5 } else { 4 };
    let inner = t.get(open..t.len() - 1)?;
    let (channels, alpha) = if let Some(pos) = inner.find('/') {
        let raw = inner.get(pos + 1..)?.trim();
        let a = crate::fnum::parse_f32(raw)?;
        if !a.is_finite() {
            return None;
        }
        (
            alloc::string::String::from(inner[..pos].trim()),
            Some(a.clamp(0.0, 1.0)),
        )
    } else if has_a_suffix {
        let mut it = inner.split(',');
        let r = it.next()?.trim();
        let g = it.next()?.trim();
        let bl = it.next()?.trim();
        let raw = it.next()?.trim();
        let a = crate::fnum::parse_f32(raw)?;
        if !a.is_finite() {
            return None;
        }
        (alloc::format!("{r},{g},{bl}"), Some(a.clamp(0.0, 1.0)))
    } else {
        return None;
    };
    Some((alloc::format!("rgb({channels})"), alpha))
}

/// CSS Color 4 HSL→RGB：c/x/m 六扇区公式，单次舍入到 u8。
/// hue 已在调用方解析为浮点角度（可为负/超 360）。
/// no_std 纪律：`rem_euclid`/`floor` 的 std 形态不可用，手写取模
/// （`h − 360·floor(h/360)`，libm::floorf）。
fn hsl_to_rgb8(hue: f32, s: f32, l: f32) -> (u8, u8, u8) {
    let h = hue - 360.0 * libm::floorf(hue / 360.0); // → [0,360)
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let h60 = h / 60.0;
    let x = c * (1.0 - (h60 - 2.0 * libm::floorf(h60 / 2.0) - 1.0).abs());
    let m = l - c / 2.0;
    let sector = h60 as i32; // h ∈ [0,360) → sector ∈ 0..=5
    let (rp, gp, bp) = match sector {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let chan = |v: f32| -> u8 { libm::roundf(((v + m) * 255.0).clamp(0.0, 255.0)) as u8 };
    (chan(rp), chan(gp), chan(bp))
}

/// CSS 扩展色关键字表（147 个，按字母序；`transparent` 不收——alpha
/// 语义需乘透明度，与本项目 MISSING_PAINT/none 约定冲突，未支持）。
const NAMED_COLORS: [(&str, [u8; 3]); 147] = [
    ("aliceblue", [0xf0, 0xf8, 0xff]),
    ("antiquewhite", [0xfa, 0xeb, 0xd7]),
    ("aqua", [0x00, 0xff, 0xff]),
    ("aquamarine", [0x7f, 0xff, 0xd4]),
    ("azure", [0xf0, 0xff, 0xff]),
    ("beige", [0xf5, 0xf5, 0xdc]),
    ("bisque", [0xff, 0xe4, 0xc4]),
    ("black", [0x00, 0x00, 0x00]),
    ("blanchedalmond", [0xff, 0xeb, 0xcd]),
    ("blue", [0x00, 0x00, 0xff]),
    ("blueviolet", [0x8a, 0x2b, 0xe2]),
    ("brown", [0xa5, 0x2a, 0x2a]),
    ("burlywood", [0xde, 0xb8, 0x87]),
    ("cadetblue", [0x5f, 0x9e, 0xa0]),
    ("chartreuse", [0x7f, 0xff, 0x00]),
    ("chocolate", [0xd2, 0x69, 0x1e]),
    ("coral", [0xff, 0x7f, 0x50]),
    ("cornflowerblue", [0x64, 0x95, 0xed]),
    ("cornsilk", [0xff, 0xf8, 0xdc]),
    ("crimson", [0xdc, 0x14, 0x3c]),
    ("cyan", [0x00, 0xff, 0xff]),
    ("darkblue", [0x00, 0x00, 0x8b]),
    ("darkcyan", [0x00, 0x8b, 0x8b]),
    ("darkgoldenrod", [0xb8, 0x86, 0x0b]),
    ("darkgray", [0xa9, 0xa9, 0xa9]),
    ("darkgreen", [0x00, 0x64, 0x00]),
    ("darkgrey", [0xa9, 0xa9, 0xa9]),
    ("darkkhaki", [0xbd, 0xb7, 0x6b]),
    ("darkmagenta", [0x8b, 0x00, 0x8b]),
    ("darkolivegreen", [0x55, 0x6b, 0x2f]),
    ("darkorange", [0xff, 0x8c, 0x00]),
    ("darkorchid", [0x99, 0x32, 0xcc]),
    ("darkred", [0x8b, 0x00, 0x00]),
    ("darksalmon", [0xe9, 0x96, 0x7a]),
    ("darkseagreen", [0x8f, 0xbc, 0x8f]),
    ("darkslateblue", [0x48, 0x3d, 0x8b]),
    ("darkslategray", [0x2f, 0x4f, 0x4f]),
    ("darkslategrey", [0x2f, 0x4f, 0x4f]),
    ("darkturquoise", [0x00, 0xce, 0xd1]),
    ("darkviolet", [0x94, 0x00, 0xd3]),
    ("deeppink", [0xff, 0x14, 0x93]),
    ("deepskyblue", [0x00, 0xbf, 0xff]),
    ("dimgray", [0x69, 0x69, 0x69]),
    ("dimgrey", [0x69, 0x69, 0x69]),
    ("dodgerblue", [0x1e, 0x90, 0xff]),
    ("firebrick", [0xb2, 0x22, 0x22]),
    ("floralwhite", [0xff, 0xfa, 0xf0]),
    ("forestgreen", [0x22, 0x8b, 0x22]),
    ("fuchsia", [0xff, 0x00, 0xff]),
    ("gainsboro", [0xdc, 0xdc, 0xdc]),
    ("ghostwhite", [0xf8, 0xf8, 0xff]),
    ("gold", [0xff, 0xd7, 0x00]),
    ("goldenrod", [0xda, 0xa5, 0x20]),
    ("gray", [0x80, 0x80, 0x80]),
    ("green", [0x00, 0x80, 0x00]),
    ("greenyellow", [0xad, 0xff, 0x2f]),
    ("grey", [0x80, 0x80, 0x80]),
    ("honeydew", [0xf0, 0xff, 0xf0]),
    ("hotpink", [0xff, 0x69, 0xb4]),
    ("indianred", [0xcd, 0x5c, 0x5c]),
    ("indigo", [0x4b, 0x00, 0x82]),
    ("ivory", [0xff, 0xff, 0xf0]),
    ("khaki", [0xf0, 0xe6, 0x8c]),
    ("lavender", [0xe6, 0xe6, 0xfa]),
    ("lavenderblush", [0xff, 0xf0, 0xf5]),
    ("lawngreen", [0x7c, 0xfc, 0x00]),
    ("lemonchiffon", [0xff, 0xfa, 0xcd]),
    ("lightblue", [0xad, 0xd8, 0xe6]),
    ("lightcoral", [0xf0, 0x80, 0x80]),
    ("lightcyan", [0xe0, 0xff, 0xff]),
    ("lightgoldenrodyellow", [0xfa, 0xfa, 0xd2]),
    ("lightgray", [0xd3, 0xd3, 0xd3]),
    ("lightgreen", [0x90, 0xee, 0x90]),
    ("lightgrey", [0xd3, 0xd3, 0xd3]),
    ("lightpink", [0xff, 0xb6, 0xc1]),
    ("lightsalmon", [0xff, 0xa0, 0x7a]),
    ("lightseagreen", [0x20, 0xb2, 0xaa]),
    ("lightskyblue", [0x87, 0xce, 0xfa]),
    ("lightslategray", [0x77, 0x88, 0x99]),
    ("lightslategrey", [0x77, 0x88, 0x99]),
    ("lightsteelblue", [0xb0, 0xc4, 0xde]),
    ("lightyellow", [0xff, 0xff, 0xe0]),
    ("lime", [0x00, 0xff, 0x00]),
    ("limegreen", [0x32, 0xcd, 0x32]),
    ("linen", [0xfa, 0xf0, 0xe6]),
    ("magenta", [0xff, 0x00, 0xff]),
    ("maroon", [0x80, 0x00, 0x00]),
    ("mediumaquamarine", [0x66, 0xcd, 0xaa]),
    ("mediumblue", [0x00, 0x00, 0xcd]),
    ("mediumorchid", [0xba, 0x55, 0xd3]),
    ("mediumpurple", [0x93, 0x70, 0xdb]),
    ("mediumseagreen", [0x3c, 0xb3, 0x71]),
    ("mediumslateblue", [0x7b, 0x68, 0xee]),
    ("mediumspringgreen", [0x00, 0xfa, 0x9a]),
    ("mediumturquoise", [0x48, 0xd1, 0xcc]),
    ("mediumvioletred", [0xc7, 0x15, 0x85]),
    ("midnightblue", [0x19, 0x19, 0x70]),
    ("mintcream", [0xf5, 0xff, 0xfa]),
    ("mistyrose", [0xff, 0xe4, 0xe1]),
    ("moccasin", [0xff, 0xe4, 0xb5]),
    ("navajowhite", [0xff, 0xde, 0xad]),
    ("navy", [0x00, 0x00, 0x80]),
    ("oldlace", [0xfd, 0xf5, 0xe6]),
    ("olive", [0x80, 0x80, 0x00]),
    ("olivedrab", [0x6b, 0x8e, 0x23]),
    ("orange", [0xff, 0xa5, 0x00]),
    ("orangered", [0xff, 0x45, 0x00]),
    ("orchid", [0xda, 0x70, 0xd6]),
    ("palegoldenrod", [0xee, 0xe8, 0xaa]),
    ("palegreen", [0x98, 0xfb, 0x98]),
    ("paleturquoise", [0xaf, 0xee, 0xee]),
    ("palevioletred", [0xdb, 0x70, 0x93]),
    ("papayawhip", [0xff, 0xef, 0xd5]),
    ("peachpuff", [0xff, 0xda, 0xb9]),
    ("peru", [0xcd, 0x85, 0x3f]),
    ("pink", [0xff, 0xc0, 0xcb]),
    ("plum", [0xdd, 0xa0, 0xdd]),
    ("powderblue", [0xb0, 0xe0, 0xe6]),
    ("purple", [0x80, 0x00, 0x80]),
    ("red", [0xff, 0x00, 0x00]),
    ("rosybrown", [0xbc, 0x8f, 0x8f]),
    ("royalblue", [0x41, 0x69, 0xe1]),
    ("saddlebrown", [0x8b, 0x45, 0x13]),
    ("salmon", [0xfa, 0x80, 0x72]),
    ("sandybrown", [0xf4, 0xa4, 0x60]),
    ("seagreen", [0x2e, 0x8b, 0x57]),
    ("seashell", [0xff, 0xf5, 0xee]),
    ("sienna", [0xa0, 0x52, 0x2d]),
    ("silver", [0xc0, 0xc0, 0xc0]),
    ("skyblue", [0x87, 0xce, 0xeb]),
    ("slateblue", [0x6a, 0x5a, 0xcd]),
    ("slategray", [0x70, 0x80, 0x90]),
    ("slategrey", [0x70, 0x80, 0x90]),
    ("snow", [0xff, 0xfa, 0xfa]),
    ("springgreen", [0x00, 0xff, 0x7f]),
    ("steelblue", [0x46, 0x82, 0xb4]),
    ("tan", [0xd2, 0xb4, 0x8c]),
    ("teal", [0x00, 0x80, 0x80]),
    ("thistle", [0xd8, 0xbf, 0xd8]),
    ("tomato", [0xff, 0x63, 0x47]),
    ("turquoise", [0x40, 0xe0, 0xd0]),
    ("violet", [0xee, 0x82, 0xee]),
    ("wheat", [0xf5, 0xde, 0xb3]),
    ("white", [0xff, 0xff, 0xff]),
    ("whitesmoke", [0xf5, 0xf5, 0xf5]),
    ("yellow", [0xff, 0xff, 0x00]),
    ("yellowgreen", [0x9a, 0xcd, 0x32]),
];

/// 命名色查表（大小写不敏感，线性扫描——仅解析期，非像素热路径）。
fn named_color(s: &str) -> Option<Color> {
    NAMED_COLORS
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(s))
        .map(|(_, rgb)| Color::Rgb8 {
            r: rgb[0],
            g: rgb[1],
            b: rgb[2],
        })
}

/// 单个十六进制位（0-15）。
fn nibble(hex: &str, i: usize) -> Option<u8> {
    hex.as_bytes()
        .get(i)
        .and_then(|b| (*b as char).to_digit(16))
        .map(|d| d as u8)
}

/// 两位十六进制字节。
fn byte_pair(hex: &str, i: usize) -> Option<u8> {
    u8::from_str_radix(hex.get(i..i + 2)?, 16).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_short_and_long() {
        assert_eq!(
            parse_color("#f00"),
            Some(Color::Rgb8 { r: 255, g: 0, b: 0 })
        );
        assert_eq!(
            parse_color("#00ff00"),
            Some(Color::Rgb8 { r: 0, g: 255, b: 0 })
        );
        assert_eq!(
            parse_color("#abc"),
            Some(Color::Rgb8 {
                r: 170,
                g: 187,
                b: 204
            })
        );
    }

    #[test]
    fn keywords_and_invalid() {
        assert_eq!(parse_color("CURRENTCOLOR"), Some(Color::CurrentColor));
        assert_eq!(parse_color("inherit"), Some(Color::Inherit));
        assert_eq!(parse_color("#12"), None);
        assert_eq!(parse_color("#gg0000"), None);
        assert_eq!(parse_color("notacolor"), None);
    }

    /// hsl() 函数色（车道NB）：CSS Color 4 HSL→RGB。基准值对照 Chrome
    /// （hsl(47,70%,50%) 逐通道 ±1 内；纯色端点逐位）。
    #[test]
    fn hsl_fn_colors() {
        let rgb = |s| match parse_color(s) {
            Some(Color::Rgb8 { r, g, b }) => Some((r, g, b)),
            _ => None,
        };
        // 端点：hsl(0,100%,50%)=red；hsl(120,100%,25%)=#008000（green 暗档）
        assert_eq!(rgb("hsl(0, 100%, 50%)"), Some((255, 0, 0)));
        assert_eq!(rgb("hsl(120, 100%, 25%)"), Some((0, 128, 0)));
        assert_eq!(rgb("hsl(240, 100%, 100%)"), Some((255, 255, 255)));
        assert_eq!(rgb("hsl(240, 100%, 0%)"), Some((0, 0, 0)));
        // 语料同款：hsl(47,70%,50%)（CSS4 公式手算 (217,178,38)；与
        // Chrome 的一致性由 n05 压力语料像素差分背书）
        assert_eq!(rgb("hsl(47,70%,50%)"), Some((217, 178, 38)));
        // modern 空白分隔 + 无单位 s/l（CSS4 number = 百分比/100）
        assert_eq!(rgb("hsl(120 50% 50%)"), rgb("hsl(120, 50%, 50%)"));
        assert_eq!(rgb("hsl(120 0.5 0.5)"), rgb("hsl(120, 50%, 50%)"));
        // hue 负值/超界取模；deg 单位
        assert_eq!(rgb("hsl(-180deg, 100%, 50%)"), rgb("hsl(180, 100%, 50%)"));
        assert_eq!(rgb("hsl(480, 100%, 50%)"), rgb("hsl(120, 100%, 50%)"));
        // 大小写不敏感；非法（缺参/坏单位）回落 named_color
        assert_eq!(rgb("HSL(90, 100%, 50%)"), rgb("hsl(90, 100%, 50%)"));
        assert_eq!(rgb("hsl(90, 100%)"), None);
    }

    /// 车道NG（css-color）：CSS Color 4 alpha 形态——rgba()/hsla()
    /// legacy 逗号、rgb()/hsl() modern 空白+斜杠、`#rrggbbaa`/`#rgba`、
    /// transparent、none 分量、turn/rad/grad 角度。基准值对照 Chrome
    /// cg 探针采样（±1 由合成链 BA 白底臂逐位等价背书）。
    #[test]
    fn css_color4_alpha_forms() {
        let rgb = |s| match parse_color(s) {
            Some(Color::Rgb8 { r, g, b }) => Some((r, g, b, 255u8)),
            Some(Color::Rgba { r, g, b, a }) => Some((r, g, b, a)),
            _ => None,
        };
        // legacy 逗号 4 参（cg1：Chrome 采样白底合成 (255,127,127)）
        assert_eq!(rgb("rgba(255, 0, 0, 0.5)"), Some((255, 0, 0, 128)));
        // rgb() legacy 4 参同型（cg4）
        assert_eq!(rgb("rgb(255, 0, 0, 0.5)"), Some((255, 0, 0, 128)));
        // modern 空白 + 斜杠（cg2）
        assert_eq!(rgb("rgb(255 0 0 / 0.5)"), Some((255, 0, 0, 128)));
        // 百分比通道 + 百分比 alpha（cg3）
        assert_eq!(rgb("rgb(100% 0% 0% / 50%)"), Some((255, 0, 0, 128)));
        // rgba() 3 参 = rgb() 全别名（cg5 Chrome (0,128,0) 不透明）
        assert_eq!(rgb("rgba(0, 128, 0)"), Some((0, 128, 0, 255)));
        // hsla legacy（cg6：a8=128）
        assert_eq!(rgb("hsla(120, 100%, 50%, 0.5)"), Some((0, 255, 0, 128)));
        // hsl modern + deg + 斜杠（cg7：0.4 → a8=102）
        assert_eq!(rgb("hsl(120deg 100% 50% / 0.4)"), Some((0, 255, 0, 102)));
        // alpha 越界钳位；rgba 也可 3 参空格
        assert_eq!(rgb("rgba(0 0 255 / 2)"), Some((0, 0, 255, 255)));
        assert_eq!(rgb("rgba(0 0 255 / -1)"), Some((0, 0, 255, 0)));
        // 8 位 hex（cg8：0x80=128）；4 位 hex（cg9：#0f08 → 0x88=136）
        assert_eq!(rgb("#ff000080"), Some((255, 0, 0, 128)));
        assert_eq!(rgb("#0f08"), Some((0, 255, 0, 136)));
        assert_eq!(rgb("#0f0"), Some((0, 255, 0, 255)));
        // transparent = rgba(0,0,0,0)（cg10：Chrome 透出底色非黑）
        assert_eq!(rgb("transparent"), Some((0, 0, 0, 0)));
        assert_eq!(rgb("TRANSPARENT"), Some((0, 0, 0, 0)));
        // none 分量 = 0（cg15：Chrome rgb(none 255 0) → (0,255,0)）
        assert_eq!(rgb("rgb(none 255 0)"), Some((0, 255, 0, 255)));
        // turn/rad/grad 角度单位（cg14：0.25turn → hue90 → (128,255,0)）。
        // rad/grad 对照选扇区内值（90° 恰在 127.5 舍入刀锋上，f32 换算
        // 与 Blink double 会差 ±1，属量化地板非解析错误）。
        assert_eq!(rgb("hsl(0.25turn, 100%, 50%)"), Some((128, 255, 0, 255)));
        assert_eq!(
            rgb("hsl(60deg, 100%, 50%)"),
            rgb("hsl(1.0471976rad, 100%, 50%)")
        );
        assert_eq!(
            rgb("hsl(108deg, 100%, 50%)"),
            rgb("hsl(120grad, 100%, 50%)")
        );
        // 非法形态维持 None：5 参 / modern 4 通道无斜杠 / 未知单位 /
        // 坏 hex 位长
        assert_eq!(rgb("rgb(1,2,3,4,5)"), None);
        assert_eq!(rgb("rgb(1 2 3 4)"), None);
        assert_eq!(rgb("hsl(90deg 50% 50% / 0.5 / 2)"), None);
        assert_eq!(rgb("#12345"), None);
        assert_eq!(rgb("#1234567"), None);
    }

    /// 车道NE（stress2）：rgba()/斜杠 alpha 形态识别——pygal 全量
    /// `fill:rgba(249,249,249,1)` 写法，旧 parse_color 整条丢弃。
    /// 重构片段必须能被 parse_color 消费（闭环断言）。
    #[test]
    fn split_css_alpha_forms() {
        let p = |s: &str| split_css_alpha(s);
        // legacy 逗号四段（pygal 同款）
        let (rgb_fn, a) = p("rgba(249,249,249,1)").expect("legacy rgba");
        assert_eq!((rgb_fn.as_str(), a), ("rgb(249,249,249)", Some(1.0)));
        assert_eq!(
            parse_color(&rgb_fn),
            Some(Color::Rgb8 {
                r: 249,
                g: 249,
                b: 249
            })
        );
        // 空白宽容 + 半透明
        let (rgb_fn, a) = p("rgba( 0 , 0 , 0 , 0.5 )").expect("spaced rgba");
        assert_eq!((rgb_fn.as_str(), a), ("rgb(0,0,0)", Some(0.5)));
        // modern 斜杠形态（rgb 与 rgba 双前缀）
        let (rgb_fn, a) = p("rgb(1 2 3 / 0.25)").expect("slash rgb");
        assert_eq!((rgb_fn.as_str(), a), ("rgb(1 2 3)", Some(0.25)));
        let (rgb_fn, a) = p("RGBA(1 2 3 / 1)").expect("upper rgba slash");
        assert_eq!((rgb_fn.as_str(), a), ("rgb(1 2 3)", Some(1.0)));
        // 非法/非 alpha 形态 → None（走原路径）
        assert_eq!(p("rgba(1,2,3)"), None, "三段逗号不是 rgba");
        assert_eq!(p("rgba(bad)"), None);
        assert_eq!(p("#ffffff"), None);
        assert_eq!(p("rgb(1,2,3)"), None, "无斜杠 rgb() 不劫持");
        // 百分比通道透传（parse_color 消费侧既有能力）
        let (rgb_fn, a) = p("rgba(50%,50%,50%,0.5)").expect("pct rgba");
        assert_eq!(a, Some(0.5));
        assert_eq!(
            parse_color(&rgb_fn).map(|c| matches!(c, Color::Rgb8 { .. })),
            Some(true)
        );
    }

    /// 车道CI（parser-fuzz）：rgb() 函数色对多字节字符容错——修前
    /// `rgb(é)`（body 切片 `s[4..len-1]` 切进 é）与 `é😀ab`（前缀
    /// `s[..3]` 切进 😀）均 char boundary panic。非法即 None（回落
    /// 命名色精确匹配），合法形态逐位不变。
    #[test]
    fn rgb_fn_multibyte_tolerant() {
        assert_eq!(parse_color("rgb(é)"), None);
        assert_eq!(parse_color("é😀ab"), None);
        assert_eq!(parse_color("中中中中中中"), None);
        // 合法形态（含空格/大小写）不变
        assert_eq!(
            parse_color("rgb(1, 2, 3)"),
            Some(Color::Rgb8 { r: 1, g: 2, b: 3 })
        );
        assert_eq!(
            parse_color("RGB( 255 165 0 )"),
            Some(Color::Rgb8 {
                r: 255,
                g: 165,
                b: 0
            })
        );
    }

    /// 命名色（CSS 扩展色关键字 147 个，车道E 随 W3C 标准集接入）：
    /// 大小写不敏感；W3C 套件高频色抽查 + 最长关键字。
    #[test]
    fn named_colors() {
        let rgb = |r: u8, g: u8, b: u8| Some(Color::Rgb8 { r, g, b });
        assert_eq!(parse_color("red"), rgb(255, 0, 0));
        assert_eq!(parse_color("blue"), rgb(0, 0, 255));
        assert_eq!(parse_color("lime"), rgb(0, 255, 0));
        assert_eq!(parse_color("black"), rgb(0, 0, 0));
        assert_eq!(parse_color("white"), rgb(255, 255, 255));
        assert_eq!(parse_color("aqua"), rgb(0, 255, 255));
        assert_eq!(parse_color("gold"), rgb(255, 215, 0));
        assert_eq!(parse_color("LIME"), rgb(0, 255, 0), "大小写不敏感");
        assert_eq!(parse_color("LightGoldenrodYellow"), rgb(250, 250, 210));
        assert_eq!(parse_color("  red  "), rgb(255, 0, 0), "首尾空白");
        assert_eq!(parse_color("redd"), None);
    }

    /// rgb() 函数色：整数/百分比、逗号或空白分隔、越界钳位、非法 None。
    #[test]
    fn rgb_functional() {
        let rgb = |r: u8, g: u8, b: u8| Some(Color::Rgb8 { r, g, b });
        assert_eq!(parse_color("rgb(255,165,0)"), rgb(255, 165, 0));
        assert_eq!(parse_color("RGB( 10 , 20 , 30 )"), rgb(10, 20, 30));
        assert_eq!(parse_color("rgb(255 165 0)"), rgb(255, 165, 0), "CSS4 空格");
        assert_eq!(parse_color("rgb(100%,0%,50%)"), rgb(255, 0, 128));
        assert_eq!(parse_color("rgb(50%,50%,50%)"), rgb(128, 128, 128));
        assert_eq!(parse_color("rgb(300,-10,0)"), rgb(255, 0, 0), "越界钳位");
        assert_eq!(parse_color("rgb(12.5,0,0)"), rgb(13, 0, 0), "浮点取整");
        assert_eq!(parse_color("rgb(1,2)"), None);
        assert_eq!(parse_color("rgb(abc,0,0)"), None);
        assert_eq!(parse_color("rgb(255,165,0"), None, "未闭合");
    }
}
