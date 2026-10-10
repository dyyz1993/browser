//! 位图 mip 链（Skia kMedium 等价）+ 16.16 定点双线性采样（车道BB）。
//!
//! Chrome/Skia 对 `<image>`/feImage 位图缩放（image-rendering: auto）的
//! 实测规格（`tests/oracle/kmedium_ref.py` 对 bb-up/bb-down 探针逐位拟合，
//! 2026-09-11）：
//!
//! 1. **链生成**：premul 域逐级 1/2 box 平均（f64 累加），层级尺寸
//!    `max(1, dim >> 1)`，每级落地 u8 时 **floor**（非四舍五入——
//!    127.5 → 127 实证）。1:1 抽帧与 Chrome 链逐位一致（L1/L2/L3
//!    exact=1.000/0.999/1.000）。
//! 2. **层级选择**：`level = floor(log2(max(df_x, df_y)))`，df = 逆
//!    矩阵行向量模（设备像素覆盖的源纹素数）；< 2（放大/1:1）时
//!    level = 0。已验证 2x→L1、3.048x→L1、4x→L2、4.923x→L2、
//!    5.818x→L2、8x→L3、1.4545x→L0、各向异性→L1（max 轴）。
//! 3. **采样器**（SkBitmapProcState 传统定点路径）：坐标折算到层级
//!    空间 `u_lvl = u_src · (lw/iw) − 0.5`（半像素偏移在**层级空间**），
//!    乘 65536 **floor** 成 16.16 定点；基纹素 `x0 = xi >> 16`，
//!    权重 `wx = (xi >> 12) & 0xF`（**4bit 截断**，1/16 粒度）。
//!    13/11/21/aniso 四区域 tol2=1.000/1.000/0.952/1.000 逐位复现。
//! 4. **混合**：premul u8 域双轴 f32 混合后单次 floor 落 u8，再
//!    unpremul（`c·255/a` 后 floor）。透明区 premul 黑（a=0 → 0 色）
//!    自然不晕染。
//!
//! 与 Chrome 的已知残余：feImage 路径的 Chrome 端 ramp 略缓于普通
//! `<image>`（img-01 feImage 行值 12/74/160/232 vs 普通 0/0/63/143/239，
//! 同几何）——Chrome feImage 走了不同精度管线；我们统一用普通路径
//! 采样器，feImage 探针收敛但不到 0%（登记 CONFORMANCE）。

use crate::dl::ImgData;
use alloc::vec::Vec;

/// 单个 mip 层级（premul RGBA8，行主序）。Clone（车道RL）：retained
/// 池携带式预计算的槽位对齐用（`Vec::resize` 约束）；纯数据拷贝，
/// 语义中性。
#[derive(Clone)]
pub struct MipLevel {
    pub w: u32,
    pub h: u32,
    pub rgba: Vec<u8>,
}

/// 层级数上限（内存守卫：2^16 已远超任何真实位图；链总字节
/// < 源图 1/3，天然有界）。
const MAX_LEVELS: u32 = 16;

/// 生成到 `need` 级为止的 mip 链（含 level 0 的拷贝语义：level 0 即
/// 源图 premul 化）。`need` 已含 level 0 计数。纯函数——车道hwB 起取代
/// 原 `ensure_chain`（ImgData 内嵌 `RefCell` 缓存）：内嵌缓存使
/// DisplayList 携带内部可变态（!Sync），行带并行下同图跨 band 并发
/// 首建会 borrow_mut 冲突。链的驻留点上移到渲染调用方（全帧 = 帧内
/// 局部缓存；band = FrameCache 预生成共享只读），确定性重建输出逐
/// 字节一致，DL 因此是可跨线程共享的纯数据。
pub fn build_chain(data: &ImgData, need: u32) -> Vec<MipLevel> {
    let mut levels = Vec::new();
    levels.push(MipLevel {
        w: data.w,
        h: data.h,
        rgba: premul(&data.rgba),
    });
    while (levels.len() as u32) < need && (levels.len() as u32) < MAX_LEVELS {
        let prev = levels.last().expect("non-empty");
        if prev.w == 1 && prev.h == 1 {
            break; // 已到 1×1，重复层级无信息
        }
        levels.push(halve(prev));
    }
    levels
}

/// 需要的层级数（`df` = 逆矩阵行模，见模块文档；≥ 2 才进链）。
pub fn level_count(df_x: f32, df_y: f32) -> u32 {
    // f32::log2 是 std 专属——libm::log2f。退化/非有限防御为 0 级。
    let df = df_x.max(df_y);
    if !df.is_finite() || df <= 1.0 {
        return 1;
    }
    let l = libm::log2f(df);
    if l.is_nan() || l <= 0.0 {
        return 1;
    }
    // floor + 1（层级数含 level 0）；上限 MAX_LEVELS
    ((l as i64 + 1) as u32).min(MAX_LEVELS)
}

/// premul 折算（直 RGBA8 → premul RGBA8，floor）。
fn premul(src: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(src.len());
    for px in src.chunks_exact(4) {
        let a = px[3] as u32;
        out.push(((px[0] as u32 * a + 127) / 255).min(255) as u8);
        out.push(((px[1] as u32 * a + 127) / 255).min(255) as u8);
        out.push(((px[2] as u32 * a + 127) / 255).min(255) as u8);
        out.push(px[3]);
    }
    out
}

/// 逐级 1/2 box 平均（premul 域）。f64 累加（与 Python 拟合口径一致；
/// u32 累加 + 整除的整数口径在奇数尺寸分数权重处与 Chrome 有 ±1 漂移）。
/// 每级输出 floor（Chrome 实证 127.5 → 127）。
fn halve(prev: &MipLevel) -> MipLevel {
    let (sw, sh) = (prev.w as usize, prev.h as usize);
    let dw = (prev.w as usize >> 1).max(1);
    let dh = (prev.h as usize >> 1).max(1);
    let sx = sw as f64 / dw as f64;
    let sy = sh as f64 / dh as f64;
    let mut out = alloc::vec![0u8; dw * dh * 4];
    for dy in 0..dh {
        let y_lo = dy as f64 * sy;
        let y_hi = (dy + 1) as f64 * sy;
        let iy1 = (libm::ceil(y_hi) as usize).min(sh);
        for dx in 0..dw {
            let x_lo = dx as f64 * sx;
            let x_hi = (dx + 1) as f64 * sx;
            let ix1 = (libm::ceil(x_hi) as usize).min(sw);
            let mut acc = [0.0f64; 4];
            let mut wsum = 0.0f64;
            let mut yy = libm::floor(y_lo) as usize;
            while yy < iy1 {
                let wy = y_hi.min(yy as f64 + 1.0) - y_lo.max(yy as f64);
                let row = yy * sw;
                let mut xx = libm::floor(x_lo) as usize;
                while xx < ix1 {
                    let wx = x_hi.min(xx as f64 + 1.0) - x_lo.max(xx as f64);
                    let w = wx * wy;
                    let o = (row + xx) * 4;
                    acc[0] += prev.rgba[o] as f64 * w;
                    acc[1] += prev.rgba[o + 1] as f64 * w;
                    acc[2] += prev.rgba[o + 2] as f64 * w;
                    acc[3] += prev.rgba[o + 3] as f64 * w;
                    wsum += w;
                    xx += 1;
                }
                yy += 1;
            }
            let o = (dy * dw + dx) * 4;
            out[o] = libm::floor(acc[0] / wsum) as u8;
            out[o + 1] = libm::floor(acc[1] / wsum) as u8;
            out[o + 2] = libm::floor(acc[2] / wsum) as u8;
            out[o + 3] = libm::floor(acc[3] / wsum) as u8;
        }
    }
    MipLevel {
        w: dw as u32,
        h: dh as u32,
        rgba: out,
    }
}

/// 在 premul 纹素上做 16.16 定点双轴混合（车道BB 规格 3/4 条）。
/// `u`/`v` 为层级 0 源坐标（[0,w)×[0,h) 内，调用方已守卫）；`lw`/`lh`
/// 为层级尺寸。返回直色 RGBA8（unpremul，floor）。
pub fn sample_level(lvl: &MipLevel, lw: u32, lh: u32, iw: f32, ih: f32, u: f32, v: f32) -> [u8; 4] {
    // 坐标折算到层级空间（半像素偏移在层级空间——Python 拟合口径）
    let ul = (u * (lw as f32 / iw) - 0.5).max(0.0);
    let vl = (v * (lh as f32 / ih) - 0.5).max(0.0);
    // 16.16 定点（floor）；as 截断即 floor（非负域）。上限钳到最后一纹素。
    let sw = lw as usize;
    let sh = lh as usize;
    let xi = ((ul * 65536.0) as i64).min(((sw as i64) << 16) - 1).max(0);
    let yi = ((vl * 65536.0) as i64).min(((sh as i64) << 16) - 1).max(0);
    let x0 = (xi >> 16) as usize;
    let y0 = (yi >> 16) as usize;
    let x1 = (x0 + 1).min(sw - 1);
    let y1 = (y0 + 1).min(sh - 1);
    let wx = ((xi >> 12) & 0xF) as f32 / 16.0;
    let wy = ((yi >> 12) & 0xF) as f32 / 16.0;

    let at = |xx: usize, yy: usize| -> [f32; 4] {
        let o = (yy * sw + xx) * 4;
        [
            lvl.rgba[o] as f32,
            lvl.rgba[o + 1] as f32,
            lvl.rgba[o + 2] as f32,
            lvl.rgba[o + 3] as f32,
        ]
    };
    let p00 = at(x0, y0);
    let p01 = at(x1, y0);
    let p10 = at(x0, y1);
    let p11 = at(x1, y1);
    // 单次 floor 落 u8（双轴 f32 混合后量化一次——拟合口径）
    let mut pm = [0u8; 4];
    for c in 0..4 {
        let top = p00[c] * (1.0 - wx) + p01[c] * wx;
        let bot = p10[c] * (1.0 - wx) + p11[c] * wx;
        pm[c] = libm::floorf(top * (1.0 - wy) + bot * wy) as u8;
    }
    // unpremul（floor）：a=0 保持全 0（premul 黑）
    let a = pm[3] as u32;
    if a == 0 {
        return [0, 0, 0, 0];
    }
    [
        ((pm[0] as u32 * 255) / a) as u8,
        ((pm[1] as u32 * 255) / a) as u8,
        ((pm[2] as u32 * 255) / a) as u8,
        pm[3],
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::{vec, vec::Vec};

    fn img(w: u32, h: u32, rgba: Vec<u8>) -> ImgData {
        ImgData { w, h, rgba }
    }

    /// 链生成：4×4 不透明灰度渐变，2 次减半后 1×1 = 四象限均值 floor。
    #[test]
    fn chain_halving_floor() {
        // 4×4：左半 255 白，右半 254
        let mut px = vec![0u8; 64];
        for y in 0..4 {
            for x in 0..4 {
                let v = if x < 2 { 255u8 } else { 254 };
                let o = (y * 4 + x) * 4;
                px[o] = v;
                px[o + 1] = v;
                px[o + 2] = v;
                px[o + 3] = 255;
            }
        }
        let data = img(4, 4, px);
        let chain = build_chain(&data, 3);
        assert_eq!(chain.len(), 3);
        assert_eq!((chain[1].w, chain[1].h), (2, 2));
        // L1 左列 = (255+255+255+255)/4=255，右列 = 254
        //（行主序：pixel(1,0) 在字节 [4..8]，pixel(0,1) 在 [8..12]）
        assert_eq!(chain[1].rgba[0], 255);
        assert_eq!(chain[1].rgba[4], 254);
        assert_eq!(chain[1].rgba[8], 255);
        assert_eq!(chain[1].rgba[12], 254);
        assert_eq!((chain[2].w, chain[2].h), (1, 1));
        assert_eq!(chain[2].rgba[0], 254); // (255*3+254)/4 = 254.75 floor
    }

    /// 奇数尺寸：面积平均的分数权重（3×3 → 1×1 = 全图均值 floor）。
    #[test]
    fn chain_odd_area_average() {
        let mut px = vec![0u8; 36];
        for i in 0..9 {
            let o = i * 4;
            px[o] = 100 + i as u8; // 100..108
            px[o + 3] = 255;
        }
        let data = img(3, 3, px);
        let chain = build_chain(&data, 2);
        assert_eq!((chain[1].w, chain[1].h), (1, 1));
        // 均值 = (100+..+108)/9 = 104
        assert_eq!(chain[1].rgba[0], 104);
    }

    /// 层级选择：floor(log2(max(df)))（含放大钳 0 与各向异性 max）。
    #[test]
    fn level_selection() {
        assert_eq!(level_count(0.3, 0.3), 1); // 放大 → L0（数=级数，L0）
        assert_eq!(level_count(1.4545, 1.4545), 1); // log2 0.54 → L0
        assert_eq!(level_count(2.0, 2.0), 2); // L1
        assert_eq!(level_count(3.048, 3.048), 2); // 1.608 → L1
        assert_eq!(level_count(4.0, 4.0), 3); // L2
        assert_eq!(level_count(5.818, 5.818), 3); // 2.54 → L2
        assert_eq!(level_count(8.0, 8.0), 4); // L3
        assert_eq!(level_count(3.2, 1.6), 2); // 各向异性 max 轴
        assert_eq!(level_count(1.0, 8.0), 4);
    }

    /// 采样器：放大 3.125 的 Chrome 实测 ramp（bb-up img@3.125 左缘
    /// R 通道 0/0/63/143/239/255）逐位复现——源用 32×16 四色图左缘
    /// （绿→红过渡）。纹理直接手造等效 1×16 竖条不可行（x 权重独立），
    /// 用完整几何：32×16 图 row0=绿 row1=红。
    #[test]
    fn sampler_upscale_ramp_quantized() {
        // 竖直过渡带：2 行绿 + 14 行红（bb 探针源 row0 绿 row1 红，
        // 但 32/100 的 x 缩放不影响左缘单列——x0=0,x1=1 全绿/全红）
        let mut px = vec![0u8; 32 * 16 * 4];
        for y in 0..16 {
            for x in 0..32 {
                let o = (y * 32 + x) * 4;
                let (r, g) = if y < 1 { (0u8, 255u8) } else { (255, 0) };
                px[o] = r;
                px[o + 1] = g;
                px[o + 3] = 255;
            }
        }
        let data = img(32, 16, px);
        // df = 0.32 → L0
        let chain = build_chain(&data, 1);
        let l0 = &chain[0];
        // 设备行 r 的源坐标 v = (r+0.5)*16/50（→ 层级 0 空间）
        // 设备行 r 的源坐标 v = (r+0.5)·(16/50)；Chrome 实测 ramp
        let expect = [0u8, 0, 63, 143, 239, 255];
        for (r, &want) in expect.iter().enumerate() {
            let v = (r as f32 + 0.5) * (16.0 / 50.0);
            let px = sample_level(l0, 32, 16, 32.0, 16.0, 0.5, v);
            assert_eq!(px[0], want, "row {r}");
        }
    }

    /// 采样器：premul 透明邻域不晕染（a=0 premul 黑）。
    #[test]
    fn sampler_premul_transparent_edge() {
        // 2×2：左列白不透明，右列全透明
        let px = vec![
            255, 255, 255, 255, 0, 0, 0, 0, 255, 255, 255, 255, 0, 0, 0, 0,
        ];
        let data = img(2, 2, px);
        let chain = build_chain(&data, 1);
        // u=1.0/v=1.0（列/行正中）→ 左右各半：premul 混合 a=127，
        // 直色 = 127*255/127 = 255（无黑边——premul 域采样的意义）
        let px = sample_level(&chain[0], 2, 2, 2.0, 2.0, 1.0, 1.0);
        assert_eq!(px[3], 127);
        assert_eq!(px[0], 255);
    }

    /// 链缓存：二次请求不重建（指针稳定即可观察为同长度 + 内容一致）。
    #[test]
    fn chain_cached() {
        let data = img(4, 4, alloc::vec![255; 64]);
        let a = build_chain(&data, 2);
        let first_len = a.len();
        drop(a);
        let b = build_chain(&data, 2);
        assert_eq!(b.len(), first_len);
    }
}
