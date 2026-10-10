//! linearGradient / radialGradient（M3）：定义收集、参数烘焙、像素采样。
//!
//! 数据流三段：
//! 1. **收集**（[`collect`]）：树构建期把 gradient/stop 元素的原始属性串
//!    存进 [`GradRaw`]/[`StopRaw`]（presence 语义必需——radial 默认 50%、
//!    fx 默认 = cx，无法用裸 f32 + 0.0 默认表达），树建成后按 id 汇成
//!    `BTreeMap<String, GradientDef>`。渐变可定义在树中任意位置（defs 内
//!    或内联）。
//! 2. **烘焙**（[`bake_paint`]）：形状发射时把 [0,1] 参数空间解析成设备
//!    空间端点/圆心/半径——objectBoundingBox 用平化后点集的 AABB 把
//!    [0,1] 端点映射进去（r 相对归一化对角线 `sqrt(w²+h²)/sqrt(2)`）；
//!    userSpaceOnUse 端点经累积矩阵映射、r 按平均缩放。
//! 3. **采样**（[`sample_linear`]/[`sample_radial`]）：光栅化期逐像素求值
//!    ——线性按轴投影 t；径向按 SVG 规范焦点公式（见 [`sample_radial`]）；
//!    spreadMethod 三态折叠后停靠点分段线性插值（直 RGB+a）。
//!
//! 已知不做（语料未用，见 M3 车道汇报）：`url(#id) fallback-color`
//! 回退色语法。`xlink:href`/`href` 继承链已于车道E 落地（见
//! [`collect`] 的两阶段解析）。

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::builder::ShapeAttrs;
use crate::dl::{GradStop, LinearGradient, Paint, RadialGradient, Rgba8, SpreadMethod};
use crate::geom::{lin_non_similarity, Mat2x3};
use crate::style::{parse_color, Color};
use crate::tree::{NodeId, Tag, Tree, NULL};

/// gradientUnits（默认 objectBoundingBox）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GradUnits {
    ObjectBoundingBox,
    UserSpaceOnUse,
}

/// linearGradient/radialGradient 元素的原始属性串（树构建期原样收集）。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GradRaw {
    pub units: Option<String>,
    pub spread: Option<String>,
    pub x1: Option<String>,
    pub y1: Option<String>,
    pub x2: Option<String>,
    pub y2: Option<String>,
    pub cx: Option<String>,
    pub cy: Option<String>,
    pub r: Option<String>,
    pub fx: Option<String>,
    pub fy: Option<String>,
    /// 车道BI：SVG2 `fr`（焦点圆半径）原文（radialgradient-basic-002）。
    pub fr: Option<String>,
    /// 车道P：渐变元素自身的 `stop-color` 原文（stop-color 是继承属性，
    /// 渐变级设置对子 stop 生效——stop 的 `inherit` 解析源；
    /// styling-inherit-01-b 考点）。href 链继承同其余字段。
    pub stop_color: Option<String>,
    /// 车道P：`color-interpolation` 原文（`linearRGB` → 停靠点线性域
    /// 增密，painting-render-01-b 考点；缺省 sRGB 不动）。
    pub interp: Option<String>,
}

/// stop 子元素的原始属性串。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StopRaw {
    pub offset: Option<String>,
    pub color: Option<String>,
    pub opacity: Option<String>,
}

/// [`collect`] 阶段一的中间态：单个渐变元素的自身原始数据
/// （href 继承解析前）。
struct GradNode {
    radial: bool,
    raw: GradRaw,
    /// 自身直接 `<stop>` 子元素（已解析未定稿；空 = 待链继承）。
    stops: Vec<GradStop>,
    /// 自身 `gradientTransform` 原文。
    transform: Option<String>,
    /// 自身 `href`/`xlink:href`（`#id` 原文；树构建期通用 href 槽）。
    href: Option<String>,
}

/// [`collect`] 产物：href 继承链解析后的渐变定义 + gradientTransform
/// 矩阵（同源同键——transform 沿链继承，见 [`collect`] 注释）。
#[derive(Default)]
pub struct GradMaps {
    pub defs: BTreeMap<String, GradientDef>,
    pub transforms: BTreeMap<String, Mat2x3>,
}

/// 已解析、未烘焙的渐变定义（停靠点已定稿：排序 + 钳位 + 单调化）。
#[derive(Clone, Debug, PartialEq)]
pub struct GradientDef {
    pub radial: bool,
    pub units: GradUnits,
    pub spread: SpreadMethod,
    /// 坐标参数：(数值, 是否百分比)。线性用 x1/y1/x2/y2，径向用 cx/cy/r/fx/fy。
    pub p: GradParams,
    pub stops: Vec<GradStop>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct GradParams {
    pub x1: Option<(f32, bool)>,
    pub y1: Option<(f32, bool)>,
    pub x2: Option<(f32, bool)>,
    pub y2: Option<(f32, bool)>,
    pub cx: Option<(f32, bool)>,
    pub cy: Option<(f32, bool)>,
    pub r: Option<(f32, bool)>,
    pub fx: Option<(f32, bool)>,
    pub fy: Option<(f32, bool)>,
    pub fr: Option<(f32, bool)>,
}

/// 全树收集 linearGradient/radialGradient 定义 + gradientTransform 矩阵，
/// 两阶段（车道E）：
/// href 继承链深度上限（车道DI）。合法语料链 ≤3；病态长链的 visited
/// 线性回扫曾把 N 渐变 × 全链解析推到 O(N³)（挂死级），cap 后单渐变
/// 解析工作量有界。超限按链止降级（已继承字段保留）。
const MAX_HREF_CHAIN: usize = 64;

/// 1. **原始收集**：树构建期属性串按 id 汇成 [`GradNode`]（含自身
///    stops/transform/href；同名 id 首个胜出——XML 幂等语义）；
/// 2. **href 链解析**：`xlink:href`/`href`（SVG 1.1 / SVG2）引用另一
///    渐变——**stops 全量继承**被引用者（自身无 `<stop>` 时，沿链最近
///    有停靠者胜）；可继承属性（units/spread/xy 焦点半径/transform）
///    **自身缺省才继承**、最近定义者胜（不组合）。链式多级逐级解析；
///    **环引用截断环边**保终止（环上双方各取对方自身属性，Chrome 同
///    类语义按引用失败处理，此处宽容渲染）。无 id 的定义丢弃；链解析
///    后仍无 stops 的定义**保留**（0 停引用按规范不描绘，见
///    [`bake_paint`]；计一条诊断告警）。
pub fn collect(tree: &Tree, shapes: &[ShapeAttrs], warnings: &mut u32) -> GradMaps {
    // 阶段一：全树原始收集
    let mut nodes: BTreeMap<String, GradNode> = BTreeMap::new();
    let mut stack: Vec<NodeId> = vec![tree.root()];
    while let Some(id) = stack.pop() {
        let node = tree.get(id);
        if matches!(node.tag, Tag::LinearGradient | Tag::RadialGradient) {
            // 车道AK2：非渲染语境（文本内容元素直接子）中的渐变不进
            // 注册表——WPT gradient-inheritance-not-in-rendertree-01
            // 实证：<text> 内的渐变定义不经 href 链供给引用者（引用
            // 者按 0 停「不描绘」处理，透出下层）。
            let ctx_bad =
                node.parent != NULL && matches!(tree.get(node.parent).tag, Tag::Text | Tag::Tspan);
            if !ctx_bad {
                if let Some(s) = shapes.get(id as usize) {
                    if let (Some(raw), Some(gid)) = (s.grad.as_ref(), s.id.clone()) {
                        nodes.entry(gid).or_insert_with(|| GradNode {
                            radial: node.tag == Tag::RadialGradient,
                            raw: raw.clone(),
                            stops: own_stops(tree, id, shapes),
                            transform: s.grad_transform.clone(),
                            href: s.href.clone(),
                        });
                    }
                }
            }
        }
        let mut c = node.first_child;
        while c != NULL {
            stack.push(c);
            c = tree.get(c).next_sibling;
        }
    }
    // 阶段二：逐 id 解析 href 继承链 → defs + transforms
    let mut maps = GradMaps::default();
    for (gid, gn) in &nodes {
        let mut raw = gn.raw.clone();
        let mut stops = if gn.stops.is_empty() {
            None
        } else {
            Some(gn.stops.clone())
        };
        let mut transform = gn.transform.clone();
        let mut visited: Vec<&str> = vec![gid.as_str()];
        let mut cur = gn.href.as_deref();
        while let Some(h) = cur {
            // 车道DI：链深上限。链走查的 visited 线性回扫是 O(链长²)，
            // 而 N 个渐变各解全链 = O(N³) 总量（1500 链实测 parse 4s、
            // 2000 链 6.8s——病态文档挂死级）。合法语料链 ≤3，64 远超
            // 语义需求；超限按链止降级（已继承字段保留，与环截断同款
            // 宽容口径）。
            if visited.len() >= MAX_HREF_CHAIN {
                break;
            }
            let Some(tid) = h.trim().strip_prefix('#') else {
                break; // 无 # 前缀（外部引用）或空：链止
            };
            if tid.is_empty() || visited.contains(&tid) {
                break; // 环边截断（含自环）
            }
            let Some(next) = nodes.get(tid) else {
                break; // 未知/非渐变目标：链止
            };
            visited.push(tid);
            fill_raw(&mut raw, &next.raw);
            if stops.is_none() && !next.stops.is_empty() {
                stops = Some(next.stops.clone());
            }
            if transform.is_none() && next.transform.is_some() {
                transform = next.transform.clone();
            }
            cur = next.href.as_deref();
        }
        if stops.is_none() {
            *warnings += 1; // 链解析后仍无 stop：规范「不描绘」（等同
                            // fill=none）——定义保留使引用可解析（区别于未知引用的品红），
                            // bake_paint 见 0 停返回 None
        }
        let stops = stops.unwrap_or_default();
        if let Some(t) = &transform {
            maps.transforms
                .insert(gid.clone(), crate::geom::parse_transform(t));
        }
        maps.defs
            .entry(gid.clone())
            .or_insert_with(|| build_def(gn.radial, &raw, stops));
    }
    maps
}

/// 渐变元素自身的直接 `<stop>` 子元素（文档序，已解析未定稿）。
/// 车道P：stop 的 currentColor/inherit（及缺省）对宿主渐变的
/// `color`/`stop-color` 解析（见 [`parse_stop`]）。
/// Chrome 预言机实证（pservers-grad-18-b 逐块采样）：`color` 是普通
/// 继承属性——currentColor 沿渐变的**祖先链**取最近指定值；
/// `stop-color` 的 inherit 不走祖先链（`<g stop-color=red>` 不影响
/// 后代 stop）——只取渐变元素自身。
fn own_stops(tree: &Tree, id: NodeId, shapes: &[ShapeAttrs]) -> Vec<GradStop> {
    let ga = shapes.get(id as usize);
    let own_color = ga.and_then(|a| a.color);
    let ctx_stop_color = ga
        .and_then(|a| a.grad.as_ref())
        .and_then(|gr| gr.stop_color.as_deref())
        .and_then(|v| resolve_stop_color(v, own_color, None));
    // currentColor 解析源：自身 > 最近祖先的 color
    let ctx_color = match own_color {
        Some(_) => own_color,
        None => {
            let mut anc = tree.get(id).parent;
            let mut found = None;
            while anc != NULL {
                if let Some(a) = shapes.get(anc as usize) {
                    if a.color.is_some() {
                        found = a.color;
                        break;
                    }
                }
                anc = tree.get(anc).parent;
            }
            found
        }
    };
    let mut stops: Vec<GradStop> = Vec::new();
    let mut c = tree.get(id).first_child;
    while c != NULL {
        if let Some(s) = shapes.get(c as usize) {
            if let Some(st) = s.stop.as_ref() {
                stops.push(parse_stop(st, ctx_color, ctx_stop_color));
            }
        }
        c = tree.get(c).next_sibling;
    }
    stops
}

/// href 链字段继承：`dst` 缺省（None）的字段从 `src` 补（最近定义者胜）。
fn fill_raw(dst: &mut GradRaw, src: &GradRaw) {
    let fields: [(&mut Option<String>, &Option<String>); 14] = [
        (&mut dst.units, &src.units),
        (&mut dst.spread, &src.spread),
        (&mut dst.x1, &src.x1),
        (&mut dst.y1, &src.y1),
        (&mut dst.x2, &src.x2),
        (&mut dst.y2, &src.y2),
        (&mut dst.cx, &src.cx),
        (&mut dst.cy, &src.cy),
        (&mut dst.r, &src.r),
        (&mut dst.fx, &src.fx),
        (&mut dst.fy, &src.fy),
        (&mut dst.fr, &src.fr),
        // 车道P：stop-color 沿 href 链继承（继承属性语义）
        (&mut dst.stop_color, &src.stop_color),
        (&mut dst.interp, &src.interp),
    ];
    for (d, s) in fields {
        if d.is_none() {
            *d = s.clone();
        }
    }
}

/// 单个 gradient 元素（href 解析后的有效属性 + 停靠点）→ GradientDef。
/// 1 停仍收集（bake_paint 期降级纯色）；0 停定义也保留（bake_paint
/// 返回 None = 不描绘）。数值级告警已由树构建期的 warn_num（line 共用
/// 属性臂）记录，不重复计。
fn build_def(radial: bool, raw: &GradRaw, stops: Vec<GradStop>) -> GradientDef {
    let units = match raw.units.as_deref().map(str::trim) {
        Some("userSpaceOnUse") => GradUnits::UserSpaceOnUse,
        _ => GradUnits::ObjectBoundingBox, // 缺省 / objectBoundingBox / 未知值
    };
    let spread = match raw.spread.as_deref().map(str::trim) {
        Some("reflect") => SpreadMethod::Reflect,
        Some("repeat") => SpreadMethod::Repeat,
        _ => SpreadMethod::Pad, // 缺省 / pad / 未知值
    };
    let p = GradParams {
        x1: raw.x1.as_deref().and_then(parse_len),
        y1: raw.y1.as_deref().and_then(parse_len),
        x2: raw.x2.as_deref().and_then(parse_len),
        y2: raw.y2.as_deref().and_then(parse_len),
        cx: raw.cx.as_deref().and_then(parse_len),
        cy: raw.cy.as_deref().and_then(parse_len),
        r: raw.r.as_deref().and_then(parse_len),
        fx: raw.fx.as_deref().and_then(parse_len),
        fy: raw.fy.as_deref().and_then(parse_len),
        fr: raw.fr.as_deref().and_then(parse_len),
    };
    let stops = finalize_stops(stops);
    // 车道P：color-interpolation="linearRGB" → 停靠点在线性域插值。
    // 用增密重采样近似（sRGB 分段线性逼近「linear lerp + 反线性化」
    // 曲线；8-bit 输出下 64 段/跨误差远小于 0.5 LSB），不动 dl 契约。
    let stops = if raw.interp.as_deref().map(str::trim) == Some("linearRGB") {
        densify_linear_rgb(&stops, 64)
    } else {
        stops
    };
    GradientDef {
        radial,
        units,
        spread,
        p,
        stops,
    }
}

/// sRGB 8-bit → 线性域 [0,1]（IEC 61966-2-1 分段，与 raster blend 同式）。
fn srgb_to_lin(c: u8) -> f32 {
    let c = c as f32 / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        libm::powf((c + 0.055) / 1.055, 2.4)
    }
}

/// 线性域 [0,1] → sRGB 8-bit（四舍五入）。
fn lin_to_srgb(v: f32) -> u8 {
    let c = if v <= 0.003_130_8 {
        12.92 * v
    } else {
        1.055 * libm::powf(v, 1.0 / 2.4) - 0.055
    };
    libm::roundf(c.clamp(0.0, 1.0) * 255.0) as u8
}

/// 车道P：linearRGB 插值的停靠点增密——每跨 `per_span` 段重采样，
/// 中间色 = 线性域 lerp 后反线性化回 sRGB（alpha 仍直线插值——规范
/// 只对颜色通道定义插值域）。硬边（offset 相等）不增密。
fn densify_linear_rgb(stops: &[GradStop], per_span: usize) -> Vec<GradStop> {
    if stops.len() < 2 {
        return stops.to_vec();
    }
    let mut out: Vec<GradStop> = Vec::with_capacity(stops.len() * per_span);
    out.push(stops[0]);
    for w in stops.windows(2) {
        let (a, b) = (&w[0], &w[1]);
        let (la, lb) = (
            [
                srgb_to_lin(a.color.r),
                srgb_to_lin(a.color.g),
                srgb_to_lin(a.color.b),
            ],
            [
                srgb_to_lin(b.color.r),
                srgb_to_lin(b.color.g),
                srgb_to_lin(b.color.b),
            ],
        );
        let n = if b.offset > a.offset { per_span } else { 0 };
        for k in 1..=n {
            // u=k/n：k=n 恰落 b.offset（保真停靠端点，避免重采样漂移）
            let u = k as f32 / n as f32;
            let t = a.offset + (b.offset - a.offset) * u;
            let lin = [
                la[0] + (lb[0] - la[0]) * u,
                la[1] + (lb[1] - la[1]) * u,
                la[2] + (lb[2] - la[2]) * u,
            ];
            let mix = |x: u8, y: u8| libm::roundf(x as f32 + (y as f32 - x as f32) * u) as u8;
            let alpha = mix(a.color.a, b.color.a);
            // k==n 时落在 b 的 offset 上，直接取 b 原色（避免重采样舍入漂移）
            let color = if k == n {
                b.color
            } else {
                Rgba8 {
                    r: lin_to_srgb(lin[0]),
                    g: lin_to_srgb(lin[1]),
                    b: lin_to_srgb(lin[2]),
                    a: alpha,
                }
            };
            out.push(GradStop { offset: t, color });
        }
    }
    out
}

/// 数值或百分比：`"50%"` → `(50, true)`；`"0.5"` → `(0.5, false)`。
/// 车道AK2：绝对 CSS 单位按 96px 基准换算为数值（SVG2 长度；Chrome
/// 语义——oBB 空间里 `x2="1in"` 等价 `x2="96"`，WPT gradient-
/// objectBoundingBox-absolute-units）。非法 / 非有限 → None。
fn parse_len(v: &str) -> Option<(f32, bool)> {
    let t = v.trim();
    match t.strip_suffix('%') {
        Some(n) => crate::fnum::parse_f32(n.trim())
            .filter(|x| x.is_finite())
            .map(|x| (x, true)),
        None => {
            let lower = t.to_ascii_lowercase();
            let (num, unit) = match lower.char_indices().find(|(_, c)| c.is_ascii_alphabetic()) {
                Some((i, _)) => lower.split_at(i),
                None => (lower.as_str(), ""),
            };
            if !unit.is_empty() {
                let k = match unit {
                    "px" => 1.0,
                    "pt" => 96.0 / 72.0,
                    "pc" => 16.0,
                    "mm" => 96.0 / 25.4,
                    "cm" => 96.0 / 2.54,
                    "in" => 96.0,
                    "q" => 96.0 / 101.6,
                    _ => return None, // em/ex 等相对单位不支持
                };
                return crate::fnum::parse_f32(num.trim())
                    .filter(|x| x.is_finite())
                    .map(|x| (x * k, false));
            }
            crate::fnum::parse_f32(t)
                .filter(|x| x.is_finite())
                .map(|x| (x, false))
        }
    }
}

/// stop-color 原文 → 颜色（车道P）：`currentColor` 解析到宿主渐变的
/// `color`（链上未指定 → None，调用方落黑默认）；`inherit` 解析到宿主
/// 渐变的 `stop-color`。color-prop-01-b / styling-inherit-01-b 考点。
fn resolve_stop_color(
    raw: &str,
    ctx_color: Option<Rgba8>,
    ctx_stop_color: Option<Rgba8>,
) -> Option<Rgba8> {
    match parse_color(raw) {
        Some(Color::Rgb8 { r, g, b }) => Some(Rgba8 { r, g, b, a: 255 }),
        // 车道NG：stop-color 自带 alpha（rgba()/#rrggbbaa/…）——
        // parse_stop 与 stop-opacity 相乘
        Some(Color::Rgba { r, g, b, a }) => Some(Rgba8 { r, g, b, a }),
        Some(Color::CurrentColor) => ctx_color,
        Some(Color::Inherit) => ctx_stop_color,
        None => None,
    }
}

/// 单个 stop 原始属性 → GradStop（offset 钳 [0,1]；stop-color 默认黑；
/// stop-opacity 乘进 color.a）。
/// `ctx_color`/`ctx_stop_color`：宿主渐变元素解析后的 `color` /
/// `stop-color`——stop 自身的 `currentColor`/`inherit`（及**缺省值**，
/// stop-color 为继承属性）从这里解析。
fn parse_stop(raw: &StopRaw, ctx_color: Option<Rgba8>, ctx_stop_color: Option<Rgba8>) -> GradStop {
    let offset = match raw.offset.as_deref().and_then(parse_len) {
        Some((v, pct)) => {
            let t = if pct { v / 100.0 } else { v };
            t.clamp(0.0, 1.0)
        }
        None => 0.0, // 缺省 / 非法按 0（宽容，与 opacity 族处理一致）
    };
    let rgba = raw
        .color
        .as_deref()
        .and_then(|v| resolve_stop_color(v, ctx_color, ctx_stop_color))
        // 缺省 / 非法：黑（规范默认 stop-color: black）
        .unwrap_or(Rgba8 {
            r: 0,
            g: 0,
            b: 0,
            a: 255,
        });
    let op = raw
        .opacity
        .as_deref()
        // 车道OS：<percentage> 接受（`stop-opacity="50%"`，Chrome pct
        // 探针实证；旧纯 number 解析拒收 % 整条回退 1）
        .and_then(crate::fnum::parse_ratio_or_pct)
        .map(|(n, pct)| if pct { n / 100.0 } else { n })
        .filter(|n| n.is_finite())
        .map(|v| v.clamp(0.0, 1.0))
        .unwrap_or(1.0);
    GradStop {
        offset,
        color: Rgba8 {
            r: rgba.r,
            g: rgba.g,
            b: rgba.b,
            // 车道NG：stop-color 自带 alpha × stop-opacity 相乘（Chrome
            // cg12 探针 0.5×0.5 → a8=64 逐位实证）。色 alpha 恒 255 时
            // 等价 `round(255·op)`——既有语料逐位不变。
            a: libm::roundf(255.0 * op * (f32::from(rgba.a) / 255.0)).clamp(0.0, 255.0) as u8,
        },
    }
}

/// 停靠点定稿：**文档序**运行最大值钳制——后停靠 offset 小于之前
/// 最大者抬升到该值（规范原文；等值硬边保留，重叠点取后者，Chrome
/// 同款，pservers-grad-16-b）。不做重排序：钳后文档序即单调不减。
/// （先 sort 再钳的旧实现把 blue@0.1 排回队首，右半残留前停靠色，
/// 与 Chrome 的「钳到 0.5 + 纯色垫」不符。）
fn finalize_stops(mut stops: Vec<GradStop>) -> Vec<GradStop> {
    let mut prev = 0.0f32;
    for st in stops.iter_mut() {
        if st.offset < prev {
            st.offset = prev;
        } else {
            prev = st.offset;
        }
    }
    stops
}

// ============ 烘焙：参数空间 → 设备空间 ============

/// 把渐变定义烘焙成 [`Paint`]。
/// - `bbox`：objectBoundingBox 模式的设备空间 AABB（None/零面积 → 不描绘）；
/// - `m`：userSpaceOnUse 模式的累积矩阵（引用渐变的元素所在坐标系）；
/// - `vw/vh`：根视口尺寸（userSpaceOnUse 百分比解析用，罕见路径）。
///
/// 0 停不描绘；1 停按规范降级为纯色。
pub fn bake_paint(
    def: &GradientDef,
    bbox: Option<[f32; 4]>,
    m: Mat2x3,
    vw: f32,
    vh: f32,
) -> Option<Paint> {
    if def.stops.is_empty() {
        return None;
    }
    if def.stops.len() == 1 {
        return Some(Paint::Solid(def.stops[0].color));
    }
    if def.radial {
        bake_radial(def, bbox, m, vw, vh).map(Paint::RadialGradient)
    } else {
        bake_linear(def, bbox, m, vw, vh).map(Paint::LinearGradient)
    }
}

fn bake_linear(
    def: &GradientDef,
    bbox: Option<[f32; 4]>,
    m: Mat2x3,
    vw: f32,
    vh: f32,
) -> Option<LinearGradient> {
    // 默认 x1=0% y1=0% x2=100% y2=0%（SVG 规范）
    let (p0, p1) = if def.units == GradUnits::UserSpaceOnUse {
        (
            m.apply(user_coord(def.p.x1, 0.0, vw), user_coord(def.p.y1, 0.0, vh)),
            m.apply(
                user_coord(def.p.x2, 100.0, vw),
                user_coord(def.p.y2, 0.0, vh),
            ),
        )
    } else {
        let bb = nonzero_bbox(bbox)?;
        let (w, h) = (bb[2] - bb[0], bb[3] - bb[1]);
        let u0 = frac_t(def.p.x1, 0.0);
        let v0 = frac_t(def.p.y1, 0.0);
        let u1 = frac_t(def.p.x2, 1.0);
        let v1 = frac_t(def.p.y2, 0.0);
        let (du, dv) = (u1 - u0, v1 - v0);
        if du == 0.0 || dv == 0.0 || w == h {
            // 轴对齐 / 正方形 bbox：设备空间端点 = 单位空间轴的像，
            // 与单位空间投影逐位一致（保持既有路径零漂移）
            (
                (bb[0] + u0 * w, bb[1] + v0 * h),
                (bb[0] + u1 * w, bb[1] + v1 * h),
            )
        } else {
            // SVG 1.1 objectBoundingBox 单位空间语义（车道E）：t 在单位
            // 正方形（bbox 的仿射像）里对轴做欧氏投影，再仿射回设备
            // ——非正方形 bbox 的斜轴下等 t 线不与设备轴垂直（Chrome
            // 实测：pservers-grad-04 对角渐变逐点钉死）。目标线性函数
            // （A = du/w，B = dv/h，L = du²+dv²）：
            //   t(x,y) = (A·x + B·y − D)/L，
            //   D = A·bbx + B·bby + du·u0 + dv·v0
            // 用轴 (p0,p1) 精确重表任意线性函数：
            //   p0 = (A,B)·D/(A²+B²)，p1 = (A,B)·(D+L)/(A²+B²)
            let a = du / w;
            let b = dv / h;
            let l = du * du + dv * dv;
            let ab2 = a * a + b * b;
            let d = a * bb[0] + b * bb[1] + du * u0 + dv * v0;
            (
                (a * d / ab2, b * d / ab2),
                (a * (d + l) / ab2, b * (d + l) / ab2),
            )
        }
    };
    Some(LinearGradient {
        p0,
        p1,
        stops: def.stops.clone(),
        spread: def.spread,
    })
}

fn bake_radial(
    def: &GradientDef,
    bbox: Option<[f32; 4]>,
    m: Mat2x3,
    vw: f32,
    vh: f32,
) -> Option<RadialGradient> {
    if def.units == GradUnits::UserSpaceOnUse {
        // 默认 cx/cy/r = 50%；百分比按视口尺寸近似解析（罕见路径）
        let cx = user_coord(def.p.cx, 50.0, vw);
        let cy = user_coord(def.p.cy, 50.0, vh);
        let r = user_coord(def.p.r, 50.0, diag_len(vw, vh));
        // fx 默认 = cx（含 cx 缺省 50% 的情形）
        let fx = match def.p.fx {
            Some(_) => user_coord(def.p.fx, 50.0, vw),
            None => cx,
        };
        let fy = match def.p.fy {
            Some(_) => user_coord(def.p.fy, 50.0, vh),
            None => cy,
        };
        let c = m.apply(cx, cy);
        let f = m.apply(fx, fy);
        // 车道PD：CTM 线性部分非相似（各向异性 scale/skew）时设备等值
        // 线是**椭圆**（Chrome/Skia 把采样点经总矩阵逆映射回梯度定义
        // 空间求 t）——mean_scale 圆近似是大面积色带错位的根源（noto
        // 探针 p09 与 gt 各向异性同值 0.3536 互证）。新路径：S = lin(m)⁻¹
        // 存全 2×2，r/fr 保持用户单位（梯度定义空间），采样端 grad_xy
        // 完成逆映射。相似变换（均匀缩放×旋转/翻转）走旧路径逐位不变。
        if lin_non_similarity(&m) {
            if let Some(minv) = m.inverse() {
                return Some(RadialGradient {
                    c,
                    r,
                    // fr 与 r 同域：用户单位原值（% 相对归一化对角线解析不变）
                    fr: user_coord(def.p.fr, 0.0, diag_len(vw, vh)),
                    f,
                    stops: def.stops.clone(),
                    spread: def.spread,
                    sx: minv.a,
                    sy: minv.d,
                    shx: minv.c,
                    shy: minv.b,
                });
            }
        }
        // 车道BI：fr 与 r 同域（用户单位经矩阵平均缩放；% 相对归一化对角线）
        let fr = user_coord(def.p.fr, 0.0, diag_len(vw, vh)) * m.mean_scale();
        Some(RadialGradient {
            c,
            r: r * m.mean_scale(),
            fr,
            f,
            stops: def.stops.clone(),
            spread: def.spread,
            sx: 1.0,
            sy: 1.0,
            shx: 0.0,
            shy: 0.0,
        })
    } else {
        let bb = nonzero_bbox(bbox)?;
        let (w, h) = (bb[2] - bb[0], bb[3] - bb[1]);
        let cx = frac_t(def.p.cx, 0.5);
        let cy = frac_t(def.p.cy, 0.5);
        let fxt = match def.p.fx {
            Some(_) => frac_t(def.p.fx, 0.5),
            None => cx,
        };
        let fyt = match def.p.fy {
            Some(_) => frac_t(def.p.fy, 0.5),
            None => cy,
        };
        let stops = def.stops.clone();
        let spread = def.spread;
        // 近方形判定用相对容差（1e-2）：扁平化多边形的 bbox 带真圆的
        // 弦高不对称（实测 r=5..84 → 相对 |w−h| 7.3e-3 封顶，见 path::
        // ellipse_pts 的 TOL 自适应分段），若据此走椭圆路径会让既有圆
        // 渲染产生 ±1 LSB 漂移——几何上 w≈h 时两模型等价（Chrome 对真
        // 圆的 bbox 精确方形，单位空间椭圆 ≡ 圆），吸附回设备圆路径；
        // 代价是真·近方形（宽高差 <1%）设计的椭圆化误差 ≤0.5% 半径，
        // 亚像素级。
        let near_square = (w - h).abs() <= 1e-2 * w.max(h);
        if near_square {
            // 正方形 bbox：单位空间椭圆 ≡ 设备空间圆（frac·diag(w,w) =
            // frac·w），保持既有设备圆路径逐位不变（wt/radial-focal 红线）
            let frv = frac_t(def.p.fr, 0.0);
            Some(RadialGradient {
                c: (bb[0] + cx * w, bb[1] + cy * h),
                // r 相对归一化对角线 sqrt(w²+h²)/sqrt(2)（SVG 规范）
                r: frac_t(def.p.r, 0.5) * diag_len(w, h),
                fr: frv * diag_len(w, h),
                f: (bb[0] + fxt * w, bb[1] + fyt * h),
                stops,
                spread,
                sx: 1.0,
                sy: 1.0,
                shx: 0.0,
                shy: 0.0,
            })
        } else {
            // 非正方形 bbox：Chrome/Skia 把渐变定义在 bbox 单位空间算圆锥
            // 再仿射回设备（设备等值线 = 椭圆）。单位空间的归一化对角线
            // = 1，故 r 直接取分数值；c/f 存设备空间点，采样时坐标差按
            // (1/w, 1/h) 逆缩放到单位空间求 t（wt/radial-focal，f7 探针）。
            Some(RadialGradient {
                c: (bb[0] + cx * w, bb[1] + cy * h),
                r: frac_t(def.p.r, 0.5),
                fr: frac_t(def.p.fr, 0.0),
                f: (bb[0] + fxt * w, bb[1] + fyt * h),
                stops,
                spread,
                sx: 1.0 / w,
                sy: 1.0 / h,
                shx: 0.0,
                shy: 0.0,
            })
        }
    }
}

/// bbox 分数：百分比 /100，裸数即分数；缺省用 `default`。
fn frac_t(v: Option<(f32, bool)>, default: f32) -> f32 {
    match v {
        Some((n, true)) => n / 100.0,
        Some((n, false)) => n,
        None => default,
    }
}

/// userSpaceOnUse 坐标：裸数 = 用户单位；百分比 = 视口尺寸分数（近似）；
/// 缺省按 `default_pct`%。
fn user_coord(v: Option<(f32, bool)>, default_pct: f32, dim: f32) -> f32 {
    match v {
        Some((n, true)) => n / 100.0 * dim,
        Some((n, false)) => n,
        None => default_pct / 100.0 * dim,
    }
}

/// 归一化对角线 sqrt(w²+h²)/sqrt(2)。
fn diag_len(w: f32, h: f32) -> f32 {
    libm::sqrtf(w * w + h * h) / core::f32::consts::SQRT_2
}

/// 零宽/高的 AABB 视为无 bbox（objectBoundingBox 渐变不描绘，规范语义）。
fn nonzero_bbox(bb: Option<[f32; 4]>) -> Option<[f32; 4]> {
    bb.filter(|b| b[2] > b[0] && b[3] > b[1])
}

// ============ 采样：设备空间像素 → 颜色 ============

/// paint 单点求值（纯色直接返回；raster 的通用分发口）。
pub fn sample_paint(paint: &Paint, x: f32, y: f32) -> Rgba8 {
    match paint {
        Paint::Solid(c) => *c,
        Paint::LinearGradient(g) => sample_linear(g, x, y),
        Paint::RadialGradient(g) => sample_radial(g, x, y),
    }
}

/// 线性渐变采样：t = (p−p0)·d / |d|²，spread 折叠后插值。
/// p0 == p1 退化：整面末停色（SVG 2）。
pub fn sample_linear(g: &LinearGradient, x: f32, y: f32) -> Rgba8 {
    let (dx, dy) = (g.p1.0 - g.p0.0, g.p1.1 - g.p0.1);
    let len2 = dx * dx + dy * dy;
    if len2 <= 0.0 {
        return last_stop_color(&g.stops);
    }
    let t = ((x - g.p0.0) * dx + (y - g.p0.1) * dy) / len2;
    interp_stops(
        &g.stops,
        apply_spread(t, g.spread),
        bayer_off(x as i32, y as i32, TILE_SHIFT_TALL),
    )
}

/// 车道LG：外焦点（|F−C| ≥ r）Skia conical 分支常量。
///
/// Chrome/Skia 实证（SkConicalGradient.cpp + SkRasterPipeline_opts.h，
/// 探针级逐像素验证 pservers-grad-13 外焦点 3 格干净区 0 失配）：
/// Blink 把 (焦点F, r0=0, 圆心C, r1=r) 原样交给 two-point conical——
/// **不钳边、不投影**。归一半径 `fR1 = r/d`（d=|C−F|），分支：
/// - `fR1 > 1`（内焦点）：well_behaved，既有规范公式（逐位不变红线）；
/// - `|1−fR1| ≤ 2.44e-4`（焦点贴圆）：focal_on_circle 阶段；
/// - `fR1 < 1`（外焦点）：`xy_to_2pt_conical_greater` + 二值 mask。
#[derive(Clone, Copy, Debug, PartialEq)]
enum OuterFocal {
    /// t = √(px²−py²) − px·(1/fR1)（postScale 后坐标）；t≤0 或 NaN → 锥外
    Greater {
        /// C − F（梯度空间）
        ex: f32,
        ey: f32,
        /// 1/d²
        inv_l2: f32,
        /// fR1/(fR1²−1)（负值）
        s: f32,
        /// 1/√|fR1²−1|
        tsc: f32,
        /// 1/fR1
        p0: f32,
    },
    /// t = x + y²/x（x=0.5x₀, y=0.5y₀，矩阵 postScale(0.5,0.5)）；px=0 → NaN → 锥外
    OnCircle { ex: f32, ey: f32, inv_l2: f32 },
}

/// Skia `SkScalarNearlyZero` 缺省容差（1/(1<<12)，isFocalOnCircle 判据）。
const NEARLY_ZERO_SKIA: f32 = 1.0 / (1u32 << 12) as f32;

/// mask 透明（Skia `apply_vector_mask` 把全通道清零——alpha 0 不上屏，
/// 与 stop-opacity=0 同一机制，语料已大量存在）。
const TRANSPARENT: Rgba8 = Rgba8 {
    r: 0,
    g: 0,
    b: 0,
    a: 0,
};

/// 车道PD：设备→梯度空间逆映射 2×2 的 x 分量（`Δx·sx + Δy·shx`）。
/// `shx == 0.0`（轴对齐既有语境）时 `Δy·0.0 = ±0.0`，对有限值加法
/// 位型不变——既有路径逐位保留。
#[inline]
fn grad_x(g: &RadialGradient, dx: f32, dy: f32) -> f32 {
    dx * g.sx + dy * g.shx
}

/// 逆映射 y 分量（`Δx·shy + Δy·sy`；`shy == 0.0` 同上逐位不变）。
#[inline]
fn grad_y(g: &RadialGradient, dx: f32, dy: f32) -> f32 {
    dx * g.shy + dy * g.sy
}

/// 逆映射全向量（`(grad_x, grad_y)`，逐分量位型与拆开算一致）。
#[inline]
fn grad_xy(g: &RadialGradient, dx: f32, dy: f32) -> (f32, f32) {
    (grad_x(g, dx, dy), grad_y(g, dx, dy))
}

/// 车道LG：外焦点帧（|F−C| ≥ r 时 Some；内焦点 None = 既有公式逐位不变）。
fn outer_focal_frame(g: &RadialGradient) -> Option<OuterFocal> {
    let ex = grad_x(g, g.c.0 - g.f.0, g.c.1 - g.f.1);
    let ey = grad_y(g, g.c.0 - g.f.0, g.c.1 - g.f.1);
    let d2 = ex * ex + ey * ey;
    if d2 < g.r * g.r {
        return None;
    }
    let inv_l2 = 1.0 / d2;
    let fr1 = g.r * libm::sqrtf(inv_l2);
    if (1.0 - fr1).abs() <= NEARLY_ZERO_SKIA {
        return Some(OuterFocal::OnCircle { ex, ey, inv_l2 });
    }
    let s = fr1 / (fr1 * fr1 - 1.0);
    let tsc = 1.0 / libm::sqrtf((fr1 * fr1 - 1.0).abs());
    let p0 = 1.0 / fr1;
    Some(OuterFocal::Greater {
        ex,
        ey,
        inv_l2,
        s,
        tsc,
        p0,
    })
}

/// 车道LG：外焦点 t 场（归一空间 x₀/y₀ → postScale → Skia 阶段公式）。
/// `None` = `mask_2pt_conical_degenerates`（t≤0 或 NaN）→ 锥外透明。
#[inline]
fn outer_focal_t(of: OuterFocal, gx: f32, gy: f32) -> Option<f32> {
    match of {
        OuterFocal::Greater {
            ex,
            ey,
            inv_l2,
            s,
            tsc,
            p0,
        } => {
            let x0 = (gx * ex + gy * ey) * inv_l2;
            let y0 = (gx * ey - gy * ex) * inv_l2;
            let px = x0 * s;
            let py = y0 * tsc;
            // q<0 → sqrtf NaN；`!(t > 0)` ≡ Skia `(t <= 0) | (t != t)` 取反
            let t = libm::sqrtf(px * px - py * py) - px * p0;
            if t > 0.0 {
                Some(t)
            } else {
                None
            }
        }
        OuterFocal::OnCircle { ex, ey, inv_l2 } => {
            let x0 = (gx * ex + gy * ey) * inv_l2;
            let y0 = (gx * ey - gy * ex) * inv_l2;
            let (px, py) = (x0 * 0.5, y0 * 0.5);
            let t = px + py * py / px;
            if t > 0.0 {
                Some(t)
            } else {
                None
            }
        }
    }
}

/// 径向渐变采样（SVG 规范焦点公式，在「梯度空间」求值）：
/// 梯度空间坐标 = 设备坐标差按 (sx, sy) 逆缩放（圆语义 sx=sy=1 即设备
/// 空间；oBB 非方形 bbox 为 bbox 单位空间，等值线椭圆——见
/// [`RadialGradient`]）。
///
/// 内焦点（|F−C| < r）：像素 p 相对焦点 F 的射线交渐变圆（圆心 C、
/// 半径 r）于 `p' = F + u·(p−F)`，则 `t = 1/u`。解二次方程
/// `|F + u·d − C|² = r²`（d = p−F，e = F−C，均梯度空间）得
/// `u = (√((e·d)² + |d|²(r²−|e|²)) − e·d) / |d|²`。
///
/// 外焦点（|F−C| ≥ r，车道LG）：Chrome/Skia **不钳边也不投影**——
/// Blink 把 (焦点, 0, 圆心, r) 原样交给 `SkConicalGradient`（FocalData
/// fFocalX=0），走 two-point conical 的 `xy_to_2pt_conical_greater` 阶段
/// 加 `mask_2pt_conical_degenerates`（见 SkConicalGradient.cpp /
/// SkRasterPipeline_opts.h；DB 车道「三模型证伪」漏掉的正是锥区二值
/// mask——锥外整片透明非 F=C blob）。落地模型见 [`outer_focal_frame`]。
///
/// r ≤ 0 末停色。
pub fn sample_radial(g: &RadialGradient, x: f32, y: f32) -> Rgba8 {
    if g.r <= 0.0 {
        return last_stop_color(&g.stops);
    }
    let (ex0, ey0) = (
        grad_x(g, g.f.0 - g.c.0, g.f.1 - g.c.1),
        grad_y(g, g.f.0 - g.c.0, g.f.1 - g.c.1),
    );
    let degenerate = ex0 * ex0 + ey0 * ey0 >= g.r * g.r;
    // fr>0 分支的消费坐标保持「钳边后」原式（车道LG 零回归红线：
    // fr>0 + 外焦点的既有口径不动）；fr==0 的外焦点在下方分流
    let (fx, fy) = if g.fr > 0.0 && degenerate {
        (g.c.0, g.c.1)
    } else {
        (g.f.0, g.f.1)
    };
    let (ex, ey) = (
        grad_x(g, fx - g.c.0, fy - g.c.1),
        grad_y(g, fx - g.c.0, fy - g.c.1),
    );
    let (dx, dy) = grad_xy(g, x - fx, y - fy);
    let dd = dx * dx + dy * dy;
    // 车道BI：SVG2 fr>0（焦点圆）——双圆二次方程直接解 t（更大的根）。
    // fr == 0 走焦点路径（逐位不变红线）；fr >= r 的非法形态
    // 亦按 0 处理（简化登记，语料未用）。
    if g.fr > 0.0 && g.fr < g.r {
        // |B − tA|² = (fr + t·dr)²，A = c−f、B = p−f（均梯度空间）：
        //   a·t² − 2b·t + c0 = 0，a = A·A − dr²，b = A·B + fr·dr，c0 = B·B − fr²
        let dr = g.r - g.fr;
        let a = ex * ex + ey * ey - dr * dr;
        let b = g.fr * dr - (ex * dx + ey * dy);
        let c0 = dd - g.fr * g.fr;
        let disc = b * b - a * c0;
        let t = if a.abs() <= 1e-6 * g.r * g.r {
            // 双圆内切（a≈0）：退化线性方程
            if b.abs() <= 1e-12 {
                return last_stop_color(&g.stops);
            }
            c0 / (2.0 * b)
        } else if disc <= 0.0 {
            return interp_stops(
                &g.stops,
                apply_spread(f32::MAX / 4.0, g.spread),
                bayer_off(x as i32, y as i32, TILE_SHIFT_TALL),
            );
        } else {
            let s = libm::sqrtf(disc);
            if a > 0.0 {
                (b + s) / a
            } else {
                (b - s) / a
            }
        };
        return interp_stops(
            &g.stops,
            apply_spread(t, g.spread),
            bayer_off(x as i32, y as i32, TILE_SHIFT_TALL),
        );
    }
    // 车道LG：外焦点（|F−C| ≥ r）走 Skia conical greater/on-circle 语义
    if degenerate {
        if let Some(of) = outer_focal_frame(g) {
            let (ox, oy) = grad_xy(g, x - g.f.0, y - g.f.1);
            return match outer_focal_t(of, ox, oy) {
                None => TRANSPARENT,
                Some(t) => interp_stops(
                    &g.stops,
                    apply_spread(t, g.spread),
                    bayer_off(x as i32, y as i32, TILE_SHIFT_TALL),
                ),
            };
        }
    }
    let (ex, ey) = (
        grad_x(g, fx - g.c.0, fy - g.c.1),
        grad_y(g, fx - g.c.0, fy - g.c.1),
    );
    let (dx, dy) = grad_xy(g, x - fx, y - fy);
    let dd = dx * dx + dy * dy;
    // 车道BI：SVG2 fr>0（焦点圆）——双圆二次方程直接解 t（更大的根）。
    // fr == 0 走焦点路径（逐位不变红线）；fr >= r 的非法形态
    // 亦按 0 处理（简化登记，语料未用）。
    if g.fr > 0.0 && g.fr < g.r {
        let dr = g.r - g.fr;
        // |B − tA|² = (fr + t·dr)²，A = c−f、B = p−f（均梯度空间）：
        //   a·t² − 2b·t + c0 = 0，a = A·A − dr²，b = A·B + fr·dr，c0 = B·B − fr²
        let a = ex * ex + ey * ey - dr * dr;
        let b = g.fr * dr - (ex * dx + ey * dy);
        let c0 = dd - g.fr * g.fr;
        let disc = b * b - a * c0;
        let t = if a.abs() <= 1e-6 * g.r * g.r {
            // 双圆内切（a≈0）：退化线性方程
            if b.abs() <= 1e-12 {
                return last_stop_color(&g.stops);
            }
            c0 / (2.0 * b)
        } else if disc <= 0.0 {
            return interp_stops(
                &g.stops,
                apply_spread(f32::MAX / 4.0, g.spread),
                bayer_off(x as i32, y as i32, TILE_SHIFT_TALL),
            );
        } else {
            let s = libm::sqrtf(disc);
            if a > 0.0 {
                (b + s) / a
            } else {
                (b - s) / a
            }
        };
        return interp_stops(
            &g.stops,
            apply_spread(t, g.spread),
            bayer_off(x as i32, y as i32, TILE_SHIFT_TALL),
        );
    }
    if dd <= 0.0 {
        // p 恰在焦点上：t = 0
        return interp_stops(
            &g.stops,
            apply_spread(0.0, g.spread),
            bayer_off(x as i32, y as i32, TILE_SHIFT_TALL),
        );
    }
    let ee = ex * ex + ey * ey;
    let ed = ex * dx + ey * dy;
    let disc = ed * ed + dd * (g.r * g.r - ee);
    if disc <= 0.0 {
        // 数值边界（射线与圆相切/不相交）：按轴外处理
        return interp_stops(
            &g.stops,
            apply_spread(f32::MAX / 4.0, g.spread),
            bayer_off(x as i32, y as i32, TILE_SHIFT_TALL),
        );
    }
    let u = (libm::sqrtf(disc) - ed) / dd;
    if u <= 0.0 {
        return last_stop_color(&g.stops);
    }
    interp_stops(
        &g.stops,
        apply_spread(1.0 / u, g.spread),
        bayer_off(x as i32, y as i32, TILE_SHIFT_TALL),
    )
}

/// spreadMethod 折叠：pad 钳 [0,1]；repeat 取小数；reflect 周期 2 锯齿折叠。
fn apply_spread(t: f32, s: SpreadMethod) -> f32 {
    if t.is_nan() {
        return 0.0;
    }
    match s {
        SpreadMethod::Pad => t.clamp(0.0, 1.0),
        SpreadMethod::Repeat => t - libm::floorf(t),
        SpreadMethod::Reflect => {
            let mut u = t - 2.0 * libm::floorf(t / 2.0); // ∈ [0, 2)
            if u > 1.0 {
                u = 2.0 - u;
            }
            u
        }
    }
}

/// 停靠点插值：t 落点分段线性（直 RGB+a，非预乘）；t 在首/末停外钳边界色。
fn interp_stops(stops: &[GradStop], t: f32, dith: f32) -> Rgba8 {
    let empty = Rgba8 {
        r: 0,
        g: 0,
        b: 0,
        a: 0,
    };
    let Some(first) = stops.first() else {
        return empty;
    };
    if t < first.offset {
        return first.color;
    }
    let last = &stops[stops.len() - 1];
    if t >= last.offset {
        return last.color;
    }
    for w in stops.windows(2) {
        let (a, b) = (&w[0], &w[1]);
        if t >= a.offset && t < b.offset {
            let denom = b.offset - a.offset;
            let u = if denom > 0.0 {
                (t - a.offset) / denom
            } else {
                1.0 // 等值停靠：硬边跳到后者
            };
            return lerp_rgba_dith(a.color, b.color, u, dith);
        }
    }
    last.color
}

/// 8×8 有序抖动（Chrome 148 预言机逐位逆向，2026-09-10 车道A）。
///
/// Chrome headless-shell 对渐变做 8×8 有序抖动：Skia 源码
/// （`src/opts/SkRasterPipeline_opts.h` HIGHP dither / `src/gpu/DitherUtils.cpp`
/// MakeDitherLUT）的交织矩阵 `M = y0x0y1x1y2x2`（即下表，[row&7][px&7]），
/// 幅度 `(M−31.5)/64` LSB。**索引带分块相位**：Chrome 分块光栅化下
/// sk_FragCoord 是块内坐标，实测（900×600 探针逐带 0.00% 复核）相位
/// 为 `2·((c>>8)&3)`——每 256 设备像素步进 +2，x/y 两轴独立；
/// 但视口宽 <512 时 tile 更高（64×1024 探针实测 512 步进，s4-02
/// 260 宽画布无步进）——Chrome 视口宽度决定 tile 尺寸的启发式。
/// 颜色 = 理想浮点 lerp（像素中心），输出 `floor(c+d+0.5)`。
/// 2 停靠/3 停靠、垂直/水平/径向同表同相位（多组探针交叉验证）。
/// 旧 4×4 表与实测差 29pp 噪声（frog 31.3% 残差的主因）。
const DITHER8: [[u8; 8]; 8] = [
    [0, 48, 12, 60, 3, 51, 15, 63],
    [32, 16, 44, 28, 35, 19, 47, 31],
    [8, 56, 4, 52, 11, 59, 7, 55],
    [40, 24, 36, 20, 43, 27, 39, 23],
    [2, 50, 14, 62, 1, 49, 13, 61],
    [34, 18, 46, 30, 33, 17, 45, 29],
    [10, 58, 6, 54, 9, 57, 5, 53],
    [42, 26, 38, 22, 41, 25, 37, 21],
];

/// 分块相位步进周期（右移位数）：视口宽 ≥512 → 256px tile（>>8）；
/// 否则 512px 高 tile（>>9）。见 [`DITHER8`] 注释。
pub const TILE_SHIFT_WIDE: u32 = 8;
pub const TILE_SHIFT_TALL: u32 = 9;

/// 分块相位修正后的坐标：`c + 2·((c>>shift)&3)`。
#[inline]
fn phased(c: i32, shift: u32) -> i32 {
    c + 2 * ((c >> shift) & 3)
}

/// 像素 (px, row) 的抖动偏移（LSB 单位 f32）：`(M − 31.5)/64`。
#[inline]
pub(crate) fn bayer_off(px: i32, row: i32, shift: u32) -> f32 {
    (DITHER8[(phased(row, shift) & 7) as usize][(phased(px, shift) & 7) as usize] as f32 - 31.5)
        * (1.0 / 64.0)
}

/// 带抖动的插值：各通道加 Bayer 偏移后再取整（对齐 Chrome 渐变观感）。
fn lerp_rgba_dith(a: Rgba8, b: Rgba8, u: f32, d: f32) -> Rgba8 {
    let ch = |x: u8, y: u8| -> u8 {
        round_half_away(x as f32 + (y as f32 - x as f32) * u + d).clamp(0.0, 255.0) as u8
    };
    Rgba8 {
        r: ch(a.r, b.r),
        g: ch(a.g, b.g),
        b: ch(a.b, b.b),
        a: ch(a.a, b.a),
    }
}

fn last_stop_color(stops: &[GradStop]) -> Rgba8 {
    stops.last().map(|st| st.color).unwrap_or(Rgba8 {
        r: 0,
        g: 0,
        b: 0,
        a: 0,
    })
}

// ============ 性能：span 求值器（wt/perf-span，逐位等价提速） ============

/// 位级 roundf（半值远离零）：与 `libm::roundf` 对全部输入（含 ±0/次正规/
/// ±inf/NaN）**逐位一致**，纯 ALU 位运算无函数调用。热点来源：lerp_rgba
/// 每像素 4 通道 × libm 软件实现 roundf（见 perf-2026-09-09 profile，
/// truncf/roundf 占 frog 渲染 ~25%）。
#[inline]
fn round_half_away(x: f32) -> f32 {
    let b = x.to_bits();
    let abs = b & 0x7fff_ffff;
    let e = ((abs >> 23) & 0xff) as i32 - 127; // 无偏指数；次正规数极小
    if e >= 23 {
        // |x| ≥ 2^23（含 ±inf 与 NaN）：尾数已是整数 / 特殊值，原样返回
        return x;
    }
    let sign = b & 0x8000_0000;
    if e < -1 {
        // |x| < 0.5（含 ±0 与全部次正规数）→ ±0
        return f32::from_bits(sign);
    }
    if e == -1 {
        // 0.5 ≤ |x| < 1：一律进到 ±1（半值远离零）
        return f32::from_bits(sign | 0x3f80_0000);
    }
    // 1 ≤ |x| < 2^23：尾数定点化后按半值档舍入（恰半也进位 = 远离零）
    let shift = (23 - e) as u32; // 1..=23
    let m = (abs & 0x007f_ffff) | 0x0080_0000; // 24 位定点尾数
    let half = 1u32 << (shift - 1);
    let frac = m & ((1u32 << shift) - 1);
    let int = m >> shift;
    let r = (int + u32::from(frac >= half)) as f32; // r ≤ 2^23，f32 精确
    f32::from_bits(sign | r.to_bits())
}

/// 停靠点插值（带段索引缓存）：扫描线内相邻像素 t 单调，通常命中同一停靠段。
/// `idx` 缓存上次命中段起点：t ≥ stops[idx].offset 时更早的窗口不可能命中
/// （其上界 ≤ stops[idx].offset ≤ t 与 `t < b.offset` 矛盾），从 idx 起扫
/// 与从头扫的首命中一致 → 与 [`interp_stops`] 结果逐位相同。
fn interp_stops_dith(stops: &[GradStop], t: f32, idx: &mut usize, dith: f32) -> Rgba8 {
    let empty = Rgba8 {
        r: 0,
        g: 0,
        b: 0,
        a: 0,
    };
    let Some(first) = stops.first() else {
        return empty;
    };
    if t < first.offset {
        return first.color;
    }
    let last = &stops[stops.len() - 1];
    if t >= last.offset {
        return last.color;
    }
    let mut i = if *idx < stops.len() && t >= stops[*idx].offset {
        *idx
    } else {
        0
    };
    while i + 1 < stops.len() {
        let (a, b) = (&stops[i], &stops[i + 1]);
        if t >= a.offset && t < b.offset {
            *idx = i;
            let denom = b.offset - a.offset;
            let u = if denom > 0.0 {
                (t - a.offset) / denom
            } else {
                1.0 // 等值停靠：硬边跳到后者
            };
            return lerp_rgba_dith(a.color, b.color, u, dith);
        }
        i += 1;
    }
    last.color
}

/// premul 域下限钳制：`c·α + dither` 的负值钳 0——Chrome/Skia premul
/// 颜色不可能为负，负抖动在暗色/低 α 通道被吃掉（at4/at3 探针逐位
/// 实证：`round(max(c·α+d, 0) + 255(1−α))` 与 Chrome 0.00% 失配）。
#[inline]
fn clamp_pm(v: f32) -> f32 {
    v.clamp(0.0, 255.0)
}

/// 停靠点插值的 **f32 无舍入**版本（车道AT）：返回 `([r,g,b] 直色 f32,
/// alpha f32[0,255])`，不做 u8 量化与抖动——[`SpanEval::at_premul`] 的
/// 插值基元。段扫描/等值停靠硬边语义与 [`interp_stops_dith`] 一致
/// （idx 缓存同构）。
fn interp_stops_f(stops: &[GradStop], t: f32, idx: &mut usize) -> ([f32; 3], f32) {
    let stop_f = |c: &Rgba8| ([c.r as f32, c.g as f32, c.b as f32], c.a as f32);
    let Some(first) = stops.first() else {
        return ([0.0; 3], 0.0);
    };
    if t < first.offset {
        return stop_f(&first.color);
    }
    let last = &stops[stops.len() - 1];
    if t >= last.offset {
        return stop_f(&last.color);
    }
    let mut i = if *idx < stops.len() && t >= stops[*idx].offset {
        *idx
    } else {
        0
    };
    while i + 1 < stops.len() {
        let (a, b) = (&stops[i], &stops[i + 1]);
        if t >= a.offset && t < b.offset {
            *idx = i;
            let denom = b.offset - a.offset;
            let u = if denom > 0.0 {
                (t - a.offset) / denom
            } else {
                1.0 // 等值停靠：硬边跳到后者
            };
            let (ca, cb) = (&a.color, &b.color);
            let ch = |x: u8, y: u8| x as f32 + (y as f32 - x as f32) * u;
            return (
                [ch(ca.r, cb.r), ch(ca.g, cb.g), ch(ca.b, cb.b)],
                ch(ca.a, cb.a),
            );
        }
        i += 1;
    }
    stop_f(&last.color)
}

/// 渐变 span 求值器：光栅化扫描线复用（`begin_row` 每行一次 → `at` 每像素）。
///
/// 与逐像素 `sample_paint` 的**逐位等价**保证（同表达式树）：
/// - 线性：行常量 `cy = (yc−p0.y)·dy` 与轴向量 `len2` 按原式预计算，
///   `t = ((x−p0.x)·dx + cy)/len2` 与原式每像素相同运算序 → 相同位。
///   `dx == 0`（垂直轴，frog 天空/水面形态）时行内 t 恒定：任一有限
///   `x−p0.x` 乘 0 得 ±0，`±0 + cy ≡ cy`（仅 cy=−0 时差个 −0.0 符号，
///   下游 spread/插值比较中 ±0.0 等价）→ 每行求值一次常数色。
/// - 径向：行常量（焦点退化折算、e 向量、r²−|e|²）按原式预计算，
///   每像素其余运算与 [`sample_radial`] 逐步相同。
/// - 停靠段索引缓存跨行保留（见 [`interp_stops_dith`]）。
pub struct SpanEval<'a> {
    /// 当前扫描线采样 y（begin_row 设置；径向 at() 用）
    yc: f32,
    /// 抖动分块相位步进（视口宽度决定，见 [`DITHER8`]）
    shift: u32,
    kind: SpanKind<'a>,
}

enum SpanKind<'a> {
    /// 行内/整面常数色（线性垂直轴的每行常数 / p0==p1 / r≤0 / 纯色退化）
    Const(Rgba8),
    Linear {
        g: &'a LinearGradient,
        dx: f32,
        len2: f32,
        /// (yc − p0.y) · dy，行常量
        cy: f32,
        /// 垂直轴（dx==0）时的每行常数色；非垂直轴 None
        row_const: Option<Rgba8>,
        idx: usize,
    },
    Radial {
        g: &'a RadialGradient,
        /// 焦点（车道LG：外焦点不再钳边，恒原 F）
        fx: f32,
        fy: f32,
        /// F − C（折算后，梯度空间：已乘 sx/sy）
        ex: f32,
        ey: f32,
        /// 设备→梯度空间逆缩放（每像素作用在坐标差上；车道PD：shx/shy
        /// 为交叉项，完整逆映射 `Δx·sx + Δy·shx / Δx·shy + Δy·sy`）
        sx: f32,
        sy: f32,
        shx: f32,
        shy: f32,
        /// r² − |e|²
        r2_ee: f32,
        idx: usize,
        /// 车道LG：外焦点帧（|F−C| ≥ r 时 Some，走 Skia conical 分支）
        outer: Option<OuterFocal>,
    },
}

impl<'a> SpanEval<'a> {
    /// 绑定 paint（渐变二类；纯色勿入——raster 已有纯色快路径）。
    /// `shift`：抖动分块相位步进（视口宽 ≥512 用 [`TILE_SHIFT_WIDE`]，
    /// 否则 [`TILE_SHIFT_TALL`]——Chrome tile 尺寸启发式，见 [`DITHER8`]）。
    pub fn new(paint: &'a Paint, shift: u32) -> Self {
        match paint {
            Paint::LinearGradient(g) => {
                let (dx, dy) = (g.p1.0 - g.p0.0, g.p1.1 - g.p0.1);
                let len2 = dx * dx + dy * dy;
                let const_color = if len2 <= 0.0 {
                    Some(last_stop_color(&g.stops)) // p0 == p1 退化（SVG 2）
                } else {
                    None
                };
                match const_color {
                    Some(c) => SpanEval {
                        yc: 0.0,
                        shift,
                        kind: SpanKind::Const(c),
                    },
                    None => SpanEval {
                        yc: 0.0,
                        shift,
                        kind: SpanKind::Linear {
                            g,
                            dx,
                            len2,
                            cy: 0.0,
                            row_const: None,
                            idx: 0,
                        },
                    },
                }
            }
            Paint::RadialGradient(g) => {
                if g.r <= 0.0 {
                    return SpanEval {
                        yc: 0.0,
                        shift,
                        kind: SpanKind::Const(last_stop_color(&g.stops)),
                    };
                }
                // 车道LG：|F−C| ≥ r 走 Skia conical greater/on-circle 语义
                //（与 sample_radial 同步；内焦点既有公式逐位不变）。
                // fx/fy/ex/ey 存钳边值仅供建档 fr>0 分支消费（既有口径）；
                // 外焦点 t 场在 at() 里从原 F 重取坐标差
                let outer = outer_focal_frame(g);
                let (fx, fy) = if outer.is_some() {
                    (g.c.0, g.c.1)
                } else {
                    (g.f.0, g.f.1)
                };
                let (ex, ey) = (
                    grad_x(g, fx - g.c.0, fy - g.c.1),
                    grad_y(g, fx - g.c.0, fy - g.c.1),
                );
                let ee = ex * ex + ey * ey;
                let r2 = g.r * g.r;
                SpanEval {
                    yc: 0.0,
                    shift,
                    kind: SpanKind::Radial {
                        g,
                        fx,
                        fy,
                        ex,
                        ey,
                        sx: g.sx,
                        sy: g.sy,
                        shx: g.shx,
                        shy: g.shy,
                        r2_ee: r2 - ee,
                        idx: 0,
                        outer,
                    },
                }
            }
            Paint::Solid(c) => SpanEval {
                yc: 0.0,
                shift,
                kind: SpanKind::Const(*c),
            },
        }
    }

    /// 换扫描线：预计算行常量。线性垂直轴折叠出每行常数色。
    pub fn begin_row(&mut self, yc: f32) {
        self.yc = yc;
        match &mut self.kind {
            SpanKind::Const(_) => {}
            SpanKind::Linear {
                g,
                dx,
                cy,
                row_const,
                ..
            } => {
                let dy = g.p1.1 - g.p0.1;
                *cy = (yc - g.p0.1) * dy;
                // 抖动模式下行内色随 Bayer(x) 变化——行常量捷径停用
                //（cy 预计算仍生效，at() 里 t 用 cy/len2 与原式等价）
                let _ = *dx;
                *row_const = None;
            }
            SpanKind::Radial { .. } => {}
        }
    }

    /// 垂直轴（dx==0）线性渐变的每行 8 色图案（wt/perf-raster）。
    /// 像素 px 的颜色 = `colors[(px + 2·((px>>shift)&3)) & 7]`——
    /// Chrome 8×8 抖动列相位沿行按 2·((px>>shift)&3) 步进（256/512px
    /// 分块），块内周期 8。
    ///
    /// 与 [`Self::at`] **逐位一致**：dx==0 时行内 t 恒为 cy/len2
    /// （(x−p0.x)·0 = ±0，±0+cy ≡ cy；cy=±0 时商 ±0 在 spread 比较/
    /// 插值算术中值等价），ts 每行求值一次，8 色仅抖动列不同——
    /// 直接复用 [`interp_stops_dith`] 同函数同实参（idx 缓存推进
    /// 序列：行内 ts 恒定 → 与逐像素收敛到同段）。Const 退化
    /// （p0==p1/r≤0/纯色/停靠段外钳边色）返回 8 份常量。
    /// 非垂直轴线性 / 径向返回 None（不支持，走逐像素）。
    pub fn vertical_row_colors(&mut self) -> Option<[Rgba8; 8]> {
        let drow = phased((self.yc - 0.5) as i32, self.shift) & 7;
        match &mut self.kind {
            SpanKind::Const(c) => Some([*c; 8]),
            SpanKind::Linear {
                g,
                dx,
                len2,
                cy,
                idx,
                ..
            } => {
                if *dx != 0.0 {
                    return None;
                }
                let ts = apply_spread(*cy / *len2, g.spread);
                let mut out = [Rgba8 {
                    r: 0,
                    g: 0,
                    b: 0,
                    a: 0,
                }; 8];
                for (j, o) in out.iter_mut().enumerate() {
                    let d = (DITHER8[drow as usize][j] as f32 - 31.5) * (1.0 / 64.0);
                    *o = interp_stops_dith(&g.stops, ts, idx, d);
                }
                Some(out)
            }
            SpanKind::Radial { .. } => None,
        }
    }

    /// 像素求值（x = px + 0.5；y 即 begin_row 的 yc）。
    /// 渐变走 4×4 Bayer 抖动（Chrome/Skia 同款观感）；Const 色不受影响。
    #[inline]
    pub fn at(&mut self, x: f32) -> Rgba8 {
        let yc = self.yc;
        let dith = bayer_off((x - 0.5) as i32, (yc - 0.5) as i32, self.shift);
        match &mut self.kind {
            SpanKind::Const(c) => *c,
            SpanKind::Linear {
                g,
                dx,
                len2,
                cy,
                row_const,
                idx,
            } => {
                if let Some(c) = *row_const {
                    return c;
                }
                let t = ((x - g.p0.0) * *dx + *cy) / *len2;
                interp_stops_dith(&g.stops, apply_spread(t, g.spread), idx, dith)
            }
            SpanKind::Radial {
                g,
                fx,
                fy,
                ex,
                ey,
                sx,
                sy,
                shx,
                shy,
                r2_ee,
                idx,
                outer,
            } => {
                // 坐标差先逆映射到梯度空间（sx=sy=1、shx=shy=0 时乘法
                // 为 ×1.0/×0.0 位型无操作——既有路径逐位不变）
                let (dx, dy) = (
                    (x - *fx) * *sx + (yc - *fy) * *shx,
                    (x - *fx) * *shy + (yc - *fy) * *sy,
                );
                let dd = dx * dx + dy * dy;
                // 车道BI：SVG2 fr>0——双圆二次方程直接解 t（与
                // sample_radial 的 fr 分支同式同序，逐位一致）
                if g.fr > 0.0 && g.fr < g.r {
                    let dr = g.r - g.fr;
                    let a = *ex * *ex + *ey * *ey - dr * dr;
                    let b = g.fr * dr - (*ex * dx + *ey * dy);
                    let c0 = dd - g.fr * g.fr;
                    let disc = b * b - a * c0;
                    let t = if a.abs() <= 1e-6 * g.r * g.r {
                        if b.abs() <= 1e-12 {
                            return last_stop_color(&g.stops);
                        }
                        c0 / (2.0 * b)
                    } else if disc <= 0.0 {
                        return interp_stops_dith(
                            &g.stops,
                            apply_spread(f32::MAX / 4.0, g.spread),
                            idx,
                            dith,
                        );
                    } else {
                        let s = libm::sqrtf(disc);
                        if a > 0.0 {
                            (b + s) / a
                        } else {
                            (b - s) / a
                        }
                    };
                    return interp_stops_dith(&g.stops, apply_spread(t, g.spread), idx, dith);
                }
                // 车道LG：外焦点（|F−C| ≥ r）Skia conical 分支（与
                // sample_radial 同式同序；坐标差从原 F 重取）
                if let Some(of) = *outer {
                    let (ox, oy) = (
                        (x - g.f.0) * *sx + (yc - g.f.1) * *shx,
                        (x - g.f.0) * *shy + (yc - g.f.1) * *sy,
                    );
                    return match outer_focal_t(of, ox, oy) {
                        None => TRANSPARENT,
                        Some(t) => {
                            interp_stops_dith(&g.stops, apply_spread(t, g.spread), idx, dith)
                        }
                    };
                }
                if dd <= 0.0 {
                    // p 恰在焦点上：t = 0
                    return interp_stops_dith(&g.stops, apply_spread(0.0, g.spread), idx, dith);
                }
                let ed = *ex * dx + *ey * dy;
                let disc = ed * ed + dd * *r2_ee;
                if disc <= 0.0 {
                    // 数值边界（射线与圆相切/不相交）：按轴外处理
                    return interp_stops_dith(
                        &g.stops,
                        apply_spread(f32::MAX / 4.0, g.spread),
                        idx,
                        dith,
                    );
                }
                let u = (libm::sqrtf(disc) - ed) / dd;
                if u <= 0.0 {
                    return last_stop_color(&g.stops);
                }
                // 车道AT：dith 必须传入——旧代码走 interp_stops_cached（dith=0）
                // 把径向渐变的 8×8 抖动整个丢掉（±1 LSB 级偏差 ~40% 像素，
                // span_eval_matches_sample_paint_radial 的 ±1 容差掩盖了它）
                interp_stops_dith(&g.stops, apply_spread(1.0 / u, g.spread), idx, dith)
            }
        }
    }

    /// 像素求值的 f32 输出（车道AT，Chrome/Skia 光栅管线模型）：
    /// 返回 `(premul RGB [0,255], 直 alpha [0,1])`——premul 域加 8×8 抖动
    /// 并钳 `[0,255]`（负抖动不越过 0——Chrome `clamp` 语义，暗色/低 α
    /// 通道实测逐位），alpha **不加抖动**（Skia dither 只作用 RGB）。
    /// 与 [`Self::at`] 的差异：不做 unpremul 中间量化、alpha 无抖动——
    /// Chrome「插值 → premul → dither → 合成 → 单次舍入」管线的输出基元，
    /// 消费方（raster 渐变填充）据此对帧缓冲做单次舍入 src-over。
    /// 不透明渐变（a=1）经此路径与 [`Self::at`] 逐字节一致（pm=clamp(c+d)
    /// 的舍入 ≡ at 的 round(c+d)）。
    #[inline]
    pub fn at_premul(&mut self, x: f32) -> ([f32; 3], f32) {
        let yc = self.yc;
        let dith = bayer_off((x - 0.5) as i32, (yc - 0.5) as i32, self.shift);
        match &mut self.kind {
            SpanKind::Const(c) => {
                let a = c.a as f32 / 255.0;
                ([c.r as f32 * a, c.g as f32 * a, c.b as f32 * a], a)
            }
            SpanKind::Linear {
                g,
                dx: _,
                len2,
                cy,
                row_const: _,
                idx,
            } => {
                // 与 at() 同式同运算序：((x−p0.x)·dx + cy)/len2
                let t = ((x - g.p0.0) * (g.p1.0 - g.p0.0) + *cy) / *len2;
                let ts = apply_spread(t, g.spread);
                let (c, a) = interp_stops_f(&g.stops, ts, idx);
                let k = a / 255.0;
                (
                    [
                        clamp_pm(c[0] * k + dith),
                        clamp_pm(c[1] * k + dith),
                        clamp_pm(c[2] * k + dith),
                    ],
                    k,
                )
            }
            SpanKind::Radial {
                g,
                fx,
                fy,
                ex,
                ey,
                sx,
                sy,
                shx,
                shy,
                r2_ee,
                idx,
                outer,
            } => {
                let (dx, dy) = (
                    (x - *fx) * *sx + (yc - *fy) * *shx,
                    (x - *fx) * *shy + (yc - *fy) * *sy,
                );
                let dd = dx * dx + dy * dy;
                // 车道BI：SVG2 fr>0——双圆二次方程直接解 t（与 at() 的
                // fr 分支同式，只是走 interp_stops_f/premul 管线）
                if g.fr > 0.0 && g.fr < g.r {
                    let dr = g.r - g.fr;
                    let a = *ex * *ex + *ey * *ey - dr * dr;
                    let b = g.fr * dr - (*ex * dx + *ey * dy);
                    let c0 = dd - g.fr * g.fr;
                    let disc = b * b - a * c0;
                    let ts = if a.abs() <= 1e-6 * g.r * g.r {
                        if b.abs() <= 1e-12 {
                            let (c, a) = interp_stops_f(&g.stops, 1.0, idx);
                            let k = a / 255.0;
                            return (
                                [
                                    clamp_pm(c[0] * k + dith),
                                    clamp_pm(c[1] * k + dith),
                                    clamp_pm(c[2] * k + dith),
                                ],
                                k,
                            );
                        }
                        c0 / (2.0 * b)
                    } else if disc <= 0.0 {
                        apply_spread(f32::MAX / 4.0, g.spread)
                    } else {
                        let s = libm::sqrtf(disc);
                        apply_spread(if a > 0.0 { (b + s) / a } else { (b - s) / a }, g.spread)
                    };
                    let (c, a) = interp_stops_f(&g.stops, ts, idx);
                    let k = a / 255.0;
                    return (
                        [
                            clamp_pm(c[0] * k + dith),
                            clamp_pm(c[1] * k + dith),
                            clamp_pm(c[2] * k + dith),
                        ],
                        k,
                    );
                }
                // 车道LG：外焦点（|F−C| ≥ r）Skia conical 分支（坐标差从
                // 原 F 重取）；mask 透明 = ([0,0,0], 0.0)
                if let Some(of) = *outer {
                    let (ox, oy) = (
                        (x - g.f.0) * *sx + (yc - g.f.1) * *shx,
                        (x - g.f.0) * *shy + (yc - g.f.1) * *sy,
                    );
                    return match outer_focal_t(of, ox, oy) {
                        None => ([0.0, 0.0, 0.0], 0.0),
                        Some(t) => {
                            let (c, a) = interp_stops_f(&g.stops, apply_spread(t, g.spread), idx);
                            let k = a / 255.0;
                            (
                                [
                                    clamp_pm(c[0] * k + dith),
                                    clamp_pm(c[1] * k + dith),
                                    clamp_pm(c[2] * k + dith),
                                ],
                                k,
                            )
                        }
                    };
                }
                if dd <= 0.0 {
                    let (c, a) = interp_stops_f(&g.stops, apply_spread(0.0, g.spread), idx);
                    let k = a / 255.0;
                    return (
                        [
                            clamp_pm(c[0] * k + dith),
                            clamp_pm(c[1] * k + dith),
                            clamp_pm(c[2] * k + dith),
                        ],
                        k,
                    );
                }
                let ed = *ex * dx + *ey * dy;
                let disc = ed * ed + dd * *r2_ee;
                if disc <= 0.0 {
                    let (c, a) =
                        interp_stops_f(&g.stops, apply_spread(f32::MAX / 4.0, g.spread), idx);
                    let k = a / 255.0;
                    return (
                        [
                            clamp_pm(c[0] * k + dith),
                            clamp_pm(c[1] * k + dith),
                            clamp_pm(c[2] * k + dith),
                        ],
                        k,
                    );
                }
                let u = (libm::sqrtf(disc) - ed) / dd;
                if u <= 0.0 {
                    // 焦点射线反向：pad 语义钳末停色（仍过抖动管线）
                    let (c, a) = interp_stops_f(&g.stops, 1.0, idx);
                    let k = a / 255.0;
                    return (
                        [
                            clamp_pm(c[0] * k + dith),
                            clamp_pm(c[1] * k + dith),
                            clamp_pm(c[2] * k + dith),
                        ],
                        k,
                    );
                }
                let ts = apply_spread(1.0 / u, g.spread);
                let (c, a) = interp_stops_f(&g.stops, ts, idx);
                let k = a / 255.0;
                (
                    [
                        clamp_pm(c[0] * k + dith),
                        clamp_pm(c[1] * k + dith),
                        clamp_pm(c[2] * k + dith),
                    ],
                    k,
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 标量 ±1 容差（抖动折叠点专用）。
    fn close_u8(a: u8, b: u8) {
        assert!((i16::from(a) - i16::from(b)).abs() <= 1, "{a} vs {b}");
    }

    /// 抖动容差断言（±1/通道）。
    fn close(a: Rgba8, b: Rgba8) {
        let d = |x: u8, y: u8| (i16::from(x) - i16::from(y)).abs();
        assert!(
            d(a.r, b.r) <= 1 && d(a.g, b.g) <= 1 && d(a.b, b.b) <= 1 && d(a.a, b.a) <= 1,
            "rgba {a:?} vs {b:?}"
        );
    }

    fn rgba(r: u8, g: u8, b: u8, a: u8) -> Rgba8 {
        Rgba8 { r, g, b, a }
    }

    fn lin(p0: (f32, f32), p1: (f32, f32), spread: SpreadMethod) -> LinearGradient {
        LinearGradient {
            p0,
            p1,
            stops: vec![
                GradStop {
                    offset: 0.0,
                    color: rgba(0, 0, 0, 255),
                },
                GradStop {
                    offset: 1.0,
                    color: rgba(255, 255, 255, 255),
                },
            ],
            spread,
        }
    }

    /// pad/reflect/repeat 三态折叠：黑→白渐变轴 [0,1]，t=x。
    #[test]
    fn spread_fold_periods() {
        let g = lin((0.0, 0.0), (1.0, 0.0), SpreadMethod::Pad);
        // pad：轴外钳边界色
        assert_eq!(sample_linear(&g, -1.0, 0.0).r, 0);
        assert_eq!(sample_linear(&g, 1.5, 0.0).r, 255);

        let g = lin((0.0, 0.0), (1.0, 0.0), SpreadMethod::Reflect);
        // reflect：周期 2 锯齿折叠（±1：渐变 Bayer 抖动使 127.5 折叠点
        // 在不同像素坐标可能取 127 或 128）
        close_u8(sample_linear(&g, 1.5, 0.0).r, 128);
        close_u8(sample_linear(&g, 2.5, 0.0).r, 128);
        close_u8(sample_linear(&g, 3.0, 0.0).r, 255);
        close_u8(sample_linear(&g, -0.25, 0.0).r, 64);

        let g = lin((0.0, 0.0), (1.0, 0.0), SpreadMethod::Repeat);
        // repeat：周期 1 取模（±1 抖动容差）
        close_u8(sample_linear(&g, 2.25, 0.0).r, 64);
        close_u8(sample_linear(&g, -0.25, 0.0).r, 191); // 255×0.75=191.25 → 191
    }

    /// 3 停靠点中段混合 + 等值停靠硬边。
    #[test]
    fn interp_three_stops_and_hard_edge() {
        let g = LinearGradient {
            p0: (0.0, 0.0),
            p1: (1.0, 0.0),
            stops: vec![
                GradStop {
                    offset: 0.0,
                    color: rgba(255, 0, 0, 255),
                },
                GradStop {
                    offset: 0.5,
                    color: rgba(0, 255, 0, 255),
                },
                GradStop {
                    offset: 1.0,
                    color: rgba(0, 0, 255, 255),
                },
            ],
            spread: SpreadMethod::Pad,
        };
        close(sample_linear(&g, 0.0, 0.0), rgba(255, 0, 0, 255));
        close(sample_linear(&g, 0.5, 0.0), rgba(0, 255, 0, 255));
        // t=0.25：红→绿中点 = (128,128,0)
        close(sample_linear(&g, 0.25, 0.0), rgba(128, 128, 0, 255));
        // t=0.75：绿→蓝中点 = (0,128,128)
        close(sample_linear(&g, 0.75, 0.0), rgba(0, 128, 128, 255));
        close(sample_linear(&g, 1.5, 0.0), rgba(0, 0, 255, 255));

        // 等值停靠（0.4 黑 / 0.4 白）在 t=0.4 处硬边跳白
        let hard = LinearGradient {
            p0: (0.0, 0.0),
            p1: (1.0, 0.0),
            stops: vec![
                GradStop {
                    offset: 0.4,
                    color: rgba(0, 0, 0, 255),
                },
                GradStop {
                    offset: 0.4,
                    color: rgba(255, 255, 255, 255),
                },
                GradStop {
                    offset: 1.0,
                    color: rgba(255, 0, 0, 255),
                },
            ],
            spread: SpreadMethod::Pad,
        };
        close(sample_linear(&hard, 0.4, 0.0), rgba(255, 255, 255, 255));
        close(sample_linear(&hard, 0.35, 0.0), rgba(0, 0, 0, 255));
    }

    /// 径向焦点规范公式 vs |p−c|/r 近似：偏焦点下两者差异巨大，
    /// 本测试钉住规范公式（f=(2,0)、r=5、p=(2,0.5) → t≈0.109）。
    #[test]
    fn radial_focus_spec_formula() {
        let g = RadialGradient {
            c: (0.0, 0.0),
            fr: 0.0,
            r: 5.0,
            f: (2.0, 0.0),
            stops: vec![
                GradStop {
                    offset: 0.0,
                    color: rgba(0, 0, 0, 255),
                },
                GradStop {
                    offset: 1.0,
                    color: rgba(255, 255, 255, 255),
                },
            ],
            spread: SpreadMethod::Pad,
            sx: 1.0,
            sy: 1.0,
            shx: 0.0,
            shy: 0.0,
        };
        // 焦点正上方 p=(2, 0.5)：近似公式会给出 t≈0.412（灰 105），
        // 规范公式 t = |p−F|/|p′−F| ≈ 0.109 → 灰 28
        let px = sample_radial(&g, 2.0, 0.5);
        let d3 = |a: u8, b: i32| (i16::from(a) - b as i16).abs();
        assert!(
            d3(px.r, 28) <= 1 && d3(px.g, 28) <= 1 && d3(px.b, 28) <= 1,
            "规范焦点公式"
        );
        // 中心（=焦点）：t=0 纯黑
        assert_eq!(sample_radial(&g, 2.0, 0.0).r, 0);
        // 圆缘 p=(0,5)：t=1 纯白
        assert_eq!(sample_radial(&g, 0.0, 5.0).r, 255);

        // |F−C| ≥ r（车道LG）：Chrome/Skia 不钳边不投影——Blink 原样交
        // SkConicalGradient（FocalData fFocalX=0）。d=5 分 fR1<1（greater）
        // 与 |1−fR1|≤2.44e-4（on-circle）两分支各钉金黄值
        let deg = RadialGradient {
            f: (5.0, 0.0),
            r: 4.0,
            ..g.clone()
        };
        // greater（d=5 > r=4，fR1=0.8）：轴向 t = x₀/(1−fR1) = 5·x₀
        // 焦点自身：t=0 → `mask_2pt_conical_degenerates`（t≤0 含恰 0）
        // → 锥顶点透明（Skia 逐像素二值 mask，无 AA）
        assert_eq!(sample_radial(&deg, 5.0, 0.0), rgba(0, 0, 0, 0));
        // 轴向 x₀=0.04（p=(4.8,0)）→ t=0.2 → 灰 51（±1 抖动）
        close(sample_radial(&deg, 4.8, 0.0), rgba(51, 51, 51, 255));
        // 轴向 x₀=0.1（p=(4.5,0)）→ t=0.5 → 灰 128（±1 抖动）
        close(sample_radial(&deg, 4.5, 0.0), rgba(128, 128, 128, 255));
        // 轴向 x₀=0.2（p=(4,0)，朝焦点贴缘）→ t=1 → 末停白；pad 外推仍白
        assert_eq!(sample_radial(&deg, 4.0, 0.0), rgba(255, 255, 255, 255));
        assert_eq!(sample_radial(&deg, 3.5, 0.0), rgba(255, 255, 255, 255));
        // 焦点后方（x₀<0 → t≤0）与锥外（|gy| > 1.333·|gx| → NaN）→ mask 透明
        assert_eq!(sample_radial(&deg, 6.0, 0.0), rgba(0, 0, 0, 0));
        assert_eq!(sample_radial(&deg, 5.0, 2.0), rgba(0, 0, 0, 0));
        // 离轴可见点 p=(4.5,0.5)：t=0.42476 → 灰 ~108（±1 抖动）
        close(sample_radial(&deg, 4.5, 0.5), rgba(108, 108, 108, 255));
        // on-circle（d=5=r，fR1=1 恰贴圆）：t = x₀/2（postScale(0.5,0.5)）
        let onc = RadialGradient {
            f: (5.0, 0.0),
            r: 5.0,
            ..g.clone()
        };
        // (3,0)：x₀=0.4 → t=0.2 → 灰 51（若误走 greater 会得 204，判别位）
        close(sample_radial(&onc, 3.0, 0.0), rgba(51, 51, 51, 255));
        // (0,5)：x₀=1, y₀=1 → t=1 → 白（greater 下 NaN 透明，判别位）
        assert_eq!(sample_radial(&onc, 0.0, 5.0), rgba(255, 255, 255, 255));
        // 焦点：px=0 → NaN → mask 透明
        assert_eq!(sample_radial(&onc, 5.0, 0.0), rgba(0, 0, 0, 0));
    }

    /// 径向 r=0 / 线性 p0==p1 退化：末停色。
    #[test]
    fn degenerate_shapes_last_stop() {
        let r0 = RadialGradient {
            fr: 0.0,
            c: (0.0, 0.0),
            r: 0.0,
            f: (0.0, 0.0),
            stops: vec![
                GradStop {
                    offset: 0.0,
                    color: rgba(255, 0, 0, 255),
                },
                GradStop {
                    offset: 1.0,
                    color: rgba(0, 0, 255, 255),
                },
            ],
            spread: SpreadMethod::Pad,
            sx: 1.0,
            sy: 1.0,
            shx: 0.0,
            shy: 0.0,
        };
        assert_eq!(sample_radial(&r0, 3.0, 4.0), rgba(0, 0, 255, 255));

        let p0eq = lin((1.0, 1.0), (1.0, 1.0), SpreadMethod::Pad);
        assert_eq!(sample_linear(&p0eq, 9.0, 9.0), rgba(255, 255, 255, 255));
    }

    /// 车道BI：SVG2 `fr` 焦点圆半径。同心情形 t = (|p−f| − fr)/(r − fr)：
    /// 焦点圆内 pad 钳首停、fr 处 t=0、中段线性、r 外末停。SpanEval
    /// （raster 快路径）与逐像素 sample_radial 同式同序——半整数采样点
    /// 上抖动坐标一致，要求逐位相等。
    #[test]
    fn radial_fr_focal_radius_geometry() {
        let mk = || RadialGradient {
            c: (0.0, 0.0),
            fr: 50.0,
            r: 100.0,
            f: (0.0, 0.0),
            stops: vec![
                GradStop {
                    offset: 0.0,
                    color: rgba(0, 0, 255, 255),
                },
                GradStop {
                    offset: 1.0,
                    color: rgba(255, 255, 0, 255),
                },
            ],
            spread: SpreadMethod::Pad,
            sx: 1.0,
            sy: 1.0,
            shx: 0.0,
            shy: 0.0,
        };
        let g = mk();
        // 焦点圆内（|p| < fr）：pad 钳首停色（修复前 fr 缺失 → 中心即起变）
        assert_eq!(sample_radial(&g, 25.0, 0.0), rgba(0, 0, 255, 255));
        // fr 边界：t = 0
        close(sample_radial(&g, 50.0, 0.0), rgba(0, 0, 255, 255));
        // 中段（|p| = fr + 0.75·(r−fr) = 87.5）：t = 0.75（±1 抖动容差）
        let mid = sample_radial(&g, 87.5, 0.0);
        close_u8(mid.r, 191); // 255×0.75 = 191.25
        close_u8(mid.g, 191);
        close_u8(mid.b, 64); // 255×0.25 = 63.75
                             // r 外：末停色
        assert_eq!(sample_radial(&g, 120.0, 0.0), rgba(255, 255, 0, 255));

        // SpanEval::at 与逐像素逐位一致（半整数点抖动坐标同参）
        let paint = Paint::RadialGradient(mk());
        let mut ev = SpanEval::new(&paint, TILE_SHIFT_TALL);
        ev.begin_row(0.5);
        for &px in &[20.5, 49.5, 50.5, 87.5, 99.5, 120.5] {
            assert_eq!(ev.at(px), sample_radial(&g, px, 0.5), "x={px}");
        }
    }

    /// 车道BI：fr >= r 非法形态按 0 处理（简化登记）——走既有焦点路径。
    #[test]
    fn radial_fr_ge_r_falls_back() {
        let g = RadialGradient {
            c: (0.0, 0.0),
            fr: 100.0,
            r: 100.0,
            f: (0.0, 0.0),
            stops: vec![
                GradStop {
                    offset: 0.0,
                    color: rgba(0, 0, 255, 255),
                },
                GradStop {
                    offset: 1.0,
                    color: rgba(255, 255, 0, 255),
                },
            ],
            spread: SpreadMethod::Pad,
            sx: 1.0,
            sy: 1.0,
            shx: 0.0,
            shy: 0.0,
        };
        // 焦点路径语义：中心 t=0 蓝、r 外黄（与 fr=0 同构，不 panic）
        close(sample_radial(&g, 0.0, 0.0), rgba(0, 0, 255, 255));
        assert_eq!(sample_radial(&g, 120.0, 0.0), rgba(255, 255, 0, 255));
    }

    /// parse_len / parse_stop / finalize_stops 解析变体。
    #[test]
    fn parse_len_and_stop_variants() {
        assert_eq!(parse_len("50%"), Some((50.0, true)));
        assert_eq!(parse_len(" 0.5 "), Some((0.5, false)));
        assert_eq!(parse_len("-3%"), Some((-3.0, true)));
        assert_eq!(parse_len("abc"), None);
        assert_eq!(parse_len(""), None);

        let st = parse_stop(
            &StopRaw {
                offset: Some(String::from("75%")),
                color: Some(String::from("#102030")),
                opacity: Some(String::from("0.5")),
            },
            None,
            None,
        );
        assert_eq!(st.offset, 0.75);
        assert_eq!(
            st.color,
            rgba(0x10, 0x20, 0x30, 128) // round(255×0.5)=128
        );

        // 缺省：offset=0、黑、不透明；越界钳位
        let d = parse_stop(&StopRaw::default(), None, None);
        assert_eq!(d.offset, 0.0);
        assert_eq!(d.color, rgba(0, 0, 0, 255));
        let over = parse_stop(
            &StopRaw {
                offset: Some(String::from("1.5")),
                ..StopRaw::default()
            },
            None,
            None,
        );
        assert_eq!(over.offset, 1.0);

        // 文档序运行最大钳制：乱序输入不重排，后停小者抬到运行最大
        // （规范原文 + Chrome，pservers-grad-16-b：右半为末停纯色垫）
        let clamped = finalize_stops(vec![
            GradStop {
                offset: 0.8,
                color: rgba(0, 0, 255, 255),
            },
            GradStop {
                offset: 0.2,
                color: rgba(255, 0, 0, 255),
            },
            GradStop {
                offset: 0.1,
                color: rgba(0, 255, 0, 255),
            },
        ]);
        let offs: Vec<f32> = clamped.iter().map(|s| s.offset).collect();
        assert_eq!(offs, vec![0.8, 0.8, 0.8], "文档序不重排，全部抬到 0.8");
        let mono = finalize_stops(vec![
            GradStop {
                offset: 0.6,
                color: rgba(255, 0, 0, 255),
            },
            GradStop {
                offset: 0.2,
                color: rgba(0, 0, 255, 255),
            },
        ]);
        assert_eq!(mono[1].offset, 0.6, "回落停靠抬升到前停");
    }

    /// bake 默认值 + 百分比：radial 全缺省（50% 族）在 8×4 bbox（非方形
    /// → 单位椭圆语境）上 c=(4,2)、r=0.5（单位半径）、sx=1/8、sy=1/4、
    /// f=c；linear 缺省 x2=100%。
    #[test]
    fn bake_defaults_and_percent() {
        let def = GradientDef {
            radial: true,
            units: GradUnits::ObjectBoundingBox,
            spread: SpreadMethod::Pad,
            p: GradParams::default(),
            stops: vec![
                GradStop {
                    offset: 0.0,
                    color: rgba(255, 255, 255, 255),
                },
                GradStop {
                    offset: 1.0,
                    color: rgba(0, 0, 0, 255),
                },
            ],
        };
        let baked = bake_radial(&def, Some([0.0, 0.0, 8.0, 4.0]), Mat2x3::IDENTITY, 8.0, 4.0)
            .unwrap_or_else(|| panic!("radial bake 不应 None"));
        assert_eq!(baked.c, (4.0, 2.0));
        assert_eq!(baked.f, baked.c, "fx/fy 缺省 = cx/cy");
        // 非方形 bbox：单位空间语义（wt/radial-focal）——r=单位分数、
        // 逆缩放 = 1/bbox 尺寸
        assert_eq!(baked.r.to_bits(), (0.5f32).to_bits(), "r={}", baked.r);
        assert_eq!(baked.sx.to_bits(), (1.0f32 / 8.0).to_bits());
        assert_eq!(baked.sy.to_bits(), (1.0f32 / 4.0).to_bits());

        // linear 缺省：p0=(0,0) p1=(w,0)
        let ldef = GradientDef {
            radial: false,
            ..def.clone()
        };
        let lb = bake_linear(
            &ldef,
            Some([0.0, 0.0, 8.0, 4.0]),
            Mat2x3::IDENTITY,
            8.0,
            4.0,
        )
        .unwrap_or_else(|| panic!("linear bake 不应 None"));
        assert_eq!((lb.p0, lb.p1), ((0.0, 0.0), (8.0, 0.0)));

        // 百分比与裸数等价：x2="100%" ≡ x2 缺省 ≡ x2="1"
        let pct = GradientDef {
            p: GradParams {
                x2: Some((100.0, true)),
                ..GradParams::default()
            },
            ..ldef.clone()
        };
        let pb = bake_linear(&pct, Some([0.0, 0.0, 8.0, 4.0]), Mat2x3::IDENTITY, 8.0, 4.0)
            .unwrap_or_else(|| panic!("pct bake 不应 None"));
        assert_eq!(pb.p1, (8.0, 0.0));

        // fx 显式缺省链：cx=25% 且无 fx → f.x = 0.25×w
        let fx_def = GradientDef {
            p: GradParams {
                cx: Some((25.0, true)),
                ..GradParams::default()
            },
            ..def
        };
        let fb = bake_radial(
            &fx_def,
            Some([0.0, 0.0, 8.0, 4.0]),
            Mat2x3::IDENTITY,
            8.0,
            4.0,
        )
        .unwrap_or_else(|| panic!("fx bake 不应 None"));
        assert_eq!(fb.c, (2.0, 2.0));
        assert_eq!(fb.f, (2.0, 2.0));

        // 零面积 bbox：不描绘
        assert!(bake_linear(
            &ldef,
            Some([0.0, 0.0, 0.0, 4.0]),
            Mat2x3::IDENTITY,
            8.0,
            4.0
        )
        .is_none());
        assert!(bake_linear(&ldef, None, Mat2x3::IDENTITY, 8.0, 4.0).is_none());

        // userSpaceOnUse：端点经矩阵映射、r 按平均缩放
        let udef = GradientDef {
            units: GradUnits::UserSpaceOnUse,
            p: GradParams {
                x1: Some((10.0, false)),
                y1: Some((0.0, false)),
                x2: Some((110.0, false)),
                y2: Some((0.0, false)),
                ..GradParams::default()
            },
            ..ldef
        };
        let m = Mat2x3::translation(5.0, 1.0).mul(&Mat2x3::scaling(2.0, 1.0));
        let ub = bake_linear(&udef, Some([0.0, 0.0, 8.0, 4.0]), m, 200.0, 100.0)
            .unwrap_or_else(|| panic!("user bake 不应 None"));
        assert_eq!((ub.p0, ub.p1), ((25.0, 1.0), (225.0, 1.0)));
    }

    /// 车道E：oBB 对角轴的单位空间语义——非正方形 bbox 下 t 按单位
    /// 正方形（bbox 仿射像）内的欧氏投影取值（Chrome 实测，pservers-
    /// grad-04 逐点钉死），等 t 线不与设备轴垂直；轴对齐/正方形路径
    /// 保持旧端点构造（零漂移）。
    #[test]
    fn bake_linear_obb_unit_space_diagonal() {
        let def = GradientDef {
            radial: false,
            units: GradUnits::ObjectBoundingBox,
            spread: SpreadMethod::Pad,
            p: GradParams {
                x1: Some((0.0, false)),
                y1: Some((0.0, false)),
                x2: Some((1.0, false)),
                y2: Some((1.0, false)),
                ..GradParams::default()
            },
            stops: vec![
                GradStop {
                    offset: 0.0,
                    color: rgba(0, 0, 0, 255),
                },
                GradStop {
                    offset: 1.0,
                    color: rgba(255, 255, 255, 255),
                },
            ],
        };
        let g = bake_linear(
            &def,
            Some([20.0, 20.0, 460.0, 100.0]),
            Mat2x3::IDENTITY,
            480.0,
            360.0,
        )
        .unwrap_or_else(|| panic!("bake 不应 None"));
        let (dx, dy) = (g.p1.0 - g.p0.0, g.p1.1 - g.p0.1);
        let len2 = dx * dx + dy * dy;
        let t = |x: f32, y: f32| ((x - g.p0.0) * dx + (y - g.p0.1) * dy) / len2;
        // 单位空间投影真值：u=(x−20)/440、v=(y−20)/80，轴 (0,0)→(1,1)
        for (x, y) in [
            (240.0f32, 25.5f32),
            (100.0, 60.0),
            (30.0, 90.0),
            (455.5, 95.0),
        ] {
            let expect = ((x - 20.0) / 440.0 + (y - 20.0) / 80.0) / 2.0;
            assert!(
                (t(x, y) - expect).abs() < 1e-4,
                "t({x},{y})={} expect={expect}",
                t(x, y)
            );
        }
        // 端点 t：p0 处 0、p1 处 1（构造自检）
        assert!(t(g.p0.0, g.p0.1).abs() < 1e-5);
        assert!((t(g.p1.0, g.p1.1) - 1.0).abs() < 1e-5);
        // 轴对齐（dv==0）仍走旧端点：p0=(bbx+u0·w, bby+v0·h)
        let hdef = GradientDef {
            p: GradParams {
                x1: Some((0.0, false)),
                y1: Some((0.0, false)),
                x2: Some((1.0, false)),
                ..GradParams::default()
            },
            ..def
        };
        let hg = bake_linear(
            &hdef,
            Some([20.0, 20.0, 460.0, 100.0]),
            Mat2x3::IDENTITY,
            480.0,
            360.0,
        )
        .unwrap_or_else(|| panic!("bake 不应 None"));
        assert_eq!((hg.p0, hg.p1), ((20.0, 20.0), (460.0, 20.0)));
    }

    /// bake_paint：0 停不描绘、1 停降级纯色。
    #[test]
    fn bake_paint_stop_count_semantics() {
        let mut def = GradientDef {
            radial: false,
            units: GradUnits::ObjectBoundingBox,
            spread: SpreadMethod::Pad,
            p: GradParams::default(),
            stops: Vec::new(),
        };
        assert!(bake_paint(&def, Some([0.0, 0.0, 4.0, 4.0]), Mat2x3::IDENTITY, 4.0, 4.0).is_none());
        def.stops.push(GradStop {
            offset: 0.0,
            color: rgba(9, 8, 7, 255),
        });
        assert_eq!(
            bake_paint(&def, Some([0.0, 0.0, 4.0, 4.0]), Mat2x3::IDENTITY, 4.0, 4.0),
            Some(Paint::Solid(rgba(9, 8, 7, 255)))
        );
    }

    /// sample_paint 分发：Solid 直返、两类渐变各走各的。
    #[test]
    fn sample_paint_dispatch() {
        close(
            sample_paint(&Paint::Solid(rgba(1, 2, 3, 4)), 0.0, 0.0),
            rgba(1, 2, 3, 4),
        );
        let lg = Paint::LinearGradient(lin((0.0, 0.0), (1.0, 0.0), SpreadMethod::Pad));
        assert_eq!(sample_paint(&lg, 0.5, 7.0).r, 128);
        let rg = Paint::RadialGradient(RadialGradient {
            c: (0.0, 0.0),
            fr: 0.0,
            r: 1.0,
            f: (0.0, 0.0),
            stops: vec![
                GradStop {
                    offset: 0.0,
                    color: rgba(255, 0, 0, 255),
                },
                GradStop {
                    offset: 1.0,
                    color: rgba(0, 0, 255, 255),
                },
            ],
            spread: SpreadMethod::Pad,
            sx: 1.0,
            sy: 1.0,
            shx: 0.0,
            shy: 0.0,
        });
        close(sample_paint(&rg, 0.5, 0.0), rgba(128, 0, 128, 255));
    }

    /// 位级 round 与 libm::roundf 全量逐位等价：随机位模式 + 边界值 +
    /// 半值邻域（ties-away 语义钉死：126.5→127、127.5→128）。
    #[test]
    fn fast_round_bitwise_matches_libm() {
        // 关键半值（正向）
        for x in [0.5f32, 1.5, 2.5, 3.5, 126.5, 127.5, 254.5] {
            assert_eq!(round_half_away(x).to_bits(), libm::roundf(x).to_bits());
        }
        // 关键半值（负向：远离零）
        for x in [-0.5f32, -1.5, -126.5, -127.5] {
            assert_eq!(round_half_away(x).to_bits(), libm::roundf(x).to_bits());
        }
        // 边界档：±0 / 次正规 / <0.5 / 恰在 2^23 上 / 巨大值 / inf
        for x in [
            0.0f32,
            -0.0,
            f32::from_bits(1), // 最小次正规
            -f32::from_bits(1),
            0.49999997,
            -0.49999997,
            8388607.5, // 2^23 邻域（不可表示，编译期折叠）
            8388608.0, // 2^23 恰好：整数
            16777216.0,
            f32::MAX,
            f32::MIN,
            f32::INFINITY,
            f32::NEG_INFINITY,
        ] {
            assert_eq!(round_half_away(x).to_bits(), libm::roundf(x).to_bits());
        }
        // NaN：两边都是 NaN
        assert!(round_half_away(f32::NAN).is_nan());
        // 随机位模式（LCG，确定性）：跳过 NaN 的逐位比对（libm 可能换 payload）
        let mut s = 0x2545_F491u32;
        let mut checked = 0u32;
        for _ in 0..1_000_000u32 {
            s = s.wrapping_mul(1664525).wrapping_add(1013904223);
            let x = f32::from_bits(s);
            if x.is_nan() {
                assert!(round_half_away(x).is_nan());
                continue;
            }
            assert_eq!(
                round_half_away(x).to_bits(),
                libm::roundf(x).to_bits(),
                "x={x} bits={s:#x}"
            );
            checked += 1;
        }
        assert!(checked > 900_000, "随机样本量应足够：{checked}");
    }

    /// SpanEval 与逐像素 sample_paint 逐位等价：线性（斜轴/垂直轴/退化轴）×
    /// 三种 spread × 多行多像素网格全量比对。
    #[test]
    fn span_eval_matches_sample_paint_linear() {
        let mk = |p0: (f32, f32), p1: (f32, f32), spread: SpreadMethod| LinearGradient {
            p0,
            p1,
            stops: vec![
                GradStop {
                    offset: 0.0,
                    color: rgba(255, 0, 0, 255),
                },
                GradStop {
                    offset: 0.3,
                    color: rgba(0, 255, 0, 200),
                },
                GradStop {
                    offset: 0.3,
                    color: rgba(0, 0, 255, 90),
                },
                GradStop {
                    offset: 0.7,
                    color: rgba(128, 128, 0, 255),
                },
                GradStop {
                    offset: 1.0,
                    color: rgba(0, 255, 255, 10),
                },
            ],
            spread,
        };
        for spread in [
            SpreadMethod::Pad,
            SpreadMethod::Repeat,
            SpreadMethod::Reflect,
        ] {
            // 斜轴 / 垂直轴（frog 形态）/ 水平轴 / p0==p1 退化
            for (p0, p1) in [
                ((-2.0, -3.0), (7.5, 4.25)),
                ((0.0, 0.0), (0.0, 10.0)),
                ((0.0, 0.0), (10.0, 0.0)),
                ((1.0, 1.0), (1.0, 1.0)),
            ] {
                let g = mk(p0, p1, spread);
                let paint = Paint::LinearGradient(g.clone());
                let mut ev = SpanEval::new(&paint, TILE_SHIFT_TALL);
                for row in 0..64i32 {
                    let y = row as f32 + 0.5;
                    ev.begin_row(y);
                    for px in 0..64i32 {
                        let x = px as f32 + 0.5;
                        // 车道AT：收紧为逐位相等（±1 容差曾掩盖径向丢抖动 bug）
                        assert_eq!(ev.at(x), sample_linear(&g, x, y), "({px},{row})");
                    }
                }
            }
        }
    }

    /// SpanEval 与逐像素 sample_radial 逐位等价：同心/偏心/退化焦点 ×
    /// 圆/单位椭圆（sx≠sy）× 多行网格。
    #[test]
    fn span_eval_matches_sample_paint_radial() {
        for (c, r, f, g_sx, g_sy) in [
            ((0.0f32, 0.0f32), 8.0f32, (0.0f32, 0.0f32), 1.0f32, 1.0f32), // 同心圆
            ((4.0, 4.0), 8.0, (6.0, 3.0), 1.0, 1.0),                      // 偏心焦点
            ((4.0, 4.0), 8.0, (12.0, 4.0), 1.0, 1.0),                     // 焦点在圆上 → on-circle
            ((4.0, 4.0), 8.0, (16.0, 4.0), 1.0, 1.0), // 车道LG：外焦点 → greater
            ((4.0, 4.0), 0.0, (4.0, 4.0), 1.0, 1.0),  // r=0 → 末停色
            ((8.0, 8.0), 0.5, (11.0, 7.0), 1.0 / 16.0, 1.0 / 8.0), // 单位椭圆+偏焦点
            ((8.0, 8.0), 0.5, (8.0, 8.0), 1.0 / 16.0, 1.0 / 8.0), // 单位椭圆同心
            ((8.0, 8.0), 0.5, (12.5, 8.0), 1.0 / 16.0, 1.0 / 8.0), // 车道LG：椭圆外焦点 → greater
        ] {
            let g = RadialGradient {
                c,
                fr: 0.0,
                r,
                f,
                stops: vec![
                    GradStop {
                        offset: 0.0,
                        color: rgba(255, 255, 0, 255),
                    },
                    GradStop {
                        offset: 0.5,
                        color: rgba(0, 128, 255, 128),
                    },
                    GradStop {
                        offset: 1.0,
                        color: rgba(255, 0, 128, 255),
                    },
                ],
                spread: SpreadMethod::Pad,
                sx: g_sx,
                sy: g_sy,
                shx: 0.0,
                shy: 0.0,
            };
            let paint = Paint::RadialGradient(g.clone());
            let mut ev = SpanEval::new(&paint, TILE_SHIFT_TALL);
            for row in 0..32i32 {
                let y = row as f32 + 0.5;
                ev.begin_row(y);
                for px in 0..32i32 {
                    let x = px as f32 + 0.5;
                    // 车道AT：收紧为逐位相等——±1 容差曾掩盖径向 span
                    // 路径丢失 8×8 抖动的 bug（dith 恒 0）
                    assert_eq!(ev.at(x), sample_radial(&g, x, y), "({px},{row})");
                }
            }
        }
    }

    /// 车道AT 回归钉：sample_radial 的输出必须含 8×8 抖动（round(c+d)）。
    /// 旧 bug：SpanEval 径向走 interp_stops_cached（dith=0），抖动整丢——
    /// 本测试在「抖动翻转舍入」的像素上钉死 dither 参与。
    #[test]
    fn radial_span_keeps_dither() {
        let g = RadialGradient {
            c: (128.0, 128.0),
            fr: 0.0,
            r: 128.0,
            f: (128.0, 128.0),
            stops: vec![
                GradStop {
                    offset: 0.0,
                    color: rgba(0, 0, 0, 255),
                },
                GradStop {
                    offset: 1.0,
                    color: rgba(255, 255, 255, 255),
                },
            ],
            spread: SpreadMethod::Pad,
            sx: 1.0,
            sy: 1.0,
            shx: 0.0,
            shy: 0.0,
        };
        let mut checked = 0;
        for row in 0..64i32 {
            let y = row as f32 + 0.5;
            for px in 0..64i32 {
                let x = px as f32 + 0.5;
                let dist = libm::sqrtf((x - 128.0) * (x - 128.0) + (y - 128.0) * (y - 128.0));
                let t = dist / 128.0;
                if !(0.05..0.95).contains(&t) {
                    continue;
                }
                let d = bayer_off(px, row, TILE_SHIFT_TALL);
                let ideal = 255.0 * t; // 黑→白 lerp 的 R/G/B 同值
                let want = libm::roundf(ideal + d).clamp(0.0, 255.0) as u8;
                // 仅统计「抖动确实翻转舍入」的样本（有/无 dither 结果不同）
                if want as f32 == libm::roundf(ideal) {
                    continue;
                }
                let got = sample_radial(&g, x, y);
                assert_eq!(got.r, want, "radial dither ({px},{row}) t={t} d={d}");
                assert_eq!(got.g, want);
                assert_eq!(got.b, want);
                checked += 1;
            }
        }
        assert!(checked > 200, "翻转样本应足够：{checked}");
    }

    /// at_premul 的 Chrome/Skia 管线模型钉：pm = clamp(c·α + dither, 0, 255)、
    /// alpha 无抖动。与探针 at3/at4 逐位实证的 Chrome 公式一致。
    #[test]
    fn at_premul_matches_chrome_pipeline() {
        let g = LinearGradient {
            p0: (0.0, 0.0),
            p1: (64.0, 0.0), // 水平轴：t=(x)/64（像素中心 x=px+0.5）
            stops: vec![
                GradStop {
                    offset: 0.0,
                    color: rgba(46, 134, 193, 255),
                },
                GradStop {
                    offset: 0.5,
                    color: rgba(46, 134, 193, 51),
                },
                GradStop {
                    offset: 1.0,
                    color: rgba(233, 69, 96, 178),
                },
            ],
            spread: SpreadMethod::Pad,
        };
        let paint = Paint::LinearGradient(g.clone());
        let mut ev = SpanEval::new(&paint, TILE_SHIFT_TALL);
        for row in 0..8i32 {
            let y = row as f32 + 0.5;
            ev.begin_row(y);
            for px in 0..64i32 {
                let x = px as f32 + 0.5;
                let (pm, sa) = ev.at_premul(x);
                // 手推期望：t → 段内 u → c_f/a_f → premul+dither+clamp
                let t = (x / 64.0).clamp(0.0, 1.0);
                let (cf, af) = if t < 0.5 {
                    let u = t / 0.5;
                    ([46.0f32, 134.0, 193.0], 255.0 + (51.0 - 255.0) * u)
                } else {
                    let u = (t - 0.5) / 0.5;
                    (
                        [
                            46.0f32 + (233.0 - 46.0) * u,
                            134.0 + (69.0 - 134.0) * u,
                            193.0 + (96.0 - 193.0) * u,
                        ],
                        51.0 + (178.0 - 51.0) * u,
                    )
                };
                let d = bayer_off(px, row, TILE_SHIFT_TALL);
                let k = af / 255.0;
                let want_pm = [
                    clamp_pm(cf[0] * k + d),
                    clamp_pm(cf[1] * k + d),
                    clamp_pm(cf[2] * k + d),
                ];
                for c in 0..3 {
                    assert_eq!(pm[c], want_pm[c], "pm[{c}] ({px},{row})");
                }
                assert_eq!(sa, k, "sa ({px},{row})");
            }
        }
    }

    /// 不透明渐变：at_premul 舍入后与 at() 逐字节一致（frog/主语料
    /// 不透明渐变 sha 不变的保证）。
    #[test]
    fn at_premul_opaque_equiv_at() {
        let g = lin((0.0, 0.0), (10.0, 0.0), SpreadMethod::Pad);
        let paint = Paint::LinearGradient(g);
        let mut ev = SpanEval::new(&paint, TILE_SHIFT_TALL);
        for row in 0..16i32 {
            let y = row as f32 + 0.5;
            ev.begin_row(y);
            for px in 0..16i32 {
                let x = px as f32 + 0.5;
                let c8 = ev.at(x);
                let (pm, sa) = ev.at_premul(x);
                assert_eq!(sa, 1.0);
                assert_eq!(pm[0].round() as u8, c8.r, "r ({px},{row})");
                assert_eq!(pm[1].round() as u8, c8.g, "g ({px},{row})");
                assert_eq!(pm[2].round() as u8, c8.b, "b ({px},{row})");
            }
        }
    }

    /// premul clamp 钉：全 alpha→0 渐变的暗通道 pm = max(d, 0)——负抖动
    /// 不越过 0（Chrome clamp 语义，at4 探针 0.00% 失配的根）。
    #[test]
    fn at_premul_negative_dither_clamped() {
        let g = LinearGradient {
            p0: (0.0, 0.0),
            p1: (16.0, 0.0),
            stops: vec![
                GradStop {
                    offset: 0.0,
                    color: rgba(0, 0, 0, 255),
                },
                GradStop {
                    offset: 1.0,
                    color: rgba(0, 0, 0, 0),
                },
            ],
            spread: SpreadMethod::Pad,
        };
        let paint = Paint::LinearGradient(g);
        let mut ev = SpanEval::new(&paint, TILE_SHIFT_TALL);
        for row in 0..8i32 {
            let y = row as f32 + 0.5;
            ev.begin_row(y);
            for px in 0..16i32 {
                let x = px as f32 + 0.5;
                let (pm, _sa) = ev.at_premul(x);
                let d = bayer_off(px, row, TILE_SHIFT_TALL);
                for (ci, &pm_c) in pm.iter().enumerate() {
                    assert_eq!(pm_c, d.max(0.0), "pm[{ci}] ({px},{row}) d={d}");
                }
            }
        }
    }

    /// interp_stops_dith（dith=0，即旧 cached 路径）与 interp_stops 逐位等价：
    /// 顺扫/跳扫/回扫混合（缓存命中与失效路径都要走到）。
    #[test]
    fn interp_cached_matches_plain() {
        let stops = vec![
            GradStop {
                offset: 0.0,
                color: rgba(255, 0, 0, 255),
            },
            GradStop {
                offset: 0.25,
                color: rgba(0, 255, 0, 255),
            },
            GradStop {
                offset: 0.5,
                color: rgba(0, 0, 255, 255),
            },
            GradStop {
                offset: 0.5,
                color: rgba(255, 255, 0, 255),
            },
            GradStop {
                offset: 1.0,
                color: rgba(0, 255, 255, 255),
            },
        ];
        let mut idx = 0usize;
        // 顺扫（缓存命中）+ 回扫（缓存失效）+ 越界端点
        for t in [
            0.1f32, 0.2, 0.3, 0.4, 0.6, 0.7, 0.8, 0.9, // 顺扫
            0.15, 0.05, -0.5, 1.5, 0.5, 0.5, 0.25, // 回扫与边界
            0.35, 0.45, 0.55, 0.65,
        ] {
            assert_eq!(
                interp_stops_dith(&stops, t, &mut idx, 0.0),
                interp_stops(&stops, t, 0.0),
                "t={t}"
            );
        }
        // 全区间细扫：单调推进下逐点等价
        let mut idx2 = 0usize;
        let mut t = -0.2f32;
        while t <= 1.2 {
            assert_eq!(
                interp_stops_dith(&stops, t, &mut idx2, 0.0),
                interp_stops(&stops, t, 0.0),
                "t={t}"
            );
            t += 0.0011;
        }
    }

    /// wt/radial-focal：oBB 非正方形 bbox 的单位空间椭圆语义——bake 出
    /// sx=1/w、sy=1/h、r=单位分数；设备等值线为单位圆的仿射像（椭圆）。
    #[test]
    fn bake_radial_obb_ellipse_unit_space() {
        let def = GradientDef {
            radial: true,
            units: GradUnits::ObjectBoundingBox,
            spread: SpreadMethod::Pad,
            p: GradParams::default(), // cx=cy=50%、r=50%、f=c
            stops: vec![
                GradStop {
                    offset: 0.0,
                    color: rgba(255, 243, 196, 255),
                },
                GradStop {
                    offset: 1.0,
                    color: rgba(233, 69, 96, 255),
                },
            ],
        };
        let baked = bake_radial(
            &def,
            Some([30.0, 45.0, 230.0, 125.0]), // 200×80
            Mat2x3::IDENTITY,
            260.0,
            170.0,
        )
        .unwrap_or_else(|| panic!("bake 不应 None"));
        assert_eq!((baked.c, baked.f), ((130.0, 85.0), (130.0, 85.0)));
        assert_eq!(baked.r.to_bits(), (0.5f32).to_bits(), "单位空间半径=分数");
        assert_eq!(baked.sx.to_bits(), (1.0f32 / 200.0).to_bits());
        assert_eq!(baked.sy.to_bits(), (1.0f32 / 80.0).to_bits());

        // 椭圆等值线：长轴端点 (230,85.5) 单位距 ≥0.5 → t=1 末停色
        assert_eq!(sample_radial(&baked, 230.0, 85.5), rgba(233, 69, 96, 255));
        // 单位距 0.25（设备 (180,85.5)）→ t≈0.5 中点色（±1 抖动容差）
        close(sample_radial(&baked, 180.0, 85.5), rgba(244, 156, 146, 255));
        // 短轴端点 (130,125)：单位距恰 0.5 → t=1（旧圆模型 t≈0.6，可辨）
        assert_eq!(sample_radial(&baked, 130.0, 125.0), rgba(233, 69, 96, 255));
        // 顶边 (68.5,45.5) 单位距 0.582>0.5 → pad 钳末停色
        assert_eq!(sample_radial(&baked, 68.5, 45.5), rgba(233, 69, 96, 255));
    }

    /// wt/radial-focal 红线：近方形 bbox（扁平化真圆的末位级不对称）
    /// 吸附回设备圆路径——sx=sy=1、r=frac·diag 与精确正方形同式逐位
    /// 一致（既有圆渲染零漂移）；明显非方形才走单位椭圆。
    #[test]
    fn bake_radial_obb_near_square_stays_device_circle() {
        let def = GradientDef {
            radial: true,
            units: GradUnits::ObjectBoundingBox,
            spread: SpreadMethod::Pad,
            p: GradParams::default(),
            stops: vec![
                GradStop {
                    offset: 0.0,
                    color: rgba(255, 255, 0, 255),
                },
                GradStop {
                    offset: 1.0,
                    color: rgba(0, 0, 255, 255),
                },
            ],
        };
        // 相对不对称 4e-4 < 1e-2：吸附圆路径
        let near = bake_radial(
            &def,
            Some([0.0, 0.0, 48.0, 47.98]),
            Mat2x3::IDENTITY,
            240.0,
            130.0,
        )
        .unwrap_or_else(|| panic!("bake 不应 None"));
        assert_eq!(near.sx, 1.0);
        assert_eq!(near.sy, 1.0);
        let expect_r = 0.5 * diag_len(48.0, 47.98);
        assert_eq!(near.r.to_bits(), expect_r.to_bits());

        // frog sun 尺度（r=42 扁平化：84×83.71，相对 3.4e-3）：仍吸附
        let frog = bake_radial(
            &def,
            Some([698.0, 63.0, 782.0, 146.71]),
            Mat2x3::IDENTITY,
            900.0,
            480.0,
        )
        .unwrap_or_else(|| panic!("bake 不应 None"));
        assert_eq!(frog.sx, 1.0);
        assert_eq!(frog.sy, 1.0);
        let expect_r2 = 0.5 * diag_len(84.0, 83.71);
        assert_eq!(frog.r.to_bits(), expect_r2.to_bits());

        // 相对不对称 2.5（200×80）：单位椭圆路径
        let aniso = bake_radial(
            &def,
            Some([0.0, 0.0, 200.0, 80.0]),
            Mat2x3::IDENTITY,
            260.0,
            170.0,
        )
        .unwrap_or_else(|| panic!("bake 不应 None"));
        assert_eq!(aniso.sx.to_bits(), (1.0f32 / 200.0).to_bits());
        assert_eq!(aniso.sy.to_bits(), (1.0f32 / 80.0).to_bits());
        assert_eq!(aniso.r.to_bits(), (0.5f32).to_bits());
    }

    /// wt/radial-focal：偏焦点椭圆的规范公式语义——焦点射线在单位空间
    /// 求交；焦点处 t=0；|F−C| ≥ r（单位空间）退化折算圆心，与同心
    /// 逐位一致。
    #[test]
    fn radial_ellipse_focal_spec_semantics() {
        let def = GradientDef {
            radial: true,
            units: GradUnits::ObjectBoundingBox,
            spread: SpreadMethod::Pad,
            p: GradParams {
                fx: Some((0.32, false)),
                fy: Some((0.35, false)),
                ..GradParams::default()
            },
            stops: vec![
                GradStop {
                    offset: 0.0,
                    color: rgba(255, 243, 196, 255),
                },
                GradStop {
                    offset: 1.0,
                    color: rgba(233, 69, 96, 255),
                },
            ],
        };
        let baked = bake_radial(
            &def,
            Some([30.0, 45.0, 230.0, 125.0]),
            Mat2x3::IDENTITY,
            260.0,
            170.0,
        )
        .unwrap_or_else(|| panic!("bake 不应 None"));
        // 焦点设备坐标 (30+0.32·200, 45+0.35·80) = (94, 73)：t=0 首停色
        assert_eq!(sample_radial(&baked, 94.0, 73.0), rgba(255, 243, 196, 255));

        // 外焦点（车道LG）：单位焦点距 1.0 ≥ r=0.5 → Skia conical
        // greater 语义（fR1=0.5）。三采样点 t 均 >1 → pad 钳末停，
        // 逐位精确（interp_stops 端点钳色不走抖动）
        let deg_def = GradientDef {
            p: GradParams {
                fx: Some((0.5, false)),
                fy: Some((1.5, false)),
                ..GradParams::default()
            },
            ..def
        };
        let deg = bake_radial(
            &deg_def,
            Some([30.0, 45.0, 230.0, 125.0]),
            Mat2x3::IDENTITY,
            260.0,
            170.0,
        )
        .unwrap_or_else(|| panic!("bake 不应 None"));
        for (x, y) in [(129.5f32, 124.5f32), (68.5, 45.5), (179.5, 85.5)] {
            assert_eq!(
                sample_radial(&deg, x, y),
                rgba(233, 69, 96, 255),
                "外焦点 pad 钳末停 ({x},{y})"
            );
        }
    }

    /// 车道PD：各向异性 userSpaceOnUse 烘焙——S = lin(m)⁻¹ 存全 2×2，
    /// r/fr 保持用户单位（不乘 mean_scale）。scale(2,1) 的逆 = (0.5,1)
    /// 轴对齐（shx=shy=0）。
    #[test]
    fn radial_aniso_user_bake_general() {
        let def = GradientDef {
            radial: true,
            units: GradUnits::UserSpaceOnUse,
            spread: SpreadMethod::Pad,
            p: GradParams {
                cx: Some((3.0, false)),
                cy: Some((4.0, false)),
                r: Some((2.0, false)),
                fr: Some((0.5, false)),
                ..GradParams::default()
            },
            stops: vec![
                GradStop {
                    offset: 0.0,
                    color: rgba(0, 0, 255, 255),
                },
                GradStop {
                    offset: 1.0,
                    color: rgba(255, 0, 0, 255),
                },
            ],
        };
        let g = bake_radial(&def, None, Mat2x3::scaling(2.0, 1.0), 0.0, 0.0)
            .unwrap_or_else(|| panic!("bake 不应 None"));
        assert_eq!(g.c, (6.0, 4.0));
        assert_eq!(g.f, (6.0, 4.0));
        // r/fr：用户单位原值（旧径会 ×2）
        assert_eq!(g.r, 2.0);
        assert_eq!(g.fr, 0.5);
        // S = lin(scale(2,1))⁻¹ = scale(0.5,1)
        assert_eq!(g.sx, 0.5);
        assert_eq!(g.sy, 1.0);
        assert_eq!(g.shx, 0.0);
        assert_eq!(g.shy, 0.0);
    }

    /// 车道PD：相似变换（旋转 30°）必须走旧路径——字段保持 (1,1,0,0)、
    /// r 乘 mean_scale，逐位口径不漂。
    #[test]
    fn radial_similarity_stays_legacy() {
        let def = GradientDef {
            radial: true,
            units: GradUnits::UserSpaceOnUse,
            spread: SpreadMethod::Pad,
            p: GradParams {
                r: Some((2.0, false)),
                ..GradParams::default()
            },
            stops: vec![
                GradStop {
                    offset: 0.0,
                    color: rgba(0, 0, 255, 255),
                },
                GradStop {
                    offset: 1.0,
                    color: rgba(255, 0, 0, 255),
                },
            ],
        };
        let g = bake_radial(&def, None, Mat2x3::rotation(30.0), 0.0, 0.0)
            .unwrap_or_else(|| panic!("bake 不应 None"));
        assert_eq!((g.sx, g.sy, g.shx, g.shy), (1.0, 1.0, 0.0, 0.0));
        assert_eq!(g.r, 2.0); // mean_scale(rot)=1
    }

    /// 车道PD：各向异性下采样等价性——烘焙(m, r_user) 后 sample(m·q)
    /// 必须等于单位设备圆 paint 在 q 的采样（±1 抖动容差）。这是
    /// 「设备等值线 = m(圆)」语义的直接钉死。
    #[test]
    fn radial_aniso_sample_equiv_device_circle() {
        let stops = || {
            vec![
                GradStop {
                    offset: 0.0,
                    color: rgba(0, 0, 255, 255),
                },
                GradStop {
                    offset: 1.0,
                    color: rgba(255, 0, 0, 255),
                },
            ]
        };
        let def = || GradientDef {
            radial: true,
            units: GradUnits::UserSpaceOnUse,
            spread: SpreadMethod::Pad,
            p: GradParams {
                r: Some((1.0, false)),
                ..GradParams::default()
            },
            stops: stops(),
        };
        let aniso = bake_radial(&def(), None, Mat2x3::scaling(2.0, 1.0), 0.0, 0.0)
            .unwrap_or_else(|| panic!("bake 不应 None"));
        let unit = bake_radial(&def(), None, Mat2x3::IDENTITY, 0.0, 0.0)
            .unwrap_or_else(|| panic!("bake 不应 None"));
        // m·q 与 q 成对采样：椭圆等值线 = 单位圆在 m 下的像
        for (qx, qy) in [
            (0.5f32, 0.0f32),
            (0.0, 0.5),
            (-0.3, 0.4),
            (libm::sqrtf(0.5), -libm::sqrtf(0.5)),
            (1.5, 0.0), // pad 区：末停
        ] {
            let (px, py) = Mat2x3::scaling(2.0, 1.0).apply(qx, qy);
            close(sample_radial(&aniso, px, py), sample_radial(&unit, qx, qy));
        }
    }

    /// 车道PD：带交叉项（旋转椭圆）的 SpanEval 与 sample_radial 逐位
    /// 等价（既有 span_eval_matches_sample_paint_radial 只盖轴对齐）。
    #[test]
    fn span_eval_matches_sample_radial_cross_terms() {
        // gt = R(45)·S(2,1) 的逆线性部分 = S(0.5,1)·R(−45)：
        // (sx, sy, shx, shy) = (0.5c, c, 0.5s, −s)
        let c45 = libm::sqrtf(0.5);
        let s45 = c45;
        for (c, r, f, sx, sy, shx, shy) in [
            // 旋转椭圆同心 / 偏焦点 / 外焦点（f 焦点为设备空间点）
            (
                64.0f32,
                40.0f32,
                (64.0, 64.0),
                c45 * 0.5,
                c45,
                s45 * 0.5,
                -s45,
            ),
            (64.0, 40.0, (74.0, 58.0), c45 * 0.5, c45, s45 * 0.5, -s45),
            (64.0, 40.0, (110.0, 20.0), c45 * 0.5, c45, s45 * 0.5, -s45),
        ] {
            let g = RadialGradient {
                c: (c, c),
                fr: 0.0,
                r,
                f,
                stops: vec![
                    GradStop {
                        offset: 0.0,
                        color: rgba(255, 255, 0, 255),
                    },
                    GradStop {
                        offset: 0.5,
                        color: rgba(0, 128, 255, 128),
                    },
                    GradStop {
                        offset: 1.0,
                        color: rgba(255, 0, 128, 255),
                    },
                ],
                spread: SpreadMethod::Pad,
                sx,
                sy,
                shx,
                shy,
            };
            let paint = Paint::RadialGradient(g.clone());
            let mut ev = SpanEval::new(&paint, TILE_SHIFT_TALL);
            for row in 0..32i32 {
                let y = row as f32 + 0.5;
                ev.begin_row(y);
                for px in 0..32i32 {
                    let x = px as f32 + 0.5;
                    assert_eq!(ev.at(x), sample_radial(&g, x, y), "({px},{row})");
                }
            }
        }
    }
}
