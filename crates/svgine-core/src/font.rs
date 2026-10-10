//! Tier C 真字体：最小 TrueType 解析（ADR-0008）。
//!
//! 覆盖：sfnt 目录、head（units_per_em/indexToLocFormat）、maxp
//!（numGlyphs）、hhea/hmtx（advance）、cmap（format 4/12）、loca
//!（short/long）、glyf（simple 轮廓 + 复合字形一级组件展开，递归
//! 深度上限防炸弹）。字形输出 [`GlyphOutline`] = nonzero 填充子路
//! 径（on-curve 起点），接现有 `path` 平化/填充管线。
//!
//! no_std：纯字节解析（u16/u32 BE），零分配解析表目录（按需偏移读）。
//! 宿主注入 TTF 字节（宪法：资源归宿主）；未注入时 text 走 Hershey
//! 旧路径（逐字节不变，ADR-0008 向后兼容红线）。

use crate::path::Subpath;
use alloc::vec::Vec;

/// 解析失败（表缺失/魔数错/越界）。保守失败：调用方回落 Hershey。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FontError;

/// 已解析的字体：保留源字节引用 + 表偏移缓存（零拷贝）。
pub struct Font<'a> {
    data: &'a [u8],
    units_per_em: f32,
    index_to_loc_format: u16,
    num_glyphs: u16,
    cmap_off: usize,
    hmtx_off: usize,
    num_h_metrics: u16,
    glyf_off: usize,
    loca_off: usize,
    /// 车道DC：x-height（em 单位）；0.0 = 无可信值。OS/2 v2+ sxHeight
    /// 优先，v1/缺表从 'x' 字形轮廓顶合成（见 [`Font::x_height_em`]）。
    x_height: f32,
    /// 车道GA：hhea ascent/descent（em；descent 带号 ≤0）——文本垂直
    /// 度量模型（baseline-shift 关键字量、纵排 central 列轴）的字体侧
    /// 输入（[`Font::ascent_descent_em`]）。
    asc_desc: (f32, f32),
}

/// 单字形轮廓：quadratic 已平化为折线子路径（设备无关、em 归一化后
/// 的用户空间坐标，y 向上——调用方按 SVG y 向下翻转变换）。
pub struct GlyphOutline {
    pub subs: Vec<Subpath>,
    /// em 单位的 advance width（未缩放；布局层乘 fontSize/upem）。
    pub advance_em: f32,
}

fn u16_at(d: &[u8], o: usize) -> Option<u16> {
    d.get(o..o + 2).map(|b| u16::from_be_bytes([b[0], b[1]]))
}
fn i16_at(d: &[u8], o: usize) -> Option<i16> {
    d.get(o..o + 2).map(|b| i16::from_be_bytes([b[0], b[1]]))
}
fn u32_at(d: &[u8], o: usize) -> Option<u32> {
    d.get(o..o + 4)
        .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

impl<'a> Font<'a> {
    /// 解析 sfnt 结构并缓存关键表偏移。只读校验，不做全表扫描。
    pub fn parse(data: &'a [u8]) -> Result<Font<'a>, FontError> {
        let ver = u32_at(data, 0).ok_or(FontError)?;
        if ver != 0x0001_0000 && ver != 0x7474_6366 {
            return Err(FontError); // TrueType 1.0 / 'ttcf' 集合首字体
        }
        let num = u16_at(data, 4).ok_or(FontError)? as usize;
        let mut t = TableOffsets::default();
        for i in 0..num {
            let rec = 12 + 16 * i;
            if rec + 16 > data.len() {
                return Err(FontError);
            }
            let tag = &data[rec..rec + 4];
            let off = u32_at(data, rec + 8).ok_or(FontError)? as usize;
            let len = u32_at(data, rec + 12).ok_or(FontError)? as usize;
            if off + len > data.len() {
                return Err(FontError);
            }
            match tag {
                b"cmap" => t.cmap = Some((off, len)),
                b"head" => t.head = Some(off),
                b"maxp" => t.maxp = Some(off),
                b"hhea" => t.hhea = Some(off),
                b"hmtx" => t.hmtx = Some((off, len)),
                b"glyf" => t.glyf = Some((off, len)),
                b"loca" => t.loca = Some((off, len)),
                b"OS/2" => t.os2 = Some(off),
                _ => {}
            }
        }
        let head = t.head.ok_or(FontError)?;
        let units_per_em = u16_at(data, head + 18).ok_or(FontError)? as f32;
        if units_per_em <= 0.0 {
            return Err(FontError);
        }
        let index_to_loc_format = u16_at(data, head + 50).ok_or(FontError)?;
        let maxp = t.maxp.ok_or(FontError)?;
        let num_glyphs = u16_at(data, maxp + 4).ok_or(FontError)?;
        let hhea = t.hhea.ok_or(FontError)?;
        let num_h_metrics = u16_at(data, hhea + 34).ok_or(FontError)?;
        // 车道GA：hhea 垂直度量（ascender ≥0 / descender ≤0，i16）。
        let asc_desc = (
            i16_at(data, hhea + 4).unwrap_or(0) as f32 / units_per_em,
            i16_at(data, hhea + 6).unwrap_or(0) as f32 / units_per_em,
        );
        let (cmap_off, _) = t.cmap.ok_or(FontError)?;
        let (hmtx_off, _) = t.hmtx.ok_or(FontError)?;
        let (glyf_off, _) = t.glyf.ok_or(FontError)?;
        let (loca_off, _) = t.loca.ok_or(FontError)?;
        // 车道DC：x-height = font-size-adjust 分母（CSS Fonts：first
        // available font 的 x-height）。OS/2 v2+ 读 sxHeight；v1/缺表
        // 从 'x' 字形轮廓顶合成（CoreText 对 v1 字体同款语义——chrome
        // xh 探针 PinSans 实测 0.543 vs 合成 0.5469）。
        let mut x_height = 0.0f32;
        if let Some(o) = t.os2 {
            if u16_at(data, o).is_some_and(|v| v >= 2) {
                if let Some(sx) = u16_at(data, o + 86) {
                    x_height = sx as f32 / units_per_em;
                }
            }
        }
        let mut font = Font {
            data,
            units_per_em,
            index_to_loc_format,
            num_glyphs,
            cmap_off,
            hmtx_off,
            num_h_metrics,
            glyf_off,
            loca_off,
            x_height,
            asc_desc,
        };
        if font.x_height <= 0.0 {
            if let Some(g) = font.glyph_id(0x78) {
                if g != 0 {
                    if let Some(o) = font.outline(g) {
                        let top = o
                            .subs
                            .iter()
                            .flat_map(|s| s.pts.iter().map(|p| p.1))
                            .fold(0.0f32, f32::max);
                        if top > 0.0 && top < 2.0 {
                            font.x_height = top;
                        }
                    }
                }
            }
        }
        Ok(font)
    }

    pub fn units_per_em(&self) -> f32 {
        self.units_per_em
    }

    /// 车道DC：first available font 的 x-height（em 单位，CSS Fonts
    /// `font-size-adjust` 分母）。OS/2 v2+ sxHeight 优先；v1/缺表从
    /// 'x' 字形轮廓顶合成（Chrome/CoreText 同款语义）。None = 无可信
    /// 值（调用方回落 Hershey 常数 X_HEIGHT_EM）。
    pub fn x_height_em(&self) -> Option<f32> {
        if self.x_height > 0.0 {
            Some(self.x_height)
        } else {
            None
        }
    }

    /// 车道GA：hhea ascent/descent（em；descent **带号 ≤0**）。
    /// Chrome 文本垂直度量模型的字体侧输入（活体探针 ga-bs 双字体
    /// 钉定，2026-09-12）：baseline-shift super/sub 关键字量 =
    /// (asc − desc)/2 em（PinSans 0.582 实测 0.567/0.600、Noto 0.724
    /// 实测 0.733/0.728，±0.5px 量化内）；纵排 central 列轴偏移 =
    /// (asc + desc)/2 em（BG dominant-baseline central 同式家族：
    /// fallback Times 0.3375 独立钉值互验）。
    pub fn ascent_descent_em(&self) -> (f32, f32) {
        self.asc_desc
    }

    /// unicode 码点 → glyph id（cmap format 4/12，取首个命中的子表）。
    pub fn glyph_id(&self, cp: u32) -> Option<u16> {
        let d = self.data;
        let co = self.cmap_off;
        let n = u16_at(d, co + 2)? as usize;
        for i in 0..n {
            let rec = co + 4 + 8 * i;
            let off = co + u32_at(d, rec + 4)? as usize;
            let fmt = u16_at(d, off)?;
            let gid = match fmt {
                4 => self.cmap4_lookup(off, cp),
                12 => self.cmap12_lookup(off, cp),
                _ => None,
            };
            if gid.is_some() {
                return gid;
            }
        }
        None
    }

    fn cmap4_lookup(&self, off: usize, cp: u32) -> Option<u16> {
        if cp > 0xFFFF {
            return None;
        }
        let cp = cp as u16;
        let d = self.data;
        let seg_x2 = u16_at(d, off + 6)? as usize;
        let segs = seg_x2 / 2;
        let end_base = off + 14;
        // 二分 endCode
        let (mut lo, mut hi) = (0usize, segs);
        while lo < hi {
            let mid = (lo + hi) / 2;
            let end = u16_at(d, end_base + mid * 2)?;
            if cp <= end {
                hi = mid;
            } else {
                lo = mid + 1;
            }
        }
        if lo >= segs {
            return None;
        }
        // 规范布局：endCode[segs] | reservedPad(2B) | startCode[segs]
        //          | idDelta[segs] | idRangeOffset[segs] | glyphIdArray
        let start_base = end_base + seg_x2 + 2;
        let start = u16_at(d, start_base + lo * 2)?;
        if cp < start {
            return None;
        }
        let delta_base = start_base + seg_x2;
        let delta = i16_at(d, delta_base + lo * 2)? as i32;
        let range_off_base = delta_base + seg_x2;
        let ro = u16_at(d, range_off_base + lo * 2)? as usize;
        if ro == 0 {
            return Some(((cp as i32 + delta) & 0xFFFF) as u16);
        }
        // glyphIdArray 偶数偏移寻址（相对 range_off_base + lo*2）
        let gid_off = range_off_base + lo * 2 + ro + (cp - start) as usize * 2;
        let gid = u16_at(d, gid_off)? as i32;
        if gid == 0 {
            None
        } else {
            Some((gid + delta) as u16)
        }
    }

    fn cmap12_lookup(&self, off: usize, cp: u32) -> Option<u16> {
        let d = self.data;
        let n = u32_at(d, off + 12)? as usize;
        let (mut lo, mut hi) = (0usize, n);
        while lo < hi {
            let mid = (lo + hi) / 2;
            let rec = off + 16 + mid * 12;
            let start = u32_at(d, rec)?;
            let end = u32_at(d, rec + 4)?;
            if cp < start {
                hi = mid;
            } else if cp > end {
                lo = mid + 1;
            } else {
                let gid = u32_at(d, rec + 8)? + (cp - start);
                return Some(gid as u16);
            }
        }
        None
    }

    /// glyph id → advance width（em 单位；截短表尾部用最后一条）。
    pub fn advance_em(&self, gid: u16) -> Option<f32> {
        if gid >= self.num_glyphs {
            return None;
        }
        let n = self.num_h_metrics as usize;
        if n == 0 {
            return None;
        }
        let idx = (gid as usize).min(n - 1);
        let w = u16_at(self.data, self.hmtx_off + idx * 4)?;
        Some(w as f32 / self.units_per_em)
    }

    /// glyph id → 轮廓（em 用户空间，y 向上）。复合字形一级组件展开
    ///（递归深度上限 8）。空字形（空格）→ 空 subs。
    pub fn outline(&self, gid: u16) -> Option<GlyphOutline> {
        if gid >= self.num_glyphs {
            return None;
        }
        let (off, end) = self.glyph_offset(gid)?;
        if off == end {
            // 空字形（loca 相等：空格/占位）——**不得**读下一字形的头
            return Some(GlyphOutline {
                subs: Vec::new(),
                advance_em: self.advance_em(gid)?,
            });
        }
        let mut subs = Vec::new();
        self.collect_outline_at(off, &mut subs, 0)?;
        Some(GlyphOutline {
            subs,
            advance_em: self.advance_em(gid)?,
        })
    }

    /// loca[i] 与 loca[i+1]（glyf 表内绝对偏移）。相等 = 空字形。
    fn glyph_offset(&self, gid: u16) -> Option<(usize, usize)> {
        let i = gid as usize;
        let o = self.loca_off;
        if self.index_to_loc_format == 0 {
            let a = u16_at(self.data, o + i * 2)? as usize * 2;
            let b = u16_at(self.data, o + i * 2 + 2)? as usize * 2;
            Some((self.glyf_off + a, self.glyf_off + b))
        } else {
            let a = u32_at(self.data, o + i * 4)? as usize;
            let b = u32_at(self.data, o + i * 4 + 4)? as usize;
            Some((self.glyf_off + a, self.glyf_off + b))
        }
    }

    fn collect_outline(&self, gid: u16, subs: &mut Vec<Subpath>, depth: u32) -> Option<()> {
        if depth > 8 {
            return Some(()); // 复合炸弹截断（防空循环字体）
        }
        let (off, _end) = self.glyph_offset(gid)?;
        self.collect_outline_at(off, subs, depth)
    }

    fn collect_outline_at(&self, off: usize, subs: &mut Vec<Subpath>, depth: u32) -> Option<()> {
        let d = self.data;
        if off + 10 > d.len() {
            return Some(());
        }
        let ncontours = i16_at(d, off)?;
        if ncontours >= 0 {
            self.simple_outline(off, ncontours as usize, subs)
        } else {
            self.composite_outline(off, subs, depth)
        }
    }

    /// simple 轮廓：endPts → 逐点（on/off 二次序列）→ 折线子路径。
    /// quadratic 中点插值：off-off 取中点；on-off 取 off 为控制点。
    fn simple_outline(&self, off: usize, ncontours: usize, subs: &mut Vec<Subpath>) -> Option<()> {
        let d = self.data;
        let mut p = off + 10;
        let mut ends = Vec::with_capacity(ncontours);
        for _ in 0..ncontours {
            ends.push(u16_at(d, p)? as usize);
            p += 2;
        }
        let npts = (*ends.last()?).checked_add(1)?;
        let il = u16_at(d, p)? as usize;
        p += 2 + il;
        // flags：可变长（REPEAT 位 3）
        let mut flags = Vec::with_capacity(npts);
        while flags.len() < npts {
            let f = d.get(p).copied()?;
            p += 1;
            flags.push(f);
            if f & 0x08 != 0 {
                let rep = d.get(p).copied()?;
                p += 1;
                for _ in 0..rep {
                    if flags.len() >= npts {
                        break;
                    }
                    flags.push(f);
                }
            }
        }
        // x 坐标（SHORT_VECTOR/相同位）
        let mut xs = Vec::with_capacity(npts);
        let mut x: i32 = 0;
        for &f in &flags {
            if f & 0x02 != 0 {
                let dx = d.get(p).copied()? as i32;
                p += 1;
                x += if f & 0x10 != 0 { dx } else { -dx };
            } else if f & 0x10 == 0 {
                x += i16_at(d, p)? as i32;
                p += 2;
            }
            xs.push(x);
        }
        // y 坐标
        let mut ys = Vec::with_capacity(npts);
        let mut y: i32 = 0;
        for &f in &flags {
            if f & 0x04 != 0 {
                let dy = d.get(p).copied()? as i32;
                p += 1;
                y += if f & 0x20 != 0 { dy } else { -dy };
            } else if f & 0x20 == 0 {
                y += i16_at(d, p)? as i32;
                p += 2;
            }
            ys.push(y);
        }
        let upem = self.units_per_em;
        let pt = |i: usize| -> (f32, f32) { (xs[i] as f32 / upem, ys[i] as f32 / upem) };
        // 逐轮廓提取折线（on/off 标志 = flags bit 0）
        let mut start = 0usize;
        for &end in &ends {
            if end < start || end >= npts {
                start = end + 1;
                continue;
            }
            let pts: Vec<(f32, f32)> = (start..=end).map(pt).collect();
            let ons: Vec<bool> = flags[start..=end].iter().map(|&f| f & 0x01 != 0).collect();
            append_quad_polyline(&pts, &ons, subs);
            start = end + 1;
        }
        Some(())
    }

    /// 复合字形：ARGS_ARE_XY_VALUES 平移组件（缩放/旋转长尾不展开，
    /// 平移是最常见形态；非常量矩阵组件按单位阵近似并计入误差——
    /// 报告登记）。递归组件展开（深度上限在 collect_outline）。
    fn composite_outline(&self, off: usize, subs: &mut Vec<Subpath>, depth: u32) -> Option<()> {
        let d = self.data;
        let mut p = off + 10;
        loop {
            let flags = u16_at(d, p)?;
            let gid = u16_at(d, p + 2)?;
            p += 4;
            // ARGS_ARE_XY_VALUES（bit 1）才有平移参数——本实现只支持它
            let has_xy = flags & 0x0002 != 0;
            let (dx, dy) = if flags & 0x0001 != 0 {
                // ARG_1_AND_2_ARE_WORDS
                let a = i16_at(d, p)? as f32;
                let b = i16_at(d, p + 2)? as f32;
                p += 4;
                (a, b)
            } else {
                let a = d.get(p).copied()? as i8 as f32;
                let b = d.get(p + 1).copied()? as i8 as f32;
                p += 2;
                (a, b)
            };
            if flags & 0x0008 != 0 {
                p += 2; // WE_HAVE_A_SCALE（F2Dot14，缩放长尾不应用）
            } else if flags & 0x0040 != 0 {
                p += 4; // X_AND_Y_SCALE
            } else if flags & 0x0080 != 0 {
                p += 8; // 2x2 矩阵
            }
            if has_xy {
                // 组件子轮廓收集后整体平移（em 域）
                let before = subs.len();
                self.collect_outline(gid, subs, depth + 1)?;
                let upem = self.units_per_em;
                let (tx, ty) = (dx / upem, dy / upem);
                for sp in &mut subs[before..] {
                    for (x, y) in sp.pts.iter_mut() {
                        *x += tx;
                        *y += ty;
                    }
                }
            } else {
                self.collect_outline(gid, subs, depth + 1)?;
            }
            if flags & 0x0020 == 0 {
                break; // MORE_COMPONENTS
            }
        }
        Some(())
    }
}

#[derive(Default)]
struct TableOffsets {
    cmap: Option<(usize, usize)>,
    head: Option<usize>,
    maxp: Option<usize>,
    hhea: Option<usize>,
    hmtx: Option<(usize, usize)>,
    glyf: Option<(usize, usize)>,
    loca: Option<(usize, usize)>,
    os2: Option<usize>,
}

/// on/off 二次点序列 → 闭合折线子路径。规则：
/// - on 点 = 顶点直接落；
/// - off 点 = 控制点；off-off 相邻 → 两控制点中点为隐式 on；
/// - quadratic (p0, ctrl, p1) 按中点细分两段直线（字号渲染精度足够，
///   后续平化管线按 TOL 自适应）。
fn append_quad_polyline(pts: &[(f32, f32)], ons: &[bool], subs: &mut Vec<Subpath>) {
    let n = pts.len();
    if n < 2 || ons.len() != n {
        return;
    }
    let pt = |i: usize| -> (f32, f32) { pts[i % n] };
    let is_on = |i: usize| -> bool { ons[i % n] };
    // 起点：首个 on 点；全 off → 首对相邻点中点
    let s = (0..n).find(|&i| is_on(i));
    let (start_pt, s) = match s {
        Some(i) => (pt(i), i),
        None => (midpoint(pt(0), pt(1)), 0),
    };
    let mut poly: Vec<(f32, f32)> = Vec::new();
    poly.push(start_pt);
    let mut ctrl: Option<(f32, f32)> = None;
    for k in 1..=n {
        let i = s + k;
        let cur = pt(i);
        // poly 非空不变式：起点已落，quad_split/每次追加均保持
        let last = match poly.last() {
            Some(&p) => p,
            None => start_pt,
        };
        if is_on(i) {
            match ctrl.take() {
                Some(c) => quad_split(last, c, cur, &mut poly),
                None => poly.push(cur),
            }
        } else {
            match ctrl {
                None => ctrl = Some(cur),
                Some(pc) => {
                    let mid = midpoint(pc, cur);
                    quad_split(last, pc, mid, &mut poly);
                    poly.push(mid);
                    ctrl = Some(cur);
                }
            }
        }
    }
    if let Some(c) = ctrl {
        // 收尾：控制点挂起 → quadratic 回到起点
        let last = poly.last().copied().unwrap_or(start_pt);
        quad_split(last, c, start_pt, &mut poly);
    }
    if poly.len() >= 3 {
        subs.push(Subpath {
            pts: poly,
            closed: true,
        });
    }
}

fn midpoint(a: (f32, f32), b: (f32, f32)) -> (f32, f32) {
    ((a.0 + b.0) * 0.5, (a.1 + b.1) * 0.5)
}

/// quadratic (p0,c,p1) → 中点细分为两段直线（p0-m, m-p1）。
fn quad_split(p0: (f32, f32), c: (f32, f32), p1: (f32, f32), poly: &mut Vec<(f32, f32)>) {
    let m = midpoint(midpoint(p0, c), midpoint(c, p1));
    poly.push(m);
    poly.push(p1);
}
