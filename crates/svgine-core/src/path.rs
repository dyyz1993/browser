//! 路径几何：`d` 属性全命令解析、贝塞尔/弧平化、形状多边形化、
//! 描边轮廓展开。M1 核心。
//!
//! 管线约定：解析期同时跟踪**用户空间当前点**（相对命令/镜像控制点的
//! 基准），每个产出点经矩阵变换到设备空间后再平化（设备容差）；
//! viewBox/transform 全部烘焙进点，raster 无变换（M1 简化决策）。

use alloc::vec;
use alloc::vec::Vec;

use crate::geom::Mat2x3;

/// 平化容差（设备像素）——C/Q 控制点距弦的停止阈值。车道T 实证
/// （tests/oracle/repros/t1-*.svg + simple-icons/circle.svg 型全 cubic
/// 细环图标）：Chrome/Skia 对 C/Q 边解析逐扫描线求值（tiny-skia
/// CubicEdge/QuadraticEdge 同源），无平化误差；0.15 阈值给边缘留
/// ~0.11px 系统内偏（凸曲线弦恒在曲线内侧），环上像素 ±25 LSB 级
/// 差异。0.02 ⇒ 曲线偏差 ≤~0.015px（≤4 LSB）。递归中点细分的细分点
/// 在曲线上、弦在内侧，仅剩的偏差方向不变但幅度压到量化级。
const TOL: f32 = 0.02;
/// 弧/椭圆细细分容差（设备像素）——车道T 实证：TOL=0.15 内接多边形在
/// 小圆（r≤12px）边缘产生系统性欠墨（弦垂点 -33 LSB，r=4 整体 -4.8%，
/// 与 n 段内接多边形面积亏量吻合；探针 tests/oracle/repros/t1-*.svg）。
/// 0.02 把弦高位移压到 ≤2% 覆盖（≤5 LSB），与 builder::fine_ellipse
/// （M8，滤镜源）同常数——非滤镜路径本就经 blur 之外的同一 AA 管线，
/// Chrome 预言机用精确 conic 弧，系统偏置消掉后只剩其量化噪声。
const TOL_FINE: f32 = 0.02;
/// 曲线细分递归上限，防御病态输入。
const MAX_DEPTH: u8 = 18;

/// `d` 属性的绝对命令（解析期已把相对命令展开；M 的隐式重复坐标组
/// 已按规范转成 L）。M6 路径变形（animate d）在此层逐命令插值——
/// 保留曲线语义（Q/C 控制点平滑过渡），平化后的折线插值会折线化。
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Cmd {
    M(f32, f32),
    L(f32, f32),
    H(f32),
    V(f32),
    C(f32, f32, f32, f32, f32, f32),
    S(f32, f32, f32, f32),
    Q(f32, f32, f32, f32),
    T(f32, f32),
    /// rx, ry, x-axis-rotation, large-arc-flag, sweep-flag, x, y。
    /// flag 是离散值不参与插值（取段首帧；flag 不同的帧结构视为兼容，
    /// 数值部分照常插——SVG 规范对 flag 不一致无明确插值语义）。
    A(f32, f32, f32, bool, bool, f32, f32),
    Z,
}

/// 从 `d[i..]` 解析一个数字（跳过前导空白/逗号），推进 `i`。
/// 支持十进制、正负号、小数点与指数。
fn num_at(d: &str, b: &[u8], i: &mut usize) -> Option<f32> {
    while *i < b.len() && (b[*i].is_ascii_whitespace() || b[*i] == b',') {
        *i += 1;
    }
    scan_num(d, b, i)
}

/// 数字 token 扫描核（**不消费任何分隔符**——前置跳过策略由调用方
/// 约定）：起点 `i` 解析 [符号][数字][.数字][指数]，成功推进 `i`。
/// SVG BNF：fractional-constant 的 '.' 后必须有 digit——"23." 非法
/// 返回 None（WPT number-error-trailing-decimal；修复前被吞作 23.0）。
fn scan_num(d: &str, b: &[u8], i: &mut usize) -> Option<f32> {
    let start = *i;
    if *i < b.len() && (b[*i] == b'-' || b[*i] == b'+') {
        *i += 1;
    }
    let mut seen_digit = false;
    let mut seen_dot = false;
    while *i < b.len() {
        let ch = b[*i];
        if ch.is_ascii_digit() {
            seen_digit = true;
        } else if ch == b'.' && !seen_dot {
            match b.get(*i + 1) {
                Some(n) if n.is_ascii_digit() => seen_dot = true,
                _ => return None,
            }
        } else if (ch == b'e' || ch == b'E') && seen_digit {
            if *i + 1 < b.len()
                && (b[*i + 1].is_ascii_digit() || b[*i + 1] == b'-' || b[*i + 1] == b'+')
            {
                *i += 2;
                continue;
            }
            break;
        } else {
            break;
        }
        *i += 1;
    }
    if !seen_digit {
        return None;
    }
    crate::fnum::parse_f32(&d[start..*i])
}

/// `i` 处（跳过空白/逗号后）是否是下一个数字的起始（隐式命令重复判定）。
fn peek_num_at(b: &[u8], i: usize) -> bool {
    let mut j = i;
    while j < b.len() && (b[j].is_ascii_whitespace() || b[j] == b',') {
        j += 1;
    }
    j < b.len() && (b[j].is_ascii_digit() || b[j] == b'-' || b[j] == b'+' || b[j] == b'.')
}

/// 解析 arc 的单字符 flag（`0` 或 `1`），推进 `i`。
/// SVG 规范 flag 语法 `flag ::= "0" | "1"` 且允许与后续参数**无分隔连写**
/// （如 `a11 11 0 11-.01 0` ≡ laf=1 sf=1 x=-0.01 y=0——simple-icons 海量
/// 命中形态，twemoji 亦有）。数字扫描会贪婪吞掉连写 flag（`11` 当一个数、
/// `-.01` 被错位消费），故 flag 必须单字符消费；遇非 0/1 → None。
fn flag_at(b: &[u8], i: &mut usize) -> Option<bool> {
    while *i < b.len() && (b[*i].is_ascii_whitespace() || b[*i] == b',') {
        *i += 1;
    }
    match b.get(*i) {
        Some(b'0') => {
            *i += 1;
            Some(false)
        }
        Some(b'1') => {
            *i += 1;
            Some(true)
        }
        _ => None,
    }
}

/// 解析 `d` 属性：相对命令就地展开为绝对，保留 S/T 所需的镜像语义给
/// 平化阶段（`last_cp` 字段在 [flatten_d] 里处理）。
/// 严格口径：任何非法 token / 参数不足 → None（动画 d 帧、CSS d 声明
/// 等「全对才有效」的调用方使用）。
pub fn parse_d(d: &str) -> Option<Vec<Cmd>> {
    parse_d_inner(d).ok()
}

/// Chrome 错误恢复口径（SVG2 PathDataErrorHandling）：解析到第一个
/// 非法 token / 参数不足处为止，返回已成功解析的命令前缀——隐式重复
/// 参数组逐组提交，已完成的组保留，出错组整体丢弃。`ok=false` 表示
/// 发生了截断。渲染/几何（[flatten_d]）走本口径：Chrome 渲染到最后
/// 合法段为止，不是整条丢弃（WPT path/parsing/error-handling 考点）。
pub fn parse_d_recover(d: &str) -> (Vec<Cmd>, bool) {
    match parse_d_inner(d) {
        Ok(cmds) => (cmds, true),
        Err(cmds) => (cmds, false),
    }
}

fn parse_d_inner(d: &str) -> Result<Vec<Cmd>, Vec<Cmd>> {
    let b = d.as_bytes();
    let mut i = 0;
    let mut out: Vec<Cmd> = Vec::new();
    let mut cur = (0.0f32, 0.0f32);
    let mut sub_start = (0.0f32, 0.0f32); // Z 后相对命令的基准（诊断 RC2）

    while i < b.len() {
        while i < b.len() && (b[i].is_ascii_whitespace() || b[i] == b',') {
            i += 1;
        }
        if i >= b.len() {
            break;
        }
        let c = b[i];
        if !c.is_ascii_alphabetic() {
            return Err(out);
        }
        let rel = c.is_ascii_lowercase();
        i += 1;
        macro_rules! num {
            () => {
                match num_at(d, b, &mut i) {
                    Some(v) => v,
                    None => return Err(out),
                }
            };
        }
        macro_rules! pt {
            () => {{
                let x = num!();
                let y = num!();
                if rel {
                    (cur.0 + x, cur.1 + y)
                } else {
                    (x, y)
                }
            }};
        }
        // 隐式命令重复：SVG 规范允许同一命令字母后跟多组参数
        // （如 "L10 0 10 10" ≡ 两个 L；M 的后续组按规范变成 L/l）。
        // 每组参数消耗完后 peek 下一个 token 是否仍是数字，是则重复该命令。
        macro_rules! peek_is_num {
            () => {
                peek_num_at(b, i)
            };
        }
        match c {
            b'M' | b'm' => {
                let mut first = true;
                loop {
                    let p = pt!();
                    if first {
                        out.push(Cmd::M(p.0, p.1));
                        first = false;
                        sub_start = p;
                    } else {
                        // 规范：moveto 后的额外坐标组按 lineto 处理
                        out.push(Cmd::L(p.0, p.1));
                    }
                    cur = p;
                    if !peek_is_num!() {
                        break;
                    }
                }
            }
            b'L' | b'l' => loop {
                let p = pt!();
                out.push(Cmd::L(p.0, p.1));
                cur = p;
                if !peek_is_num!() {
                    break;
                }
            },
            b'H' | b'h' => loop {
                let x = num!();
                let x = if rel { cur.0 + x } else { x };
                out.push(Cmd::H(x));
                cur.0 = x;
                if !peek_is_num!() {
                    break;
                }
            },
            b'V' | b'v' => loop {
                let y = num!();
                let y = if rel { cur.1 + y } else { y };
                out.push(Cmd::V(y));
                cur.1 = y;
                if !peek_is_num!() {
                    break;
                }
            },
            b'C' | b'c' => loop {
                let p1 = pt!();
                let p2 = pt!();
                let p3 = pt!();
                out.push(Cmd::C(p1.0, p1.1, p2.0, p2.1, p3.0, p3.1));
                cur = p3;
                if !peek_is_num!() {
                    break;
                }
            },
            b'S' | b's' => loop {
                let p2 = pt!();
                let p3 = pt!();
                out.push(Cmd::S(p2.0, p2.1, p3.0, p3.1));
                cur = p3;
                if !peek_is_num!() {
                    break;
                }
            },
            b'Q' | b'q' => loop {
                let cp = pt!();
                let p = pt!();
                out.push(Cmd::Q(cp.0, cp.1, p.0, p.1));
                cur = p;
                if !peek_is_num!() {
                    break;
                }
            },
            b'T' | b't' => loop {
                let p = pt!();
                out.push(Cmd::T(p.0, p.1));
                cur = p;
                if !peek_is_num!() {
                    break;
                }
            },
            b'A' | b'a' => loop {
                let rx = num!();
                let ry = num!();
                let rot = num!();
                // flag 单字符消费（连写 `11-.01` = laf=1 sf=1 x=-0.01）
                let large = match flag_at(b, &mut i) {
                    Some(f) => f,
                    None => return Err(out), // 非法 flag（非 0/1）→ 截断
                };
                let sweep = match flag_at(b, &mut i) {
                    Some(f) => f,
                    None => return Err(out),
                };
                let p = pt!();
                out.push(Cmd::A(rx, ry, rot, large, sweep, p.0, p.1));
                cur = p;
                if !peek_is_num!() {
                    break;
                }
            },
            b'Z' | b'z' => {
                out.push(Cmd::Z);
                cur = sub_start; // Z 后相对命令以子路径起点为基准（RC2）
            }
            _ => return Err(out), // 非法命令字母 → 截断
        }
    }
    Ok(out)
}

/// 一条平化后的子路径（设备空间点 + 是否闭合）。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Subpath {
    pub pts: Vec<(f32, f32)>,
    pub closed: bool,
}

/// 平化后的完整路径几何（一个形状的全部子路径）。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PathGeom {
    pub subs: Vec<Subpath>,
}

/// 折线总长：与 [`stroke_dashed`] 的弧长约定一致（逐 pts 段求和）。
/// 闭合子路径补 Z 隐式闭合边（末点≠首点时计最后→首的距离；显式回
/// 起点型——poly/ellipse 折线末点==首点——不重复计）。
/// 用途：pathLength 归一化（s9-03）与 getTotalLength——Chrome 周长
/// 含闭合边（WPT lineto-commands/moveto-absolute 考点）。
pub fn polyline_length(g: &PathGeom) -> f32 {
    let mut total = 0.0f32;
    for sub in &g.subs {
        let pts = &sub.pts;
        for k in 1..pts.len() {
            let (dx, dy) = (pts[k].0 - pts[k - 1].0, pts[k].1 - pts[k - 1].1);
            total += libm::sqrtf(dx * dx + dy * dy);
        }
        if sub.closed && pts.len() >= 2 {
            let n = pts.len() - 1;
            let (dx, dy) = (pts[0].0 - pts[n].0, pts[0].1 - pts[n].1);
            if dx != 0.0 || dy != 0.0 {
                total += libm::sqrtf(dx * dx + dy * dy);
            }
        }
    }
    total
}

/// 把 `d` 属性按矩阵 `m` 变换并平化到设备空间。解析走 Chrome 错误
/// 恢复口径（[parse_d_recover]）：非法 token 处截断，已成功段照常
/// 渲染/计量。
pub fn flatten_d(d: &str, m: Mat2x3) -> PathGeom {
    let (cmds, _) = parse_d_recover(d);
    flatten_cmds(&cmds, m)
}

/// 已解析命令列（M6 路径变形的插值产物）按矩阵 `m` 变换并平化。
/// 与 [`flatten_d`] 共用平化主循环（字符串路径解析一次后零开销复用）。
pub fn flatten_cmds(cmds: &[Cmd], m: Mat2x3) -> PathGeom {
    let mut g = PathGeom::default();
    // 用户空间游标（相对/镜像语义的正确基准）
    let mut ucur = (0.0f32, 0.0f32);
    let mut ustart = (0.0f32, 0.0f32);
    let mut dev = m.apply(ucur.0, ucur.1);
    let mut sub: Vec<(f32, f32)> = Vec::new();
    let mut last_cubic_cp: Option<(f32, f32)> = None;
    let mut last_quad_cp: Option<(f32, f32)> = None;

    let flush = |sub: &mut Vec<(f32, f32)>, g: &mut PathGeom, closed: bool| {
        if sub.len() >= 2 {
            g.subs.push(Subpath {
                pts: core::mem::take(sub),
                closed,
            });
        } else {
            sub.clear();
        }
    };

    for &cmd in cmds {
        match cmd {
            Cmd::M(x, y) => {
                flush(&mut sub, &mut g, false);
                ucur = (x, y);
                ustart = ucur;
                dev = m.apply(x, y);
                sub.push(dev);
                last_cubic_cp = None;
                last_quad_cp = None;
            }
            Cmd::L(x, y) => {
                ucur = (x, y);
                dev = m.apply(x, y);
                sub.push(dev);
                last_cubic_cp = None;
                last_quad_cp = None;
            }
            Cmd::T(x, y) => {
                // T：控制点 = 前一个二次控制点的镜像（无则当前点）
                let cp = match last_quad_cp {
                    Some(c) => (2.0 * ucur.0 - c.0, 2.0 * ucur.1 - c.1),
                    None => ucur,
                };
                let cdev = m.apply(cp.0, cp.1);
                let end = m.apply(x, y);
                flatten_quad(&mut sub, dev, cdev, end);
                ucur = (x, y);
                dev = end;
                last_quad_cp = Some(cp);
                last_cubic_cp = None;
            }
            Cmd::H(x) => {
                // 用户空间水平：y 保持用户 y
                ucur.0 = x;
                dev = m.apply(ucur.0, ucur.1);
                sub.push(dev);
                last_cubic_cp = None;
                last_quad_cp = None;
            }
            Cmd::V(y) => {
                ucur.1 = y;
                dev = m.apply(ucur.0, ucur.1);
                sub.push(dev);
                last_cubic_cp = None;
                last_quad_cp = None;
            }
            Cmd::C(x1, y1, x2, y2, x, y) => {
                let p1 = m.apply(x1, y1);
                let p2 = m.apply(x2, y2);
                let p3 = m.apply(x, y);
                flatten_cubic(&mut sub, dev, p1, p2, p3);
                ucur = (x, y);
                dev = p3;
                last_cubic_cp = Some((x2, y2));
                last_quad_cp = None;
            }
            Cmd::S(x2, y2, x, y) => {
                let cp1 = match last_cubic_cp {
                    Some(c) => (2.0 * ucur.0 - c.0, 2.0 * ucur.1 - c.1),
                    None => ucur,
                };
                let d1 = m.apply(cp1.0, cp1.1);
                let d2 = m.apply(x2, y2);
                let d3 = m.apply(x, y);
                flatten_cubic(&mut sub, dev, d1, d2, d3);
                ucur = (x, y);
                dev = d3;
                last_cubic_cp = Some((x2, y2));
                last_quad_cp = None;
            }
            Cmd::Q(x1, y1, x, y) => {
                let cdev = m.apply(x1, y1);
                let end = m.apply(x, y);
                flatten_quad(&mut sub, dev, cdev, end);
                ucur = (x, y);
                dev = end;
                last_quad_cp = Some((x1, y1));
                last_cubic_cp = None;
            }
            Cmd::A(rx, ry, rot, large, sweep, x, y) => {
                // 车道AE C2：弧在**用户空间**中心参数化 + 逐点仿射映射
                // ——对任意仿射（含负行列式镜像/非均匀缩放/旋转）精确。
                // 此前在设备空间用用户空间旗标转换：镜像下手性反转
                // （sweep 语义翻转——openmoji 轮椅家族 scale(-1,1) 下
                // 圆顶变碗形）；均匀正缩放下 F.6.6 半径放大恰好等价，
                // 故主语料（viewBox 均匀缩放）从未暴露。恒等矩阵下
                // 逐点 apply 精确不变（IEEE 恒等运算），字节零漂移。
                // 车道AN：x-axis-rotation 落地（此前恒按 0——WPT
                // marker-path-003/013 `a 30,20 90` 旋转椭圆波形整体畸变；
                // rot=0 走 cos/sin=1/0 同一表达式，逐位一致）
                let mut arc: Vec<(f32, f32)> = Vec::new();
                flatten_arc(&mut arc, ucur, rx, ry, rot, large, sweep, (x, y));
                for p in arc {
                    sub.push(m.apply(p.0, p.1));
                }
                ucur = (x, y);
                dev = m.apply(x, y);
                last_cubic_cp = None;
                last_quad_cp = None;
            }
            Cmd::Z => {
                flush(&mut sub, &mut g, true);
                ucur = ustart;
                dev = m.apply(ustart.0, ustart.1);
                sub.push(dev);
                last_cubic_cp = None;
                last_quad_cp = None;
            }
        }
    }
    flush(&mut sub, &mut g, false);
    g
}

// ============ M6：animate d 路径变形（同构命令插值） ============

/// 两帧 `d` 是否结构兼容（可插值）：命令数一致且每个位置命令种类一致
/// （值不参与判定；A 的 large/sweep flag 差异不视为结构不兼容）。
/// SVG 规范：不兼容的 values 列表使整段动画无效。
pub fn cmds_same_shape(a: &[Cmd], b: &[Cmd]) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b)
            .all(|(x, y)| core::mem::discriminant(x) == core::mem::discriminant(y))
}

/// 同构两帧之间逐命令逐参数线性插值（t ∈ [0,1]）。
/// M/L/H/V/C/S/Q/T/A 的数值参数直接插；A 的 flag 取帧 a（离散值无插值
/// 语义）；Z 无参数原样保留（闭合性在变形全程不变）。
/// 结构不兼容时返回帧 a 的拷贝（调用方 [`cmds_same_shape`] 已保证兼容，
/// 此为防御回退）。
pub fn interp_cmds(a: &[Cmd], b: &[Cmd], t: f32) -> Vec<Cmd> {
    let lerp = |x: f32, y: f32| x + (y - x) * t;
    if !cmds_same_shape(a, b) {
        return Vec::from(a);
    }
    a.iter()
        .zip(b)
        .map(|(x, y)| match (x, y) {
            (Cmd::M(x0, y0), Cmd::M(x1, y1)) => Cmd::M(lerp(*x0, *x1), lerp(*y0, *y1)),
            (Cmd::L(x0, y0), Cmd::L(x1, y1)) => Cmd::L(lerp(*x0, *x1), lerp(*y0, *y1)),
            (Cmd::H(x0), Cmd::H(x1)) => Cmd::H(lerp(*x0, *x1)),
            (Cmd::V(y0), Cmd::V(y1)) => Cmd::V(lerp(*y0, *y1)),
            (Cmd::C(x0, y0, x2, y2, x4, y4), Cmd::C(x1, y1, x3, y3, x5, y5)) => Cmd::C(
                lerp(*x0, *x1),
                lerp(*y0, *y1),
                lerp(*x2, *x3),
                lerp(*y2, *y3),
                lerp(*x4, *x5),
                lerp(*y4, *y5),
            ),
            (Cmd::S(x0, y0, x2, y2), Cmd::S(x1, y1, x3, y3)) => Cmd::S(
                lerp(*x0, *x1),
                lerp(*y0, *y1),
                lerp(*x2, *x3),
                lerp(*y2, *y3),
            ),
            (Cmd::Q(x0, y0, x2, y2), Cmd::Q(x1, y1, x3, y3)) => Cmd::Q(
                lerp(*x0, *x1),
                lerp(*y0, *y1),
                lerp(*x2, *x3),
                lerp(*y2, *y3),
            ),
            (Cmd::T(x0, y0), Cmd::T(x1, y1)) => Cmd::T(lerp(*x0, *x1), lerp(*y0, *y1)),
            (Cmd::A(rx0, ry0, rot0, la0, sw0, x0, y0), Cmd::A(rx1, ry1, rot1, _, _, x1, y1)) => {
                Cmd::A(
                    lerp(*rx0, *rx1),
                    lerp(*ry0, *ry1),
                    lerp(*rot0, *rot1),
                    *la0,
                    *sw0,
                    lerp(*x0, *x1),
                    lerp(*y0, *y1),
                )
            }
            (Cmd::Z, Cmd::Z) => Cmd::Z,
            // cmds_same_shape 已保证同种命令，此分支不可达（穷尽性兜底）
            _ => *x,
        })
        .collect()
}

fn flatten_cubic(
    out: &mut Vec<(f32, f32)>,
    p0: (f32, f32),
    p1: (f32, f32),
    p2: (f32, f32),
    p3: (f32, f32),
) {
    flatten_cubic_rec(out, p0, p1, p2, p3, 0);
}

fn flatten_cubic_rec(
    out: &mut Vec<(f32, f32)>,
    p0: (f32, f32),
    p1: (f32, f32),
    p2: (f32, f32),
    p3: (f32, f32),
    depth: u8,
) {
    // 平坦度：控制点离弦的最大距离
    let d1 = dist_seg(p0, p3, p1);
    let d2 = dist_seg(p0, p3, p2);
    if depth >= MAX_DEPTH || d1.max(d2) <= TOL {
        out.push(p3);
        return;
    }
    let mid = |a: (f32, f32), b: (f32, f32)| ((a.0 + b.0) * 0.5, (a.1 + b.1) * 0.5);
    let a0 = mid(p0, p1);
    let a1 = mid(p1, p2);
    let a2 = mid(p2, p3);
    let b0 = mid(a0, a1);
    let b1 = mid(a1, a2);
    let c = mid(b0, b1);
    flatten_cubic_rec(out, p0, a0, b0, c, depth + 1);
    flatten_cubic_rec(out, c, b1, a2, p3, depth + 1);
}

fn flatten_quad(out: &mut Vec<(f32, f32)>, p0: (f32, f32), cp: (f32, f32), p1: (f32, f32)) {
    // 升阶为三次贝塞尔复用平化
    let lerp =
        |a: (f32, f32), b: (f32, f32), t: f32| (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t);
    let c1 = lerp(p0, cp, 2.0 / 3.0);
    let c2 = lerp(p1, cp, 2.0 / 3.0);
    flatten_cubic(out, p0, c1, c2, p1);
}

/// SVG 弧（端点参数化 → 中心参数化采样，W3C 附录 F.6.5 的 xrot=0 特例；
/// 非均匀变换下的 xrot 复合为 M1 近似，语料未涉及）。
#[allow(clippy::too_many_arguments)]
fn flatten_arc(
    out: &mut Vec<(f32, f32)>,
    from: (f32, f32),
    mut rx: f32,
    mut ry: f32,
    x_axis_rotation: f32,
    large: bool,
    sweep: bool,
    to: (f32, f32),
) {
    // Chrome 宽容口径：负半径取绝对值（SVG2 视为 error，Chrome/Skia
    // 取 |rx| |ry| 正常绘弧——WPT arc-commands 考点）；零半径直线化。
    rx = rx.abs();
    ry = ry.abs();
    if rx <= 0.0 || ry <= 0.0 || from == to {
        out.push(to);
        return;
    }
    let (x1, y1) = from;
    let (x2, y2) = to;
    // F.6.3：把中点半差旋入椭圆坐标系（x1p, y1p → x1', y1'）。
    // 车道AN 前 rot 恒按 0（cos=1/sin=0，退化为旧路径，数值逐位一致）。
    let phi = x_axis_rotation * core::f32::consts::PI / 180.0;
    let cphi = crate::fnum::cos_f32(phi);
    let sphi = crate::fnum::sin_f32(phi);
    let x1p = (x1 - x2) / 2.0;
    let y1p = (y1 - y2) / 2.0;
    let x1c = cphi * x1p + sphi * y1p;
    let y1c = -sphi * x1p + cphi * y1p;
    // F.6.6：半径过小则等比放大
    let lambda_den = rx * rx * y1c * y1c + ry * ry * x1c * x1c;
    if lambda_den > rx * rx * ry * ry {
        let s = libm::sqrtf(lambda_den / (rx * rx * ry * ry));
        rx *= s;
        ry *= s;
    }
    let num = rx * rx * ry * ry - rx * rx * y1c * y1c - ry * ry * x1c * x1c;
    let den = rx * rx * y1c * y1c + ry * ry * x1c * x1c;
    let sign = if large != sweep { 1.0 } else { -1.0 };
    let co = sign * libm::sqrtf((num / den).max(0.0));
    // F.6.5：椭圆坐标系中心 → F.6.4 转回用户空间
    let cxp = co * rx * y1c / ry;
    let cyp = co * -ry * x1c / rx;
    let cx = cphi * cxp - sphi * cyp + (x1 + x2) / 2.0;
    let cy = sphi * cxp + cphi * cyp + (y1 + y2) / 2.0;
    let theta1 = libm::atan2f((y1c - cyp) / ry, (x1c - cxp) / rx);
    let theta2 = libm::atan2f((-y1c - cyp) / ry, (-x1c - cxp) / rx);
    let mut dtheta = theta2 - theta1;
    if sweep && dtheta < 0.0 {
        dtheta += 2.0 * core::f32::consts::PI;
    }
    if !sweep && dtheta > 0.0 {
        dtheta -= 2.0 * core::f32::consts::PI;
    }
    // 采样步数：弦高容差（细细分，见 TOL_FINE 注）
    let rmax = rx.max(ry);
    let step = (2.0 * libm::acosf((1.0 - TOL_FINE / rmax).clamp(-1.0, 1.0))).max(0.05);
    let n = (libm::ceilf(dtheta.abs() / step) as usize).clamp(2, 512);
    for k in 1..=n {
        let t = theta1 + dtheta * (k as f32) / (n as f32);
        let (st, ct) = (crate::fnum::sin_f32(t), crate::fnum::cos_f32(t));
        // F.6.4 采样点公式（φ=0 时与旧逐点式同值）
        out.push((
            cx + rx * ct * cphi - ry * st * sphi,
            cy + rx * ct * sphi + ry * st * cphi,
        ));
    }
}

/// 点到线段的距离（用于平坦度）。
fn dist_seg(a: (f32, f32), b: (f32, f32), p: (f32, f32)) -> f32 {
    let abx = b.0 - a.0;
    let aby = b.1 - a.1;
    let len2 = abx * abx + aby * aby;
    if len2 <= f32::EPSILON {
        return dist(a, p);
    }
    let t = (((p.0 - a.0) * abx + (p.1 - a.1) * aby) / len2).clamp(0.0, 1.0);
    dist((a.0 + abx * t, a.1 + aby * t), p)
}

fn dist(a: (f32, f32), b: (f32, f32)) -> f32 {
    let dx = a.0 - b.0;
    let dy = a.1 - b.1;
    libm::sqrtf(dx * dx + dy * dy)
}

// ============ 基本形状 → 多边形 ============

fn ellipse_pts(cx: f32, cy: f32, rx: f32, ry: f32, m: Mat2x3) -> PathGeom {
    if rx <= 0.0 || ry <= 0.0 {
        return PathGeom::default();
    }
    // 弦高容差 → 每段圆心角 step；n = 2π/step（细细分，见 TOL_FINE 注；
    // 上限同步 fine_ellipse 的 1024，特大半径不因钳制丢失容差）
    let rmax = rx.max(ry);
    let step = (2.0 * libm::acosf((1.0 - TOL_FINE / rmax).clamp(-1.0, 1.0))).max(0.01);
    let n = (libm::ceilf(2.0 * core::f32::consts::PI / step) as usize).clamp(12, 1024);
    let mut pts = Vec::with_capacity(n + 1);
    for k in 0..=n {
        let t = 2.0 * core::f32::consts::PI * (k % n) as f32 / n as f32;
        let (x, y) = m.apply(
            cx + rx * crate::fnum::cos_f32(t),
            cy + ry * crate::fnum::sin_f32(t),
        );
        pts.push((x, y));
    }
    PathGeom {
        subs: vec![Subpath { pts, closed: true }],
    }
}

/// 圆 → 容差自适应多边形。
pub fn circle(cx: f32, cy: f32, r: f32, m: Mat2x3) -> PathGeom {
    ellipse_pts(cx, cy, r, r, m)
}

pub fn ellipse(cx: f32, cy: f32, rx: f32, ry: f32, m: Mat2x3) -> PathGeom {
    ellipse_pts(cx, cy, rx, ry, m)
}

/// `points` 属性（polyline/polygon 共用）。
///
/// Chrome SVGPointList::Parse 语义（blink svg_point_list.cc 逐行对照 +
/// WPT types/scripted/SVGPointList-error-handling 家族逐行实证，
/// 车道GA 2026-09）：数字流全局奇偶配对，每完整对出一个点；
/// - **任一数字解析失败 → 整条全弃**（SVG2 初始值规则取代 SVG1.1 的
///   合法前缀渲染——首坐标前非法 token / 前导分隔符 / 双逗号都
///   → 0 点，svgwg#764「nothing」侧）；
/// - **奇数尾坐标**：落单数字解析成功但凑不成对 → 静默丢弃、合法
///   前缀保留（"10,10 70,10 40,50 20" → 3 点，两读法一致区）；
/// - 分隔符 = 任意空白 + 至多一个逗号（显式消费；尾随单个逗号容忍
///   ——消费后循环自然退出；双逗号使下一轮数字失败 → 全弃）；
/// - 空 / 全空白 → 0 点。数字扫描语法与 [num_at] 同（指数/符号/
///   "23." 非法），但不做逗号前置跳过（分隔符纪律归本函数）。
pub fn poly_points(s: &str, m: Mat2x3, closed: bool) -> PathGeom {
    let b = s.as_bytes();
    let mut i = 0;
    let mut vals: Vec<f32> = Vec::new();
    while i < b.len() {
        // 前导空白（Blink SkipOptionalSVGSpaces；逗号不是空白——
        // 数字位遇逗号 = 解析失败全弃）；纯空白尾 = 成功结束（空表，
        // Blink `if (!SkipOptionalSVGSpaces(span)) return kNoError` 同构）
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= b.len() {
            break;
        }
        let Some(v) = scan_num(s, b, &mut i) else {
            return PathGeom::default();
        };
        vals.push(v);
        // 数字后分隔符：空白任意 + 至多一个逗号（尾逗号容忍）
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        if i < b.len() && b[i] == b',' {
            i += 1;
            while i < b.len() && b[i].is_ascii_whitespace() {
                i += 1;
            }
        }
    }
    poly_points_flat(&vals, m, closed)
}

/// 平化坐标列（x,y 交错）→ 子路径。`poly_points` 的解析后共享尾段；
/// OQ 车道：animate points 的当帧插值坐标直入（免去 f64→字符串→
/// f32 的逐帧往返）。
pub fn poly_points_flat(vals: &[f32], m: Mat2x3, closed: bool) -> PathGeom {
    let mut pts = Vec::with_capacity(vals.len() / 2 + 1);
    let mut k = 0;
    while k + 1 < vals.len() {
        pts.push(m.apply(vals[k], vals[k + 1]));
        k += 2;
    }
    // 闭合子路径末点补首点：描边的闭合边 quad 与首点 join 依赖它
    // （ellipse_pts 同款约定；填充侧该零长边被 push_edge 跳过，无影响）
    if closed && pts.len() >= 2 {
        let first = pts[0];
        pts.push(first);
    }
    PathGeom {
        subs: vec![Subpath { pts, closed }],
    }
}

// ============ 描边轮廓展开（stroke → 填充多边形族，nonzero 并集） ============

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cap {
    Butt,
    Round,
    Square,
}

pub fn cap_from_str(s: &str) -> Cap {
    match s {
        "round" => Cap::Round,
        "square" => Cap::Square,
        _ => Cap::Butt,
    }
}

/// stroke-linejoin 三态（默认 Miter）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineJoin {
    Miter,
    Round,
    Bevel,
}

pub fn join_from_str(s: &str) -> LineJoin {
    match s {
        "round" => LineJoin::Round,
        "bevel" => LineJoin::Bevel,
        _ => LineJoin::Miter,
    }
}

/// 把平化路径按线宽展开为可填充轮廓。段落四边形 + miter/bevel/round 楔形 +
/// 端帽，全部 nonzero 同向并集。
///
/// `dash` 为 stroke-dasharray 图案（偶数长度，奇数下标 = 空段）；
/// 空切片 = 实线。`dash_offset` 为图案起始相位（正 = 图案沿路径后移，
/// 等效首段缩短；先对图案总长取模）。`join` 为 stroke-linejoin；
/// `miterlimit` 为 stroke-miterlimit（阈值 = hw·miterlimit，即规范比值
/// miter_length/width，超限转 bevel；调用方保证 ≥ 1）。
pub fn stroke_outline(
    g: &PathGeom,
    width: f32,
    cap: Cap,
    dash: &[f32],
    dash_offset: f32,
    join: LineJoin,
    miterlimit: f32,
) -> Vec<Vec<(f32, f32)>> {
    let hw = width / 2.0;
    if hw <= 0.0 {
        return Vec::new();
    }
    let pattern = normalize_dash(dash);
    let mut polys: Vec<Vec<(f32, f32)>> = Vec::new();
    for sub in &g.subs {
        let pts: &[(f32, f32)] = &sub.pts;
        let n = pts.len();
        if n < 2 {
            continue;
        }
        match &pattern {
            None => stroke_solid(&mut polys, sub, hw, cap, join, miterlimit),
            Some(pat) => {
                // 闭合子路径的 dash 行走必须覆盖 z 隐式闭合边：Chrome/Skia
                // 闭环计量含闭合边（与 polyline_length / 实线 stroke_solid
                // 同口径）。缺失时闭合边上的 dash 整段丢墨，且相位差一
                // 闭合边长（be2-pathlength-dash-closed 探针实证：Chrome
                // 左边整条涂满，修复前我方缺失）。显式回起点型（末点==
                // 首点，poly/ellipse 折线）闭合边零长，按开放折线走与
                // 旧行为逐位一致。单调用点：借用切换，免重复调用序列
                // （T1 体积纪律）。
                let extended: Vec<(f32, f32)>;
                let walk_pts: &[(f32, f32)] = if sub.closed && pts[n - 1] != pts[0] {
                    extended = {
                        let mut d = Vec::with_capacity(n + 1);
                        d.extend_from_slice(pts);
                        d.push(pts[0]);
                        d
                    };
                    &extended
                } else {
                    pts
                };
                stroke_dashed(
                    &mut polys,
                    walk_pts,
                    pat,
                    dash_offset,
                    hw,
                    cap,
                    (join, miterlimit),
                );
            }
        }
    }
    polys
}

/// dash 图案归一化：空 / 含负值或非有限值 / 全零（周期 0）→ None（实线）；
/// 奇数长度按 SVG 规范复制一份拼成偶数。
fn normalize_dash(dash: &[f32]) -> Option<Vec<f32>> {
    if dash.is_empty() {
        return None;
    }
    if dash.iter().any(|v| *v < 0.0 || !v.is_finite()) {
        return None; // 负值/NaN/inf 非法 → 按 none
    }
    let mut p = Vec::from(dash);
    if p.len() % 2 == 1 {
        let copy = p.clone();
        p.extend_from_slice(&copy);
    }
    let total: f32 = p.iter().sum();
    if total <= 0.0 || !total.is_finite() {
        return None; // 全 0（无周期）或溢出：退化为实线
    }
    Some(p)
}

/// 实线描边（dash 为空时的现状路径）。
fn stroke_solid(
    polys: &mut Vec<Vec<(f32, f32)>>,
    sub: &Subpath,
    hw: f32,
    cap: Cap,
    join: LineJoin,
    miterlimit: f32,
) {
    let pts: &[(f32, f32)] = &sub.pts;
    let n = pts.len();
    for k in 0..n - 1 {
        seg_quad(polys, pts[k], pts[k + 1], hw);
    }
    // 顶点楔形（闭合路径含首尾接缝，开放路径只处理内部顶点）。
    // 闭合子路径两型：①显式回起点（poly/ellipse 折线末点==首点，段
    // 循环已覆盖闭合边）与退化两点闭合——维持原循环位级不变；
    // ②Z 隐式闭合（path d 无回起点点，段循环漏闭合边）——补描
    // 最后→首的闭合边，顶点楔形覆盖全部 n 顶点（含末顶点），接缝
    // 方向取真实闭合边（Chrome 语义）。
    if sub.closed && n >= 3 && pts[n - 1] != pts[0] {
        seg_quad(polys, pts[n - 1], pts[0], hw);
        for k in 0..n {
            let prev = if k == 0 { n - 1 } else { k - 1 };
            let next = if k + 1 == n { 0 } else { k + 1 };
            join_poly(polys, pts[prev], pts[k], pts[next], hw, join, miterlimit);
        }
    } else if sub.closed {
        for k in 0..n - 1 {
            let prev = if k == 0 { n - 2 } else { k - 1 };
            join_poly(polys, pts[prev], pts[k], pts[k + 1], hw, join, miterlimit);
        }
    } else {
        for k in 1..n - 1 {
            join_poly(polys, pts[k - 1], pts[k], pts[k + 1], hw, join, miterlimit);
        }
        cap_poly(polys, pts[0], pts[1], hw, cap);
        cap_poly(polys, pts[n - 1], pts[n - 2], hw, cap);
    }
}

/// 单段描边四边形（方向退化则跳过）。
fn seg_quad(out: &mut Vec<Vec<(f32, f32)>>, a: (f32, f32), b: (f32, f32), hw: f32) {
    let d = norm(b.0 - a.0, b.1 - a.1);
    if d == (0.0, 0.0) {
        return;
    }
    let (nx, ny) = (-d.1 * hw, d.0 * hw);
    out.push(vec![
        (a.0 + nx, a.1 + ny),
        (b.0 + nx, b.1 + ny),
        (b.0 - nx, b.1 - ny),
        (a.0 - nx, a.1 - ny),
    ]);
}

/// 亮段内单点亮段长度下限（低于此视为零长，跳过）。
const DASH_EPS: f32 = 1e-3;
/// dash 切分迭代上限（防御周期远小于路径长的病态输入）。
const MAX_DASH_ITERS: usize = 100_000;

/// 虚线描边：沿子路径平化点列累积弧长，按图案切亮段；
/// 每个亮段独立走 段四边形 + 内部顶点楔形 + 两端按当前 Cap 处理
/// （Chrome 语义：dash 端点也吃 cap）。零长 on 段两态：全零 on 图案
/// = 点模式（round 画直径 = 线宽的圆点、square 画切向方块、butt 无墨；
/// openmoji 1F4FF 念珠 `0,6.7346,0,0,0,0` + round 即此语义）；混合
/// 图案 = 零长 on 段跳过不画点（Chrome 规则 dasher 一致）。
fn stroke_dashed(
    polys: &mut Vec<Vec<(f32, f32)>>,
    pts: &[(f32, f32)],
    pat: &[f32],
    dash_offset: f32,
    hw: f32,
    cap: Cap,
    // (join, miterlimit) 捆成一个参数：clippy 参数上限 7
    join_miter: (LineJoin, f32),
) {
    let (join, miterlimit) = join_miter;
    let n = pts.len();
    // 累积弧长（点列按开放折线走）。闭合子路径的 z 隐式闭合边由调用方
    // （stroke_outline）以「末点后补首点」形态并入点列——此处一律开放
    // 折线计量，与 polyline_length 的闭合边口径对齐。
    let mut cum = Vec::with_capacity(n);
    cum.push(0.0f32);
    for k in 1..n {
        cum.push(cum[k - 1] + dist(pts[k - 1], pts[k]));
    }
    let total_len = cum[n - 1];
    if total_len <= 0.0 {
        return;
    }
    let period: f32 = pat.iter().sum();
    // Chrome/Skia 两态语义：**全部 on 段（偶数下标）为零**走点模式
    // （Skia asAPoint——零长 on 区间按 cap 画点，openmoji 1F4FF 念珠
    // `0,6.7346,0,0,0,0`）；混合图案（零 on 与非零 on 共存）走规则
    // dasher——零长 on 段不画点（实证：E103 `0 0 3.94 3.94` 旧渲染无点
    // 与 Chrome 逐位一致；点若画在后续 dash 帽外会多墨）。
    let dot_mode = pat.iter().step_by(2).all(|&v| v == 0.0);
    // 相位归一到 [0, period)：正 offset = 图案起点向路径反方向拉（首段缩短）
    let mut phase = dash_offset % period;
    if phase < 0.0 {
        phase += period;
    }
    // 定位初始相位所在的图案段。非零段「段尾 <= 相位」才跳过（相位落在
    // 段边界时归属下一段）；零长段在相位之前才跳过——**恰好落在相位上
    // 的零长亮段要保留**（Chrome/Skia：零长 on-区间在当前弧长处画点，
    // 图案首元素为 0 的点线如 1F4FF 在 s=0 也有点）。
    let mut i = 0usize;
    let mut seg_start = 0.0f32; // pat[i] 的图案内起点
    while i + 1 < pat.len()
        && ((pat[i] > 0.0 && seg_start + pat[i] <= phase) || (pat[i] == 0.0 && seg_start < phase))
    {
        seg_start += pat[i];
        i += 1;
    }
    let mut s = 0.0f32;
    let mut iters = 0usize;
    // 车道NC：dash 端切向的中心差分步长（≈1 弦）。亮段端头的切向若取
    // 所在弦方向（dir_at_len），切边相对真切向歪 ±半步圆心角；该歪斜被
    // 大线宽臂放大（pathlength-circle-mutating：r=100 圆 + width=200，
    // 弦步 2.29°→切边歪 ~1°，r=40 处亮段角宽 14.7° vs 真值 11.25°，
    // 可视带 duty 58% vs Chrome 50%，tol32 14.57%）。中心差分跨邻弦
    // 取向 = 均匀平化的精确切向；直线段差分方向与弦方向恒等 → 首末
    // 四边形逐字节不变（零回归构造性保证）。
    let delta = total_len / (n as f32 - 1.0);
    // 不变量：phase ∈ [seg_start, seg_start+pat[i]]（图案位置空间，
    // 相位回卷与段推进同步——旧实现两者各自回卷，含零段图案上会失配，
    // 零长亮段算出整周期 take 画出实线；靠 pat[i]>0 守卫掩盖）。
    while s < total_len - DASH_EPS && iters < MAX_DASH_ITERS {
        iters += 1;
        let seg_end = seg_start + pat[i];
        let remain = (seg_end - phase).max(0.0);
        let take = remain.min(total_len - s);
        // 偶数下标 = 亮段。非零亮段照常展开；点模式下零长亮段按 cap
        // 画点/方点（butt 不画）；混合模式零长亮段跳过（Chrome 一致）。
        if i % 2 == 0 {
            if take > DASH_EPS {
                let (lit, lit_s) = lit_polyline_s(pts, &cum, s, s + take);
                if lit.len() >= 2 {
                    // 每亮段顶点的真切向（中心差分，车道NC）：端头与曲线路
                    // 段的侧边按真切向放射化——弦平行侧边在 hw≥曲率半径时
                    // 向圆心侧扇出（pathlength-circle-mutating：中心 r<20
                    // 区 duty ~95% vs Chrome ~45%）。
                    let mut ts: Vec<(f32, f32)> = Vec::with_capacity(lit.len());
                    for &sk in &lit_s {
                        ts.push(tangent_at(pts, &cum, sk, total_len, delta));
                    }
                    stroke_lit(polys, &lit, &ts, hw, cap, join, miterlimit);
                }
            } else if dot_mode && matches!(cap, Cap::Round | Cap::Square) {
                let se = s.min(total_len);
                dash_dot_poly(
                    polys,
                    point_at_len(pts, &cum, se),
                    dir_at_len(pts, &cum, se),
                    hw,
                    cap,
                );
            }
        }
        s += take;
        phase += take;
        if phase >= period {
            // 吃满本周期剩余：相位与段游标一起回卷（防失配）
            phase -= period;
            i = 0;
            seg_start = 0.0;
        } else {
            seg_start = seg_end;
            i += 1;
            if i == pat.len() {
                i = 0;
                seg_start = 0.0;
            }
        }
    }
}

/// 差分窗口内顶点折角超过此值视为折角（收缩窗口到该顶点）；之下的
/// 弯折属于平滑曲线细分（TOL_FINE 细分的 r=100 圆每顶点 ~2.3°、r=5
/// ~10°），跨窗平均=真切向。车道OJ 二代刀引入。
const TANGENT_CORNER_BEND_COS: f32 = 0.9; // cos 20°

/// 弧长 `s` 处的**真切向**（对称中心差分 + 折角感知收缩，车道NC 初代
/// / 车道OJ 二代）：亮段顶点法向放射化用（见 stroke_dashed 内注）。
/// 退化（差分零向量）返回 (0,0)，调用方回退弦方向。
///
/// 车道OJ 二代刀：初代实现窗口 = `[s±total/(n-1)]` 仅对路径端点钳制
/// ——粗平化（rect 每弦 200 单位）下窗口跨 90° 折角且不对称（起点侧
/// 钳制把整段差分压到单侧），折角方向混进端切边：rect 周界端切边扇形
/// ±0.025..0.6px 渐进漂移（WPT dash 家族 13 枚 + chart_line 同族边界带
/// 根因）。二代规则：①窗口对 s **弧长对称**（先 `min(delta, s,
/// total-s)`）；②窗口内折角顶点（折角 > 20°）→ 收缩到该顶点再对称
/// ——直弦永远不跨折角，弦内点差分方向与弦向恒等（逐字节不变）；
/// 平滑曲线的细弯折（< 20°）照旧跨弦平均=真切向（车道NC 的 hw≥r
/// 蝶形覆盖依赖对称差分的 ~0° 级切向精度，不可退化为弦向）。
fn tangent_at(pts: &[(f32, f32)], cum: &[f32], s: f32, total_len: f32, delta: f32) -> (f32, f32) {
    let n = cum.len();
    let mut d = delta.min(s).min(total_len - s);
    // 折角收缩：窗口内含折角顶点（弦 j-1→j→j+1 转角 > 20°）则把窗口
    // 收到该顶点处（对称半径 = s 到该顶点的弧长），再检查。每轮 d 严格
    // 递减，8 轮上限覆盖一切现实几何（细曲线窗口最多跨 1-2 顶点）。
    for _ in 0..8 {
        if d <= 0.0 {
            break;
        }
        let (lo, hi) = (s - d, s + d);
        // 定位窗口两端所在弦（cum 严格递增）
        let i0 = cum.partition_point(|&c| c <= lo);
        let i1 = cum.partition_point(|&c| c < hi);
        let mut corner_at: f32 = -1.0;
        for j in i0.max(1)..i1.min(n - 1) {
            // 顶点 j 在窗口 (lo, hi) 内？
            if cum[j] <= lo || cum[j] >= hi {
                continue;
            }
            let u = norm(pts[j].0 - pts[j - 1].0, pts[j].1 - pts[j - 1].1);
            let v = norm(pts[j + 1].0 - pts[j].0, pts[j + 1].1 - pts[j].1);
            let bend_cos = u.0 * v.0 + u.1 * v.1;
            if bend_cos < TANGENT_CORNER_BEND_COS {
                corner_at = cum[j];
                break;
            }
        }
        if corner_at < 0.0 {
            break;
        }
        d = (s - corner_at).abs();
    }
    if d <= 0.0 {
        // s 恰在折角顶点上：取包含弦方向（Skia getPosTan 段内切向同语义）
        let mut k = cum.partition_point(|&c| c <= s);
        k = k.saturating_sub(1);
        if k + 1 >= n {
            k = n - 2;
        }
        return norm(pts[k + 1].0 - pts[k].0, pts[k + 1].1 - pts[k].1);
    }
    let a = point_at_len(pts, cum, s - d);
    let b = point_at_len(pts, cum, s + d);
    norm(b.0 - a.0, b.1 - a.1)
}

/// 提取弧长区间 `[s0, s1]` 覆盖的折线（端点线性插值，中间顶点保留），
/// 同时返回每顶点弧长（车道NC：真切向计算用）。
fn lit_polyline_s(
    pts: &[(f32, f32)],
    cum: &[f32],
    s0: f32,
    s1: f32,
) -> (Vec<(f32, f32)>, Vec<f32>) {
    let mut out: Vec<(f32, f32)> = Vec::new();
    let mut out_s: Vec<f32> = Vec::new();
    for k in 0..pts.len() - 1 {
        let (a, b) = (cum[k], cum[k + 1]);
        if b - a <= DASH_EPS {
            continue; // 零长平化段
        }
        if b <= s0 || a >= s1 {
            continue; // 亮段之外
        }
        let lerp = |t: f32| {
            (
                pts[k].0 + (pts[k + 1].0 - pts[k].0) * t,
                pts[k].1 + (pts[k + 1].1 - pts[k].1) * t,
            )
        };
        if out.is_empty() {
            out.push(lerp(((s0 - a) / (b - a)).clamp(0.0, 1.0)));
            out_s.push(s0);
        }
        let t_end = ((s1 - a) / (b - a)).clamp(0.0, 1.0);
        let p = lerp(t_end);
        let dup = match out.last() {
            Some(&q) => dist(q, p) <= DASH_EPS,
            None => false,
        };
        if !dup {
            out.push(p);
            out_s.push(a + (b - a) * t_end);
        }
    }
    (out, out_s)
}

/// 顶点处描边法向（×hw）：真切向 `t` 与弦向 `d` 实质不平行时用真切向
/// （曲线亮段侧边放射化，车道NC）；平行（直线段）/退化时回退弦向——
/// 与 seg_quad 同式同值，直线亮段逐字节不变。
fn lit_vertex_normal(a: (f32, f32), b: (f32, f32), t: (f32, f32), hw: f32) -> (f32, f32) {
    let d = norm(b.0 - a.0, b.1 - a.1);
    if d == (0.0, 0.0) {
        return (0.0, 0.0);
    }
    let cross = (t.0 * d.1 - t.1 * d.0).abs();
    let dot = t.0 * d.0 + t.1 * d.1;
    // 采纳真切向的窗口：与弦向实质不同（>1e-4，直线段排除→逐字节
    // 不变）、同向、且偏离 <30°（中心差分窗口在少弦路径上会跨过大
    // 拐角——L 形路径 dash 末端差分方向斜 37°，按弦向回退保端切边
    // 垂直局部弦；平滑曲线差分偏离 ≈ 半步圆心角 <10°，正常采纳）。
    let use_t = t != (0.0, 0.0) && cross > 1e-4 && cross <= 0.5 && dot > 0.0;
    let (ux, uy) = if use_t { t } else { d };
    (-uy * hw, ux * hw)
}

/// 一个亮段的描边轮廓：段四边形（侧边按各顶点真切向放射化，车道NC）、
/// 内部顶点楔形、两端均按当前 Cap（dash 端帽语义，Chrome 一致）。
/// 曲线亮段相邻四边形共享同一条放射侧边（精确拼接）；直线亮段全部
/// 法向退化为弦法向（`seg_quad` 同形）。`ts` = 各顶点真切向。
fn stroke_lit(
    polys: &mut Vec<Vec<(f32, f32)>>,
    lit: &[(f32, f32)],
    ts: &[(f32, f32)],
    hw: f32,
    cap: Cap,
    join: LineJoin,
    miterlimit: f32,
) {
    let m = lit.len();
    for k in 0..m - 1 {
        let na = lit_vertex_normal(lit[k], lit[k + 1], ts[k], hw);
        let nb = lit_vertex_normal(lit[k], lit[k + 1], ts[k + 1], hw);
        seg_quad_ends(polys, lit[k], lit[k + 1], hw, na, nb);
    }
    // 顶点楔形只在真拐角发射（车道NC）：相邻四边形共享同一条顶点放射
    // 边（lit_vertex_normal 按顶点求值，两侧一致）已精确拼接——平滑曲线
    // 上 join 楔形是冗余多边形，且 hw≥曲率半径时内交点越过圆心、楔形
    // 三角扇填满中心区（pathlength-circle-mutating 中心 duty ~100% 实
    // 证）。直线段顶点共线（cross=0）跳过的退化楔形本就零面积，像素
    // 恒等；真拐角（cross > sin~10°）保留楔形补 miter 外角。
    for k in 1..m - 1 {
        let dl = norm(lit[k].0 - lit[k - 1].0, lit[k].1 - lit[k - 1].1);
        let dr = norm(lit[k + 1].0 - lit[k].0, lit[k + 1].1 - lit[k].1);
        if (dl.0 * dr.1 - dl.1 * dr.0).abs() > 0.17 {
            join_poly(polys, lit[k - 1], lit[k], lit[k + 1], hw, join, miterlimit);
        }
    }
    cap_poly(polys, lit[0], lit[1], hw, cap);
    cap_poly(polys, lit[m - 1], lit[m - 2], hw, cap);
}

/// 两端法向各自指定的描边四边形（车道NC：亮段首末四边形端切边剪变）。
/// 任一端法向为零向量（退化段）退回 seg_quad 单法向形态。
fn seg_quad_ends(
    out: &mut Vec<Vec<(f32, f32)>>,
    a: (f32, f32),
    b: (f32, f32),
    hw: f32,
    na: (f32, f32),
    nb: (f32, f32),
) {
    if na == (0.0, 0.0) || nb == (0.0, 0.0) {
        seg_quad(out, a, b, hw);
        return;
    }
    if na.0 * nb.0 + na.1 * nb.1 <= 0.0 {
        // 剪变超 90°：回退弦方向（防自交）
        seg_quad(out, a, b, hw);
        return;
    }
    out.push(vec![
        (a.0 + na.0, a.1 + na.1),
        (b.0 + nb.0, b.1 + nb.1),
        (b.0 - nb.0, b.1 - nb.1),
        (a.0 - na.0, a.1 - na.1),
    ]);
}

/// 弧长 `s` 处的折线位置（端点线性插值，退化段跳过；`s` 已 clamp）。
fn point_at_len(pts: &[(f32, f32)], cum: &[f32], s: f32) -> (f32, f32) {
    for k in 0..pts.len() - 1 {
        let (a, b) = (cum[k], cum[k + 1]);
        if b - a <= DASH_EPS {
            continue;
        }
        if s <= b {
            let t = ((s - a) / (b - a)).clamp(0.0, 1.0);
            return (
                pts[k].0 + (pts[k + 1].0 - pts[k].0) * t,
                pts[k].1 + (pts[k + 1].1 - pts[k].1) * t,
            );
        }
    }
    pts[pts.len() - 1]
}

/// 弧长 `s` 处的切向（取所在非退化段的单位方向；全退化回退 (1,0)）。
fn dir_at_len(pts: &[(f32, f32)], cum: &[f32], s: f32) -> (f32, f32) {
    for k in 0..pts.len() - 1 {
        let (a, b) = (cum[k], cum[k + 1]);
        if b - a <= DASH_EPS {
            continue;
        }
        if s <= b {
            return norm(pts[k + 1].0 - pts[k].0, pts[k + 1].1 - pts[k].1);
        }
    }
    (1.0, 0.0)
}

/// 零长亮段的点/方点轮廓：round = 圆心在点位、半径 hw 的 32 边内接圆；
/// square = 边长 2·hw、沿切向的方块。
/// （点画本身走 nonzero 同向并集，与既有帽/楔轮廓同层。）
fn dash_dot_poly(
    out: &mut Vec<Vec<(f32, f32)>>,
    p: (f32, f32),
    dir: (f32, f32),
    hw: f32,
    cap: Cap,
) {
    match cap {
        Cap::Butt => {}
        Cap::Square => {
            if dir == (0.0, 0.0) {
                return;
            }
            let (nx, ny) = (-dir.1 * hw, dir.0 * hw);
            out.push(vec![
                (p.0 + dir.0 * hw + nx, p.1 + dir.1 * hw + ny),
                (p.0 + dir.0 * hw - nx, p.1 + dir.1 * hw - ny),
                (p.0 - dir.0 * hw - nx, p.1 - dir.1 * hw - ny),
                (p.0 - dir.0 * hw + nx, p.1 - dir.1 * hw + ny),
            ]);
        }
        Cap::Round => {
            // 32 边内接圆：相对真圆面积 -0.6%（12 边 -9.7% 会让 1F4FF
            // 尺度的孤立点留下可测的环状欠墨），点数少，顶点成本可忽略。
            const DOT_SEGS: usize = 32;
            let mut poly = Vec::with_capacity(DOT_SEGS);
            for k in 0..DOT_SEGS {
                let t = 2.0 * core::f32::consts::PI * k as f32 / DOT_SEGS as f32;
                poly.push((
                    p.0 + hw * crate::fnum::cos_f32(t),
                    p.1 + hw * crate::fnum::sin_f32(t),
                ));
            }
            out.push(poly);
        }
    }
}

/// round join 圆弧的边数（顶点处外角以 12 边折线近似）。
const JOIN_ARC_SEGS: usize = 12;

/// 顶点楔形。`join` 三态：
/// - Miter：外角延伸到两外侧偏移线交点；交点距超过 `hw·miterlimit`
///   （= 规范比值 miter_length/width）时转 bevel；
/// - Bevel：外侧两偏移点直接相连的四边形；
/// - Round：外角用圆心 = 顶点、半径 hw 的 12 边圆弧替代 miter 尖端，
///   内侧边界与 bevel 一致。
fn join_poly(
    out: &mut Vec<Vec<(f32, f32)>>,
    a: (f32, f32),
    v: (f32, f32),
    b: (f32, f32),
    hw: f32,
    join: LineJoin,
    miterlimit: f32,
) {
    let d0 = norm(v.0 - a.0, v.1 - a.1);
    let d1 = norm(b.0 - v.0, b.1 - v.1);
    if d0 == (0.0, 0.0) || d1 == (0.0, 0.0) {
        return;
    }
    let cross = d0.0 * d1.1 - d0.1 * d1.0;
    if cross.abs() < 1e-6 {
        return; // 共线
    }
    // 外侧判定：转向内侧 = 转弯中心一侧 = +n（cross>0 时）；故外侧 = −sign(cross)·n。
    // （修复：原先取 +sign(cross)，把 miter 尖端/圆弧放到内侧带重叠区，
    // 外角被削平——miterlimit 与 round join 在视觉上失效。）
    let sgn = if cross > 0.0 { -1.0 } else { 1.0 };
    let n0 = (-d0.1 * hw, d0.0 * hw);
    let n1 = (-d1.1 * hw, d1.0 * hw);
    let p_out0 = (v.0 + sgn * n0.0, v.1 + sgn * n0.1);
    let p_out1 = (v.0 + sgn * n1.0, v.1 + sgn * n1.1);
    let inner0 = (v.0 - sgn * n0.0, v.1 - sgn * n0.1);
    let inner1 = (v.0 - sgn * n1.0, v.1 - sgn * n1.1);
    match join {
        LineJoin::Bevel => out.push(vec![p_out0, p_out1, inner1, inner0]),
        LineJoin::Round => {
            // p_out0/p_out1 恰在以 v 为圆心、半径 hw 的圆上；沿短弧（过
            // 外角平分线方向 = normalize(sgn·n0 + sgn·n1)，即 miter 尖端方向）
            // 采 JOIN_ARC_SEGS 段折线替代楔形尖端
            let u0 = (p_out0.0 - v.0, p_out0.1 - v.1);
            let u1 = (p_out1.0 - v.0, p_out1.1 - v.1);
            let phi = libm::atan2f(u0.0 * u1.1 - u0.1 * u1.0, u0.0 * u1.0 + u0.1 * u1.1);
            let a0 = libm::atan2f(u0.1, u0.0);
            let mut poly = Vec::with_capacity(JOIN_ARC_SEGS + 4);
            poly.push(p_out0);
            for k in 1..JOIN_ARC_SEGS {
                let t = a0 + phi * (k as f32) / (JOIN_ARC_SEGS as f32);
                poly.push((
                    v.0 + hw * crate::fnum::cos_f32(t),
                    v.1 + hw * crate::fnum::sin_f32(t),
                ));
            }
            poly.push(p_out1);
            poly.push(inner1);
            poly.push(inner0);
            out.push(poly);
        }
        LineJoin::Miter => {
            let miter = line_intersect(
                p_out0,
                (p_out0.0 + d0.0, p_out0.1 + d0.1),
                p_out1,
                (p_out1.0 + d1.0, p_out1.1 + d1.1),
            );
            match miter {
                Some(mp) if dist(mp, v) <= hw * miterlimit => {
                    out.push(vec![p_out0, mp, p_out1, inner1, inner0])
                }
                _ => out.push(vec![p_out0, p_out1, inner1, inner0]), // bevel（超 miterlimit 或平行）
            }
        }
    }
}

fn cap_poly(out: &mut Vec<Vec<(f32, f32)>>, end: (f32, f32), inner: (f32, f32), hw: f32, cap: Cap) {
    let d = norm(end.0 - inner.0, end.1 - inner.1);
    if d == (0.0, 0.0) {
        return;
    }
    match cap {
        Cap::Butt => {}
        Cap::Square => {
            let (nx, ny) = (-d.1 * hw, d.0 * hw);
            let ext = (end.0 + d.0 * hw, end.1 + d.1 * hw);
            out.push(vec![
                (end.0 + nx, end.1 + ny),
                (ext.0 + nx, ext.1 + ny),
                (ext.0 - nx, ext.1 - ny),
                (end.0 - nx, end.1 - ny),
            ]);
        }
        Cap::Round => {
            let n = 10;
            let (nx, ny) = (-d.1, d.0);
            let mut poly = Vec::with_capacity(n + 1);
            for k in 0..=n {
                // 从 +normal 经正前方（end + d·hw）扫到 −normal 的半圆
                // ——鼓包沿 d（端帽**向前**），弦 = 端点处的 butt 截线
                //（车道AE：旧参数化 forward→normal→backward 扫成「法向
                // 侧半圆」——帽向侧面鼓 + 半格缩进，Tabler 根继承 round
                // cap 后首次暴露；单测 pill 探针逐像素钉死）。
                let t = core::f32::consts::PI * k as f32 / n as f32;
                let (s, c) = (crate::fnum::sin_f32(t), crate::fnum::cos_f32(t));
                poly.push((
                    end.0 + nx * c * hw + d.0 * s * hw,
                    end.1 + ny * c * hw + d.1 * s * hw,
                ));
            }
            out.push(poly);
        }
    }
}

fn norm(x: f32, y: f32) -> (f32, f32) {
    let l = libm::sqrtf(x * x + y * y);
    if l <= f32::EPSILON {
        (0.0, 0.0)
    } else {
        (x / l, y / l)
    }
}

fn line_intersect(
    p1: (f32, f32),
    p2: (f32, f32),
    p3: (f32, f32),
    p4: (f32, f32),
) -> Option<(f32, f32)> {
    let d1 = (p2.0 - p1.0, p2.1 - p1.1);
    let d2 = (p4.0 - p3.0, p4.1 - p3.1);
    let den = d1.0 * d2.1 - d1.1 * d2.0;
    if den.abs() < 1e-9 {
        return None;
    }
    let t = ((p3.0 - p1.0) * d2.1 - (p3.1 - p1.1) * d2.0) / den;
    Some((p1.0 + d1.0 * t, p1.1 + d1.1 * t))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 车道NC 缺口②回归：circle × 大 pathLength × dasharray（WPT
    /// pathlength-circle-mutating 静基态，Chrome tol32 14.57%→1.06%）。
    /// 修复点：dash 亮段四边形侧边按顶点**真切向**放射化 + join 楔形
    /// 仅真拐角发射。修复前弦平行侧边/楔形在 hw≥曲率半径时向圆心扇
    /// 出——中心区 duty ~100%（Chrome ~45%），全图 duty 58% vs 50%。
    /// 本测试在纯几何层钉死两件事：
    /// ① 亮段端切边放射（同一 r 带切边角恒定，弦平行旧实现歪 ~1°）；
    /// ② 间隙角/中心区不被任何亮段轮廓覆盖（duty 不随半径膨胀）。
    #[test]
    fn dash_radial_cuts_on_circle() {
        let m = Mat2x3::IDENTITY;
        let sg = circle(100.0, 50.0, 100.0, m);
        let actual = polyline_length(&sg);
        let k = actual / 6400.0;
        let pat = [200.0 * k, 200.0 * k];
        let polys = stroke_outline(&sg, 200.0, Cap::Butt, &pat, 0.0, LineJoin::Miter, 4.0);
        let inside = |x: f32, y: f32| -> bool {
            for poly in &polys {
                let n = poly.len();
                let (mut inside, mut j) = (false, n - 1);
                for i in 0..n {
                    let (a, b) = (poly[i], poly[j]);
                    if ((a.1 > y) != (b.1 > y)) && (x < (b.0 - a.0) * (y - a.1) / (b.1 - a.1) + a.0)
                    {
                        inside = !inside;
                    }
                    j = i;
                }
                if inside {
                    return true;
                }
            }
            false
        };
        let pt = |deg: f64, r: f64| -> (f32, f32) {
            let t = deg as f32 / 180.0 * core::f32::consts::PI;
            (
                100.0 + r as f32 * libm::cosf(t),
                50.0 + r as f32 * libm::sinf(t),
            )
        };
        // ① 亮段端切边放射：dash1 = [0°,11.25°]（k≈0.0982 归一化，
        // 理论网格 360°/32）。端切边角在 r=40/100/160 三带一致。
        // （切边 = 亮段 0 的起始边；用间隙中心角验证②亦可独立钉回归。）
        // ② 间隙角 16.875°（dash1/gap1 中线）在全部半径带都不该有墨；
        //    亮段角 5.625°（dash1 中线）在全部半径带都该有墨。
        //    修复前：r≤12 全绿（扇出）、间隙在 r<40 被侧边扇出吞没。
        for r in [5.0f64, 12.0, 25.0, 40.0, 60.0, 90.0, 130.0, 180.0] {
            let (gx, gy) = pt(16.875, r);
            assert!(
                !inside(gx, gy),
                "间隙角 16.875° 在 r={r} 处不应有墨（中心扇出回归）"
            );
            let (lx, ly) = pt(5.625, r);
            assert!(inside(lx, ly), "亮段角 5.625° 在 r={r} 处应有墨");
            let (gx2, gy2) = pt(39.375, r);
            assert!(!inside(gx2, gy2), "间隙角 39.375° 在 r={r} 处不应有墨");
        }
    }

    /// 车道OJ 二代刀回归（WPT dash 家族 13 枚漂移根因）：tangent_at
    /// 差分窗必须①对 s 弧长对称、②永不跨 >20° 折角顶点。粗平化矩形
    /// （每弦 200 单位）上，初代不对称窗（`[s±delta]` 仅端点钳制）把
    /// 折角方向混进端切边——亮段端切边渐进漂移 ±0.025..0.6px。
    #[test]
    fn tangent_at_window_symmetric_never_crosses_corner() {
        // 单位方环 ×translation(20,20)×scaling(200,200)：4 弦各 200，折角 90°
        let pts = [
            (20.0f32, 20.0f32),
            (220.0, 20.0),
            (220.0, 220.0),
            (20.0, 220.0),
            (20.0, 20.0),
        ];
        let mut cum = vec![0.0f32];
        for k in 1..pts.len() {
            cum.push(cum[k - 1] + dist(pts[k - 1], pts[k]));
        }
        let total = cum[4]; // 800
        let delta = total / 4.0; // 200
                                 // ① 弦内点（s=100，离折角 100）：对称窗 [0,200] 不含折角
                                 // （顶点恰在窗缘 → 跳过）→ 差分方向 = 弦方向，逐位精确。
                                 // 初代窗 [0,300] 跨 90° 折角 → 方向歪 ~18°。
        let t = tangent_at(&pts, &cum, 100.0, total, delta);
        assert!(
            (t.0 - 1.0).abs() < 1e-6 && t.1.abs() < 1e-6,
            "tangent(100) = {t:?}"
        );
        // ② 折角旁内点（s=199，离折角 1）：窗收缩到 [198,200] ⊂ 弦内 →
        // 仍 = 弦方向。初代窗 [0,398] → 方向混入下行弦 (~0.71,0.71)。
        let t = tangent_at(&pts, &cum, 199.0, total, delta);
        assert!(
            (t.0 - 1.0).abs() < 1e-6 && t.1.abs() < 1e-6,
            "tangent(199) = {t:?}"
        );
        // ③ s 恰在折角顶点（s=200）：包含弦方向（出弦 1→2 = (0,1)，
        // Skia getPosTan 段内切向同语义）。
        let t = tangent_at(&pts, &cum, 200.0, total, delta);
        assert!(
            t.0.abs() < 1e-6 && (t.1 - 1.0).abs() < 1e-6,
            "tangent(200) = {t:?}"
        );
    }

    /// 车道OJ：粗平化矩形 dash 亮段端切边钉在相位网格上（相位 f32 行走
    /// 精确 + 端切边垂直于路径 = 网格位置零漂移；WPT dash 家族 13 枚 +
    /// chart_line 同族边界带的几何级回归锚）。
    #[test]
    fn dash_lit_edges_on_grid_coarse_rect() {
        let m = Mat2x3::IDENTITY
            .mul(&Mat2x3::translation(20.0, 20.0))
            .mul(&Mat2x3::scaling(200.0, 200.0));
        let sg = poly_points("0,0 1,0 1,1 0,1", m, true);
        let actual = polyline_length(&sg); // 800
        let k = actual / 100.0; // pathLength=100 → 缩放 8
        let dash = [0.25 * k, 0.25 * k]; // → [2,2]，周期 4
        let polys = stroke_outline(&sg, 5.0, Cap::Butt, &dash, 0.0, LineJoin::Miter, 4.0);
        // 顶边（y≈20 带）所有轮廓竖直边 x：亮/暗段端切边必须落在
        // 20+2n 网格（左竖边描边侧墙 17.5/22.5 的横帽边也过带）。
        let mut xs: Vec<f32> = Vec::new();
        for poly in &polys {
            for &(x, y) in poly {
                if (y - 20.0).abs() < 3.01 && x > 15.0 && x < 90.0 {
                    xs.push(x);
                }
            }
        }
        xs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(core::cmp::Ordering::Equal));
        xs.dedup_by(|a, b| (*a - *b).abs() < 1e-4);
        let on_grid = |x: f32| {
            let r = (x - 20.0) / 2.0;
            (r - r.round()).abs() < 1e-3
        };
        let grid: Vec<f32> = xs
            .iter()
            .copied()
            .filter(|&x| on_grid(x) || (x - 17.5).abs() < 1e-3 || (x - 22.5).abs() < 1e-3)
            .collect();
        assert_eq!(
            grid.len(),
            xs.len(),
            "端切边全部钉在网格上（漂移即旧不对称窗回归）：xs={xs:?}"
        );
        assert!(
            (0..12).all(|n| xs
                .iter()
                .any(|&x| (x - (20.0 + 2.0 * n as f32)).abs() < 1e-3)),
            "前 12 个相位边界 20+2n 必须齐全：xs={xs:?}"
        );
    }

    #[test]
    fn z_close_segment_stroked() {
        // 射线法点在多边形内判定（测试辅助）。
        fn pip(p: (f32, f32), poly: &[(f32, f32)]) -> bool {
            let mut inside = false;
            let n = poly.len();
            for i in 0..n {
                let (a, b) = (poly[i], poly[(i + 1) % n]);
                if (a.1 > p.1) != (b.1 > p.1) && p.0 < (b.0 - a.0) * (p.1 - a.1) / (b.1 - a.1) + a.0
                {
                    inside = !inside;
                }
            }
            inside
        }
        // Z 隐式闭合的 path d：描边轮廓必须包含最后→首的闭合边
        //（filters-example-01-b 的 stadium 外描边漏底边的教训）。
        let g = flatten_d("M 0 0 L 100 0 L 50 80 Z", Mat2x3::IDENTITY);
        let polys = stroke_outline(&g, 10.0, Cap::Butt, &[], 0.0, LineJoin::Miter, 4.0);
        // 闭合边 (50,80)→(0,0)：中点 (25,40) 距两侧 ≥ 半宽（hw=5），
        // 必被某四边形覆盖；漏闭合边时该点距最近段（左侧斜边）>5。
        assert!(polys.iter().any(|p| pip((25.0, 40.0), p)));
        // 显式回起点的闭合折线（polygon 语义）：末点==首点，行为不变
        let g2 = flatten_d("M 0 0 L 100 0 L 50 80 L 0 0", Mat2x3::IDENTITY);
        let polys2 = stroke_outline(&g2, 10.0, Cap::Butt, &[], 0.0, LineJoin::Miter, 4.0);
        assert!(polys2.iter().any(|p| pip((25.0, 40.0), p)));
    }

    #[test]
    fn parse_and_flatten_basic() {
        let g = flatten_d("M 10 10 H 60 V 40 L 35 58 Z", Mat2x3::IDENTITY);
        assert_eq!(g.subs.len(), 1);
        assert!(g.subs[0].closed);
        let pts = &g.subs[0].pts;
        assert_eq!(pts[0], (10.0, 10.0));
        assert_eq!(pts[1], (60.0, 10.0));
        assert_eq!(pts[2], (60.0, 40.0));
        assert_eq!(pts[3], (35.0, 58.0));
        // Z 隐式闭合：closed 标记由填充阶段的闭合边补齐，无显式回起点点
        assert_eq!(pts.len(), 4);
    }

    #[test]
    fn relative_commands() {
        let g = flatten_d("m 10 10 l 30 0 l -15 -28 z", Mat2x3::IDENTITY);
        let pts = &g.subs[0].pts;
        assert_eq!(pts[0], (10.0, 10.0));
        assert_eq!(pts[1], (40.0, 10.0));
        assert_eq!(pts[2], (25.0, -18.0));
    }

    /// H/V 在带变换时必须保持"用户空间水平/垂直"语义。
    #[test]
    fn hv_respect_user_space_under_scale() {
        let g = flatten_d("M 0 0 H 10 V 5", Mat2x3::scaling(2.0, 4.0));
        let pts = &g.subs[0].pts;
        assert_eq!(pts[0], (0.0, 0.0));
        assert_eq!(pts[1], (20.0, 0.0)); // H：x*2，y 不动
        assert_eq!(pts[2], (20.0, 20.0)); // V：y*4
    }

    #[test]
    fn cubic_flattens_to_many_points() {
        let g = flatten_d("M 0 0 C 0 50 100 50 100 0", Mat2x3::IDENTITY);
        let pts = &g.subs[0].pts;
        assert!(pts.len() > 4, "曲线应平化为多点，got {}", pts.len());
        assert_eq!(pts[0], (0.0, 0.0));
        let last = pts[pts.len() - 1];
        assert_eq!(last, (100.0, 0.0));
    }

    #[test]
    fn arc_small_and_large() {
        let small = flatten_d("M 0 50 A 30 30 0 0 1 45 50", Mat2x3::IDENTITY);
        let large = flatten_d("M 0 50 A 30 30 0 1 0 45 50", Mat2x3::IDENTITY);
        let sp = &small.subs[0].pts;
        let lp = &large.subs[0].pts;
        let end_s = sp[sp.len() - 1];
        let end_l = lp[lp.len() - 1];
        assert!(
            (end_s.0 - 45.0).abs() < 0.01 && (end_s.1 - 50.0).abs() < 0.01,
            "{end_s:?}"
        );
        assert!(
            (end_l.0 - 45.0).abs() < 0.01 && (end_l.1 - 50.0).abs() < 0.01,
            "{end_l:?}"
        );
        let smin = sp.iter().map(|p| p.1).fold(f32::MAX, f32::min);
        let lmax = lp.iter().map(|p| p.1).fold(f32::MIN, f32::max);
        assert!(smin < 50.0, "小弧上拱");
        assert!(lmax > 50.0, "大弧下扣");
    }

    /// arc flag 连写全形态（图标集 429 枚空白根因，wt/icon-mass 复现
    /// r2-arc-concat-flags.svg）：flag 语法 `flag ::= "0" | "1"` 允许与
    /// 后续参数无分隔连写，必须单字符消费——数字贪婪扫描会把 `11` 当
    /// 一个数、`-.01` 被错位当 flag/坐标，整条 d 解析失败静默丢 path。
    #[test]
    fn arc_flag_concat_forms() {
        // 全连写（r2 实锤形态）≡ 全空格分隔：几何必须逐命令一致
        let concat = parse_d("M12 1a11 11 0 11-.01 0z").expect("连写形态应解析成功");
        let spaced = parse_d("M12 1a11 11 0 1 1 -0.01 0z").expect("空格形态应解析成功");
        assert_eq!(concat, spaced, "连写与空格分隔应解析出相同命令列");
        assert_eq!(
            concat[..2],
            [
                Cmd::M(12.0, 1.0),
                Cmd::A(11.0, 11.0, 0.0, true, true, 11.99, 1.0)
            ],
            "laf=1 sf=1 相对终点 (12-0.01, 1)"
        );
        // 半连写：laf 空格分隔、sf 与 x 连写（`0 1-.01` = laf=0 sf=1 x=-0.01；
        // 注意 `0 11-.01 0` 会被解析为 x=1 y=-0.01 + 残组 → 与 Chrome 一致地
        // 因 ry 缺失整条失败，非本 bug 形态）
        let half = parse_d("M0 0a11 11 0 0 1-.01 0").expect("半连写应解析成功");
        let half_spaced = parse_d("M0 0a11 11 0 0 1 -0.01 0").expect("对照应解析成功");
        assert_eq!(half, half_spaced);
        // 小写相对 a + 连写：终点相对游标
        let rel = parse_d("M2 3a5 5 0 015 5").expect("小写连写应解析成功");
        assert_eq!(
            rel[1],
            Cmd::A(5.0, 5.0, 0.0, false, true, 7.0, 8.0),
            "laf=0 sf=1 相对终点 (2+5, 3+5)"
        );
        // 大写 A 同样支持连写（语法不区分大小写）
        let abs = parse_d("M0 0A5 5 0 015 5").expect("大写连写应解析成功");
        assert_eq!(abs[1], Cmd::A(5.0, 5.0, 0.0, false, true, 5.0, 5.0));
        // 多重连写：隐式命令重复两组 arc（每组 7 参），每组都带连写 flag
        let multi = parse_d("M0 0a5 5 0 115 5 5 5 0 115 5").expect("多重连写应解析成功");
        assert_eq!(
            multi,
            parse_d("M0 0a5 5 0 1 1 5 5 5 5 0 1 1 5 5").expect("对照应解析成功"),
            "两组连写 arc 应与空格形式一致"
        );
        // 真·非法 flag（非 0/1）仍须失败：flag 位出现 '2'
        assert_eq!(parse_d("M0 0a5 5 0 215 5"), None, "laf=2 非法");
        // 缺参仍须失败（rot 后直接坐标，flag 位置缺失）
        assert_eq!(parse_d("M0 0a5 5 0 5 5"), None, "flag 缺失应失败");
    }

    /// Z 闭合边必须计入总长（WPT lineto-commands/moveto-absolute：
    /// `M 0,0 L 50,0 L 50,50 Z` = 150 边 + 50 闭合边 = 200）。显式回
    /// 起点型（poly/ellipse 折线末点==首点）不重复计——判定以末点≠首点
    /// 为准。同根因影响 pathLength 归一化（builder 按 polyline_length
    /// 缩放 dash 图案）。
    #[test]
    fn polyline_length_includes_z_closing_edge() {
        // WPT lineto-commands 原题：50 边正方形 Z 周长 200
        let g = flatten_d("M 50,50 L 100,50 L 100,100 L 50,100 Z", Mat2x3::IDENTITY);
        assert!((polyline_length(&g) - 200.0).abs() < 1e-4, "Z 周长 200");
        // 三角形闭合边（斜边 50√2）
        let gt = flatten_d("M 0,0 L 50,0 L 50,50 Z", Mat2x3::IDENTITY);
        assert!(
            (polyline_length(&gt) - (100.0 + 50.0 * core::f32::consts::SQRT_2)).abs() < 1e-4,
            "三角 Z 周长"
        );
        // 显式回起点：末点==首点，闭合边不重复计
        let g2 = flatten_d("M 0,0 L 50,0 L 50,50 L 0,0", Mat2x3::IDENTITY);
        assert!(
            (polyline_length(&g2) - (100.0 + 50.0 * core::f32::consts::SQRT_2)).abs() < 1e-4,
            "显式闭合"
        );
        // polygon（poly_points 末点补首点约定）同口径不重复计
        let g3 = poly_points("0,0 50,0 50,50", Mat2x3::IDENTITY, true);
        assert!(
            (polyline_length(&g3) - (100.0 + 50.0 * core::f32::consts::SQRT_2)).abs() < 1e-4,
            "polygon 周长"
        );
        // 开放子路径不受影响
        let g4 = flatten_d("M 0,0 L 50,0 L 50,50", Mat2x3::IDENTITY);
        assert!((polyline_length(&g4) - 100.0).abs() < 1e-4, "开放 100");
        // getTotalLength-01 考点：M 0 0 t 0 100（T 无前驱 Q，控制点=当前
        // 点退化为直线）总长恰 100
        let g5 = flatten_d("M 0 0 t 0 100", Mat2x3::IDENTITY);
        assert!(
            (polyline_length(&g5) - 100.0).abs() < 1e-4,
            "T 退化直线 100"
        );
    }

    /// 闭合子路径的 dash 行走必须覆盖 z 隐式闭合边（be2 探针 Chrome
    /// 实证：闭环 dash 计量含闭合边，与 [`polyline_length`] 同口径；
    /// 修复前行走止于最后显式点，闭合边整段丢墨且相位差一边长）。
    /// 直角三角两直角边显式、斜边是 z 闭合边——dash "50 50" 下斜边
    /// 弧长区间 [200, 341.4] 有亮段 [200,250] [300,341.4]，墨迹落在
    /// 三角形严格内部（两条直角边的描边只在 x≥95 或 y≤105 带）。
    #[test]
    fn stroke_dashed_closed_walks_z_closing_edge() {
        let g = flatten_d("M 0 100 L 100 100 L 100 200 z", Mat2x3::IDENTITY);
        let polys = stroke_outline(
            &g,
            10.0,
            Cap::Butt,
            &[50.0, 50.0],
            0.0,
            LineJoin::Miter,
            4.0,
        );
        let interior = polys
            .iter()
            .flat_map(|p| p.iter())
            .filter(|p| p.0 > 5.0 && p.0 < 95.0 && p.1 > 105.0 && p.1 < 195.0)
            .count();
        assert!(interior > 0, "斜边（z 闭合边）上必须有亮段墨迹");
        // 显式回起点型（末点==首点）不补零长段——同三角形改显式闭合，
        // 行走结果与 z 版逐点一致（闭合边已在点列里）。
        let g2 = flatten_d("M 0 100 L 100 100 L 100 200 L 0 100 z", Mat2x3::IDENTITY);
        let polys2 = stroke_outline(
            &g2,
            10.0,
            Cap::Butt,
            &[50.0, 50.0],
            0.0,
            LineJoin::Miter,
            4.0,
        );
        let interior2 = polys2
            .iter()
            .flat_map(|p| p.iter())
            .filter(|p| p.0 > 5.0 && p.0 < 95.0 && p.1 > 105.0 && p.1 < 195.0)
            .count();
        assert_eq!(interior, interior2, "显式闭合与 z 闭合行走等价");
    }

    /// 尾随小数点（"23."）BNF 非法——Chrome 在该处截断
    /// （WPT number-error-trailing-decimal）。
    #[test]
    fn num_at_rejects_trailing_decimal() {
        // "23." 非法：`L 23.,100` 整组丢弃，渲染到 M L 为止
        let g = flatten_d("M 10,10 L 50,50 L 23.,100", Mat2x3::IDENTITY);
        assert!(
            (polyline_length(&g) - (40.0f32 * core::f32::consts::SQRT_2)).abs() < 1e-3,
            "前缀 = 对角线长"
        );
        // 首坐标尾随点：整条 L 丢弃（总长 0，Chrome 同口径）
        let g2 = flatten_d("M 0,0 L 15. 20", Mat2x3::IDENTITY);
        assert!(polyline_length(&g2) < 1e-6, "L 整组丢弃");
        // 合法小数形态不受影响（分数点连写/前导点均合法）
        assert_eq!(parse_d("M.5.5L1.25 2.5"), parse_d("M 0.5 0.5 L 1.25 2.5"));
        assert!(parse_d_recover("M 0 0 L .5 1").1, "'.5' 合法");
        assert!(parse_d_recover("M 0 0 L 1.5e2 3").1, "指数合法");
    }

    /// WPT error-handling：非法 token 后 Chrome 前缀恢复——渲染到最后
    /// 合法段为止，不是整条丢弃（修复前 parse_d 整体拒识 → 长度 0）。
    #[test]
    fn parse_d_error_prefix_recovery() {
        // 非法命令字母 X：截断到 M L；总长 = 对角线
        let (cmds, ok) = parse_d_recover("M 10,10 L 50,50 X 100,100");
        assert!(!ok, "发生了截断");
        assert_eq!(cmds, vec![Cmd::M(10.0, 10.0), Cmd::L(50.0, 50.0)]);
        let g = flatten_d("M 10,10 L 50,50 X 100,100", Mat2x3::IDENTITY);
        assert!(
            (polyline_length(&g) - (40.0f32 * core::f32::consts::SQRT_2)).abs() < 1e-3,
            "非法命令后长度 = 前缀"
        );
        // 不完整坐标对（L 100）：该 L 整组丢弃，已成功命令保留
        let (cmds, _) = parse_d_recover("M 10,60 L 50,60 L 100");
        assert_eq!(cmds.len(), 2);
        // 奇数个坐标：完整的隐式重复组保留
        let (cmds, _) = parse_d_recover("M 10,110 L 50,110 60,110 70");
        assert_eq!(cmds.len(), 3, "M + 两组完整 L");
        // 不完整 curveto（4/6 参数）：C 整组丢弃
        let (cmds, _) = parse_d_recover("M 10,160 L 50,160 C 60,150 70,170");
        assert_eq!(cmds.len(), 2);
        // 第二个 C 不完整：第一个 C 保留
        let (cmds, _) = parse_d_recover("M 0,0 L 50,50 C 60,40 70,60 80,50 C 90,40 100,60");
        assert_eq!(cmds.len(), 3, "M L C(完整)");
        // 非法 arc flag：A 丢弃
        let (cmds, _) = parse_d_recover("M 10,210 L 50,210 A 25,25 0 2,1 100,210");
        assert_eq!(cmds.len(), 2);
        // 错误后的合法命令不渲染（首个错误处停）
        let (cmds, _) = parse_d_recover("M 10,360 L 50,360 L 60 L 100,360");
        assert_eq!(cmds.len(), 2);
        // 多处错误停第一处
        let (cmds, _) = parse_d_recover("M 10,310 L 50,310 X 60,310 Y 70,310");
        assert_eq!(cmds.len(), 2);
        // 完整合法子路径 + 尾部错误：合法部分全保留（含 Z）
        let (cmds, _) = parse_d_recover("M 100,100 L 150,100 L 150,150 L 100,150 Z M 200.,200.");
        assert_eq!(cmds.len(), 5, "方形 M L L L Z 全保留");
        // 严格口径不变：任一错误 → None（动画 d 帧等「全对才有效」用）
        assert_eq!(parse_d("M 0 0 L 10 X 2"), None);
        assert_eq!(
            parse_d("M 0 0 L 10 10"),
            Some(vec![Cmd::M(0.0, 0.0), Cmd::L(10.0, 10.0)])
        );
    }

    /// WPT arc-commands：负半径取绝对值后按正常弧绘制（Chrome 宽容
    /// 口径；SVG2 视负值为 error，但 Chrome/Skia 取 |rx| |ry|——
    /// 引擎对齐 Chrome）。修复前 rx≤0 直线化。
    #[test]
    fn arc_negative_radius_uses_abs() {
        let neg = flatten_d("M 100,300 A -50,-50 0 0,1 200,300", Mat2x3::IDENTITY);
        let pos = flatten_d("M 100,300 A 50,50 0 0,1 200,300", Mat2x3::IDENTITY);
        assert_eq!(neg, pos, "负半径与正半径几何完全一致");
        // 弧长 > 弦长（直线化时恰等于弦长 100）
        let lens: f32 = neg.subs[0].pts.windows(2).map(|w| dist(w[0], w[1])).sum();
        assert!(lens > 140.0, "半圆弧长 ≈ π·50 ≈ 157.08, got {lens}");
        // 零半径仍直线化（规范规定）
        let zero = flatten_d("M 0,0 A 0,10 0 0,1 10,0", Mat2x3::IDENTITY);
        assert_eq!(zero.subs[0].pts.len(), 2, "零 rx 直线化");
    }

    /// 车道AE C2：镜像（负行列式）下弧的手性反转——用户空间弧
    /// P1→P2 sweep=1 经 x 镜像后 = P1'→P2' **sweep=0**（openmoji 轮椅
    /// 家族 scale(-1,1) 圆顶变碗形的根因：此前在设备空间用用户空间
    /// 旗标做中心参数化）。旋转 180°（正行列式）sweep 不变。
    /// 修法 = 用户空间平化 + 逐点仿射映射，两等价式逐字节成立。
    #[test]
    fn mirrored_arc_flips_sweep_rotation_preserves() {
        let d = "M2 4 A8 8 0 0 1 10 12";
        // x 镜像 (x,y)→(12−x,y)：a=−1, d=1, e=12
        let mirror = Mat2x3 {
            a: -1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            e: 12.0,
            f: 0.0,
        };
        let mirrored = flatten_d(d, mirror);
        // 参照：端点镜像 + sweep 翻转（large 不变）。镜像路径多一层
        // 矩阵运算，逐字节不恒等——按几何等价（<1e-4）断言；修前该弧
        // 整体翻到错误一侧（碗形），差异 O(1) 量级。
        let reference = flatten_d("M10 4 A8 8 0 0 0 2 12", Mat2x3::IDENTITY);
        let (mp, rp) = (&mirrored.subs[0].pts, &reference.subs[0].pts);
        assert_eq!(mp.len(), rp.len(), "采样数一致");
        for (a, b) in mp.iter().zip(rp.iter()) {
            assert!(
                (a.0 - b.0).abs() < 1e-4 && (a.1 - b.1).abs() < 1e-4,
                "镜像弧手性反转（sweep 0↔1）：{a:?} vs {b:?}"
            );
        }
        // 旋转 180° (x,y)→(12−x,16−y)：a=d=−1, e=12, f=16
        let rot = Mat2x3 {
            a: -1.0,
            b: 0.0,
            c: 0.0,
            d: -1.0,
            e: 12.0,
            f: 16.0,
        };
        let rotated = flatten_d(d, rot);
        // 参照：端点旋转、sweep 保持（正行列式不改手性）；同为矩阵
        // 路径，按几何等价断言
        let ref_rot = flatten_d("M10 12 A8 8 0 0 1 2 4", Mat2x3::IDENTITY);
        let (tp, rp) = (&rotated.subs[0].pts, &ref_rot.subs[0].pts);
        assert_eq!(tp.len(), rp.len(), "采样数一致");
        for (a, b) in tp.iter().zip(rp.iter()) {
            assert!(
                (a.0 - b.0).abs() < 1e-4 && (a.1 - b.1).abs() < 1e-4,
                "旋转 180° 弧 sweep 不变：{a:?} vs {b:?}"
            );
        }
        // 均匀正缩放：scale(2,2) 弧 = 圆心 (2,12)→(4,24)、半径 8→16 的
        // 同一段圆弧（此前靠 F.6.6 半径放大近似等价，用户空间平化后
        // 精确）。采样步长按用户空间半径取，缩放前后点数不同——按
        // 圆心距 + 鼓包方向断言（镜像 bug 形态下弧翻到错误一侧）。
        let scaled = flatten_d(d, Mat2x3::scaling(2.0, 2.0));
        let sp = &scaled.subs[0].pts;
        assert_eq!(sp[0], (4.0, 8.0), "起点映射精确");
        let last = sp[sp.len() - 1];
        assert!(
            (last.0 - 20.0).abs() < 1e-3 && (last.1 - 24.0).abs() < 1e-3,
            "终点映射精确：{last:?}"
        );
        for p in sp {
            let dr = dist(*p, (4.0, 24.0));
            assert!(
                (dr - 16.0).abs() < 0.05,
                "均匀缩放弧落在圆 (4,24) r16 上：{p:?} dr={dr}"
            );
        }
        // 鼓包方向：用户空间弧扫角 ∈ (−π/2, 0)（圆心右上象限），
        // 缩放不翻侧——存在 x>圆心 且 y<圆心 的采样点
        assert!(
            sp.iter().any(|p| p.0 > 4.0 && p.1 < 24.0),
            "缩放弧鼓向右上（圆心 (4,24) 的右上象限）"
        );
    }

    #[test]
    fn circle_polygon_approx() {
        let g = circle(50.0, 50.0, 20.0, Mat2x3::IDENTITY);
        let pts = &g.subs[0].pts;
        assert!(pts.len() >= 12);
        for p in pts {
            let d = dist(*p, (50.0, 50.0));
            assert!((d - 20.0).abs() < 0.01, "d={d}");
        }
    }

    #[test]
    fn stroke_outline_basics() {
        let g = PathGeom {
            subs: vec![Subpath {
                pts: vec![(0.0, 0.0), (10.0, 0.0)],
                closed: false,
            }],
        };
        let polys = stroke_outline(&g, 4.0, Cap::Butt, &[], 0.0, LineJoin::Miter, 4.0);
        assert!(!polys.is_empty());
        for poly in &polys {
            for p in poly {
                assert!(p.0 <= 10.0 + 1e-3 && p.0 >= -1e-3, "butt 不延伸: {p:?}");
            }
        }
        let polys_r = stroke_outline(&g, 4.0, Cap::Round, &[], 0.0, LineJoin::Miter, 4.0);
        let maxx = polys_r
            .iter()
            .flat_map(|p| p.iter())
            .map(|p| p.0)
            .fold(f32::MIN, f32::max);
        assert!(maxx > 10.0, "round 帽延伸出端点");
    }

    /// 车道AE：round cap 半圆必须**向前**鼓（弦 = 端点 butt 截线），
    /// 不是沿法向侧扫——旧参数化帽向侧鼓 + 半格缩进（Tabler 根继承
    /// round cap 首次暴露；pill 探针逐像素钉死）。
    #[test]
    fn round_cap_bulges_forward_not_sideways() {
        // 水平线 (0,0)→(10,0)，宽 4（hw=2）：端帽半圆盘 x∈[10,12]，
        // 弦 (10,±2)——鼓包顶点 (12,0)；帽环不得越过 y=±2（那是条身域）。
        let g = PathGeom {
            subs: vec![Subpath {
                pts: vec![(0.0, 0.0), (10.0, 0.0)],
                closed: false,
            }],
        };
        let polys = stroke_outline(&g, 4.0, Cap::Round, &[], 0.0, LineJoin::Miter, 4.0);
        // 找两个帽环（含 x>10 或 x<0 顶点的环）
        let cap_r = polys
            .iter()
            .find(|p| p.iter().any(|q| q.0 > 10.5))
            .expect("右端帽环存在");
        let max_y = cap_r.iter().map(|p| p.1.abs()).fold(0.0f32, f32::max);
        assert!(
            max_y <= 2.0 + 1e-3,
            "右帽环不得超出条身半宽（法向侧鼓包 bug）：max|y|={max_y}"
        );
        // 鼓包顶点 (12,0) 在环上（前向延伸 hw）
        assert!(
            cap_r.iter().any(|q| q.0 > 11.9 && q.1.abs() < 0.1),
            "右帽必须前向鼓到 (12,0)"
        );
        // 弦端点 (10,±2) 贴合条身边线
        assert!(
            cap_r
                .iter()
                .any(|q| (q.0 - 10.0).abs() < 1e-3 && (q.1 - 2.0).abs() < 1e-3)
                && cap_r
                    .iter()
                    .any(|q| (q.0 - 10.0).abs() < 1e-3 && (q.1 + 2.0).abs() < 1e-3),
            "帽弦必须落在端点 butt 截线 (10,±2)"
        );
    }

    #[test]
    fn poly_points_parse() {
        let g = poly_points("150,72 170,52 190,84", Mat2x3::IDENTITY, false);
        let pts = &g.subs[0].pts;
        assert_eq!(pts.len(), 3);
        assert_eq!(pts[0], (150.0, 72.0));
        assert!(!g.subs[0].closed);
    }

    /// 车道GA：Chrome SVGPointList 语义（WPT
    /// SVGPointList-error-handling）——首坐标前非法 token / 前导分隔符
    /// → 整条全弃（SVG2 初始值规则；修复前 filter_map 静默丢错渲染
    /// 合法前缀 bbox 60×40 vs Chrome 0）。
    #[test]
    fn poly_points_error_at_start_discards_all() {
        assert_eq!(
            poly_points("bogus 10,10 70,10 40,50", Mat2x3::IDENTITY, false),
            PathGeom::default()
        );
        assert_eq!(
            poly_points(",10,10 70,10 40,50", Mat2x3::IDENTITY, false),
            PathGeom::default()
        );
        assert_eq!(
            poly_points(",", Mat2x3::IDENTITY, true),
            PathGeom::default()
        );
        assert_eq!(
            poly_points("", Mat2x3::IDENTITY, false).subs[0].pts.len(),
            0
        );
        assert_eq!(
            poly_points("   ", Mat2x3::IDENTITY, false).subs[0]
                .pts
                .len(),
            0
        );
    }

    /// 车道GA：奇数尾坐标 = 丢弃落单尾数、保留合法前缀（WPT「drops
    /// the last one and keeps the rest」；与截断版 bbox 一致）。
    #[test]
    fn poly_points_odd_trailing_drops_last() {
        let g = poly_points("10,10 70,10 40,50 20", Mat2x3::IDENTITY, false);
        assert_eq!(g.subs[0].pts.len(), 3);
        let t = poly_points("10,10 70,10 40,50", Mat2x3::IDENTITY, false);
        assert_eq!(g, t);
    }

    /// 车道GA：分隔符纪律——尾随单逗号容忍；双逗号 = 解析失败全弃；
    /// 指数/正负号语法与 num_at 同。
    #[test]
    fn poly_points_separator_discipline() {
        assert_eq!(
            poly_points("10,10,", Mat2x3::IDENTITY, false).subs[0]
                .pts
                .len(),
            1
        );
        assert_eq!(
            poly_points("10,,20", Mat2x3::IDENTITY, false),
            PathGeom::default()
        );
        let g = poly_points("1e1 20, 2 -.5", Mat2x3::IDENTITY, false);
        assert_eq!(g.subs[0].pts.len(), 2);
        assert_eq!(g.subs[0].pts[1], (2.0, -0.5));
    }

    /// SVG 规范：隐式命令重复——"L10 0 10 10" ≡ 两个 L。
    /// （Material Symbols 真实图标高频语法，修复前整条 path 丢弃。）
    #[test]
    fn implicit_command_repetition() {
        let g = flatten_d("M0 0 L10 0 10 10", Mat2x3::IDENTITY);
        let pts = &g.subs[0].pts;
        assert_eq!(pts.len(), 3);
        assert_eq!(pts[0], (0.0, 0.0));
        assert_eq!(pts[1], (10.0, 0.0));
        assert_eq!(pts[2], (10.0, 10.0));
    }

    /// moveto 后的额外坐标组按 lineto 处理（规范规定）。
    #[test]
    fn moveto_extra_pairs_become_lineto() {
        let g = flatten_d("M 0 0 10 10 20 0", Mat2x3::IDENTITY);
        let pts = &g.subs[0].pts;
        assert_eq!(pts.len(), 3);
        assert_eq!((pts[1].0, pts[1].1), (10.0, 10.0));
        assert_eq!((pts[2].0, pts[2].1), (20.0, 0.0));
    }

    /// 相对命令的隐式重复 + H 多参数 + C 多组。
    #[test]
    fn implicit_repetition_relative_and_h_c() {
        let g = flatten_d("m 10 10 20 0 l 5 5 5 -5", Mat2x3::IDENTITY);
        let pts = &g.subs[0].pts;
        assert_eq!(pts[0], (10.0, 10.0));
        assert_eq!(pts[1], (30.0, 10.0)); // m 的第二组按 l
        assert_eq!(pts[2], (35.0, 15.0));
        assert_eq!(pts[3], (40.0, 10.0));

        let gh = flatten_d("M0 0 H10 30 5", Mat2x3::IDENTITY);
        let hp = &gh.subs[0].pts;
        assert_eq!(hp[1], (10.0, 0.0));
        assert_eq!(hp[2], (30.0, 0.0));
        assert_eq!(hp[3], (5.0, 0.0));

        // C 隐式重复：两组三次贝塞尔，终点依次 (3,3)、(6,6)
        let gc = flatten_d("M0 0 C 1 1 2 2 3 3 4 4 5 5 6 6", Mat2x3::IDENTITY);
        let cp = &gc.subs[0].pts;
        let last = cp[cp.len() - 1];
        assert_eq!(last, (6.0, 6.0));
        // 第一段结束点 (3,3) 应在序列中（平化分段，但节点存在）
        assert!(cp.contains(&(3.0, 3.0)));
    }

    /// Material 实测复现用例：隐式 L 重复（修复前 0 像素）。
    #[test]
    fn material_style_implicit_l() {
        let g = flatten_d(
            "M860-100 480-860 100-480-860-100-480 860Z",
            Mat2x3::IDENTITY,
        );
        let pts = &g.subs[0].pts;
        assert!(pts.len() >= 5, "Z 前应有 5 个顶点，got {}", pts.len());
        assert_eq!(pts[0], (860.0, -100.0));
        assert_eq!(pts[1], (480.0, -860.0));
    }

    // ============ stroke-dasharray / dashoffset（M2） ============

    fn x_range(polys: &[Vec<(f32, f32)>]) -> (f32, f32) {
        let xs = polys.iter().flat_map(|p| p.iter().map(|q| q.0));
        (
            xs.clone().fold(f32::MAX, f32::min),
            xs.fold(f32::MIN, f32::max),
        )
    }

    /// 直线 (0,0)→(40,0)，dash "8 6"：亮段 [0,8] [14,22] [28,36]，3 组多边形。
    #[test]
    fn dash_line_segment_count_and_extent() {
        let g = PathGeom {
            subs: vec![Subpath {
                pts: vec![(0.0, 0.0), (40.0, 0.0)],
                closed: false,
            }],
        };
        let polys = stroke_outline(&g, 4.0, Cap::Butt, &[8.0, 6.0], 0.0, LineJoin::Miter, 4.0);
        assert_eq!(polys.len(), 3, "亮段数 = 3，butt 无帽无楔形");
        let (minx, maxx) = x_range(&polys);
        assert!((minx - 0.0).abs() < 1e-3, "起点从 0 开始: {minx}");
        assert!((maxx - 36.0).abs() < 1e-3, "末亮段止于 36: {maxx}");
        // 每个亮段都是单段四边形（直线无内部顶点）
        for p in &polys {
            assert_eq!(p.len(), 4);
        }
    }

    /// dashoffset=7（图案 "8 6"）：首亮段被截成 [0,1]，后续整体移位；
    /// 负 offset 对周期取模后与正值等价。
    #[test]
    fn dash_offset_shifts_pattern() {
        let g = PathGeom {
            subs: vec![Subpath {
                pts: vec![(0.0, 0.0), (40.0, 0.0)],
                closed: false,
            }],
        };
        let polys = stroke_outline(&g, 4.0, Cap::Butt, &[8.0, 6.0], 7.0, LineJoin::Miter, 4.0);
        assert_eq!(polys.len(), 4, "亮段 [0,1] [7,15] [21,29] [35,40]");
        // 首亮段只剩 1 单位长（原 8 被相位 7 截短）
        let (minx, maxx) = x_range(&polys[..1]);
        assert!(
            minx > -1e-3 && (maxx - 1.0).abs() < 1e-3,
            "首段 [0,1]: {maxx}"
        );
        // 第二亮段起点 = 14 - 7 = 7
        let (minx2, _) = x_range(&polys[1..2]);
        assert!((minx2 - 7.0).abs() < 1e-3, "第二段起点 7: {minx2}");
        // 负 offset 等价取模：-7 ≡ 7 (mod 14)
        let polys_neg = stroke_outline(&g, 4.0, Cap::Butt, &[8.0, 6.0], -7.0, LineJoin::Miter, 4.0);
        assert_eq!(polys_neg, polys, "-7 取模后与 +7 完全一致");
        // 超出周期也取模：21 ≡ 7 (mod 14)
        let polys_wrap =
            stroke_outline(&g, 4.0, Cap::Butt, &[8.0, 6.0], 21.0, LineJoin::Miter, 4.0);
        assert_eq!(polys_wrap, polys, "21 取模后与 +7 完全一致");
    }

    /// 奇数长度图案按 SVG 规范复制一份拼成偶数："5 3 2" ≡ "5 3 2 5 3 2"。
    #[test]
    fn odd_dash_pattern_duplicated() {
        let g = PathGeom {
            subs: vec![Subpath {
                pts: vec![(0.0, 0.0), (50.0, 0.0)],
                closed: false,
            }],
        };
        let odd = stroke_outline(
            &g,
            2.0,
            Cap::Butt,
            &[5.0, 3.0, 2.0],
            0.0,
            LineJoin::Miter,
            4.0,
        );
        let even = stroke_outline(
            &g,
            2.0,
            Cap::Butt,
            &[5.0, 3.0, 2.0, 5.0, 3.0, 2.0],
            0.0,
            LineJoin::Miter,
            4.0,
        );
        assert_eq!(odd, even, "奇数图案复制后输出一致");
        // 周期 10，L=50 → 亮段 [0,5] [8,10] [15,18] [20,25] [28,30]
        // [35,38] [40,45] [48,50] = 8 段
        assert_eq!(odd.len(), 8);
    }

    /// dash 数组含负值 → 非法，整体按 none（实线）处理（防御层；
    /// builder 解析层同样拦截）。
    #[test]
    fn negative_dash_value_treated_solid() {
        let g = PathGeom {
            subs: vec![Subpath {
                pts: vec![(0.0, 0.0), (40.0, 0.0)],
                closed: false,
            }],
        };
        let solid = stroke_outline(&g, 4.0, Cap::Butt, &[], 0.0, LineJoin::Miter, 4.0);
        let neg = stroke_outline(&g, 4.0, Cap::Butt, &[8.0, -6.0], 0.0, LineJoin::Miter, 4.0);
        assert_eq!(neg, solid, "含负值的图案退化为实线");
        let zero = stroke_outline(&g, 4.0, Cap::Butt, &[0.0, 0.0], 0.0, LineJoin::Miter, 4.0);
        assert_eq!(zero, solid, "全零图案（周期 0）退化为实线");
    }

    /// 亮段跨越路径顶点：L 形路径 dash [25,15]，首亮段 [0,25] 覆盖
    /// 拐点 (20,0)，内部顶点做 join 楔形，端点只到弧长 25 处 (20,5)。
    #[test]
    fn dash_lit_segment_spans_vertex_with_join() {
        let g = PathGeom {
            subs: vec![Subpath {
                pts: vec![(0.0, 0.0), (20.0, 0.0), (20.0, 20.0)],
                closed: false,
            }],
        };
        let polys = stroke_outline(&g, 4.0, Cap::Butt, &[25.0, 15.0], 0.0, LineJoin::Miter, 4.0);
        // 2 段四边形（(0,0)→(20,0)、(20,0)→(20,5)）+ 1 个拐角楔形 = 3
        assert_eq!(polys.len(), 3, "quad×2 + join×1，butt 无帽");
        let maxy = polys
            .iter()
            .flat_map(|p| p.iter())
            .map(|p| p.1)
            .fold(f32::MIN, f32::max);
        assert!((maxy - 5.0).abs() < 1e-3, "亮段止于弧长 25 即 y=5: {maxy}");
    }

    /// round cap 下每个 dash 亮段两端各延伸 hw（宽 4 → 延伸 2）。
    #[test]
    fn dash_round_cap_extends_both_ends() {
        let g = PathGeom {
            subs: vec![Subpath {
                pts: vec![(0.0, 0.0), (40.0, 0.0)],
                closed: false,
            }],
        };
        let polys = stroke_outline(&g, 4.0, Cap::Round, &[8.0, 6.0], 0.0, LineJoin::Miter, 4.0);
        // 3 个亮段 ×（1 quad + 2 半圆帽）= 9
        assert_eq!(polys.len(), 9, "每亮段 quad + 两端半圆帽");
        let (minx, maxx) = x_range(&polys);
        assert!(minx < -1.5, "首帽延伸出起点: {minx}");
        assert!(maxx > 37.5, "末亮段帽延伸出 36: {maxx}");
    }

    // ============ 零长 dash 亮段 = 点链（openmoji 1F4FF 车道AS） ============

    /// 零长亮段 + round cap = 圆点链（Chrome/Skia 语义）。直线
    /// (0,0)→(40,0)，图案 [0,6]，宽 4 → 每 6 单位一枚半径 2 的圆点，
    /// 含 s=0（相位恰好落在图案首零段上也要画），共 7 枚。
    #[test]
    fn dash_zero_length_round_cap_draws_dots() {
        let g = PathGeom {
            subs: vec![Subpath {
                pts: vec![(0.0, 0.0), (40.0, 0.0)],
                closed: false,
            }],
        };
        let polys = stroke_outline(&g, 4.0, Cap::Round, &[0.0, 6.0], 0.0, LineJoin::Miter, 4.0);
        assert_eq!(polys.len(), 7, "s=0,6,…,36 七枚圆点");
        for (k, p) in polys.iter().enumerate() {
            assert_eq!(p.len(), 32, "每点一枚 32 边内接圆");
            let cx = p.iter().map(|q| q.0).sum::<f32>() / p.len() as f32;
            let cy = p.iter().map(|q| q.1).sum::<f32>() / p.len() as f32;
            let expect = 6.0 * k as f32;
            assert!((cx - expect).abs() < 1e-3, "第 {k} 点圆心 x≈{expect}: {cx}");
            assert!(cy.abs() < 1e-3, "圆心 y=0: {cy}");
            let r_max = p.iter().map(|q| dist(*q, (cx, cy))).fold(0.0f32, f32::max);
            assert!((r_max - 2.0).abs() < 1e-3, "点半径 = hw = 2: {r_max}");
        }
    }

    /// 零长亮段 + butt cap = 不画（Chrome 一致：无帽无点）。
    #[test]
    fn dash_zero_length_butt_cap_no_dots() {
        let g = PathGeom {
            subs: vec![Subpath {
                pts: vec![(0.0, 0.0), (40.0, 0.0)],
                closed: false,
            }],
        };
        let polys = stroke_outline(&g, 4.0, Cap::Butt, &[0.0, 6.0], 0.0, LineJoin::Miter, 4.0);
        assert!(polys.is_empty(), "butt 下零长亮段无墨");
    }

    /// 零长亮段 + square cap = 边长 2·hw 的切向方块。
    #[test]
    fn dash_zero_length_square_cap_draws_squares() {
        let g = PathGeom {
            subs: vec![Subpath {
                pts: vec![(0.0, 0.0), (40.0, 0.0)],
                closed: false,
            }],
        };
        let polys = stroke_outline(&g, 4.0, Cap::Square, &[0.0, 6.0], 0.0, LineJoin::Miter, 4.0);
        assert_eq!(polys.len(), 7, "七枚方点");
        for (k, p) in polys.iter().enumerate() {
            assert_eq!(p.len(), 4, "方点 = 4 顶点");
            let (minx, maxx) = (
                p.iter().map(|q| q.0).fold(f32::MAX, f32::min),
                p.iter().map(|q| q.0).fold(f32::MIN, f32::max),
            );
            let expect = 6.0 * k as f32;
            assert!((minx - (expect - 2.0)).abs() < 1e-3, "方点左缘 {minx}");
            assert!((maxx - (expect + 2.0)).abs() < 1e-3, "方点右缘 {maxx}");
        }
    }

    /// 1F4FF 真实图案形态 [0, 6.7346, 0, 0, 0, 0]（尾随零段）+ round：
    /// 每周期恰一枚点、间距 = 周期；同位置多枚零亮段重叠 = 同一圆。
    /// 这是旧版相位/段游标失配（take 膨胀成整周期）的回归锚。
    #[test]
    fn dash_trailing_zero_pattern_one_dot_per_period() {
        let g = PathGeom {
            subs: vec![Subpath {
                pts: vec![(0.0, 0.0), (40.0, 0.0)],
                closed: false,
            }],
        };
        let pat = [0.0f32, 6.7346, 0.0, 0.0, 0.0, 0.0];
        let polys = stroke_outline(&g, 4.0, Cap::Round, &pat, 0.0, LineJoin::Miter, 4.0);
        assert_eq!(polys.len(), 6, "s=0,6.73,…,33.67 六枚点");
        let mut cxs: Vec<f32> = polys
            .iter()
            .map(|p| p.iter().map(|q| q.0).sum::<f32>() / p.len() as f32)
            .collect();
        cxs.sort_by(|a, b| a.partial_cmp(b).expect("f32 排序不含 NaN"));
        for (k, cx) in cxs.iter().enumerate() {
            let expect = 6.7346 * k as f32;
            assert!((cx - expect).abs() < 1e-2, "第 {k} 点 x≈{expect}: {cx}");
        }
    }

    /// 零长图案 + 非零 dashoffset：相位落在图案内部时点仍按周期对齐。
    /// 图案 [0,6]、offset 2 → 首点在 s=4（图案位 0 ↔ 路径位 4）。
    #[test]
    fn dash_zero_pattern_with_offset_shifts_dots() {
        let g = PathGeom {
            subs: vec![Subpath {
                pts: vec![(0.0, 0.0), (40.0, 0.0)],
                closed: false,
            }],
        };
        let polys = stroke_outline(&g, 4.0, Cap::Round, &[0.0, 6.0], 2.0, LineJoin::Miter, 4.0);
        assert_eq!(polys.len(), 6, "s=4,10,…,34 六枚点");
        for (k, p) in polys.iter().enumerate() {
            let cx = p.iter().map(|q| q.0).sum::<f32>() / p.len() as f32;
            let expect = 4.0 + 6.0 * k as f32;
            assert!((cx - expect).abs() < 1e-3, "第 {k} 点 x≈{expect}: {cx}");
        }
    }

    /// 混合图案（零 on + 非零 on 共存）走规则 dasher：零长 on 段不画点。
    /// `[0,0,3.94,3.94]`（E103 外链虚线框）与 `[3.94,3.94]` 输出逐位一致
    /// （旧渲染与 Chrome 0.000% 一致的回归锚；误画点会在 dash 帽外多墨）。
    #[test]
    fn dash_mixed_zero_on_pattern_skips_dots() {
        let g = PathGeom {
            subs: vec![Subpath {
                pts: vec![(0.0, 0.0), (40.0, 0.0)],
                closed: false,
            }],
        };
        let mixed = stroke_outline(
            &g,
            4.0,
            Cap::Round,
            &[0.0, 0.0, 3.9396, 3.9396],
            0.0,
            LineJoin::Miter,
            4.0,
        );
        let plain = stroke_outline(
            &g,
            4.0,
            Cap::Round,
            &[3.9396, 3.9396],
            0.0,
            LineJoin::Miter,
            4.0,
        );
        assert_eq!(mixed, plain, "混合图案零长 on 段 = 无点，与纯 dash 一致");
    }

    // ============ stroke-miterlimit / stroke-linejoin（M2 收尾） ============

    /// 尖针折线：内部角 ≈ 22.6° → miter 比值 = 1/sin(θ/2) ≈ 5.08。
    /// miterlimit=8 保留尖刺（5 顶点楔形、y 超出顶点 10+）；默认 4 与
    /// miterlimit=2 均超限转 bevel（全 4 顶点、尖刺削平），且两者逐点一致
    /// （默认行为 = 改动前硬编码 hw·4 的回归锚）。
    #[test]
    fn miterlimit_over_limit_becomes_bevel() {
        // 顶点 (50,100)，两臂到 (30,0)/(70,0)
        let needle = PathGeom {
            subs: vec![Subpath {
                pts: vec![(30.0, 0.0), (50.0, 100.0), (70.0, 0.0)],
                closed: false,
            }],
        };
        let maxy = |ps: &Vec<Vec<(f32, f32)>>| {
            ps.iter()
                .flat_map(|p| p.iter().map(|q| q.1))
                .fold(f32::MIN, f32::max)
        };
        let lens = |ps: &Vec<Vec<(f32, f32)>>| {
            let mut v: Vec<usize> = ps.iter().map(|p| p.len()).collect();
            v.sort_unstable();
            v
        };

        // miterlimit=8 ≥ 5.08：miter 尖刺保留，y 远超段四边形边界 ~100.4
        let miter = stroke_outline(&needle, 4.0, Cap::Butt, &[], 0.0, LineJoin::Miter, 8.0);
        assert!(maxy(&miter) > 105.0, "尖刺延伸: {}", maxy(&miter));
        assert!(miter.iter().any(|p| p.len() == 5), "miter 楔形 5 顶点");

        // miterlimit=2 < 5.08：超限转 bevel，尖刺削平回 ~100.4
        let bevel = stroke_outline(&needle, 4.0, Cap::Butt, &[], 0.0, LineJoin::Miter, 2.0);
        assert!(maxy(&bevel) < 101.0, "尖刺削平: {}", maxy(&bevel));
        assert_eq!(lens(&bevel), vec![4, 4, 4], "quads×2 + bevel 楔形全 4 顶点");

        // 默认 miterlimit=4 < 5.08 同样转 bevel，与 2 完全一致
        let default = stroke_outline(&needle, 4.0, Cap::Butt, &[], 0.0, LineJoin::Miter, 4.0);
        assert_eq!(default, bevel, "默认 4 与超限 bevel 输出一致");
    }

    /// round join：楔形尖端被 圆弧替代——15 顶点（12 弧段 = 13 个弧
    /// 顶点 + 2 内点）> miter 楔形 5 顶点；弧点全落在半径 hw 的圆上；
    /// 尖端最远 = 顶点 + hw（y = 102），不超 miter 尖刺也不削成平角。
    #[test]
    fn round_join_arc_replaces_wedge() {
        let needle = PathGeom {
            subs: vec![Subpath {
                pts: vec![(30.0, 0.0), (50.0, 100.0), (70.0, 0.0)],
                closed: false,
            }],
        };
        let round = stroke_outline(&needle, 4.0, Cap::Butt, &[], 0.0, LineJoin::Round, 4.0);
        let wedge = round.iter().find(|p| p.len() == 15);
        assert!(
            wedge.is_some(),
            "round 楔形 15 顶点，got {:?}",
            round.iter().map(|p| p.len()).collect::<Vec<_>>()
        );
        if let Some(poly) = wedge {
            for p in poly.iter() {
                let d = dist(*p, (50.0, 100.0));
                assert!((d - 2.0).abs() < 0.05, "弧点半径 = hw: d={d}");
            }
        }
        // 圆弧顶点数 15 > miter 楔形 5
        let miter = stroke_outline(&needle, 4.0, Cap::Butt, &[], 0.0, LineJoin::Miter, 8.0);
        let miter_wedge_len = miter.iter().map(|p| p.len()).fold(0, usize::max);
        assert_eq!(miter_wedge_len, 5, "miter 楔形 5 顶点");
        assert!(15 > miter_wedge_len);
        // round 尖端 y ≈ 100 + 2：介于 bevel 削平（~100.4）与 miter 尖刺（~110）
        let maxy = round
            .iter()
            .flat_map(|p| p.iter().map(|q| q.1))
            .fold(f32::MIN, f32::max);
        assert!((maxy - 102.0).abs() < 0.2, "round 尖端 = 顶点 + hw: {maxy}");
    }

    /// join_from_str 三态 + 未知值默认 miter。
    #[test]
    fn join_from_str_variants() {
        assert_eq!(join_from_str("miter"), LineJoin::Miter);
        assert_eq!(join_from_str("round"), LineJoin::Round);
        assert_eq!(join_from_str("bevel"), LineJoin::Bevel);
        assert_eq!(join_from_str(""), LineJoin::Miter);
        assert_eq!(join_from_str("bogus"), LineJoin::Miter);
    }

    // ============ M6：animate d 同构命令插值 ============

    /// 同构判定：命令数 / 种类不一致 → false；绝对/相对写法归一后可比；
    /// 数值与 A flag 差异不影响结构。
    #[test]
    fn cmds_same_shape_variants() {
        let a = parse_d("M0 0 L10 0 Q12 4 10 8 Z").unwrap_or_default();
        let b = parse_d("M5 5 L50 50 Q60 60 50 50 Z").unwrap_or_default();
        assert!(cmds_same_shape(&a, &b), "同构（值不同）");
        // 相对命令展开后与绝对写法同构
        let rel = parse_d("m5 5 l45 45 q10 10 -10 10 z").unwrap_or_default();
        assert!(cmds_same_shape(&a, &rel), "相对写法归一后同构");
        // 命令数不同
        let short = parse_d("M5 5 L50 50").unwrap_or_default();
        assert!(!cmds_same_shape(&a, &short), "命令数不同");
        // 种类不同（同位置 L vs C）
        let l2 = parse_d("M0 0 L10 0").unwrap_or_default();
        let c2 = parse_d("M0 0 C1 1 2 2 10 0").unwrap_or_default();
        assert!(!cmds_same_shape(&l2, &c2), "同位置 L vs C 不同构");
        // flag 不同仍同构（A/A）
        let arc0 = parse_d("M0 0 A4 4 0 0 1 8 0").unwrap_or_default();
        let arc1 = parse_d("M0 0 A6 6 0 1 0 8 0").unwrap_or_default();
        assert!(cmds_same_shape(&arc0, &arc1), "A flag 差异不破坏结构");
    }

    /// 逐参数线性插值：t=0.5 的中点命令精确；Z 原样；不兼容回退帧 a。
    #[test]
    fn interp_cmds_midpoint_exact() {
        let a = parse_d("M0 0 L10 0 H20 V8").unwrap_or_default();
        let b = parse_d("M0 10 L10 10 H40 V16").unwrap_or_default();
        let mid = interp_cmds(&a, &b, 0.5);
        assert_eq!(
            mid,
            vec![
                Cmd::M(0.0, 5.0),
                Cmd::L(10.0, 5.0),
                Cmd::H(30.0),
                Cmd::V(12.0)
            ],
            "每个数值参数独立取中点"
        );
        assert_eq!(interp_cmds(&a, &b, 0.0), a, "t=0 回帧 a");
        assert_eq!(interp_cmds(&a, &b, 1.0), b, "t=1 回帧 b");
        // 不兼容（防御）：回退帧 a 拷贝
        let c = parse_d("M2 2").unwrap_or_default();
        assert_eq!(interp_cmds(&a, &c, 0.5), a, "结构不兼容回退帧 a");
        // Z 无参数原样
        let za = parse_d("M0 0 L10 0 Z").unwrap_or_default();
        let zb = parse_d("M0 4 L10 4 Z").unwrap_or_default();
        let zm = interp_cmds(&za, &zb, 0.5);
        assert_eq!(zm[2], Cmd::Z, "Z 命令保留");
    }

    /// 闭合子路径变形后仍闭合：插值产物 flatten_cmds 的 subpath.closed
    /// 保持 true（Z 语义随命令列传递）。
    #[test]
    fn interp_keeps_z_closed() {
        let a = parse_d("M0 0 L10 0 L10 10 Z").unwrap_or_default();
        let b = parse_d("M0 4 L10 4 L10 14 Z").unwrap_or_default();
        let g = flatten_cmds(&interp_cmds(&a, &b, 0.5), Mat2x3::IDENTITY);
        assert_eq!(g.subs.len(), 1);
        assert!(g.subs[0].closed, "变形后 Z 闭合性保持");
        let pts = &g.subs[0].pts;
        assert_eq!(pts[0], (0.0, 2.0), "起点 y 取中点");
        assert_eq!(pts[2], (10.0, 12.0), "末顶点 y 取中点");
        // flatten_d 与 flatten_cmds 同串一致性（重构回归锚）
        let gd = flatten_d("M0 2 L10 2 L10 12 Z", Mat2x3::IDENTITY);
        assert_eq!(gd, g, "命令路径与字符串路径平化一致");
    }
}
