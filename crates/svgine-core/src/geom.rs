//! 几何基础：2D 仿射矩阵（SVG `transform`）、transform 列表解析、
//! viewBox/preserveAspectRatio 视口映射。
//!
//! 全 f32（ADR-0005：ESP32 FPU 仅单精度，f64 走软浮点）。

use alloc::vec::Vec;

/// SVG transform 矩阵 `[a b c d e f]`，对应
/// `| a c e |`
/// `| b d f |`
/// `| 0 0 1 |`。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Mat2x3 {
    pub a: f32,
    pub b: f32,
    pub c: f32,
    pub d: f32,
    pub e: f32,
    pub f: f32,
}

impl Mat2x3 {
    pub const IDENTITY: Self = Self {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        e: 0.0,
        f: 0.0,
    };

    pub const fn translation(tx: f32, ty: f32) -> Self {
        Self {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            e: tx,
            f: ty,
        }
    }

    pub const fn scaling(sx: f32, sy: f32) -> Self {
        Self {
            a: sx,
            b: 0.0,
            c: 0.0,
            d: sy,
            e: 0.0,
            f: 0.0,
        }
    }

    pub fn rotation(deg: f32) -> Self {
        let r = deg * core::f32::consts::PI / 180.0;
        let (s, c) = (crate::fnum::sin_f32(r), crate::fnum::cos_f32(r));
        Self {
            a: c,
            b: s,
            c: -s,
            d: c,
            e: 0.0,
            f: 0.0,
        }
    }

    pub fn skew_x(deg: f32) -> Self {
        Self {
            a: 1.0,
            b: 0.0,
            c: crate::fnum::tan_f32(deg * core::f32::consts::PI / 180.0),
            d: 1.0,
            e: 0.0,
            f: 0.0,
        }
    }

    pub fn skew_y(deg: f32) -> Self {
        Self {
            a: 1.0,
            b: crate::fnum::tan_f32(deg * core::f32::consts::PI / 180.0),
            c: 0.0,
            d: 1.0,
            e: 0.0,
            f: 0.0,
        }
    }

    /// 复合：`self.mul(&rhs)` 作用于点时**先应用 rhs 再应用 self**
    /// （对应 SVG transform 列表 `self rhs` 的语义）。
    pub fn mul(&self, rhs: &Self) -> Self {
        Self {
            a: self.a * rhs.a + self.c * rhs.b,
            b: self.b * rhs.a + self.d * rhs.b,
            c: self.a * rhs.c + self.c * rhs.d,
            d: self.b * rhs.c + self.d * rhs.d,
            e: self.a * rhs.e + self.c * rhs.f + self.e,
            f: self.b * rhs.e + self.d * rhs.f + self.f,
        }
    }

    pub fn apply(&self, x: f32, y: f32) -> (f32, f32) {
        (
            self.a * x + self.c * y + self.e,
            self.b * x + self.d * y + self.f,
        )
    }

    /// 逆矩阵（S10-03：gradientTransform 共轭用）。退化（|det| < 1e-12
    /// 或非有限）→ None。
    pub fn inverse(&self) -> Option<Self> {
        let det = self.a * self.d - self.b * self.c;
        if !det.is_finite() || det.abs() < 1e-12 {
            return None;
        }
        Some(Self {
            a: self.d / det,
            b: -self.b / det,
            c: -self.c / det,
            d: self.a / det,
            e: (self.c * self.f - self.d * self.e) / det,
            f: (self.b * self.e - self.a * self.f) / det,
        })
    }

    /// 转置（仅线性部分；平移清零——S10-03 的 oBB gradientTransform
    /// 轴向量映射 `(gtᵀ)⁻¹·v` 只作用于向量）。
    pub fn transpose_lin(&self) -> Self {
        Self {
            a: self.a,
            b: self.c,
            c: self.b,
            d: self.d,
            e: 0.0,
            f: 0.0,
        }
    }

    /// 平均缩放因子（描边宽度随变换近似缩放用）。
    pub fn mean_scale(&self) -> f32 {
        libm::sqrtf((self.a * self.d - self.b * self.c).abs()).max(0.0)
    }
}

/// CTM 线性部分是否**非相似变换**（各向异性）——车道OI 原建（描边
/// 包络 user-space 判据），车道PD 起提升到 geom 由 builder/gradient
/// 共用（径向渐变 gradientTransform / 元素变换各向异性同一判据）。
///
/// 判据：线性列向量 u=(a,b)、v=(c,d)，相似 ⇔ |u|²=|v|² 且 u·v=0。
/// 相对容差 1e-4：浮点相似（rotate 的 cos/sin 组合、均匀 scale 合成）
/// 的 |u|²−|v|² 与 u·v 在同一表达式结构下精确相等/精确消去（同操作数
/// 乘积互换），实测余量 >1e-7；真实语料各向异性（flag 族 0.71:1.01、
/// skew 1:2）幅值 ≥1e-2，两侧各留 >2 个数量级隔离。退化/非有限
/// （scale(0)/NaN）→ false（按相似走保守旧路径，与 inverse None 兜底
/// 同流——漏判只损精度，误判才动字节）。
pub fn lin_non_similarity(m: &Mat2x3) -> bool {
    let uu = m.a * m.a + m.b * m.b;
    let vv = m.c * m.c + m.d * m.d;
    let uv = m.a * m.c + m.b * m.d;
    let s = uu.max(vv);
    if !s.is_finite() || s <= 0.0 {
        return false;
    }
    // |uv| ≤ √(uu·vv) ≤ max(uu,vv)=s（Cauchy–Schwarz），以 s 为基准免
    // sqrt（no_std 纪律）。
    const TOL: f32 = 1e-4;
    (uu - vv).abs() > TOL * s || uv.abs() > TOL * s
}

/// 解析 transform 属性（函数列表），返回累积矩阵。非法输入尽力而为
/// （跳过无法解析的函数段），全失败返回单位阵。
pub fn parse_transform(s: &str) -> Mat2x3 {
    parse_transform_impl(s, false)
}

/// CSS transform 声明（`style="transform:…"`）解析（车道NF）：在属性
/// 语义之上追加绝对长度单位（`translate(12pt,9px)`，96px 基准换算）
/// 与 `deg` 角度单位——Chrome ua09 探针实证：CSS 声明位 pt/deg 生效，
/// **transform 属性位两者皆拒**（SVG 属性语法无单位，px 为 AK 登记
/// 的宽容特例），故 CSS 声明必须走本入口、与属性位分开。
pub fn parse_transform_css(s: &str) -> Mat2x3 {
    parse_transform_impl(s, true)
}

fn parse_transform_impl(s: &str, css_units: bool) -> Mat2x3 {
    let b = s.as_bytes();
    let mut i = 0;
    let mut acc = Mat2x3::IDENTITY;
    while i < b.len() {
        while i < b.len() && (b[i].is_ascii_whitespace() || b[i] == b',') {
            i += 1;
        }
        if i >= b.len() {
            break;
        }
        if !b[i].is_ascii_alphabetic() {
            i += 1;
            continue;
        }
        let name_start = i;
        while i < b.len() && b[i].is_ascii_alphabetic() {
            i += 1;
        }
        let name = &s[name_start..i];
        while i < b.len() && b[i] != b'(' {
            i += 1;
        }
        if i >= b.len() {
            break;
        }
        i += 1; // '('
        let args_start = i;
        // 括号感知参数段扫描。车道CI（parser-fuzz）：截断输入（无 ')'
        // 收尾）时参数段取到串尾——旧式 `s[args_start..i - 1]` 在截断态
        // 会把 i-1 落在多字节 UTF-8 字符中间（`translate(é中` → char
        // boundary panic）或 start>end（`rotate(` 空参数段）。
        let mut depth = 1;
        let mut closed = false;
        while i < b.len() && depth > 0 {
            if b[i] == b'(' {
                depth += 1;
            } else if b[i] == b')' {
                depth -= 1;
                if depth == 0 {
                    closed = true;
                }
            }
            i += 1;
        }
        let argstr = s[args_start..if closed { i - 1 } else { i }].trim();
        // 车道AK：`px` 后缀剥离（CSS transform 声明的长度规范形态 +
        // Chrome 对属性位 px 的宽容；SVG 1.1 属性语法无单位不受影响）。
        // 车道NF：css_units 语境追加绝对单位系数与 deg 角度（rad/grad/
        // turn 登记不支持）。
        let nums: Vec<f32> = argstr
            .split(|c: char| c.is_ascii_whitespace() || c == ',')
            .filter(|t| !t.is_empty())
            .filter_map(|t| {
                // 车道CI（parser-fuzz）：px 后缀剥离必须按**字节**判 ASCII
                // 尾缀——旧式 `t.split_at(t.len() - 2)` 对多字节收尾 token
                // （单字 `中` len=3 > 2）会切在字符中间（char boundary
                // panic）。'p''x' 均为 ASCII，字节命中即边界安全。
                if css_units {
                    if let Some((num, k)) = crate::fnum::parse_with_abs_unit(t) {
                        return crate::fnum::parse_f32(num.trim()).map(|v| v * k);
                    }
                    // deg 角度（rotate/skew）：单位系数 1（度即用户角单位）
                    if let Some(num) = strip_ascii_suffix(t, "deg") {
                        return crate::fnum::parse_f32(num.trim());
                    }
                    return crate::fnum::parse_f32(t);
                }
                let tb = t.as_bytes();
                let is_px = tb.len() > 2
                    && (tb[tb.len() - 2] == b'p' || tb[tb.len() - 2] == b'P')
                    && (tb[tb.len() - 1] == b'x' || tb[tb.len() - 1] == b'X');
                let n = if is_px { &t[..tb.len() - 2] } else { t };
                crate::fnum::parse_f32(n)
            })
            .collect();
        let m = match (name, nums.as_slice()) {
            ("translate", [tx]) => Mat2x3::translation(*tx, 0.0),
            ("translate", [tx, ty]) => Mat2x3::translation(*tx, *ty),
            ("scale", [sx]) => Mat2x3::scaling(*sx, *sx),
            ("scale", [sx, sy]) => Mat2x3::scaling(*sx, *sy),
            ("rotate", [a]) => Mat2x3::rotation(*a),
            ("rotate", [a, cx, cy]) => {
                // rotate(a cx cy) = translate(cx,cy) · rotate(a) · translate(-cx,-cy)
                Mat2x3::translation(*cx, *cy)
                    .mul(&Mat2x3::rotation(*a))
                    .mul(&Mat2x3::translation(-*cx, -*cy))
            }
            ("skewX", [a]) => Mat2x3::skew_x(*a),
            ("skewY", [a]) => Mat2x3::skew_y(*a),
            ("matrix", [a, bb, c, d, e, f]) => Mat2x3 {
                a: *a,
                b: *bb,
                c: *c,
                d: *d,
                e: *e,
                f: *f,
            },
            _ => Mat2x3::IDENTITY,
        };
        acc = acc.mul(&m);
    }
    acc
}

/// ASCII 尾缀剥离（字节级，大小写不敏感）。命中即 ASCII 边界（延续
/// 字节 ≥0x80 经 |0x20 不会等于 ASCII 小写字母），切片安全；未命中 /
/// 剥完为空 → None。
fn strip_ascii_suffix<'a>(t: &'a str, suffix: &str) -> Option<&'a str> {
    let tb = t.as_bytes();
    let sb = suffix.as_bytes();
    if tb.len() > sb.len() && tb[tb.len() - sb.len()..].eq_ignore_ascii_case(sb) {
        Some(&t[..tb.len() - sb.len()])
    } else {
        None
    }
}

/// preserveAspectRatio 解析结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AspectMode {
    /// meet：等比完整放入（留白）
    Meet { mid: bool, center: bool },
    /// slice：等比放大裁边
    Slice { mid: bool, center: bool },
    /// none：拉伸变形
    None,
}

/// 计算 viewBox → 视口的映射矩阵（SVG 规范 7.8 + preserveAspectRatio）。
/// `vw/vh` 为目标视口尺寸，`vbox` 为 (minx, miny, w, h)。
pub fn viewport_map(vw: f32, vh: f32, vbox: (f32, f32, f32, f32), par: &str) -> Mat2x3 {
    let (vx, vy, vbw, vbh) = vbox;
    if vbw <= 0.0 || vbh <= 0.0 || vw <= 0.0 || vh <= 0.0 {
        return Mat2x3::IDENTITY;
    }
    let s = par.trim();
    let mode = if s.eq_ignore_ascii_case("none") {
        AspectMode::None
    } else {
        let meet = !s.contains("slice");
        // 对齐轴：x 组件 xMin/xMid/xMax（小写 x 开头）+ y 组件
        // YMin/YMid/YMax（**大写 Y** 开头——规范词法 xMinYMax 等）。
        // 车道BH 修复：此前 y 轴用小写 "yMin"/"yMax" 匹配，规范拼写
        // 永不命中 → 纵向对齐静默退化居中（bh-par-ymin/ymax 探针
        // 51% 差，Chrome 语义 = 上/下边缘钉住）。
        let x_mid = !s.contains("xMin") && !s.contains("xMax");
        let y_center = !s.contains("YMin") && !s.contains("YMax");
        if meet {
            AspectMode::Meet {
                mid: x_mid,
                center: y_center,
            }
        } else {
            AspectMode::Slice {
                mid: x_mid,
                center: y_center,
            }
        }
    };
    match mode {
        AspectMode::None => Mat2x3::scaling(vw / vbw, vh / vbh).mul(&Mat2x3::translation(-vx, -vy)),
        AspectMode::Meet { mid, center } | AspectMode::Slice { mid, center } => {
            let scale = if matches!(mode, AspectMode::Meet { .. }) {
                (vw / vbw).min(vh / vbh)
            } else {
                (vw / vbw).max(vh / vbh)
            };
            let scaled_w = vbw * scale;
            let scaled_h = vbh * scale;
            let tx = if mid {
                (vw - scaled_w) / 2.0
            } else if s.contains("xMax") {
                vw - scaled_w
            } else {
                0.0
            };
            let ty = if center {
                (vh - scaled_h) / 2.0
            } else if s.contains("YMax") {
                vh - scaled_h
            } else {
                0.0
            };
            Mat2x3::translation(tx, ty)
                .mul(&Mat2x3::scaling(scale, scale))
                .mul(&Mat2x3::translation(-vx, -vy))
        }
    }
}

/// 车道CI（parser-fuzz）：截断 transform（参数段无 `)` 收尾）与
/// 多字节参数必须容错——旧代码 `s[args_start..i-1]` 在 `translate(é中`
/// 上 char boundary panic、`rotate(` 上 start>end panic；px 剥尾
/// `t.split_at(t.len()-2)` 在单字 `中`（len=3）上 panic。
#[test]
fn parse_transform_truncated_and_multibyte_tolerant() {
    assert_eq!(parse_transform("rotate("), Mat2x3::IDENTITY);
    assert_eq!(parse_transform("translate(é中"), Mat2x3::IDENTITY);
    assert_eq!(parse_transform("translate(中"), Mat2x3::IDENTITY);
    assert_eq!(parse_transform("scale(é"), Mat2x3::IDENTITY);
    // 正常形态逐位不变
    assert_eq!(
        parse_transform("translate(10px,20PX)"),
        parse_transform("translate(10,20)")
    );
    assert_eq!(parse_transform("translate(5,6)"), {
        let mut m = Mat2x3::IDENTITY;
        m.e = 5.0;
        m.f = 6.0;
        m
    });
}

/// 解析 viewBox 属性 "minx miny w h"。
pub fn parse_viewbox(s: &str) -> Option<(f32, f32, f32, f32)> {
    let v: Vec<f32> = s
        .split(|c: char| c.is_ascii_whitespace() || c == ',')
        .filter(|t| !t.is_empty())
        .filter_map(crate::fnum::parse_f32)
        .collect();
    match v.as_slice() {
        [x, y, w, h] => Some((*x, *y, *w, *h)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_is_noop() {
        let (x, y) = Mat2x3::IDENTITY.apply(3.0, 5.0);
        assert_eq!((x, y), (3.0, 5.0));
    }

    /// transform="translate(2,3) scale(2)"：先缩放再平移。
    #[test]
    fn translate_after_scale() {
        let m = Mat2x3::translation(2.0, 3.0).mul(&Mat2x3::scaling(2.0, 2.0));
        let (x, y) = m.apply(1.0, 1.0);
        assert_eq!((x, y), (4.0, 5.0));
    }

    #[test]
    fn scale_after_translate() {
        let m = Mat2x3::scaling(2.0, 2.0).mul(&Mat2x3::translation(1.0, 1.0));
        let (x, y) = m.apply(1.0, 1.0);
        assert_eq!((x, y), (4.0, 4.0));
    }

    #[test]
    fn mul_matches_nested_apply() {
        let m0 = Mat2x3::scaling(2.0, 3.0);
        let m1 = Mat2x3::translation(4.0, 5.0);
        let composed = m1.mul(&m0);
        let (x, y) = composed.apply(1.5, 2.5);
        let (ix, iy) = m0.apply(1.5, 2.5);
        let (jx, jy) = m1.apply(ix, iy);
        assert_eq!((x, y), (jx, jy));
    }

    #[test]
    fn parse_transform_list() {
        // translate 后接 rotate(30 10 20)
        let m = parse_transform("translate(10,20) rotate(30 10 20)");
        let manual = Mat2x3::translation(10.0, 20.0).mul(
            &Mat2x3::translation(10.0, 20.0)
                .mul(&Mat2x3::rotation(30.0))
                .mul(&Mat2x3::translation(-10.0, -20.0)),
        );
        assert_eq!(m, manual);
    }

    #[test]
    fn parse_matrix_and_skew() {
        let m = parse_transform("matrix(1,0.15,0.35,1,0,0)");
        assert_eq!(m.b, 0.15);
        assert_eq!(m.c, 0.35);
        let sk = parse_transform("skewX(45)");
        let (x, y) = sk.apply(0.0, 10.0);
        assert!((x - 10.0).abs() < 1e-4, "skewX(45) 平移 y 到 x");
        assert_eq!(y, 10.0);
    }

    /// inverse：m·m⁻¹ = 单位阵；奇异 / 退化矩阵 → None（S10-03）。
    #[test]
    fn inverse_roundtrip_and_singular() {
        let m = Mat2x3::translation(3.0, 5.0).mul(&Mat2x3::scaling(2.0, 4.0));
        let inv = m.inverse().unwrap_or_else(|| panic!("可逆矩阵应有逆"));
        let ident = m.mul(&inv);
        let (x, y) = ident.apply(7.0, 9.0);
        assert!((x - 7.0).abs() < 1e-4 && (y - 9.0).abs() < 1e-4);
        // 先逆后乘同样还原（共轭构造的正确性前提）
        let ident2 = inv.mul(&m);
        let (x2, y2) = ident2.apply(-2.0, 6.0);
        assert!((x2 + 2.0).abs() < 1e-4 && (y2 - 6.0).abs() < 1e-4);
        assert!(Mat2x3::scaling(0.0, 1.0).inverse().is_none(), "奇异无逆");
    }

    /// 100x100 的 viewBox 装进 80x80 视口（meet）：等比 0.8 居中。
    #[test]
    fn viewport_map_meet_centers() {
        let m = viewport_map(80.0, 80.0, (0.0, 0.0, 100.0, 100.0), "xMidYMid meet");
        // 中心点 (50,50) → (40,40)
        let (x, y) = m.apply(50.0, 50.0);
        assert!((x - 40.0).abs() < 1e-4 && (y - 40.0).abs() < 1e-4);
    }

    /// meet 进非方形视口：letterbox（上下留白）。
    #[test]
    fn viewport_map_letterbox() {
        let m = viewport_map(80.0, 80.0, (0.0, 0.0, 100.0, 50.0), "xMidYMid meet");
        // 宽比 0.8，高比 1.6 → 取 0.8；内容高 50*0.8=40，居中 y 偏移 20
        let (x0, y0) = m.apply(0.0, 0.0);
        let (x1, y1) = m.apply(100.0, 50.0);
        assert_eq!((x0, y0), (0.0, 20.0));
        assert_eq!((x1, y1), (80.0, 60.0));
    }

    #[test]
    fn viewport_map_slice_and_none() {
        let s = viewport_map(80.0, 80.0, (0.0, 0.0, 100.0, 50.0), "xMidYMid slice");
        let (x1, y1) = s.apply(100.0, 50.0);
        // slice 取 1.6：宽超出（x1=160），高恰满（y1=80）
        assert_eq!(y1, 80.0);
        assert!(x1 > 80.0);
        let n = viewport_map(80.0, 40.0, (0.0, 0.0, 100.0, 50.0), "none");
        let (x, y) = n.apply(100.0, 50.0);
        assert_eq!((x, y), (80.0, 40.0));
    }

    /// viewBox 带偏移：先平移回原点。
    #[test]
    fn viewport_map_offset_viewbox() {
        let m = viewport_map(80.0, 80.0, (10.0, 10.0, 80.0, 80.0), "xMidYMid meet");
        let (x, y) = m.apply(10.0, 10.0);
        assert!((x - 0.0).abs() < 1e-4 && (y - 0.0).abs() < 1e-4);
    }

    /// 车道BH：y 轴对齐关键词是**大写 Y**（规范词法 xMinYMin /
    /// xMinYMax）——此前小写 "yMin"/"yMax" 匹配永不命中，纵向对齐
    /// 静默退化居中（bh-par-ymin/ymax 探针，Chrome 51% 差实证）。
    #[test]
    fn viewport_map_y_alignment_keywords() {
        // xMinYMin：内容钉左上（200x100 装进 100x100，scale 0.5，
        // 内容 100x50 → ty=0）
        let m = viewport_map(100.0, 100.0, (0.0, 0.0, 200.0, 100.0), "xMinYMin");
        let (x, y) = m.apply(0.0, 0.0);
        assert_eq!((x, y), (0.0, 0.0), "YMin 钉顶");
        // xMinYMax：钉左下（ty = 100 − 50 = 50）
        let m = viewport_map(100.0, 100.0, (0.0, 0.0, 200.0, 100.0), "xMinYMax");
        let (x, y) = m.apply(0.0, 0.0);
        assert_eq!((x, y), (0.0, 50.0), "YMax 钉底");
        // xMaxYMax：横向钉右（200x100 装 100x50，scale 2 → 内容 200x50，
        // tx = 200−200 = 0…… slack 只出现在非决定轴：换 viewBox 100x25，
        // scale 2 → 内容 200x50，tx=0；再换 50x50：scale 2 → tx=100）
        let m = viewport_map(200.0, 100.0, (0.0, 0.0, 50.0, 50.0), "xMaxYMax");
        let (x, y) = m.apply(0.0, 0.0);
        assert_eq!((x, y), (100.0, 0.0), "xMax 钉右（y 轴无 slack）");
        // 缺省（无对齐词）仍居中
        let m = viewport_map(100.0, 100.0, (0.0, 0.0, 200.0, 100.0), "meet");
        let (x, y) = m.apply(0.0, 0.0);
        assert_eq!((x, y), (0.0, 25.0), "缺省 xMidYMid 居中");
    }
}

#[cfg(test)]
mod px_tests {
    use super::*;

    /// 车道AK：transform 函数参数接受 px 后缀（CSS 声明/Chrome 宽容形态）。
    #[test]
    fn parse_transform_strips_px_suffix() {
        let m = parse_transform("translate(29px, 11px)");
        assert_eq!(m.e, 29.0);
        assert_eq!(m.f, 11.0);
        let s = parse_transform("scale(2)");
        assert_eq!(s.a, 2.0);
        // 无单位行为不变
        let u = parse_transform("translate(3 4)");
        assert_eq!(u.e, 3.0);
        assert_eq!(u.f, 4.0);
    }

    /// 车道NF（unit-audit）：CSS transform 声明位的绝对单位与 deg 角度
    /// （Chrome ua09 探针实证：声明位 pt/deg 生效）。
    #[test]
    fn parse_transform_css_units() {
        let m = parse_transform_css("translate(12pt, 9pt)");
        assert!((m.e - 16.0).abs() < 1e-3, "12pt = 16px");
        assert!((m.f - 12.0).abs() < 1e-3);
        let q = parse_transform_css("translate(25.4mm, 101.6q)");
        assert!((q.e - 96.0).abs() < 1e-2);
        assert!((q.f - 96.0).abs() < 1e-2);
        // deg 角度（rotate 单位系数 1）
        let r = parse_transform_css("rotate(90deg)");
        let (x, y) = r.apply(1.0, 0.0);
        assert!(
            (x - 0.0).abs() < 1e-5 && (y - 1.0).abs() < 1e-5,
            "90° 逆变顺"
        );
        // px 语义不变
        let p = parse_transform_css("translate(5px, 6px)");
        assert_eq!((p.e, p.f), (5.0, 6.0));
    }

    /// 车道NF：transform **属性位**拒绝 pt/deg（Chrome 同口径——
    /// ua09 探针 cu 为 identity；px 是 AK 登记的宽容特例）。
    #[test]
    fn parse_transform_attr_rejects_pt_deg() {
        // pt token 解析失败 → 函数段跳过 → 单位阵
        let m = parse_transform("translate(12pt,9pt)");
        assert_eq!((m.e, m.f), (0.0, 0.0));
        // deg 同拒
        let r = parse_transform("rotate(30deg)");
        let (x, y) = r.apply(1.0, 0.0);
        assert!((x - 1.0).abs() < 1e-6 && (y - 0.0).abs() < 1e-6);
    }
}
