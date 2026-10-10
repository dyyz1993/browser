//! no_std PNG 解码器（M8 位图，feature `image-decode`）。
//!
//! `<image href="data:image/png;base64,...">` 的解码路径：base64 →
//! PNG 容器（IHDR/PLTE/IDAT）→ **手写 inflate**（RFC 1951：stored /
//! fixed / dynamic 三种 block，LZ77 + canonical Huffman）→ 扫描线滤波
//! 重建（filter 0-4）→ 直 RGBA8（非预乘）。
//!
//! 支持集（语料驱动 + Chrome 高频）：颜色类型 0（灰度）/2（RGB）/
//! 3（调色板 PLTE + tRNS 透明表）/4（灰度+alpha）/6（RGBA）；位深
//! 1/2/4/8（亚字节位深限 ctype 0 灰度与 ctype 3 调色板索引，展开到
//! 8-bit）；Adam7 隔行（interlace=1，七 pass 重建）。
//!
//! gAMA（车道CO，2026-09-12）：Chrome 预言机逐位实证
//! `out = round(255 · srgb_enc(sample^(1/g)))`（g = gAMA/100000）对
//! g∈{0.30, 0.56, 1.0, 2.0} 逐位一致；两个 identity 特例：
//! - 带 cHRM 的文件忽略 gAMA（smiley g=0.227+cHRM 实证 identity——
//!   Chrome 走完整 profile 路径，语料内 cHRM 文件全按原样渲染）；
//! - `|g − 1/2.2| ≤ 0.005` 视为 sRGB gamma，恒等（gam045 g=0.45
//!   实证 identity；公式本体在此窗口内也近似恒等，但 0.45 处有
//!   ≤6 LSB 偏差，必须显式短路）。
//!
//! LUT 256 项在解码期一次构建，只作用于颜色通道（alpha 不动）。
//!
//! **放弃项**（遇之返回 None，调用方按不可描绘跳过）：
//! - 16 位深（含 16-bit tRNS 色键）
//! - ctype 0/2 的 tRNS（色键透明——按像素值比对置透明，非逐索引
//!   alpha 表；语料 8 枚 nav_* 用到，诚实放弃不伪装）；ctype 4/6
//!   自带 alpha 通道，规范禁止 tRNS，出现即忽略
//!
//! 工程约束（GOALS 宪法）：
//! - `#![no_std] + alloc`，禁 unwrap/expect（失败一律 `None` 上抛）；
//! - 分配最小化：IDAT 拼接 + inflate 输出按 IHDR 尺寸预算（超预算即
//!   视为损坏数据终止，不做无界回退缓冲）；
//! - 防御病态 IHDR：单边 ≤ 4096、总像素 ≤ 4M（≈16MB RGBA 上限）。
//!
//! CRC 不校验（传输完整性由宿主保证；校验表另占 1KB flash 不划算），
//! zlib 尾部 Adler-32 忽略。

use alloc::vec;
use alloc::vec::Vec;

/// 解码产物：尺寸 + 直 RGBA8（每像素 4 字节，行主序）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodedPng {
    pub w: u32,
    pub h: u32,
    pub rgba: Vec<u8>,
}

/// 尺寸护栏（病态 IHDR 不触发大分配）。
const MAX_DIM: u32 = 4096;
const MAX_PIXELS: u32 = 4_000_000;

// ---------------------------------------------------------------------------
// 入口：完整 PNG 字节流 / base64 载荷
// ---------------------------------------------------------------------------

/// 解码完整 PNG 字节流。任何不支持/损坏形态 → None（宽容跳过）。
pub fn decode(b: &[u8]) -> Option<DecodedPng> {
    decode_tagged(b).map(|(d, _)| d)
}

/// 解码 + gAMA 管理标志：true = gAMA 色域校正**已实际改写字节**
///（gAMA 存在且非 sRGB-gamma 窗口、非 0、无 iCCP/sRGB 块；cHRM 不
/// 抑制——车道KA 活体实证）。消费方 feImage
///「字节直通再解释」注入据此跳过提亮 LUT（车道DB）：Chrome 对 tagged
/// 位图在滤镜工作域内做色域管理——解码产物已是 `srgb_enc(x^(1/g))`
/// 工作域编码，再套 as-linear LUT = 双重提亮（displace-01 实证：
/// g=1.0 的 rotate20 我方 57→189 双重 vs 预言机 130 单次）。
pub fn decode_tagged(b: &[u8]) -> Option<(DecodedPng, bool)> {
    const SIG: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    if b.len() < 8 + 12 || b[..8] != SIG {
        return None;
    }
    let mut pos = 8usize;
    let mut ihdr: Option<(u32, u32, u8, u8, u8)> = None; // (w, h, depth, ctype, interlace)
    let mut plte: Option<&[u8]> = None;
    let mut trns: Option<&[u8]> = None;
    let mut gama: Option<u32> = None;
    let mut has_chrm = false;
    let mut has_srgb = false;
    let mut has_iccp = false;
    let mut idat: Vec<u8> = Vec::new();
    while pos + 12 <= b.len() {
        let len = be32(b.get(pos..pos + 4)?)? as usize;
        let ty: [u8; 4] = [
            *b.get(pos + 4)?,
            *b.get(pos + 5)?,
            *b.get(pos + 6)?,
            *b.get(pos + 7)?,
        ];
        let data_start = pos + 8;
        let data_end = data_start.checked_add(len)?;
        if data_end + 4 > b.len() {
            return None; // 截断
        }
        let data = &b[data_start..data_end];
        match &ty {
            b"IHDR" => {
                if len != 13 {
                    return None;
                }
                let w = be32(data)?;
                let h = be32(data.get(4..8)?)?;
                let interlace = data.get(12).copied().unwrap_or(0);
                // 压缩/滤波方法必须为 0；interlace 仅放行 0/1（Adam7=1）
                if data.get(10).copied().unwrap_or(1) != 0
                    || data.get(11).copied().unwrap_or(1) != 0
                    || interlace > 1
                {
                    return None;
                }
                ihdr = Some((
                    w,
                    h,
                    data.get(8).copied()?,
                    data.get(9).copied()?,
                    interlace,
                ));
            }
            b"PLTE" => plte = Some(data),
            b"tRNS" => trns = Some(data), // ctype 3 透明表；0/2 见下方门禁
            b"gAMA" => {
                if len == 4 {
                    gama = be32(data);
                }
            }
            b"cHRM" => has_chrm = true,
            b"sRGB" => has_srgb = true,
            b"iCCP" => has_iccp = true,
            b"IDAT" => idat.extend_from_slice(data),
            b"IEND" => break,
            _ => {} // 其余辅助块（pHYs/…）忽略
        }
        pos = data_end + 4;
    }
    let (w, h, depth, ctype, interlace) = ihdr?;
    if depth != 8 && depth != 1 && depth != 2 && depth != 4 {
        return None; // 16 位深放弃（亚字节位深见下：仅灰度/调色板索引）
    }
    let ch = match ctype {
        0 => 1usize,      // 灰度
        2 => 3,           // RGB
        3 => 1,           // 调色板索引
        4 => 2,           // 灰度 + alpha
        6 => 4,           // RGBA
        _ => return None, // 其余低频形态放弃
    };
    // 亚字节位深只支持每像素单通道形态（灰度/调色板索引）；多通道
    // (RGB/RGBA/ga) 的亚字节打包规范不允许，出现即按损坏拒识。
    if depth != 8 && ch != 1 {
        return None;
    }
    // tRNS 门禁：ctype 3 的逐索引透明表放行（to_rgba 查表）；ctype 0/2
    // 的色键透明（灰度样本值/RGB 三元组比对）不支持——与「透明表放弃」
    // 的旧口径一致诚实跳过；ctype 4/6 自带 alpha，规范禁止 tRNS，忽略
    if trns.is_some() && (ctype == 0 || ctype == 2) {
        return None;
    }
    if w == 0 || h == 0 || w > MAX_DIM || h > MAX_DIM || w.checked_mul(h)? > MAX_PIXELS {
        return None;
    }
    let shift = w as usize * ch;
    let bpp = if depth == 8 { ch } else { 1 }; // 滤波器回看字节数
    let packed = |pw: u32| -> usize { (pw as usize * ch * depth as usize).div_ceil(8) };
    if idat.is_empty() {
        return None;
    }
    // 每像素 8-bit 展开后的样本流（灰度已按位深缩放、索引逐像素 1 字节）
    let mut samples = vec![0u8; shift * h as usize];
    if interlace == 0 {
        let raw_len = h as usize * (packed(w) + 1);
        let raw = inflate_zlib(&idat, raw_len)?;
        if raw.len() != raw_len {
            return None;
        }
        let px = unfilter(&raw, packed(w), h as usize, bpp)?;
        expand_samples(&px, &mut samples, w as usize, packed(w), depth, ch);
        if ctype == 0 && depth != 8 {
            for v in samples.iter_mut() {
                *v = gray8(*v, depth);
            }
        }
    } else {
        // Adam7：七 pass 独立滤波重建后按 (x0+i·sx, y0+j·sy) 散布。
        // 每行/列数为 ceil((dim−start)/step)；空 pass 无扫描线。
        // 表序 = RFC 2083 §2.5；p5/p7 的 x0=0（非 2/1——常见误记，
        // 车道CO 以 stefan 语料 24582 字节逐位解码实证钉死）。
        const PASSES: [(u32, u32, u32, u32); 7] = [
            (0, 0, 8, 8),
            (4, 0, 8, 8),
            (0, 4, 4, 8),
            (2, 0, 4, 4),
            (0, 2, 2, 4),
            (1, 0, 2, 2),
            (0, 1, 1, 2),
        ];
        let mut raw_off = 0usize;
        // 先算总预算（防御病态流）
        let mut total = 0usize;
        for &(x0, y0, sx, sy) in PASSES.iter() {
            let (pw, ph) = pass_dims(w, h, x0, y0, sx, sy);
            if pw > 0 && ph > 0 {
                total += ph as usize * (packed(pw) + 1);
            }
        }
        let raw = inflate_zlib(&idat, total)?;
        if raw.len() != total {
            return None;
        }
        for &(x0, y0, sx, sy) in PASSES.iter() {
            let (pw, ph) = pass_dims(w, h, x0, y0, sx, sy);
            if pw == 0 || ph == 0 {
                continue;
            }
            let stride = packed(pw);
            let pass_len = ph as usize * (stride + 1);
            let prown = raw.get(raw_off..raw_off + pass_len)?;
            raw_off += pass_len;
            let px = unfilter(prown, stride, ph as usize, bpp)?;
            for j in 0..ph as usize {
                for i in 0..pw as usize {
                    let x = x0 as usize + i * sx as usize;
                    let y = y0 as usize + j * sy as usize;
                    let d = y * shift + x * ch;
                    if depth == 8 {
                        // 多通道：整像素 ch 字节直拷
                        let s = j * stride + i * ch;
                        for c in 0..ch {
                            samples[d + c] = *px.get(s + c)?;
                        }
                    } else {
                        let s = sample_at(&px, stride, depth, i, j)?;
                        let v = if ctype == 0 { gray8(s, depth) } else { s };
                        for c in 0..ch {
                            samples[d + c] = v;
                        }
                    }
                }
            }
        }
    }
    let rgba = to_rgba(&samples, ch, ctype, plte, trns)?;
    let managed = gama_managed(gama, has_chrm, has_srgb, has_iccp);
    let rgba = apply_gama(rgba, gama, has_chrm, has_srgb, has_iccp);
    Some((DecodedPng { w, h, rgba }, managed))
}

/// gAMA 是否实际生效（apply_gama 改写字节的同款判据——两处判据必须
/// 保持一致，改动需同步）。
///
/// 车道KA 实证修订（2026-09-13，repros/ka1-gama-chrm-probe.svg 活体
/// Chrome 五图 1:1 全 100% ≤2）：**cHRM 在场不再抑制 gAMA 转换**——
/// Chrome 对 cHRM+gAMA 位图仍按 gAMA 传递函数做转换（色度 primaries
/// 忽略），旧口径「cHRM → 完整 profile 路径恒等」是等价区掩盖（旧语料
/// 的 cHRM 图全带 sRGB 窗口 gAMA，转换本就恒等）。iCCP/sRGB 块在场 =
/// 完整 profile 声明，跳过 gamma-only 转换（我们不做完整 ICC）。
fn gama_managed(gama: Option<u32>, has_chrm: bool, has_srgb: bool, has_iccp: bool) -> bool {
    let _ = has_chrm; // cHRM 不影响判据（KA 实证），参数保留供调用点自文档
    match gama {
        Some(code) => {
            code != 0
                && !has_srgb
                && !has_iccp
                && (code as f32 / 100_000.0 - 1.0 / 2.2).abs() > 0.005
        }
        None => false,
    }
}

/// Adam7 单 pass 的 (宽, 高)：`ceil((dim − start)/step)`，起点越界 = 0。
fn pass_dims(w: u32, h: u32, x0: u32, y0: u32, sx: u32, sy: u32) -> (u32, u32) {
    let pw = if w > x0 { (w - x0).div_ceil(sx) } else { 0 };
    let ph = if h > y0 { (h - y0).div_ceil(sy) } else { 0 };
    (pw, ph)
}

/// 打包扫描线 → 8-bit 样本流。逐行展开（行尾 padding 位不得串行——
/// 首版线性展开曾把 padding 当样本吃进，多行即错位）。灰度的位深
/// 缩放由调用方 `gray8` 负责；depth=8 走整块拷贝快路径。
fn expand_samples(
    px: &[u8],
    out: &mut [u8],
    w: usize,
    stride: usize,
    depth: u8,
    ch: usize,
) -> Option<()> {
    if depth == 8 {
        out.copy_from_slice(px);
        return Some(());
    }
    let per_byte = 8 / depth as usize;
    let mask = ((1u16 << depth) - 1) as u8;
    for j in 0..out.len() / (w * ch) {
        let prow = px.get(j * stride..j * stride + stride)?;
        for i in 0..w {
            let b = prow[i / per_byte];
            let shift = 8 - depth as usize * (i % per_byte + 1);
            let d = j * w * ch + i * ch;
            for c in 0..ch {
                out[d + c] = (b >> shift) & mask;
            }
        }
    }
    Some(())
}

/// 亚字节位深的 pass 内第 (i, j) 像素样本。
fn sample_at(px: &[u8], stride: usize, depth: u8, i: usize, j: usize) -> Option<u8> {
    if depth == 8 {
        return px.get(j * stride + i).copied();
    }
    let per_byte = 8 / depth as usize;
    let b = *px.get(j * stride + i / per_byte)?;
    let shift = 8 - depth as usize * (i % per_byte + 1);
    Some((b >> shift) & ((1u16 << depth) - 1) as u8)
}

/// 灰度样本按位深缩放到 8-bit（PNG §11.2：v·255/(2^d−1)，d∈{1,2,4}
/// 时 255/(2^d−1) ∈ {255,85,17} 均为整数乘）。
fn gray8(s: u8, depth: u8) -> u8 {
    match depth {
        1 => s * 255,
        2 => s * 85,
        4 => s * 17,
        _ => s,
    }
}

/// gAMA 色域校正（见模块文档的 Chrome 实证口径）。返回原 Vec 或
/// 就地改写后的 Vec（避免无谓再分配）。
///
/// 车道KA 修订：cHRM 在场照常转换（活体探针 ka1 五图全 100% ≤2，
/// Chrome 忽略 cHRM primaries 只走 gAMA 传递函数）；iCCP/sRGB 块
/// 在场 = 完整 profile 声明 → 跳过（sRGB 块本身即恒等声明）。
fn apply_gama(
    mut rgba: Vec<u8>,
    gama: Option<u32>,
    has_chrm: bool,
    has_srgb: bool,
    has_iccp: bool,
) -> Vec<u8> {
    let _ = has_chrm;
    let Some(code) = gama else {
        return rgba;
    };
    if has_srgb || has_iccp {
        return rgba; // 完整 profile 声明：不做 gamma-only 猜测
    }
    if code == 0 {
        return rgba;
    }
    let g = code as f32 / 100_000.0;
    if (g - 1.0 / 2.2).abs() <= 0.005 {
        return rgba; // sRGB gamma 窗口（0.45/0.45454/0.45455）恒等
    }
    let inv = 1.0 / g;
    let mut lut = [0u8; 256];
    for (i, v) in lut.iter_mut().enumerate() {
        let lin = libm::powf(i as f32 / 255.0, inv);
        let enc = if lin <= 0.003_130_8 {
            12.92 * lin
        } else {
            1.055 * libm::powf(lin, 1.0 / 2.4) - 0.055
        };
        *v = libm::roundf(enc.clamp(0.0, 1.0) * 255.0) as u8;
    }
    for p in rgba.chunks_exact_mut(4) {
        p[0] = lut[p[0] as usize];
        p[1] = lut[p[1] as usize];
        p[2] = lut[p[2] as usize];
    }
    rgba
}

/// base64（标准字母表，容忍空白/padding）→ PNG 解码。
pub fn decode_base64(s: &str) -> Option<DecodedPng> {
    decode_base64_tagged(s).map(|(d, _)| d)
}

/// 同 [`decode_base64`]，附带 gAMA 管理标志（见 [`decode_tagged`]）。
pub fn decode_base64_tagged(s: &str) -> Option<(DecodedPng, bool)> {
    decode_tagged(&b64_bytes(s)?)
}

/// base64（标准字母表，容忍空白/padding）→ 字节流。
fn b64_bytes(s: &str) -> Option<Vec<u8>> {
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
    Some(bytes)
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

/// 大端 u32。
fn be32(b: &[u8]) -> Option<u32> {
    Some(
        ((*b.first()? as u32) << 24)
            | ((*b.get(1)? as u32) << 16)
            | ((*b.get(2)? as u32) << 8)
            | *b.get(3)? as u32,
    )
}

// ---------------------------------------------------------------------------
// inflate（RFC 1951，puff 式 count/symbol 表）
// ---------------------------------------------------------------------------

/// zlib 包装层校验：CM 必须为 8（deflate）、FDICT 未设、头部 %31 == 0。
pub(crate) fn inflate_zlib(z: &[u8], out_cap: usize) -> Option<Vec<u8>> {
    let (cmf, flg) = (*z.first()?, *z.get(1)?);
    if cmf & 0x0f != 8 || flg & 0x20 != 0 || ((cmf as u16) << 8 | flg as u16) % 31 != 0 {
        return None;
    }
    inflate(&z[2..], out_cap)
}

/// deflate 裸流解压。输出严格以 `out_cap` 为预算：超出即损坏（None），
/// 不足由调用方按期望尺寸校验。
fn inflate(d: &[u8], out_cap: usize) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(out_cap.min(1 << 20));
    let mut br = BitReader { d, bc: 0, cur: 0 };
    loop {
        let last = br.bit()?;
        match br.bits(2)? {
            0 => {
                // stored：字节对齐 + LEN/NLEN + 载荷直通
                br.align();
                let len = br.u16le()? as usize;
                let nlen = br.u16le()? as usize;
                if len ^ 0xffff != nlen {
                    return None;
                }
                for _ in 0..len {
                    let byte = br.byte()?;
                    if out.len() >= out_cap {
                        return None;
                    }
                    out.push(byte);
                }
            }
            1 => {
                let (lit, dist) = fixed_tables()?;
                inflate_block(&mut br, &lit, &dist, &mut out, out_cap)?;
            }
            2 => {
                let (lit, dist) = dynamic_tables(&mut br)?;
                inflate_block(&mut br, &lit, &dist, &mut out, out_cap)?;
            }
            _ => return None, // BTYPE=11 保留
        }
        if last == 1 {
            return Some(out);
        }
    }
}

/// 长度/距离码基值与附加位宽（RFC 1951 §3.2.5）。
const LBASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LEXT: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const DBASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DEXT: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];

/// 一个 Huffman block 的符号解码循环：字面量直存、LZ77 回引按字节
/// 复制（重叠距离 < 长度时自然展开）。
fn inflate_block(
    br: &mut BitReader,
    lit: &Huff,
    dist: &Huff,
    out: &mut Vec<u8>,
    out_cap: usize,
) -> Option<()> {
    loop {
        let sym = huff_decode(br, lit)?;
        match sym {
            0..=255 => {
                if out.len() >= out_cap {
                    return None;
                }
                out.push(sym as u8);
            }
            256 => return Some(()),
            257..=285 => {
                let li = (sym - 257) as usize;
                let len = LBASE[li] as usize + br.bits(LEXT[li] as u32)? as usize;
                let dsym = huff_decode(br, dist)?;
                if dsym > 29 {
                    return None;
                }
                let di = dsym as usize;
                let d = DBASE[di] as usize + br.bits(DEXT[di] as u32)? as usize;
                if d == 0 || d > out.len() {
                    return None;
                }
                for _ in 0..len {
                    if out.len() >= out_cap {
                        return None;
                    }
                    let b = out[out.len() - d];
                    out.push(b);
                }
            }
            _ => return None,
        }
    }
}

/// LSB-first 位读取器。
struct BitReader<'a> {
    d: &'a [u8],
    /// `cur` 中剩余有效位数。
    bc: u32,
    cur: u32,
}

impl BitReader<'_> {
    fn bit(&mut self) -> Option<u32> {
        if self.bc == 0 {
            self.cur = *self.d.first()? as u32;
            self.d = &self.d[1..];
            self.bc = 8;
        }
        let b = self.cur & 1;
        self.cur >>= 1;
        self.bc -= 1;
        Some(b)
    }

    fn bits(&mut self, n: u32) -> Option<u32> {
        let mut v = 0u32;
        for i in 0..n {
            v |= self.bit()? << i;
        }
        Some(v)
    }

    /// 丢弃当前字节剩余位（stored block 对齐）。
    fn align(&mut self) {
        self.bc = 0;
    }

    /// 整字节读取（先 align）。
    fn byte(&mut self) -> Option<u8> {
        self.align();
        let b = *self.d.first()?;
        self.d = &self.d[1..];
        Some(b)
    }

    fn u16le(&mut self) -> Option<u16> {
        let lo = self.byte()? as u16;
        let hi = self.byte()? as u16;
        Some(lo | (hi << 8))
    }
}

/// canonical Huffman 表（puff 式：长度计数 + 排序符号）。
struct Huff {
    count: [u16; 16],
    sym: Vec<u16>,
}

/// 码长序列 → 表。过订阅（left < 0）→ None；不完整表允许构建
/// （单码距离树等合法形态），解码走到缺口处报错。
fn build(lengths: &[u8]) -> Option<Huff> {
    let mut count = [0u16; 16];
    for &l in lengths {
        if l > 15 {
            return None;
        }
        count[l as usize] += 1;
    }
    let mut left: i32 = 1;
    for &c in count.iter().skip(1) {
        left <<= 1;
        left -= c as i32;
        if left < 0 {
            return None; // 过订阅
        }
    }
    let mut offs = [0u16; 16];
    for l in 1..15 {
        offs[l + 1] = offs[l] + count[l];
    }
    let mut sym = vec![0u16; lengths.len()];
    for (s, &l) in lengths.iter().enumerate() {
        if l != 0 {
            let slot = &mut offs[l as usize];
            sym[*slot as usize] = s as u16;
            *slot += 1;
        }
    }
    Some(Huff { count, sym })
}

/// 逐位下行解码（puff §build/decode 同构）：按码长累计 first/index，
/// 命中即查 sym 表；走满 15 位未命中 = 损坏码。
fn huff_decode(br: &mut BitReader, h: &Huff) -> Option<u16> {
    let mut code: i32 = 0;
    let mut first: i32 = 0;
    let mut index: i32 = 0;
    for len in 1..=15usize {
        code |= br.bit()? as i32;
        let cnt = h.count[len] as i32;
        if code - cnt < first {
            return h.sym.get((index + (code - first)) as usize).copied();
        }
        index += cnt;
        first = (first + cnt) << 1;
        code <<= 1;
    }
    None
}

/// fixed Huffman 码长表（RFC 1951 §3.2.6）。
fn fixed_tables() -> Option<(Huff, Huff)> {
    let mut ll = [0u8; 288];
    for (i, v) in ll.iter_mut().enumerate() {
        *v = if i < 144 {
            8
        } else if i < 256 {
            9
        } else if i < 280 {
            7
        } else {
            8
        };
    }
    let dl = [5u8; 30];
    Some((build(&ll)?, build(&dl)?))
}

/// 码长码的读取顺序（RFC 1951 §3.2.7）。
const CORDER: [usize; 19] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

/// dynamic Huffman 头：HLIT/HDIST/HCLEN → 码长码 → 逐符号码长
///（16/17/18 的复制/填零展开）→ 字面量 + 距离两表。
fn dynamic_tables(br: &mut BitReader) -> Option<(Huff, Huff)> {
    let hlit = br.bits(5)? as usize + 257;
    let hdist = br.bits(5)? as usize + 1;
    let hclen = br.bits(4)? as usize + 4;
    if hlit > 286 || hdist > 30 {
        return None;
    }
    let mut cl = [0u8; 19];
    for i in 0..hclen {
        cl[CORDER[i]] = br.bits(3)? as u8;
    }
    let clh = build(&cl)?;
    let mut lens = vec![0u8; hlit + hdist];
    read_code_lengths(br, &mut lens, &clh)?;
    let lit = build(&lens[..hlit])?;
    let dist = build(&lens[hlit..])?;
    Some((lit, dist))
}

/// 码长序列解码（dynamic block 头的主体）：符号 0-15 直存、
/// 16 复制前值、17/18 填零（重复次数越界 = 损坏流）。
fn read_code_lengths(br: &mut BitReader, lens: &mut [u8], clh: &Huff) -> Option<()> {
    let n = lens.len();
    let mut i = 0usize;
    while i < n {
        let s = huff_decode(br, clh)?;
        match s {
            0..=15 => {
                lens[i] = s as u8;
                i += 1;
            }
            16 => {
                if i == 0 {
                    return None;
                }
                let prev = lens[i - 1];
                let rep = 3 + br.bits(2)? as usize;
                if i + rep > n {
                    return None;
                }
                for _ in 0..rep {
                    lens[i] = prev;
                    i += 1;
                }
            }
            17 => {
                let rep = 3 + br.bits(3)? as usize;
                if i + rep > n {
                    return None;
                }
                i += rep;
            }
            18 => {
                let rep = 11 + br.bits(7)? as usize;
                if i + rep > n {
                    return None;
                }
                i += rep;
            }
            _ => return None,
        }
    }
    Some(())
}

// ---------------------------------------------------------------------------
// 扫描线滤波（PNG §6，filter 0-4；bpp = 字节/像素 = ch（8 位深））
// ---------------------------------------------------------------------------

/// 去滤波：`raw`（每行 = 1 滤波字节 + stride 数据字节）→ 紧凑像素流。
pub(crate) fn unfilter(raw: &[u8], stride: usize, h: usize, bpp: usize) -> Option<Vec<u8>> {
    let mut out = vec![0u8; stride * h];
    for row in 0..h {
        let rs = row * (stride + 1);
        let f = *raw.get(rs)?;
        let src = raw.get(rs + 1..rs + 1 + stride)?;
        let orow = row * stride;
        let prow = orow.checked_sub(stride);
        for (x, &v) in src.iter().enumerate() {
            let vi = v as i32;
            let a = if x >= bpp {
                out[orow + x - bpp] as i32
            } else {
                0
            };
            let (b, c) = match prow {
                Some(p) => (
                    out[p + x] as i32,
                    if x >= bpp { out[p + x - bpp] as i32 } else { 0 },
                ),
                None => (0, 0),
            };
            let pv = match f {
                0 => vi,
                1 => vi + a,              // Sub
                2 => vi + b,              // Up
                3 => vi + ((a + b) >> 1), // Average
                4 => {
                    // Paeth
                    let p = a + b - c;
                    let (pa, pb, pc) = ((p - a).abs(), (p - b).abs(), (p - c).abs());
                    if pa <= pb && pa <= pc {
                        vi + a
                    } else if pb <= pc {
                        vi + b
                    } else {
                        vi + c
                    }
                }
                _ => return None,
            };
            out[orow + x] = (pv & 0xff) as u8;
        }
    }
    Some(out)
}

/// 像素流 → RGBA8（灰度复制 / RGB 补 alpha / RGBA 直通 / 调色板查表
///（tRNS 逐索引 alpha，短表缺省 255）/ 灰度+alpha 复制）。
/// alpha 以 0-255 直存——Chrome/Skia 的量化（`a8=round(α·255)` +
/// SkAlpha255To256 权重）在 raster draw_image 采样侧统一处理。
fn to_rgba(
    px: &[u8],
    ch: usize,
    ctype: u8,
    plte: Option<&[u8]>,
    trns: Option<&[u8]>,
) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(px.len() / ch * 4);
    match ctype {
        0 => {
            for g in px.iter() {
                out.extend_from_slice(&[*g, *g, *g, 255]);
            }
        }
        2 => {
            for p in px.chunks_exact(3) {
                out.extend_from_slice(&[p[0], p[1], p[2], 255]);
            }
        }
        4 => {
            // 灰度复制三通道 + alpha 直通
            for p in px.chunks_exact(2) {
                out.extend_from_slice(&[p[0], p[0], p[0], p[1]]);
            }
        }
        6 => {
            for p in px.chunks_exact(4) {
                out.extend_from_slice(p);
            }
        }
        3 => {
            let pal = plte?;
            if pal.is_empty() || pal.len() % 3 != 0 {
                return None;
            }
            for p in px.chunks_exact(1) {
                let e = p[0] as usize * 3;
                if e + 2 >= pal.len() {
                    return None; // 越界调色板索引
                }
                // tRNS 短表（少于调色板项数）按规范缺省不透明
                let a = trns
                    .and_then(|t| t.get(p[0] as usize))
                    .copied()
                    .unwrap_or(255);
                out.extend_from_slice(&[pal[e], pal[e + 1], pal[e + 2], a]);
            }
        }
        _ => return None,
    }
    Some(out)
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::format;
    use alloc::string::String;
    use alloc::vec;

    /// 手工构造最小 PNG（stored block 编码的 zlib 流；CRC 填零——解码器
    /// 不校验）。2×2 RGB：红/绿 // 蓝/黄。
    fn png_2x2_rgb() -> Vec<u8> {
        let raw: [u8; 14] = [
            0, 0xff, 0, 0, 0x00, 0xff, 0, // 行0：filter0 + red + green
            0, 0, 0, 0xff, 0xff, 0xff, 0, // 行1：filter0 + blue + yellow
        ];
        let zlib = zlib_stored(&raw);
        let mut v = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        // IHDR：w=2 h=2 depth=8 ctype=2 comp=0 filt=0 inter=0
        chunk(&mut v, b"IHDR", &[0, 0, 0, 2, 0, 0, 0, 2, 8, 2, 0, 0, 0]);
        chunk(&mut v, b"IDAT", &zlib);
        chunk(&mut v, b"IEND", &[]);
        v
    }

    /// 追加一个 PNG 块（长度 + 类型 + 数据 + 填零 CRC）。
    fn chunk(v: &mut Vec<u8>, ty: &[u8; 4], data: &[u8]) {
        v.extend_from_slice(&(data.len() as u32).to_be_bytes());
        v.extend_from_slice(ty);
        v.extend_from_slice(data);
        v.extend_from_slice(&[0, 0, 0, 0]);
    }

    /// zlib level-0 等价流：78 01 + stored block（BFINAL=1）+ 载荷。
    fn zlib_stored(data: &[u8]) -> Vec<u8> {
        let mut z = vec![0x78, 0x01, 0x01];
        z.extend_from_slice(&(data.len() as u16).to_le_bytes());
        z.extend_from_slice(&(!(data.len() as u16)).to_le_bytes());
        z.extend_from_slice(data);
        z.extend_from_slice(&[0, 0, 0, 0]); // Adler-32（忽略）
        z
    }

    /// 端到端：2×2 四色 PNG → RGBA 像素断言。
    #[test]
    fn decode_minimal_2x2_end_to_end() {
        let d = decode(&png_2x2_rgb()).unwrap_or_else(|| panic!("解码失败"));
        assert_eq!((d.w, d.h), (2, 2));
        assert_eq!(d.rgba.len(), 16);
        let px = |i: usize| &d.rgba[i * 4..i * 4 + 4];
        assert_eq!(px(0), &[0xff, 0, 0, 0xff]); // 红
        assert_eq!(px(1), &[0, 0xff, 0, 0xff]); // 绿
        assert_eq!(px(2), &[0, 0, 0xff, 0xff]); // 蓝
        assert_eq!(px(3), &[0xff, 0xff, 0, 0xff]); // 黄
    }

    /// 语料真实 PNG（dynamic Huffman + Sub/Up 滤波混合）：16×16 四色
    /// 象限（Google 色）。
    #[test]
    fn decode_corpus_s10_05_png() {
        let b64 = "iVBORw0KGgoAAAANSUhEUgAAABAAAAAQCAIAAACQkWg2AAAAJ0lEQVR4nGNwav2CFf3ew4oVMYxqoIkGkxXBWNErZ1OsaFQDTTQAANi2ghDtdin4AAAAAElFTkSuQmCC";
        let d = decode_base64(b64).unwrap_or_else(|| panic!("语料 PNG 解码失败"));
        assert_eq!((d.w, d.h), (16, 16));
        let at = |x: usize, y: usize| {
            let o = (y * 16 + x) * 4;
            (d.rgba[o], d.rgba[o + 1], d.rgba[o + 2], d.rgba[o + 3])
        };
        assert_eq!(at(0, 0), (66, 133, 244, 255)); // 左上蓝
        assert_eq!(at(15, 0), (251, 188, 5, 255)); // 右上黄
        assert_eq!(at(0, 15), (52, 168, 83, 255)); // 左下绿
        assert_eq!(at(15, 15), (234, 67, 53, 255)); // 右下红
        assert_eq!(at(7, 7), (66, 133, 244, 255)); // 象限内点
    }

    /// inflate stored block 直通（zlib level-0 压缩 "\0hello"）。
    #[test]
    fn inflate_stored_block() {
        let z = [
            0x78u8, 0x01, 0x01, 0x06, 0x00, 0xf9, 0xff, 0x00, 0x68, 0x65, 0x6c, 0x6c, 0x6f, 0x06,
            0x2d, 0x02, 0x15,
        ];
        let out = inflate_zlib(&z, 64).unwrap_or_else(|| panic!("stored 解压失败"));
        assert_eq!(out, b"\x00hello");
    }

    /// inflate fixed Huffman + LZ77 回引：zlib 压缩 "AAAA" 的真实输出
    ///（字面量 A + 距离 1 长度 3 的回引）。
    #[test]
    fn inflate_fixed_huffman_backref() {
        let z = [
            0x78u8, 0x9c, 0x73, 0x74, 0x74, 0x74, 0x04, 0x00, 0x02, 0x8e, 0x01, 0x05,
        ];
        let out = inflate_zlib(&z, 64).unwrap_or_else(|| panic!("fixed 解压失败"));
        assert_eq!(out, b"AAAA");
    }

    /// 输出预算超限 → None（损坏流防御）。
    #[test]
    fn inflate_budget_exceeded_is_error() {
        let z = [
            0x78u8, 0x01, 0x01, 0x06, 0x00, 0xf9, 0xff, 0x00, 0x68, 0x65, 0x6c, 0x6c, 0x6f, 0x06,
            0x2d, 0x02, 0x15,
        ];
        assert!(inflate_zlib(&z, 3).is_none());
    }

    /// 滤波重建：Sub（行内差分）+ Up（行间差分）逐字节重算。
    #[test]
    fn unfilter_sub_up() {
        // 行0 filter=1（Sub, bpp=3）：滤波 [10,20,30, 251,241,231]
        //   → 重建 [10,20,30] [(251+10)&0xff, (241+20)&0xff, (231+30)&0xff]
        //   = [10,20,30, 5,5,5]
        // 行1 filter=2（Up）：滤波 [5,5,5, 255,255,255]
        //   → 重建 [5+10, 5+20, 5+30] = [15,25,35]
        //            [(255+5)&0xff, (255+5)&0xff, (255+5)&0xff] = [4,4,4]
        let raw: [u8; 14] = [1, 10, 20, 30, 251, 241, 231, 2, 5, 5, 5, 255, 255, 255];
        let out = unfilter(&raw, 6, 2, 3).unwrap_or_else(|| panic!("unfilter 失败"));
        assert_eq!(out[..6], [10, 20, 30, 5, 5, 5]);
        assert_eq!(out[6..12], [15, 25, 35, 4, 4, 4]);
    }

    /// Paeth 单行重建：滤波值 [100, 100, 100]（a 全 0 时退化为直通）。
    #[test]
    fn unfilter_paeth_row() {
        let raw: [u8; 4] = [4, 100, 110, 120];
        let out = unfilter(&raw, 3, 1, 3).unwrap_or_else(|| panic!("unfilter 失败"));
        assert_eq!(out, [100, 110, 120]); // 首行 a=b=c=0 → Paeth 取原值
    }

    /// 调色板 PNG（ctype 3）：PLTE 两色 → 索引像素查表。
    #[test]
    fn decode_palette_png() {
        let raw: [u8; 3] = [0, 0, 1]; // 2×1：filter0 + 索引 0,1
        let zlib = zlib_stored(&raw);
        let mut v = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        chunk(&mut v, b"IHDR", &[0, 0, 0, 2, 0, 0, 0, 1, 8, 3, 0, 0, 0]);
        chunk(&mut v, b"PLTE", &[0xff, 0, 0, 0, 0, 0xff]);
        chunk(&mut v, b"IDAT", &zlib);
        chunk(&mut v, b"IEND", &[]);
        let d = decode(&v).unwrap_or_else(|| panic!("调色板解码失败"));
        assert_eq!((d.w, d.h), (2, 1));
        assert_eq!(d.rgba, [0xff, 0, 0, 0xff, 0, 0, 0xff, 0xff]);
    }

    /// 调色板 + tRNS（ctype 3）：逐索引 alpha 查表；短表缺省 255。
    /// 3 项调色板（红/绿/蓝）+ 2 项 tRNS（128/0）——第三项索引缺省
    /// 不透明（语料 bullet.png / uparrow.png 同形态）。
    #[test]
    fn decode_palette_trns_png() {
        let raw: [u8; 4] = [0, 0, 1, 2]; // 3×1：filter0 + 索引 0,1,2
        let zlib = zlib_stored(&raw);
        let mut v = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        chunk(&mut v, b"IHDR", &[0, 0, 0, 3, 0, 0, 0, 1, 8, 3, 0, 0, 0]);
        chunk(&mut v, b"PLTE", &[0xff, 0, 0, 0, 0xff, 0, 0, 0, 0xff]);
        chunk(&mut v, b"tRNS", &[0x80, 0x00]); // 短表：第三项缺省
        chunk(&mut v, b"IDAT", &zlib);
        chunk(&mut v, b"IEND", &[]);
        let d = decode(&v).unwrap_or_else(|| panic!("调色板 tRNS 解码失败"));
        assert_eq!((d.w, d.h), (3, 1));
        assert_eq!(
            d.rgba,
            [
                0xff, 0, 0, 0x80, // 索引 0：红，α=128
                0, 0xff, 0, 0x00, // 索引 1：绿，α=0（全透明）
                0, 0, 0xff, 0xff, // 索引 2：蓝，tRNS 短表缺省不透明
            ]
        );
    }

    /// 灰度 + alpha（ctype 4）：灰度复制三通道 + alpha 直通
    ///（语料 galpha.png / leftarrow.png 同形态，struct-image-10-t 用到）。
    #[test]
    fn decode_luma_alpha_png() {
        let raw: [u8; 5] = [0, 0x40, 0x80, 0xff, 0x00]; // 2×1：filter0 + (g,a)×2
        let zlib = zlib_stored(&raw);
        let mut v = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        chunk(&mut v, b"IHDR", &[0, 0, 0, 2, 0, 0, 0, 1, 8, 4, 0, 0, 0]);
        chunk(&mut v, b"IDAT", &zlib);
        chunk(&mut v, b"IEND", &[]);
        let d = decode(&v).unwrap_or_else(|| panic!("灰度+alpha 解码失败"));
        assert_eq!((d.w, d.h), (2, 1));
        assert_eq!(
            d.rgba,
            [
                0x40, 0x40, 0x40, 0x80, // 灰 0x40 复制三通道，α=0x80
                0xff, 0xff, 0xff, 0x00, // 灰 0xff，α=0（全透明）
            ]
        );
    }

    /// 语料真实文件回归：uparrow.png（ctype 3 + PLTE + tRNS，10×10）
    /// 经 base64 端到端——四角透明（α=0）、中心不透明灰（PIL 同文件
    /// 对照采样）。galpha.png（ctype 4）同路径见 struct-image-10-t 的
    /// CLI/l2 端到端（16.5KB b64 不入源码）。
    #[test]
    fn decode_corpus_palette_trns_uparrow() {
        let b64 = "iVBORw0KGgoAAAANSUhEUgAAAAoAAAAKCAMAAAC67D+PAAAABGdBTUEAALGOfPtRkwAAACBjSFJNAAB6JQAAgIMAAPn/AACA5gAAdS4AAOpfAAA6lwAAF29p5MQrAAADAFBMVEUAAAABAQECAgIDAwMEBAQFBQUGBgYHBwcICAgJCQkKCgoLCwsMDAwNDQ0ODg4PDw8QEBARERESEhITExMUFBQVFRUWFhYXFxcYGBgZGRkaGhobGxscHBwdHR0eHh4fHx8gICAhISEiIiIjIyMkJCQlJSUmJiYnJycoKCgpKSkqKiorKyssLCwtLS0uLi4vLy8wMDAxMTEyMjIzMzM0NDQ1NTU2NjY3Nzc4ODg5OTk6Ojo7Ozs8PDw9PT0+Pj4/Pz9AQEBBQUFCQkJDQ0NERERFRUVGRkZHR0dISEhJSUlKSkpLS0tMTExNTU1OTk5PT09QUFBRUVFSUlJTU1NUVFRVVVVWVlZXV1dYWFhZWVlaWlpbW1tcXFxdXV1eXl5fX19gYGBhYWFiYmJjY2NkZGRlZWVmZmZnZ2doaGhpaWlqampra2tsbGxtbW1ubm5vb29wcHBxcXFycnJzc3N0dHR1dXV2dnZ3d3d4eHh5eXl6enp7e3t8fHx9fX1+fn5/f3+AgICBgYGCgoKDg4OEhISFhYWGhoaHh4eIiIiJiYmKioqLi4uMjIyNjY2Ojo6Pj4+QkJCRkZGSkpKTk5OUlJSVlZWWlpaXl5eYmJiZmZmampqbm5ucnJydnZ2enp6fn5+goKChoaGioqKjo6OkpKSlpaWmpqanp6eoqKipqamqqqqrq6usrKytra2urq6vr6+wsLCxsbGysrKzs7O0tLS1tbW2tra3t7e4uLi5ubm6urq7u7u8vLy9vb2+vr6/v7/AwMDBwcHCwsLDw8PExMTFxcXGxsbHx8fIyMjJycnKysrLy8vMzMzNzc3Ozs7Pz8/Q0NDR0dHS0tLT09PU1NTV1dXW1tbX19fY2NjZ2dna2trb29vc3Nzd3d3e3t7f39/g4ODh4eHi4uLj4+Pk5OTl5eXm5ubn5+fo6Ojp6enq6urr6+vs7Ozt7e3u7u7v7+/w8PDx8fHy8vLz8/P09PT19fX29vb39/f4+Pj5+fn6+vr7+/v8/Pz9/f3+/v7////isF19AAABAHRSTlP///////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////8AU/cHJQAAAENJREFUeJxi+A8ELS0gEiCAGMAsMBsggBggLBAbIIAYoCwgGyCAGFrgACCAGBDaAAIIiQkQQEhMgABCYgIEEBITIMAAqExQZb3ja54AAAAASUVORK5CYII=";
        let d = decode_base64(b64).unwrap_or_else(|| panic!("uparrow.png 解码失败"));
        assert_eq!((d.w, d.h), (10, 10));
        let at = |x: usize, y: usize| {
            let o = (y * 10 + x) * 4;
            (d.rgba[o], d.rgba[o + 1], d.rgba[o + 2], d.rgba[o + 3])
        };
        assert_eq!(at(0, 0), (255, 255, 255, 0), "左上 tRNS 全透明");
        assert_eq!(at(9, 9), (255, 255, 255, 0), "右下 tRNS 全透明");
        assert_eq!(at(5, 5), (132, 132, 132, 255), "中心不透明灰（PIL 对照）");
    }

    /// Adam7 隔行（interlace=1）：4×4 RGB 七 pass 重建。夹具由
    /// Python 生成（RFC 2083 pass 表，filter 0 逐 pass 打包，动态
    /// Huffman zlib），PIL 解码同帧逐像素核对——四角+内部取样值与
    /// PIL 一致。pass 表的 p5=(0,2,2,4)/p7=(0,1,1,2) 由 stefan 语料
    /// 24582 字节预算钉死（见 decode_corpus_adam7_stefan_budget）。
    #[test]
    fn decode_adam7_rgb_4x4() {
        let b64 = "iVBORw0KGgoAAAANSUhEUgAAAAQAAAAECAIAAAFRlDm/AAAAK0lEQVR4nA3HMQEAAAzCMERUToUhDIHbkSNJSF9KSwz7WfaLKJX5GY6OjQMTggyB5SkkigAAAABJRU5ErkJggg==";
        let d = decode_base64(b64).unwrap_or_else(|| panic!("Adam7 解码失败"));
        assert_eq!((d.w, d.h), (4, 4));
        assert_eq!(d.rgba.len(), 64);
        let at = |x: usize, y: usize| {
            let o = (y * 4 + x) * 4;
            (d.rgba[o], d.rgba[o + 1], d.rgba[o + 2], d.rgba[o + 3])
        };
        assert_eq!(at(0, 0), (0, 0, 20, 255)); // p1
        assert_eq!(at(3, 3), (180, 180, 20, 255)); // p7
        assert_eq!(at(2, 1), (120, 60, 20, 255)); // p6
        assert_eq!(at(1, 2), (60, 120, 20, 255)); // p6
        assert_eq!(at(0, 2), (0, 120, 20, 255)); // p5
        assert_eq!(at(2, 0), (120, 0, 20, 255)); // p4
        assert_eq!(at(1, 0), (60, 0, 20, 255)); // p6
    }

    /// 亚字节位深调色板（4-bit，magnify.png 同形态）：3×1 两字节行
    ///（索引 0,1,2 + 行尾 padding nybble），PLTE 4 项 + tRNS 短表。
    #[test]
    fn decode_palette_4bit_trns() {
        let raw: [u8; 3] = [0, 0x01, 0x20]; // 行0：filter0 + 索引 0,1 | 2,pad
        let zlib = zlib_stored(&raw);
        let mut v = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        chunk(&mut v, b"IHDR", &[0, 0, 0, 3, 0, 0, 0, 1, 4, 3, 0, 0, 0]);
        chunk(&mut v, b"PLTE", &[10, 0, 0, 0, 20, 0, 0, 0, 30, 0, 0, 0]);
        chunk(&mut v, b"tRNS", &[0xff, 0x40, 0x00]);
        chunk(&mut v, b"IDAT", &zlib);
        chunk(&mut v, b"IEND", &[]);
        let d = decode(&v).unwrap_or_else(|| panic!("4-bit 调色板解码失败"));
        assert_eq!((d.w, d.h), (3, 1));
        assert_eq!(
            d.rgba,
            [
                10, 0, 0, 0xff, // 索引 0（高 nybble 在前）
                0, 20, 0, 0x40, // 索引 1
                0, 0, 30, 0x00, // 索引 2（低 nybble）
            ]
        );
    }

    /// 亚字节位深灰度（2-bit）：3×1 单字节行，值 1,2,3 → 规范缩放
    /// v·255/3 = 85,170,255（行尾 2 bit padding 不得串行）。
    #[test]
    fn decode_gray_2bit() {
        let raw: [u8; 2] = [0, 0b01_10_11_00];
        let zlib = zlib_stored(&raw);
        let mut v = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        chunk(&mut v, b"IHDR", &[0, 0, 0, 3, 0, 0, 0, 1, 2, 0, 0, 0, 0]);
        chunk(&mut v, b"IDAT", &zlib);
        chunk(&mut v, b"IEND", &[]);
        let d = decode(&v).unwrap_or_else(|| panic!("2-bit 灰度解码失败"));
        assert_eq!((d.w, d.h), (3, 1));
        assert_eq!(
            d.rgba,
            [
                85, 85, 85, 255, //
                170, 170, 170, 255, //
                255, 255, 255, 255, //
            ]
        );
    }

    /// gAMA 色域校正（Chrome 实证口径）：g=0.30 →
    /// `round(255·srgb_enc((s/255)^(1/g)))`，中灰 128 → 89；identity
    /// 窗口/特例：gAMA≈1/2.2（0.45/0.45455）恒等；cHRM 在场**照常
    /// 转换**（车道KA 修订：ka1 活体探针 smiley/20x20/bumpMap/sign
    /// 五图全 100% ≤2——旧「cHRM 忽略 gAMA」口径是窗口 gAMA 等价区
    /// 掩盖，已否证）。alpha 通道不参与。
    #[test]
    fn decode_gama_lut_and_identity_windows() {
        let build = |extra: &[(&[u8; 4], &[u8])]| {
            let mut v = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
            chunk(&mut v, b"IHDR", &[0, 0, 0, 2, 0, 0, 0, 1, 8, 2, 0, 0, 0]);
            for (ty, data) in extra {
                chunk(&mut v, ty, data);
            }
            chunk(
                &mut v,
                b"IDAT",
                &zlib_stored(&[0, 128, 128, 128, 200, 200, 200]),
            );
            chunk(&mut v, b"IEND", &[]);
            v
        };
        // g=0.30：128 → 89、200 → 178（f64 参考式算得）；端点 0/255
        // 不动（gamma 不变域）
        let v = build(&[(b"gAMA", &30000u32.to_be_bytes())]);
        let d = decode(&v).unwrap_or_else(|| panic!("gAMA 解码失败"));
        let px = |i: usize| &d.rgba[i * 4..i * 4 + 4];
        assert_eq!(px(0), &[89, 89, 89, 255]);
        assert_eq!(px(1), &[178, 178, 178, 255]);

        // identity 窗口：|g − 1/2.2| ≤ 0.005（0.45 / 0.45455 均在窗内）
        for code in [45000u32, 45455u32] {
            let v = build(&[(b"gAMA", &code.to_be_bytes())]);
            let d = decode(&v).unwrap_or_else(|| panic!("gAMA {code} 解码失败"));
            assert_eq!(d.rgba[0], 128, "g={code} 应恒等");
        }
        // cHRM 存在 → 照常转换（车道KA：g=0.22727 时 128 → 62）
        let chrm = [
            0u8, 0, 0x7c, 0x6f, 0, 0, 0, 0, 0, 0, 0x30, 0xa3, 0, 0, 0, 0, 0, 0, 0xbc, 0x61, 0, 0,
            0, 0, 0, 0, 0x18, 0xd0, 0, 0, 0, 0,
        ];
        let v = build(&[(b"gAMA", &22727u32.to_be_bytes()), (b"cHRM", &chrm)]);
        let d = decode(&v).unwrap_or_else(|| panic!("cHRM+gAMA 解码失败"));
        assert_eq!(d.rgba[0], 62, "cHRM 在场应照常按 gAMA 转换（KA 实证）");
    }

    /// gAMA 管理标志（车道DB）：decode_tagged 的 bool = gAMA 是否实际
    /// 改写字节。feImage「字节直通再解释」注入据此跳过 as-linear LUT
    ///（tagged 位图解码产物已是工作域编码，再套 LUT = 双重提亮，
    /// displace-01 g=1.0 贴图 189 vs 预言机 130 实证）。
    #[test]
    fn decode_tagged_gama_managed_flag() {
        let build = |extra: &[(&[u8; 4], &[u8])]| {
            let mut v = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
            chunk(&mut v, b"IHDR", &[0, 0, 0, 2, 0, 0, 0, 1, 8, 2, 0, 0, 0]);
            for (ty, data) in extra {
                chunk(&mut v, ty, data);
            }
            chunk(
                &mut v,
                b"IDAT",
                &zlib_stored(&[0, 128, 128, 128, 200, 200, 200]),
            );
            chunk(&mut v, b"IEND", &[]);
            v
        };
        // 无 gAMA / sRGB 窗口 / cHRM+gAMA / gAMA=0 → 未管理（raw 直通）
        for (name, extra) in [
            ("无 gAMA", &[] as &[(&[u8; 4], &[u8])]),
            (
                "窗口 0.45",
                &[(b"gAMA" as &[u8; 4], &45000u32.to_be_bytes() as &[u8])],
            ),
            (
                "gAMA=0",
                &[(b"gAMA" as &[u8; 4], &0u32.to_be_bytes() as &[u8])],
            ),
        ] {
            let v = build(extra);
            let (d, managed) = decode_tagged(&v).unwrap_or_else(|| panic!("{name} 解码失败"));
            assert!(!managed, "{name} 不应标记 managed");
            assert_eq!(d.rgba[0], 128, "{name} 应恒等");
            assert_eq!(d.rgba[4], 200, "{name} 第二像素应恒等");
        }
        // cHRM 在场 → gAMA 照常转换（车道KA 修订：ka1 活体探针五图
        // 全 100% ≤2，Chrome 忽略 cHRM primaries 只走 gAMA 传递函数；
        // 旧口径「cHRM → 恒等」是窗口 gAMA 等价区掩盖）
        let chrm = [
            0u8, 0, 0x7c, 0x6f, 0, 0, 0, 0, 0, 0, 0x30, 0xa3, 0, 0, 0, 0, 0, 0, 0xbc, 0x61, 0, 0,
            0, 0, 0, 0, 0x18, 0xd0, 0, 0, 0, 0,
        ];
        let v = build(&[(b"gAMA", &22727u32.to_be_bytes()), (b"cHRM", &chrm)]);
        let (d, managed) = decode_tagged(&v).unwrap_or_else(|| panic!("cHRM 解码失败"));
        assert!(managed, "cHRM+窗口外 gAMA 应标记 managed（KA 实证）");
        assert_eq!(
            d.rgba[0], 62,
            "g=0.22727：128 → srgb_enc((128/255)^(1/0.22727))=62"
        );
        // iCCP / sRGB 块在场 = 完整 profile 声明 → 跳过 gamma-only（未管理）
        let v = build(&[
            (b"gAMA", &22727u32.to_be_bytes()),
            (b"cHRM", &chrm),
            (b"iCCP", b"profile-name\x00\x00".as_slice()),
        ]);
        let (d, managed) = decode_tagged(&v).unwrap_or_else(|| panic!("iCCP 解码失败"));
        assert!(!managed, "iCCP 在场不应标记 managed");
        assert_eq!(d.rgba[0], 128, "iCCP 在场应恒等");
        let v = build(&[
            (b"gAMA", &22727u32.to_be_bytes()),
            (b"sRGB", &0u8.to_be_bytes()),
        ]);
        let (d, managed) = decode_tagged(&v).unwrap_or_else(|| panic!("sRGB 块解码失败"));
        assert!(!managed, "sRGB 块在场不应标记 managed");
        assert_eq!(d.rgba[0], 128, "sRGB 块在场应恒等");
        // g=1.0 / g=0.30 → 管理（字节已提亮到工作域编码）
        let v = build(&[(b"gAMA", &100000u32.to_be_bytes())]);
        let (d, managed) = decode_tagged(&v).unwrap_or_else(|| panic!("g=1.0 解码失败"));
        assert!(managed, "g=1.0 应标记 managed");
        assert_eq!(d.rgba[0], 188, "g=1.0：128 → srgb_enc(128/255)=188");
        let v = build(&[(b"gAMA", &30000u32.to_be_bytes())]);
        let (_, managed) = decode_tagged(&v).unwrap_or_else(|| panic!("g=0.30 解码失败"));
        assert!(managed, "g=0.30 应标记 managed");
    }

    /// 放弃项与损坏形态 → None：16 位 / 坏签名 / 截断 / tRNS 色键。
    /// （Adam7 与亚字节位深已是支持集，见上方专测。）
    #[test]
    fn unsupported_and_corrupt_forms_rejected() {
        let ihdr_data = 8 + 8; // IHDR 数据区起点（签名 8 + 长度 4 + 类型 4）
        let mut v = png_2x2_rgb();
        v[ihdr_data + 8] = 16; // 位深 16
        assert!(decode(&v).is_none(), "16 位深应放弃");

        let mut v = png_2x2_rgb();
        v[0] = 0x00; // 坏签名
        assert!(decode(&v).is_none());

        let v = png_2x2_rgb();
        // 尾部仅缺 IEND → 宽容成功（Chrome 同样接受完整 IDAT 的流）；
        // 切进 IDAT 载荷（stored LEN 与数据不符）→ None
        assert!(decode(&v[..v.len() - 10]).is_some());
        assert!(decode(&v[..v.len() - 30]).is_none(), "IDAT 载荷截断应失败");

        // tRNS 色键（ctype 2）出现在 IDAT 前 → None（ctype 0/2 不支持；
        // ctype 3 的透明表放行，见 decode_palette_trns_png）
        let mut v = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        chunk(&mut v, b"IHDR", &[0, 0, 0, 2, 0, 0, 0, 1, 8, 2, 0, 0, 0]);
        chunk(&mut v, b"tRNS", &[0, 0]);
        chunk(&mut v, b"IDAT", &zlib_stored(&[0, 1, 2, 3]));
        chunk(&mut v, b"IEND", &[]);
        assert!(decode(&v).is_none(), "ctype 0/2 的 tRNS 色键应放弃");
    }

    /// base64：padding/空白容忍 + 非法字符拒绝。
    #[test]
    fn base64_tolerance() {
        // "AQID" = [1,2,3]
        assert_eq!(decode_base64("!!!").map(|_| ()), None::<()>);
        // 用真实小 PNG 验证 padding 与空白形态等价
        let b64_full = {
            // 2x2 PNG 的 base64（由 png_2x2_rgb 生成后手工校验过）
            let p = png_2x2_rgb();
            // 简单 base64 编码器（测试专用，允许 expect 风格的固定输入）
            let tbl = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
            let mut out = String::new();
            for c in p.chunks(3) {
                let b = [c[0], *c.get(1).unwrap_or(&0), *c.get(2).unwrap_or(&0)];
                let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
                out.push(tbl[(n >> 18) as usize & 63] as char);
                out.push(tbl[(n >> 12) as usize & 63] as char);
                out.push(if c.len() > 1 {
                    tbl[(n >> 6) as usize & 63] as char
                } else {
                    '='
                });
                out.push(if c.len() > 2 {
                    tbl[n as usize & 63] as char
                } else {
                    '='
                });
            }
            out
        };
        let d = decode_base64(&b64_full).unwrap_or_else(|| panic!("padding 形态解码失败"));
        assert_eq!((d.w, d.h), (2, 2));
        let spaced = format!("{}  ", b64_full); // 尾随空白容忍
        assert!(decode_base64(&spaced).is_some());
    }
}
