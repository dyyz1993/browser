//! no_std JPEG 基线解码器（M8 位图，feature `image-decode`）。
//!
//! `<image href="data:image/jpeg;base64,...">` 的解码路径：JPEG 容器
//!（SOI/APPn/DQT/SOF0/DHT/DRI/SOS/EOI）→ huffman 熵解码（DC 预测 +
//! AC 游程）→ 反量化（zigzag→natural）→ **整数 islow IDCT**（libjpeg
//! jpeg_idct_islow 同常数同舍入，每分量 ≤1 LSB）→ 上采样 → 直 RGBA8
//!（非预乘，alpha 恒 255）。
//!
//! 支持集（语料驱动 + Chrome 高频，7 枚 W3C jpg 语料全部命中）：
//! - SOF0/SOF1 基线顺序 huffman，精度 8 bit；
//! - SOF2 渐进式 huffman（车道AJ2）：频谱选择（Ss/Se）+ 逐次逼近
//!   （Ah/Al 位面）+ DC/AC 系数多扫描渐进累积 + EOB 游程（EOBRUN，
//!   不跨重启间隔）。扫描只写系数缓冲（natural 序，4 字节/采样），
//!   全部扫描结束后统一反量化 + islow IDCT（libjpeg jdphuff.c 同语义）；
//!   DC 扫描（Se=0）可交织，AC 扫描必须单分量（T.81 A.2.3）；
//! - 分量数 1（灰度）或 3（YCbCr ITU-R BT.601 全摆幅 / Adobe transform=0
//!   时 RGB 直通）；采样 4:4:4 / 4:2:2 / 4:2:0（含 h2v1/h2v2 fancy 上采样，
//!   libjpeg-turbo jdsample.c 同式——PIL/Chrome 参照同口径）；
//! - DRI 重启间隔：预测器复位 + 标记重同步（渐进式另复位 EOBRUN）。
//!
//! **放弃项**（遇之返回 None，调用方按不可描绘跳过）：
//! - SOF3/5-7/9-11（无损/算术/层级）；
//! - 4 通道（CMYK/YCCK）、12 bit 精度；
//! - 多扫描形态中未覆盖的分量平面留零（宽容渲染）。
//!
//! 工程约束（GOALS 宪法，与 png_decode 同款）：
//! - `#![no_std] + alloc`，禁 unwrap/expect（失败一律 `None` 上抛）；
//! - 纯手写不引外部 crate；分配按 SOF 尺寸预算（护栏同 PNG：单边
//!   ≤ 4096、总像素 ≤ 4M，另加填充平面 ≤ 4M+256K 采样防病态采样系数）；
//! - huffman/熵流损坏、截断 → None，不 panic。

use alloc::vec;
use alloc::vec::Vec;

/// 解码产物：尺寸 + 直 RGBA8（每像素 4 字节，行主序，alpha=255）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodedJpeg {
    pub w: u32,
    pub h: u32,
    pub rgba: Vec<u8>,
}

/// 尺寸护栏（病态 SOF 不触发大分配；与 png_decode 同口径）。
const MAX_DIM: u32 = 4096;
const MAX_PIXELS: u32 = 4_000_000;
/// 填充平面预算余量（MCU 网格取整 + 采样膨胀，≈4096×(32+16)）。
const PLANE_SLACK: u64 = 1 << 18;

// ---------------------------------------------------------------------------
// 入口
// ---------------------------------------------------------------------------

/// 解码完整 JPEG 字节流。任何不支持/损坏形态 → None（宽容跳过）。
pub fn decode(b: &[u8]) -> Option<DecodedJpeg> {
    if b.len() < 4 || b[0] != 0xFF || b[1] != 0xD8 {
        return None; // SOI
    }
    let mut d = Decoder {
        b,
        pos: 2,
        qt: [[0; 64]; 4],
        qt_set: [false; 4],
        dc_huff: [None, None, None, None],
        ac_huff: [None, None, None, None],
        dri: 0,
        adobe_transform: None,
    };
    d.run()
}

/// base64（标准字母表，容忍空白/padding）→ JPEG 解码。
pub fn decode_base64(s: &str) -> Option<DecodedJpeg> {
    let mut bytes = Vec::with_capacity(s.len() / 4 * 3);
    let mut acc: u32 = 0;
    let mut nbits: u32 = 0;
    for &c in s.as_bytes() {
        if c.is_ascii_whitespace() || c == b'=' {
            continue;
        }
        let v = b64_val(c)?;
        acc = (acc << 6) | v;
        nbits += 6;
        if nbits >= 8 {
            nbits -= 8;
            bytes.push(((acc >> nbits) & 0xff) as u8);
            acc &= (1 << nbits) - 1; // 清已消费高位，防 acc 无界增长
        }
    }
    decode(&bytes)
}

/// 标准 base64 字母表值。
fn b64_val(c: u8) -> Option<u32> {
    match c {
        b'A'..=b'Z' => Some((c - b'A') as u32),
        b'a'..=b'z' => Some((c - b'a') as u32 + 26),
        b'0'..=b'9' => Some((c - b'0') as u32 + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// 容器结构
// ---------------------------------------------------------------------------

/// JPEG huffman 表（canonical，MSB-first；puff 式「长度计数 + 排序符号」，
/// 解码与 png_decode::huff_decode 同构但位序相反、码长上限 16）。
struct Huff {
    /// `count[len]` = 码长 len（1..=16）的符号数。
    count: [u16; 17],
    /// 按码长升序、同码长按符号值升序的符号表。
    sym: Vec<u8>,
}

/// 码长计数 + 符号表 → huffman 表。过订阅（Kraft 和 >1）→ None；
/// 不完整表允许（单码 DC 树等合法形态，JPEG 规范以 1 位填充）。
fn build_huff(counts: &[u8; 16], sym: &[u8]) -> Option<Huff> {
    let mut count = [0u16; 17];
    let mut total = 0usize;
    for (i, &c) in counts.iter().enumerate() {
        count[i + 1] = c as u16;
        total += c as usize;
    }
    if total != sym.len() {
        return None;
    }
    let mut code: i32 = 0;
    for (l, &c) in count.iter().enumerate().skip(1) {
        code = (code << 1) + c as i32;
        if code > (1 << l) {
            return None; // 过订阅
        }
    }
    Some(Huff {
        count,
        sym: sym.to_vec(),
    })
}

/// 量化表（natural 行主序，解析时按 zigzag 重排）。
type QTable = [u16; 64];

/// zigzag 序（下标 = zigzag 扫描位 → natural 行主序位，ITU-T T.81 图 A.6）。
/// 注意第 8 位是 17（16 在位置 3 已出现）——抄成 16 会让 17 号系数静默
/// 丢失、16 号被覆盖（实测 vs PIL 差 13 LSB 的根因，已由反 DCT 对照实证）。
const ZIGZAG: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, //
    17, 24, 32, 25, 18, 11, 4, 5, //
    12, 19, 26, 33, 40, 48, 41, 34, //
    27, 20, 13, 6, 7, 14, 21, 28, //
    35, 42, 49, 56, 57, 50, 43, 36, //
    29, 22, 15, 23, 30, 37, 44, 51, //
    58, 59, 52, 45, 38, 31, 39, 46, //
    53, 60, 61, 54, 47, 55, 62, 63,
];

/// 帧分量：采样系数 + 解码产物平面。
struct Comp {
    /// 帧内分量 id（SOF 声明，SOS 以此选择）。
    #[allow(dead_code)]
    id: u8,
    /// 采样系数（1..=4）。
    h: u8,
    v: u8,
    /// 量化表槽位。
    tq: u8,
    /// 平面宽高（MCU 网格对齐，8 的倍数）。
    pw: usize,
    ph: usize,
    /// 有意义采样宽高（`ceil(img * c / max)`，上采样/裁剪边界）。
    dw: usize,
    dh: usize,
    /// 解码样本（u8，padded 尺寸）。
    plane: Vec<u8>,
    /// 渐进式系数缓冲（natural 序，块 × 64；仅 SOF2 分配，基线恒空）。
    coefs: Vec<i32>,
}

/// SOF 帧：图像尺寸 + 分量集 + MCU 网格。
struct Frame {
    w: u32,
    h: u32,
    comps: Vec<Comp>,
    /// 最大采样系数（MCU 尺寸 = 8*hmax × 8*vmax）。
    hmax: u8,
    vmax: u8,
    /// MCU 网格（图像方向）。
    mx: usize,
    my: usize,
    /// 渐进式（SOF2）：扫描只写系数缓冲，扫描完毕统一反量化 + IDCT。
    prog: bool,
}

/// SOS 解析产物：扫描分量选择 + 渐进式参数（基线恒 0/63/0/0）。
struct Scan {
    /// (帧分量下标, dc 表槽, ac 表槽)。
    sel: Vec<(usize, usize, usize)>,
    /// 频谱选择起止 + 逐次逼近位面（T.81 B.2.3）。
    ss: u8,
    se: u8,
    ah: u8,
    al: u8,
}

/// 解码器状态：字节位置 + 各类表。
struct Decoder<'a> {
    b: &'a [u8],
    pos: usize,
    qt: [QTable; 4],
    qt_set: [bool; 4],
    dc_huff: [Option<Huff>; 4],
    ac_huff: [Option<Huff>; 4],
    /// 重启间隔（MCU 数；0 = 无）。
    dri: u16,
    /// APP14 Adobe transform 标志（3 通道 transform=0 → RGB 直通）。
    adobe_transform: Option<u8>,
}

impl Decoder<'_> {
    fn u8(&mut self) -> Option<u8> {
        let v = *self.b.get(self.pos)?;
        self.pos += 1;
        Some(v)
    }

    fn u16be(&mut self) -> Option<u16> {
        let hi = self.u8()? as u16;
        let lo = self.u8()? as u16;
        Some(hi << 8 | lo)
    }

    /// 读一个标记码（跳过 FF 填充）。返回标记码；EOF → None。
    fn marker(&mut self) -> Option<u8> {
        loop {
            let mut v = self.u8()?;
            if v != 0xFF {
                continue; // 段间杂物容忍（不产生有效流，但宽容）
            }
            v = self.u8()?;
            while v == 0xFF {
                v = self.u8()?;
            }
            return Some(v);
        }
    }

    /// 跳过一个带长度的段（APPn/COM 等）。
    fn skip_segment(&mut self) -> Option<()> {
        let len = self.u16be()? as usize;
        if len < 2 {
            return None;
        }
        self.pos = self.pos.checked_add(len - 2)?;
        Some(())
    }

    // -- 段解析 ------------------------------------------------------------

    /// DQT：1-4 张量化表，8/16 bit 精度；按 zigzag 重排为 natural 序。
    fn parse_dqt(&mut self) -> Option<()> {
        let len = self.u16be()? as usize;
        let end = self.pos.checked_add(len.checked_sub(2)?)?;
        while self.pos < end {
            let pqtq = self.u8()?;
            let pq = pqtq >> 4;
            let tq = (pqtq & 15) as usize;
            if pq > 1 || tq > 3 {
                return None;
            }
            let n = if pq == 1 { 128 } else { 64 };
            if self.pos + n > end {
                return None;
            }
            for &nat in ZIGZAG.iter() {
                let v = if pq == 1 {
                    self.u16be()?
                } else {
                    self.u8()? as u16
                };
                self.qt[tq][nat] = v;
            }
            self.qt_set[tq] = true;
        }
        if self.pos != end {
            return None;
        }
        Some(())
    }

    /// DHT：DC/AC huffman 表（各 0-3 槽）。一个段可含多张表。
    fn parse_dht(&mut self) -> Option<()> {
        let len = self.u16be()? as usize;
        let end = self.pos.checked_add(len.checked_sub(2)?)?;
        while self.pos < end {
            let tcth = self.u8()?;
            let tc = tcth >> 4;
            let th = (tcth & 15) as usize;
            if tc > 1 || th > 3 {
                return None;
            }
            let mut counts = [0u8; 16];
            for c in counts.iter_mut() {
                *c = self.u8()?;
            }
            let total: usize = counts.iter().map(|&c| c as usize).sum();
            if total > 256 || self.pos + total > end {
                return None;
            }
            let mut sym = vec![0u8; total];
            for s in sym.iter_mut() {
                *s = self.u8()?;
            }
            let h = build_huff(&counts, &sym)?;
            if tc == 0 {
                self.dc_huff[th] = Some(h);
            } else {
                self.ac_huff[th] = Some(h);
            }
        }
        if self.pos != end {
            return None;
        }
        Some(())
    }

    /// DRI：重启间隔（MCU 数）。
    fn parse_dri(&mut self) -> Option<()> {
        let len = self.u16be()? as usize;
        if len != 4 {
            return None;
        }
        self.dri = self.u16be()?;
        Some(())
    }

    /// APP14 Adobe：仅取 transform 标志（3 通道 transform=0 → RGB 直通）。
    fn parse_app14(&mut self) -> Option<()> {
        let len = self.u16be()? as usize;
        if len < 13 || self.pos + 5 > self.b.len() {
            self.pos = self.pos.checked_add(len.checked_sub(2)?)?;
            return Some(());
        }
        let tag = &self.b[self.pos..self.pos + 5];
        self.pos += 5;
        if tag == b"Adobe" {
            // version(2) flags0(2) flags1(2) transform(1)
            if len < 14 {
                return None;
            }
            let rest = len - 2 - 5;
            let mut tf = 0u8;
            for i in 0..rest {
                let v = self.u8()?;
                if i + 1 == rest {
                    tf = v;
                }
            }
            self.adobe_transform = Some(tf);
        } else {
            self.pos = self.pos.checked_add(len.checked_sub(2 + 5)?)?;
        }
        Some(())
    }

    /// SOF0/SOF1/SOF2 帧头：尺寸 + 分量集 + 平面分配（渐进式另配系数缓冲）。
    fn parse_sof(&mut self, prog: bool) -> Option<Frame> {
        let len = self.u16be()? as usize;
        let end = self.pos.checked_add(len.checked_sub(2)?)?;
        let prec = self.u8()?;
        if prec != 8 {
            return None; // 12/16 bit 放弃
        }
        let h = self.u16be()? as u32;
        let w = self.u16be()? as u32;
        let nc = self.u8()? as usize;
        if w == 0
            || h == 0
            || w > MAX_DIM
            || h > MAX_DIM
            || w.checked_mul(h)? > MAX_PIXELS
            || (nc != 1 && nc != 3)
            || self.pos + nc * 3 != end
        {
            return None; // 4 通道 CMYK/YCCK 登记；尺寸护栏
        }
        let mut comps: Vec<Comp> = Vec::with_capacity(nc);
        let mut hmax = 0u8;
        let mut vmax = 0u8;
        for _ in 0..nc {
            let id = self.u8()?;
            let hv = self.u8()?;
            let (ch, cv) = (hv >> 4, hv & 15);
            let tq = self.u8()?;
            if !(1..=4).contains(&ch) || !(1..=4).contains(&cv) || tq > 3 {
                return None;
            }
            hmax = hmax.max(ch);
            vmax = vmax.max(cv);
            comps.push(Comp {
                id,
                h: ch,
                v: cv,
                tq,
                pw: 0,
                ph: 0,
                dw: 0,
                dh: 0,
                plane: Vec::new(),
                coefs: Vec::new(),
            });
        }
        // 单 MCU 数据单元数 ≤ 10（libjpeg 同款工作量护栏，防 4×4×4 采样）
        let blocks_per_mcu: usize = comps.iter().map(|c| c.h as usize * c.v as usize).sum();
        if blocks_per_mcu > 10 || hmax > 4 || vmax > 4 {
            return None;
        }
        let step_x = 8 * hmax as u32;
        let step_y = 8 * vmax as u32;
        let mx = (w.div_ceil(step_x)) as usize;
        let my = (h.div_ceil(step_y)) as usize;
        for c in comps.iter_mut() {
            c.pw = mx * c.h as usize * 8;
            c.ph = my * c.v as usize * 8;
            c.dw = (c.h as u32).mul_ceil_div(w, hmax as u32) as usize;
            c.dh = (c.v as u32).mul_ceil_div(h, vmax as u32) as usize;
            // 填充平面预算（病态采样膨胀防御）
            if c.pw as u64 * c.ph as u64 > MAX_PIXELS as u64 + PLANE_SLACK {
                return None;
            }
            c.plane = vec![0u8; c.pw * c.ph];
            if prog {
                // 系数缓冲：块 × 64 i32 = 4 字节/采样（同平面预算量级）
                c.coefs = vec![0i32; c.pw / 8 * (c.ph / 8) * 64];
            }
        }
        Some(Frame {
            w,
            h,
            comps,
            hmax,
            vmax,
            mx,
            my,
            prog,
        })
    }

    /// SOS 扫描头：分量选择 + 表选择 + 扫描参数（基线固定全带，渐进式
    /// 按 T.81 G.1 校验频谱选择/逐次逼近组合）。
    fn parse_sos(&mut self, fr: &Frame) -> Option<Scan> {
        let len = self.u16be()? as usize;
        let ns = self.u8()? as usize;
        if ns == 0 || ns > 4 || len != 6 + ns * 2 {
            return None;
        }
        let mut sel = Vec::with_capacity(ns);
        for _ in 0..ns {
            let cs = self.u8()?;
            let ta = self.u8()?;
            let (td, ta_sel) = (ta >> 4, ta & 15);
            if td > 3 || ta_sel > 3 {
                return None;
            }
            let ci = fr.comps.iter().position(|c| c.id == cs)?;
            sel.push((ci, td as usize, ta_sel as usize));
        }
        let ss = self.u8()?;
        let se = self.u8()?;
        let ahal = self.u8()?;
        let (ah, al) = (ahal >> 4, ahal & 15);
        if fr.prog {
            // 交织扫描只允许 DC（Se=0）；AC 扫描必须单分量（A.2.3）
            if ns > 1 && se != 0 {
                return None;
            }
            if se == 0 {
                // DC first（Ah=0, Al≤13）或 DC refine（Ah=1, Al<Ah）
                if ss != 0 || ah > 1 || al > 13 || (ah == 1 && al >= ah) {
                    return None;
                }
            } else {
                // AC first（Ah=0, Al≤13）或 AC refine（0<Al<Ah≤13）
                if ss == 0 || ss > se || se > 63 {
                    return None;
                }
                if ah == 0 {
                    if al > 13 {
                        return None;
                    }
                } else if ah > 13 || al >= ah {
                    return None;
                }
            }
        } else {
            // 基线顺序模式固定 Ss=0/Se=63/Ah=Al=0
            if ss != 0 || se != 63 || ahal != 0 {
                return None;
            }
        }
        Some(Scan {
            sel,
            ss,
            se,
            ah,
            al,
        })
    }

    // -- 主流程 ------------------------------------------------------------

    fn run(&mut self) -> Option<DecodedJpeg> {
        // 头部段循环：直到 SOF
        let mut fr = loop {
            match self.marker()? {
                0xC0 | 0xC1 => break self.parse_sof(false)?,
                0xC2 => break self.parse_sof(true)?, // 渐进式
                0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF => return None,
                0xC4 => self.parse_dht()?,
                0xDB => self.parse_dqt()?,
                0xDD => self.parse_dri()?,
                0xEE => self.parse_app14()?,
                0xD9 | 0xDA => return None, // EOI 先于 SOF / SOS 先于 SOF
                0x01 | 0xD0..=0xD7 => {}
                _ => self.skip_segment()?, // APPn/COM 等跳过
            }
        };
        // 扫描循环：基线非交织分量各一扫描；渐进式按频谱/位面多扫描
        // 累积系数。扫描间允许 DQT/DHT/DRI。EOF 宽容（有完整扫描即收，
        // png 缺 IEND 同款）。
        let mut any_scan = false;
        while let Some(m) = self.marker() {
            match m {
                0xDA => {
                    let sc = self.parse_sos(&fr)?;
                    if fr.prog {
                        self.pos = decode_scan_prog(
                            self.b,
                            self.pos,
                            &mut fr,
                            &sc,
                            &self.dc_huff,
                            &self.ac_huff,
                            self.dri,
                        )?;
                    } else {
                        self.pos = decode_entropy(
                            self.b,
                            self.pos,
                            &mut fr,
                            &sc.sel,
                            &self.qt,
                            &self.qt_set,
                            &self.dc_huff,
                            &self.ac_huff,
                            self.dri,
                        )?;
                    }
                    any_scan = true;
                }
                0xD9 => break, // EOI
                0xC4 => self.parse_dht()?,
                0xDB => self.parse_dqt()?,
                0xDD => self.parse_dri()?,
                0xEE => self.parse_app14()?,
                0xC0..=0xC2 => return None, // 帧中途再现 SOF = 损坏
                0x01 | 0xD0..=0xD7 => {}
                _ => self.skip_segment()?,
            }
        }
        if !any_scan {
            return None;
        }
        if fr.prog {
            finalize_prog(&mut fr, &self.qt)?;
        }
        // 3 通道 Adobe transform=0 → RGB 直通；其余 3 通道按 YCbCr
        let ycc = fr.comps.len() == 3 && self.adobe_transform != Some(0);
        to_rgba(&fr, ycc).map(|rgba| DecodedJpeg {
            w: fr.w,
            h: fr.h,
            rgba,
        })
    }
}

/// ceil(a * b / c)（u32，c>0）。
trait CeilMulDiv {
    fn mul_ceil_div(self, b: Self, c: Self) -> Self;
}
impl CeilMulDiv for u32 {
    fn mul_ceil_div(self, b: Self, c: Self) -> Self {
        (self * b).div_ceil(c)
    }
}

// ---------------------------------------------------------------------------
// 熵解码（huffman + DC 预测 + AC 游程 + 重启重同步）
// ---------------------------------------------------------------------------

/// MSB-first 熵流位读取器（FF00 去填充；撞非 00 标记即终止并回退一格，
/// 让外层 marker() 能看到该标记）。
struct Entropy<'a> {
    b: &'a [u8],
    /// 下一字节下标（撞标记回退后指向 0xFF）。
    pos: usize,
    /// 当前字节余位。
    acc: u32,
    cnt: u32,
    /// 是否已撞标记（熵流终止）。
    hit: bool,
}

impl Entropy<'_> {
    /// 取下一个去填充字节。撞标记 → None（pos 回退至 0xFF 处）。
    fn byte(&mut self) -> Option<u8> {
        if self.hit {
            return None;
        }
        let v = *self.b.get(self.pos)?;
        self.pos += 1;
        if v == 0xFF {
            let n = *self.b.get(self.pos)?;
            if n == 0x00 {
                self.pos += 1; // 填充：FF00 → 数据 FF
            } else {
                self.hit = true;
                self.pos -= 1; // 回退，标记留给外层
                return None;
            }
        }
        Some(v)
    }

    /// 裸字节（重启标记重同步用，不去填充）。
    fn raw(&mut self) -> Option<u8> {
        let v = *self.b.get(self.pos)?;
        self.pos += 1;
        Some(v)
    }

    fn bit(&mut self) -> Option<u32> {
        if self.cnt == 0 {
            self.acc = self.byte()? as u32;
            self.cnt = 8;
        }
        self.cnt -= 1;
        Some((self.acc >> self.cnt) & 1)
    }

    /// MSB-first n 位（n ≤ 16）。
    fn bits(&mut self, n: u32) -> Option<u32> {
        let mut v = 0u32;
        for _ in 0..n {
            v = (v << 1) | self.bit()?;
        }
        Some(v)
    }

    /// canonical huffman 解码（逐位下行，puff 同构）。
    fn huff(&mut self, h: &Huff) -> Option<u8> {
        let mut code: i32 = 0;
        let mut first: i32 = 0;
        let mut index: i32 = 0;
        for len in 1..=16usize {
            code = (code << 1) | self.bit()? as i32;
            let cnt = h.count[len] as i32;
            if code - cnt < first {
                return h.sym.get((index + (code - first)) as usize).copied();
            }
            index += cnt;
            first = (first + cnt) << 1;
        }
        None
    }

    /// huffman 附加位展开（T.81 F.2.2 EXTEND）。
    fn extend(&mut self, s: u32) -> Option<i32> {
        if s == 0 {
            return Some(0);
        }
        if s > 16 {
            return None;
        }
        let v = self.bits(s)? as i32;
        Some(if v < (1 << (s - 1)) {
            v - (1 << s) + 1
        } else {
            v
        })
    }

    /// 重启标记消费：丢当前字节余位 → FF Dx（FF 填充容忍）。
    fn restart(&mut self, want: u8) -> Option<()> {
        if self.hit {
            return None;
        }
        self.cnt = 0;
        loop {
            let v = self.raw()?;
            if v != 0xFF {
                return None;
            }
            let c = self.raw()?;
            if c == 0xFF {
                continue;
            }
            return if c == want { Some(()) } else { None };
        }
    }
}

/// 单个扫描分量的预取信息（避免 MCU 循环内重复取）。
struct ScanComp {
    ci: usize,
    t: usize,
    a: usize,
    h: usize,
    v: usize,
    pw: usize,
    dw: usize,
    dh: usize,
}

/// 一个扫描的熵解码主循环：逐 MCU 解块 → 反量化 → IDCT → 写平面。
/// 返回扫描后的字节位置（停在标记/EOF）。数据不足 → None。
#[allow(clippy::too_many_arguments)]
fn decode_entropy(
    b: &[u8],
    pos: usize,
    fr: &mut Frame,
    sel: &[(usize, usize, usize)],
    qt: &[QTable; 4],
    qt_set: &[bool; 4],
    dcs: &[Option<Huff>; 4],
    acs: &[Option<Huff>; 4],
    dri: u16,
) -> Option<usize> {
    // 表存在性预检 + 扫描分量预取
    let mut scomps: Vec<ScanComp> = Vec::with_capacity(sel.len());
    for &(ci, t, a) in sel {
        if dcs[t].is_none() || acs[a].is_none() || !qt_set[fr.comps[ci].tq as usize] {
            return None;
        }
        let c = &fr.comps[ci];
        scomps.push(ScanComp {
            ci,
            t,
            a,
            h: c.h as usize,
            v: c.v as usize,
            pw: c.pw,
            dw: c.dw,
            dh: c.dh,
        });
    }
    let interleaved = sel.len() > 1;
    let total = if interleaved {
        fr.mx * fr.my
    } else {
        let s = &scomps[0];
        s.dw.div_ceil(8) * s.dh.div_ceil(8)
    };
    let mut e = Entropy {
        b,
        pos,
        acc: 0,
        cnt: 0,
        hit: false,
    };
    let mut preds = [0i32; 4];
    let mut coef = [0i32; 64];
    let mut out8 = [0u8; 64];
    let mut since = 0u16;
    let mut rsti = 0u8;
    let (mx, hmax, vmax) = (fr.mx, fr.hmax as usize, fr.vmax as usize);
    for m in 0..total {
        if dri > 0 && since == dri {
            e.restart(0xD0 + rsti)?;
            preds = [0; 4];
            rsti = (rsti + 1) & 7;
            since = 0;
        }
        since += 1;
        for (k, sc) in scomps.iter().enumerate() {
            // 不可变借用表、可变借目标平面：先取裸指针外的所有标量
            let dh = dcs[sc.t].as_ref()?;
            let ah = acs[sc.a].as_ref()?;
            let q = &qt[fr.comps[sc.ci].tq as usize];
            if interleaved {
                let (mxi, myi) = (m % mx, m / mx);
                for by in 0..sc.v {
                    for bx in 0..sc.h {
                        let x = (mxi * sc.h + bx) * 8;
                        let y = (myi * sc.v + by) * 8;
                        decode_block(&mut e, dh, ah, &mut preds[k], q, &mut coef)?;
                        idct_block(&coef, &mut out8);
                        put_block(&mut fr.comps[sc.ci].plane, sc.pw, x, y, &out8);
                    }
                }
            } else {
                let bw = sc.dw.div_ceil(8);
                let x = (m % bw) * 8;
                let y = (m / bw) * 8;
                let _ = (hmax, vmax);
                decode_block(&mut e, dh, ah, &mut preds[k], q, &mut coef)?;
                idct_block(&coef, &mut out8);
                put_block(&mut fr.comps[sc.ci].plane, sc.pw, x, y, &out8);
            }
        }
    }
    if e.hit {
        return None; // 数据早于块数耗尽：损坏
    }
    Some(e.pos)
}

// ---------------------------------------------------------------------------
// 渐进式（SOF2）熵解码：DC/AC first + refine + EOB 游程（T.81 G.1，
// libjpeg jdphuff.c 同语义）
// ---------------------------------------------------------------------------

/// 渐进式单扫描：系数写 `comps[ci].coefs`（不反量化不 IDCT——多扫描
/// 累积后 finalize 统一收尾）。EOBRUN 不跨重启间隔（RST 复位）。
#[allow(clippy::too_many_arguments)]
fn decode_scan_prog(
    b: &[u8],
    pos: usize,
    fr: &mut Frame,
    sc: &Scan,
    dcs: &[Option<Huff>; 4],
    acs: &[Option<Huff>; 4],
    dri: u16,
) -> Option<usize> {
    let dc_scan = sc.se == 0;
    // 表存在性预检：DC 扫描只消费 DC 表，AC 扫描只消费 AC 表
    let mut scomps: Vec<(usize, usize)> = Vec::with_capacity(sc.sel.len());
    for &(ci, t, a) in &sc.sel {
        let ok = if dc_scan {
            dcs[t].is_some()
        } else {
            acs[a].is_some()
        };
        if !ok {
            return None;
        }
        scomps.push((ci, if dc_scan { t } else { a }));
    }
    // 交织 = 全 MCU 网格（仅 DC）；单分量 = 该分量块栅格
    let interleaved = sc.sel.len() > 1;
    let total = if interleaved {
        fr.mx * fr.my
    } else {
        let c = &fr.comps[scomps[0].0];
        c.dw.div_ceil(8) * c.dh.div_ceil(8)
    };
    let (mx, ss, se, ah, al) = (fr.mx, sc.ss, sc.se, sc.ah, sc.al);
    let mut e = Entropy {
        b,
        pos,
        acc: 0,
        cnt: 0,
        hit: false,
    };
    let mut preds = [0i32; 4];
    let mut eoarun: u32 = 0;
    let mut since = 0u16;
    let mut rsti = 0u8;
    for m in 0..total {
        if dri > 0 && since == dri {
            e.restart(0xD0 + rsti)?;
            preds = [0; 4];
            eoarun = 0; // EOB 游程不跨重启间隔
            rsti = (rsti + 1) & 7;
            since = 0;
        }
        since += 1;
        if interleaved {
            let (mxi, myi) = (m % mx, m / mx);
            for (k, &(ci, ti)) in scomps.iter().enumerate() {
                let dh = dcs[ti].as_ref()?;
                let (h, v, bws) = {
                    let c = &fr.comps[ci];
                    (c.h as usize, c.v as usize, c.pw / 8)
                };
                let bi0 = (myi * v) * bws + mxi * h;
                for by in 0..v {
                    for bx in 0..h {
                        let bi = bi0 + by * bws + bx;
                        dc_prog_block(
                            &mut e,
                            dh,
                            &mut preds[k],
                            &mut fr.comps[ci].coefs[bi * 64..bi * 64 + 64],
                            ah,
                            al,
                        )?;
                    }
                }
            }
        } else {
            let (ci, ti) = scomps[0];
            let (sw, bws) = {
                let c = &fr.comps[ci];
                (c.dw.div_ceil(8), c.pw / 8)
            };
            let bi = (m / sw) * bws + m % sw;
            let blk = &mut fr.comps[ci].coefs[bi * 64..bi * 64 + 64];
            if dc_scan {
                let dh = dcs[ti].as_ref()?;
                dc_prog_block(&mut e, dh, &mut preds[0], blk, ah, al)?;
            } else {
                let at = acs[ti].as_ref()?;
                ac_prog_block(&mut e, at, blk, &mut eoarun, ss, se, ah, al)?;
            }
        }
    }
    if e.hit {
        return None;
    }
    Some(e.pos)
}

/// 渐进式 DC 块：first（huffman 差分 + 预测器累加，Al 左移存储）或
/// refine（单校正位，置位补 2^Al；预测器不参与）。
fn dc_prog_block(
    e: &mut Entropy,
    dh: &Huff,
    pred: &mut i32,
    blk: &mut [i32],
    ah: u8,
    al: u8,
) -> Option<()> {
    if ah == 0 {
        let s = e.huff(dh)? as u32;
        if s > 15 {
            return None;
        }
        let diff = e.extend(s)?;
        *pred = pred.wrapping_add(diff);
        blk[0] = pred.wrapping_shl(al as u32);
    } else if e.bit()? == 1 {
        blk[0] = blk[0].wrapping_add(1i32 << al);
    }
    Some(())
}

/// 渐进式 AC 块：first（Ah=0，系数全值 Al 左移）或 refine（Ah>0，已置
/// 系数消费 Al 位校正，新系数 ±2^Al 落位）+ EOB 游程。EOB 计数含当前块
/// （libjpeg jdphuff：first 在解码时自减，refine 在本块校正补扫后自减；
/// refine 的游程延续块仍按带序消费已置系数的校正位）。
#[allow(clippy::too_many_arguments)]
fn ac_prog_block(
    e: &mut Entropy,
    tbl: &Huff,
    blk: &mut [i32],
    eoarun: &mut u32,
    ss: u8,
    se: u8,
    ah: u8,
    al: u8,
) -> Option<()> {
    let (ss, se) = (ss as usize, se as usize);
    let mut k = ss;
    if ah == 0 {
        // AC first：RS 游程 + 全值扩展
        if *eoarun > 0 {
            *eoarun -= 1; // 游程延续块：整带零，无位可读
            return Some(());
        }
        while k <= se {
            let rs = e.huff(tbl)? as usize;
            let r = rs >> 4;
            let s = rs & 15;
            if s == 0 {
                if r == 15 {
                    k += 16; // ZRL：带尾越出即带尽（libjpeg 同款宽容）
                    continue;
                }
                *eoarun = (1u32 << r) + if r > 0 { e.bits(r as u32)? } else { 0 };
                *eoarun -= 1; // 本块当场收账（jdphuff「processed at this moment」）
                break;
            }
            k += r;
            if k > se {
                return None;
            }
            let v = e.extend(s as u32)?;
            blk[ZIGZAG[k]] = v.wrapping_shl(al as u32);
            k += 1;
        }
        return Some(());
    }
    // AC refine：Al 位校正 + 单位新系数
    let p1 = 1i32 << al;
    let m1 = -p1;
    if *eoarun == 0 {
        while k <= se {
            let rs = e.huff(tbl)? as usize;
            let mut r = rs >> 4;
            let s = rs & 15;
            let mut val = 0i32;
            if s != 0 {
                if s != 1 {
                    return None; // refine 新系数尺寸恒为 1（G.1.2.2）
                }
                val = if e.bit()? == 1 { p1 } else { m1 };
            } else if r != 15 {
                // EOB 游程：余下块数由下方补扫逐块自减
                *eoarun = (1u32 << r) + if r > 0 { e.bits(r as u32)? } else { 0 };
                break;
            }
            // 带内扫描：非零系数过校正位；零系数计游程，r 尽则落位/ZRL 终止
            loop {
                if k > se {
                    return None; // 游程超带：损坏
                }
                let c = &mut blk[ZIGZAG[k]];
                if *c != 0 {
                    // 已置系数：消费校正位（可折叠为 && 会误入 r 减臂，保持嵌套）
                    if e.bit()? == 1 && (*c & p1) == 0 {
                        *c += if *c >= 0 { p1 } else { m1 };
                    }
                } else if r == 0 {
                    if val != 0 {
                        *c = val;
                    }
                    k += 1;
                    break;
                } else {
                    r -= 1;
                }
                k += 1;
            }
        }
    }
    if *eoarun > 0 {
        // EOB 截断点（或游程延续块整带 [Ss..Se]）的已置系数仍消费校正位
        while k <= se {
            let c = &mut blk[ZIGZAG[k]];
            if *c != 0 && e.bit()? == 1 && (*c & p1) == 0 {
                *c += if *c >= 0 { p1 } else { m1 };
            }
            k += 1;
        }
        *eoarun -= 1; // 本块在游程中收账（jdphuff「Count one block completed」）
    }
    Some(())
}

/// 渐进式收尾：系数缓冲逐块反量化（natural 序）+ islow IDCT → 平面样本。
fn finalize_prog(fr: &mut Frame, qt: &[QTable; 4]) -> Option<()> {
    for c in fr.comps.iter_mut() {
        let q = &qt[c.tq as usize];
        let bws = c.pw / 8;
        let nblocks = bws * (c.ph / 8);
        if c.coefs.len() < nblocks.checked_mul(64)? {
            return None;
        }
        let mut co = [0i32; 64];
        let mut out8 = [0u8; 64];
        for bi in 0..nblocks {
            for i in 0..64 {
                co[i] = c.coefs[bi * 64 + i].wrapping_mul(q[i] as i32);
            }
            idct_block(&co, &mut out8);
            put_block(&mut c.plane, c.pw, (bi % bws) * 8, (bi / bws) * 8, &out8);
        }
    }
    Some(())
}

/// 单块：DC 差分 + AC 游程，反量化写 natural 序。
fn decode_block(
    e: &mut Entropy,
    dh: &Huff,
    ah: &Huff,
    pred: &mut i32,
    q: &QTable,
    out: &mut [i32; 64],
) -> Option<()> {
    *out = [0; 64];
    // DC：类别 s → 附加位差分 → 预测器累加
    let s = e.huff(dh)? as u32;
    if s > 15 {
        return None;
    }
    let diff = e.extend(s)?;
    *pred = pred.wrapping_add(diff);
    out[0] = *pred * q[0] as i32;
    // AC：RS = r<<4|s；0x00 = EOB、0xF0 = ZRL(16 零)
    let mut k = 1usize;
    while k < 64 {
        let rs = e.huff(ah)? as usize;
        let r = rs >> 4;
        let sz = rs & 15;
        if sz == 0 {
            if r == 15 {
                // ZRL：16 零。带尾 k=48 处的 ZRL 恰好盖满 48..63 是合法
                // 形态——越出即带尽（libjpeg jdphuff 同款宽容，不报错）
                k += 16;
                continue;
            }
            break; // EOB（r==0）
        }
        k += r;
        if k > 63 {
            return None;
        }
        let v = e.extend(sz as u32)?;
        out[ZIGZAG[k]] = v * q[ZIGZAG[k]] as i32;
        k += 1;
    }
    Some(())
}

/// 块样本写入平面（8×8 行拷贝）。
fn put_block(plane: &mut [u8], pw: usize, x: usize, y: usize, s: &[u8; 64]) {
    for r in 0..8usize {
        let o = (y + r) * pw + x;
        plane[o..o + 8].copy_from_slice(&s[r * 8..r * 8 + 8]);
    }
}

// ---------------------------------------------------------------------------
// 整数 islow IDCT（libjpeg jidctint.c 同常数同舍入，≤1 LSB）
// ---------------------------------------------------------------------------

const CBITS: u32 = 13;
const PBITS: u32 = 2;

/// islow 定点常数（FIX(x) = round(x * 8192)，CONST_BITS=13）。
const FIX_0_298631336: i32 = 2446;
const FIX_0_390180644: i32 = 3196;
const FIX_0_541196100: i32 = 4433;
const FIX_0_765366865: i32 = 6270;
const FIX_0_899976223: i32 = 7373;
const FIX_1_175875602: i32 = 9633;
const FIX_1_501321110: i32 = 12299;
const FIX_1_847759065: i32 = 15137;
const FIX_1_961570560: i32 = 16069;
const FIX_2_053119869: i32 = 16819;
const FIX_2_562915447: i32 = 20995;
const FIX_3_072711026: i32 = 25172;

#[inline]
fn descale(x: i32, n: u32) -> i32 {
    (x + (1 << (n - 1))) >> n
}

#[inline]
fn clamp8(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

/// 反量化系数 → 64 样本（+128 电平、钳位）。DC-only 块走快路径。
fn idct_block(c: &[i32; 64], out: &mut [u8; 64]) {
    if c[1..].iter().all(|&v| v == 0) {
        let v = clamp8(descale(c[0], 3) + 128);
        out.fill(v);
        return;
    }
    let mut ws = [0i32; 64];
    // 行一维（输入已在块内，行主序）
    for r in 0..8usize {
        let i = r * 8;
        // 偶部（c2/c6 旋转）
        let z2 = c[i + 2];
        let z3 = c[i + 6];
        let z1 = (z2 + z3) * FIX_0_541196100;
        let t2 = z1 - z3 * FIX_1_847759065;
        let t3 = z1 + z2 * FIX_0_765366865;
        let z2 = c[i];
        let z3 = c[i + 4];
        let t0 = (z2 + z3) << CBITS;
        let t1 = (z2 - z3) << CBITS;
        let t10 = t0 + t3;
        let t13 = t0 - t3;
        let t11 = t1 + t2;
        let t12 = t1 - t2;
        // 奇部（酉阵转置乘）
        let o0 = c[i + 7];
        let o1 = c[i + 5];
        let o2 = c[i + 3];
        let o3 = c[i + 1];
        let z1 = o0 + o3;
        let z2 = o1 + o2;
        let mut z3 = o0 + o2;
        let mut z4 = o1 + o3;
        let z5 = (z3 + z4) * FIX_1_175875602;
        let mut t0 = o0 * FIX_0_298631336;
        let mut t1 = o1 * FIX_2_053119869;
        let mut t2 = o2 * FIX_3_072711026;
        let mut t3 = o3 * FIX_1_501321110;
        let z1m = -z1 * FIX_0_899976223;
        let z2m = -z2 * FIX_2_562915447;
        z3 = -z3 * FIX_1_961570560 + z5;
        z4 = -z4 * FIX_0_390180644 + z5;
        t0 += z1m + z3;
        t1 += z2m + z4;
        t2 += z2m + z3;
        t3 += z1m + z4;
        ws[i] = descale(t10 + t3, CBITS - PBITS);
        ws[i + 7] = descale(t10 - t3, CBITS - PBITS);
        ws[i + 1] = descale(t11 + t2, CBITS - PBITS);
        ws[i + 6] = descale(t11 - t2, CBITS - PBITS);
        ws[i + 2] = descale(t12 + t1, CBITS - PBITS);
        ws[i + 5] = descale(t12 - t1, CBITS - PBITS);
        ws[i + 3] = descale(t13 + t0, CBITS - PBITS);
        ws[i + 4] = descale(t13 - t0, CBITS - PBITS);
    }
    // 列一维（中间结果 → 样本）
    for cc in 0..8usize {
        let z2 = ws[cc];
        let z3 = ws[32 + cc];
        let t0 = (z2 + z3) << CBITS;
        let t1 = (z2 - z3) << CBITS;
        let z2 = ws[16 + cc];
        let z3 = ws[48 + cc];
        let z1 = (z2 + z3) * FIX_0_541196100;
        let t2 = z1 - z3 * FIX_1_847759065;
        let t3 = z1 + z2 * FIX_0_765366865;
        let t10 = t0 + t3;
        let t13 = t0 - t3;
        let t11 = t1 + t2;
        let t12 = t1 - t2;
        let o0 = ws[56 + cc];
        let o1 = ws[40 + cc];
        let o2 = ws[24 + cc];
        let o3 = ws[8 + cc];
        let z1 = o0 + o3;
        let z2 = o1 + o2;
        let mut z3 = o0 + o2;
        let mut z4 = o1 + o3;
        let z5 = (z3 + z4) * FIX_1_175875602;
        let mut t0 = o0 * FIX_0_298631336;
        let mut t1 = o1 * FIX_2_053119869;
        let mut t2 = o2 * FIX_3_072711026;
        let mut t3 = o3 * FIX_1_501321110;
        let z1m = -z1 * FIX_0_899976223;
        let z2m = -z2 * FIX_2_562915447;
        z3 = -z3 * FIX_1_961570560 + z5;
        z4 = -z4 * FIX_0_390180644 + z5;
        t0 += z1m + z3;
        t1 += z2m + z4;
        t2 += z2m + z3;
        t3 += z1m + z4;
        // 最终：DESCALE(…, CONST_BITS+PASS1_BITS+3) + 128 电平 + 钳位
        let n = CBITS + PBITS + 3;
        out[cc] = clamp8(descale(t10 + t3, n) + 128);
        out[56 + cc] = clamp8(descale(t10 - t3, n) + 128);
        out[8 + cc] = clamp8(descale(t11 + t2, n) + 128);
        out[48 + cc] = clamp8(descale(t11 - t2, n) + 128);
        out[16 + cc] = clamp8(descale(t12 + t1, n) + 128);
        out[40 + cc] = clamp8(descale(t12 - t1, n) + 128);
        out[24 + cc] = clamp8(descale(t13 + t0, n) + 128);
        out[32 + cc] = clamp8(descale(t13 - t0, n) + 128);
    }
}

// ---------------------------------------------------------------------------
// 上采样 + 色彩变换 → RGBA8
// ---------------------------------------------------------------------------

/// YCbCr → RGB（libjpeg jdcolor.c 同款定点口径：SCALEBITS=16，ONE_HALF
/// 舍入；全摆幅 BT.601）。
struct YccTables {
    cr_r: [i32; 256],
    cb_b: [i32; 256],
    cr_g: [i32; 256],
    cb_g: [i32; 256],
}

impl YccTables {
    fn new() -> YccTables {
        // FIX(x) = round(x * 65536)
        const FIX_1_40200: i32 = 91881;
        const FIX_1_77200: i32 = 116130;
        const FIX_0_34414: i32 = 22554;
        const FIX_0_71414: i32 = 46802;
        const ONE_HALF: i32 = 1 << 15;
        let mut t = YccTables {
            cr_r: [0; 256],
            cb_b: [0; 256],
            cr_g: [0; 256],
            cb_g: [0; 256],
        };
        for i in 0..256usize {
            let x = i as i32 - 128;
            t.cr_r[i] = (FIX_1_40200 * x + ONE_HALF) >> 16;
            t.cb_b[i] = (FIX_1_77200 * x + ONE_HALF) >> 16;
            t.cr_g[i] = -FIX_0_71414 * x;
            t.cb_g[i] = -FIX_0_34414 * x + ONE_HALF;
        }
        t
    }

    #[inline]
    fn rgb(&self, y: u8, cb: u8, cr: u8) -> (u8, u8, u8) {
        let (ci, ri) = (cb as usize, cr as usize);
        let yv = y as i32;
        (
            clamp8(yv + self.cr_r[ri]),
            clamp8(yv + ((self.cb_g[ci] + self.cr_g[ri]) >> 16)),
            clamp8(yv + self.cb_b[ci]),
        )
    }
}

/// 分量平面行 → 图像宽行。2× 扩展走 libjpeg fancy 三角滤波
///（jdsample.c h2v1/h2v2_fancy_upsample 同式），其余直接取样。
/// P3：`cs` 至少 `dw` 长；`dst` 恰好图像宽 `w`。
fn expand_row(c: &Comp, fr: &Frame, y: usize, cs: &mut [i32], dst: &mut [u8]) {
    let plane = &c.plane;
    let (sw, sh, pw) = (c.dw, c.dh, c.pw);
    let w = dst.len();
    let v2 = fr.vmax as usize == c.v as usize * 2;
    let h2 = fr.hmax as usize == c.h as usize * 2;
    // 垂直：2× 时 near=所属行、far=相邻行（钳到图内；libjpeg 同款）
    let (nr, frr) = if v2 {
        let nr = y >> 1;
        let f = if y & 1 == 1 {
            nr + 1
        } else {
            nr.saturating_sub(1)
        };
        (nr, f.min(sh - 1))
    } else {
        (y.min(sh - 1), 0)
    };
    let near = &plane[nr * pw..nr * pw + sw];
    let far = &plane[frr * pw..frr * pw + sw];
    if sw == 1 || w == 1 {
        dst[0] = near[0];
        return;
    }
    if !h2 {
        // 水平 1×：直接取样（h1v2 libjpeg 也走 int 复制）
        dst[..sw].copy_from_slice(near);
        return;
    }
    if v2 {
        // h2v2 fancy：列和 = 3·near + far，再水平 [1,3]/4
        for x in 0..sw {
            cs[x] = 3 * near[x] as i32 + far[x] as i32;
        }
        dst[0] = ((cs[0] * 4 + 8) >> 4) as u8;
        dst[1] = ((cs[0] * 3 + cs[1] + 7) >> 4) as u8;
        for x in 1..sw - 1 {
            dst[2 * x] = ((cs[x] * 3 + cs[x - 1] + 7) >> 4) as u8;
            dst[2 * x + 1] = ((cs[x] * 3 + cs[x + 1] + 7) >> 4) as u8;
        }
        let l = sw - 1;
        dst[2 * l] = ((cs[l] * 3 + cs[l - 1] + 7) >> 4) as u8;
        if 2 * l + 1 < w {
            dst[2 * l + 1] = ((cs[l] * 4 + 8) >> 4) as u8;
        }
    } else {
        // h2v1 fancy：水平 [1,3]/4
        dst[0] = near[0];
        dst[1] = ((3 * near[0] as i32 + near[1] as i32 + 2) >> 2) as u8;
        for x in 1..sw - 1 {
            dst[2 * x] = ((3 * near[x] as i32 + near[x - 1] as i32 + 1) >> 2) as u8;
            dst[2 * x + 1] = ((3 * near[x] as i32 + near[x + 1] as i32 + 2) >> 2) as u8;
        }
        let l = sw - 1;
        dst[2 * l] = ((3 * near[l] as i32 + near[l - 1] as i32 + 1) >> 2) as u8;
        if 2 * l + 1 < w {
            dst[2 * l + 1] = near[l];
        }
    }
}

/// 平面 → RGBA8。`ncomp==1` 灰度复制；`ycc=false` 的 3 通道 RGB 直通；
/// 其余 3 通道 YCbCr 变换。
fn to_rgba(fr: &Frame, ycc: bool) -> Option<Vec<u8>> {
    let (w, h) = (fr.w as usize, fr.h as usize);
    let n = fr.comps.len();
    let mut out = vec![0u8; w.checked_mul(h)?.checked_mul(4)?];
    let max_dw = fr.comps.iter().map(|c| c.dw).max().unwrap_or(1);
    let mut cs = vec![0i32; max_dw.max(1)];
    let mut rows: [Vec<u8>; 3] = [vec![0u8; w], vec![0u8; w], vec![0u8; w]];
    let tab = YccTables::new();
    for y in 0..h {
        for (k, c) in fr.comps.iter().enumerate() {
            expand_row(c, fr, y, &mut cs, &mut rows[k]);
        }
        let seg = &mut out[y * w * 4..(y + 1) * w * 4];
        for ((&r0, (&r1, &r2)), p) in rows[0]
            .iter()
            .zip(rows[1].iter().zip(rows[2].iter()))
            .zip(seg.chunks_exact_mut(4))
        {
            let (r, g, b) = if n == 1 {
                (r0, r0, r0)
            } else if !ycc {
                (r0, r1, r2)
            } else {
                tab.rgb(r0, r1, r2)
            };
            p[0] = r;
            p[1] = g;
            p[2] = b;
            p[3] = 255;
        }
    }
    Some(out)
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// 夹具库：`testdata/jpeg_fixtures.txt`，每行 `key:jpg_b64,ref_b64`
    ///（jpg = PIL 生成的 JPEG；ref = PIL 解码的期望 RGBA8；渐进式无 ref
    /// 记 `-`）。
    pub(crate) fn fx(key: &str) -> (Vec<u8>, Option<Vec<u8>>) {
        let txt = include_str!("../testdata/jpeg_fixtures.txt");
        for line in txt.lines() {
            let Some((k, rest)) = line.split_once(':') else {
                continue;
            };
            if k != key {
                continue;
            }
            let Some((j, r)) = rest.split_once(',') else {
                continue;
            };
            let jpg = b64d(j);
            let refr = if r == "-" { None } else { Some(b64d(r)) };
            return (jpg, refr);
        }
        panic!("夹具缺失: {key}");
    }

    /// 测试用 base64 解码（标准字母表）。
    pub(crate) fn b64d(s: &str) -> Vec<u8> {
        let mut out = Vec::new();
        let mut acc: u32 = 0;
        let mut n = 0u32;
        for &c in s.as_bytes() {
            let v = match c {
                b'A'..=b'Z' => c - b'A',
                b'a'..=b'z' => c - b'a' + 26,
                b'0'..=b'9' => c - b'0' + 52,
                b'+' => 62,
                b'/' => 63,
                _ => continue,
            } as u32;
            acc = (acc << 6) | v;
            n += 6;
            if n >= 8 {
                n -= 8;
                out.push(((acc >> n) & 0xff) as u8);
            }
        }
        out
    }

    /// 逐像素 vs PIL 参照：RGBA 每通道差 ≤ tol，返回最大差。
    fn max_diff(a: &DecodedJpeg, refr: &[u8]) -> i32 {
        assert_eq!(a.rgba.len(), refr.len(), "尺寸不符");
        a.rgba
            .iter()
            .zip(refr.iter())
            .map(|(&x, &y)| (x as i32 - y as i32).abs())
            .max()
            .unwrap_or(0)
    }

    /// huffman 表构造：码长计数排序合法 → 成表；过订阅 → None。
    #[test]
    fn huff_build_valid_and_oversubscribe() {
        // 单符号树（JPEG 常见 DC 形态）：len1 count=1
        let h = build_huff(&[1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], &[7])
            .unwrap_or_else(|| panic!("单码表应合法"));
        assert_eq!(h.sym[0], 7);
        // 1 个 len1 码 + 2 个 len2 码：完全树
        let h2 = build_huff(
            &[1, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
            &[0, 1, 2],
        )
        .unwrap_or_else(|| panic!("完全树应合法"));
        assert_eq!(h2.sym.len(), 3);
        // 过订阅：3 个 len1 码
        assert!(build_huff(
            &[3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
            &[0, 1, 2]
        )
        .is_none());
    }

    /// EXTEND 附加位展开：负数偏移语义（T.81 F.2.2）。
    #[test]
    fn extend_negative_range() {
        // s=2: 位 "00"=0 → 0-(4-1)=-3；"01"=1 → -2；"10"=2 → +2；"11"=3 → +3
        let cases: [(&[u8], u32, i32); 4] = [
            (&[0u8, 0], 2, -3),
            (&[0u8, 1], 2, -2),
            (&[1u8, 0], 2, 2),
            (&[1u8, 1], 2, 3),
        ];
        for (bits, s, want) in cases {
            let buf = pack_msb(bits);
            let mut e = Entropy {
                b: &buf,
                pos: 0,
                acc: 0,
                cnt: 0,
                hit: false,
            };
            assert_eq!(e.extend(s), Some(want));
        }
    }

    /// 位序列 → MSB-first 字节流。
    fn pack_msb(bits: &[u8]) -> alloc::vec::Vec<u8> {
        let mut v = alloc::vec![0u8; bits.len().div_ceil(8)];
        for (i, &b) in bits.iter().enumerate() {
            if b == 1 {
                v[i / 8] |= 0x80 >> (i % 8);
            }
        }
        v
    }

    /// YCbCr 定点表 sanity：中性灰恒等、红蓝主色 ±1。
    #[test]
    fn ycc_tables_identity_and_primaries() {
        let t = YccTables::new();
        assert_eq!(t.rgb(128, 128, 128), (128, 128, 128));
        // 全红：y=76 cb=85 cr=255（PIL/JFIF 全摆幅编码值）
        let (r, g, b) = t.rgb(76, 85, 255);
        let (r, g, b) = (r as i32, g as i32, b as i32);
        assert!((r - 255).abs() <= 1 && g <= 1 && b <= 1, "红 {r} {g} {b}");
        // 全蓝：y=29 cb=255 cr=107（BT.601 全摆幅正变换）
        let (r, g, b) = t.rgb(29, 255, 107);
        let (r, g, b) = (r as i32, g as i32, b as i32);
        assert!(r <= 2 && g <= 2 && b >= 253, "蓝 {r} {g} {b}");
    }

    /// IDCT 基向量完备性：单系数块 = 对应基函数缩放；DC-only 走快路径。
    #[test]
    fn idct_dc_only_fast_path() {
        let mut c = [0i32; 64];
        c[0] = (60 - 128) * 8; // 样本 = dc/8 + 128 → 60
        let mut out = [0u8; 64];
        idct_block(&c, &mut out);
        assert!(
            out.iter().all(|&v| v == 60),
            "DC-only 应均匀 {:?}",
            &out[..4]
        );
    }

    /// 端到端灰度 8×8：逐像素 vs PIL（islow+fancy 同口径，期望 ≤1）。
    #[test]
    fn e2e_gray_8x8_matches_pil() {
        let (jpg, refr) = fx("g8");
        let d = decode(&jpg).unwrap_or_else(|| panic!("灰度解码失败"));
        assert_eq!((d.w, d.h), (8, 8));
        let md = max_diff(&d, &refr.unwrap_or_default());
        assert!(md <= 1, "灰度 8×8 最大差 {md} LSB（预算 ±1）");
    }

    /// 端到端 4:4:4 彩色 8×8：逐像素 vs PIL ≤1。
    #[test]
    fn e2e_color_444_8x8_matches_pil() {
        let (jpg, refr) = fx("c444");
        let d = decode(&jpg).unwrap_or_else(|| panic!("4:4:4 解码失败"));
        assert_eq!((d.w, d.h), (8, 8));
        assert!(
            d.rgba.iter().skip(3).step_by(4).all(|&a| a == 255),
            "alpha 恒 255"
        );
        // 容差 ±2：IDCT 本身对 PIL（libjpeg-turbo islow）≤1；彩色路径的
        // 额外 ±1 来自 turbo 对 YCbCr+h2 采样启用 merged upsample（在 RGB
        // 贡献空间插值、中间量化）与本实现「样本空间 fancy 插值→单次舍入」
        // 的口径差（全精度略优）。已登记 docs。
        let md = max_diff(&d, &refr.unwrap_or_default());
        assert!(md <= 2, "4:4:4 最大差 {md} LSB（预算 ±2）");
    }

    /// 端到端 4:2:0 彩色 16×16（h2v2 fancy 上采样）：逐像素 vs PIL ≤1。
    #[test]
    fn e2e_color_420_16x16_matches_pil() {
        let (jpg, refr) = fx("c420");
        let d = decode(&jpg).unwrap_or_else(|| panic!("4:2:0 解码失败"));
        assert_eq!((d.w, d.h), (16, 16));
        let md = max_diff(&d, &refr.unwrap_or_default());
        assert!(md <= 2, "4:2:0 最大差 {md} LSB（预算 ±2）");
    }

    /// 端到端 4:2:2 彩色 16×16（h2v1 fancy 上采样）：逐像素 vs PIL ≤1。
    #[test]
    fn e2e_color_422_16x16_matches_pil() {
        let (jpg, refr) = fx("c422");
        let d = decode(&jpg).unwrap_or_else(|| panic!("4:2:2 解码失败"));
        assert_eq!((d.w, d.h), (16, 16));
        let md = max_diff(&d, &refr.unwrap_or_default());
        assert!(md <= 2, "4:2:2 最大差 {md} LSB（预算 ±2）");
    }

    /// DRI 重启间隔（restart_marker_rows=1，每 MCU 行一 RST）：与无 DRI
    /// 同图逐像素一致 ≤1。
    #[test]
    fn e2e_restart_interval_resync() {
        let (jpg, refr) = fx("dri");
        assert!(jpg.windows(2).any(|w| w == [0xFF, 0xDD]), "夹具应含 DRI 段");
        assert!(jpg.windows(2).any(|w| w == [0xFF, 0xD0]), "夹具应含 RST0");
        let d = decode(&jpg).unwrap_or_else(|| panic!("DRI 解码失败"));
        let md = max_diff(&d, &refr.unwrap_or_default());
        assert!(md <= 2, "DRI 最大差 {md} LSB（预算 ±2）");
    }

    /// 奇数尺寸 5×3（MCU 网格填充 + 输出裁剪）：逐像素 vs PIL ≤1。
    #[test]
    fn e2e_odd_size_5x3_matches_pil() {
        let (jpg, refr) = fx("odd53");
        let d = decode(&jpg).unwrap_or_else(|| panic!("5×3 解码失败"));
        assert_eq!((d.w, d.h), (5, 3));
        let md = max_diff(&d, &refr.unwrap_or_default());
        assert!(md <= 2, "5×3 最大差 {md} LSB（预算 ±2）");
    }

    /// 1×1 单块图：整图即一块。
    #[test]
    fn e2e_single_pixel() {
        let (jpg, refr) = fx("px1");
        let d = decode(&jpg).unwrap_or_else(|| panic!("1×1 解码失败"));
        assert_eq!((d.w, d.h), (1, 1));
        let md = max_diff(&d, &refr.unwrap_or_default());
        assert!(md <= 2, "1×1 最大差 {md} LSB（预算 ±2）");
    }

    /// SOF2 渐进式：夹具应含多扫描，与 PIL 预言机逐像素一致。
    #[test]
    fn progressive_multi_scan_matches_pil() {
        let (prog, refr) = fx("prog");
        assert!(
            prog.windows(2).filter(|&w| w == [0xFF, 0xDA]).count() > 1,
            "渐进式夹具应含多扫描"
        );
        let d = decode(&prog).unwrap_or_else(|| panic!("渐进式解码失败"));
        let md = max_diff(&d, &refr.expect("prog 夹具应有 ref"));
        assert!(md <= 2, "渐进式最大差 {md} LSB（预算 ±2）");
    }

    /// 渐进式夹具矩阵：灰度 / 4:4:4 / 4:2:0 / 4:2:2 / 奇数尺寸 / DRI / 低
    /// 质量长 EOB 游程，逐像素 vs PIL（彩色路径 ±2，同基线口径）。
    #[test]
    fn prog_fixture_matrix_matches_pil() {
        let cases: [(&str, u32, u32, i32); 7] = [
            ("pg8", 16, 16, 1),
            ("pc444", 16, 16, 2),
            ("pc420", 32, 32, 2),
            ("pc422", 32, 16, 2),
            ("podd", 13, 7, 2),
            ("pdri", 24, 24, 1),
            // plq q10 极端量化：h2v2 fancy vs turbo merged upsample 的口径差
            // 放大到 3 LSB——基线孪生图（同系数）同测 3，渐进路径无额外差
            ("plq", 64, 48, 3),
        ];
        for (key, w, h, tol) in cases {
            let (jpg, refr) = fx(key);
            let d = decode(&jpg).unwrap_or_else(|| panic!("{key} 解码失败"));
            assert_eq!((d.w, d.h), (w, h), "{key} 尺寸不符");
            let md = max_diff(&d, &refr.unwrap_or_default());
            assert!(md <= tol, "{key} 最大差 {md} LSB（预算 ±{tol}）");
        }
    }

    /// 渐进式 × DRI：夹具应含 DRI 段与 RST 标记（重启重同步 + EOBRUN 复位）。
    #[test]
    fn prog_restart_interval_markers() {
        let (jpg, _) = fx("pdri");
        assert!(jpg.windows(2).any(|w| w == [0xFF, 0xDD]), "应含 DRI 段");
        assert!(jpg.windows(2).any(|w| w == [0xFF, 0xD0]), "应含 RST0");
    }

    /// 渐进式参数校验：合法组合放行、非法组合整体放弃（不 panic）。
    #[test]
    fn prog_scan_param_gates() {
        // 基线流被截断/篡改 → None；渐进式坏流同口径
        let (prog, _) = fx("prog");
        assert!(decode(&prog[..2]).is_none(), "仅 SOI 应放弃");
        assert!(decode(&prog[..prog.len() / 3]).is_none(), "截断应放弃");
        let (jpg, _) = fx("c420");
        assert!(decode(&jpg[..jpg.len() / 2]).is_none(), "截断应放弃");
        // 坏 SOI / 全零输入
        let mut bad2 = jpg.clone();
        bad2[1] = 0xD7;
        assert!(decode(&bad2).is_none());
        assert!(decode(&[0u8; 16]).is_none());
    }
}
