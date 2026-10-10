//! svgine-simd —— svgine 热路径的 SIMD 辅助（车道EF，2026-09-12）。
//!
//! 存在理由：svgine-core 是 `#![forbid(unsafe_code)]`，而 `core::arch`
//! intrinsics 一律是 unsafe fn——本 crate 承接这一小片 unsafe（同
//! svgine-ffi 先例：crate 级 deny + 函数级 allow + Safety 注释），以
//! **target-gated dependency** 挂进 svgine-core
//!（`[target.'cfg(any(target_arch = "aarch64", target_arch = "x86_64"))'.dependencies]`）：
//! riscv 等其余架构根本不构建本 crate，对 MCU 体积/行为零影响。
//!
//! # 字节级等价契约（feTurbulence 4 通道臂）
//!
//! 首个消费者是 raster::turbulence_bitmap 的内圈（车道DA 残余）：
//! 4 个颜色通道（R/G/B/A）的 octave 求和彼此独立，各占一个 lane。
//! 每条 lane 的运算序列与 scalar 参考 `raster::turb_channel`（车道DA
//! 原文）**逐 op 一致**：
//!
//! 1. 全部运算（add/sub/mul/abs/load/store）是 elementwise 的
//!    IEEE-754 单精度指令，lane 之间无任何交互（无水平归约、无
//!    重排），同 lane 内运算顺序与 scalar 逐项相同；
//! 2. 无 FMA 收缩——Rust 不做 fp 收缩，intrinsics 是显式
//!    mul + add 两条指令（NEON vfma/SSE FMA 需显式 intrinsic，
//!    编译器绝不把 addps/subps 族收缩成 fused）；
//! 3. abs = 符号位清除（NEON `vabsq` / SSE `and 0x7FFFFFFF`），
//!    对 ±0/NaN 与 scalar `f32::abs` 位型一致；
//! 4. 不触碰 FTZ/DAZ 环境（FPCR/MXCSR 缺省，engine 全程不改），
//!    subnormal 行为与 scalar 一致。
//!
//! 由此 SIMD 路径与 scalar 路径**逐字节一致**（tests 钉死：
//! raster.rs `turb_vec_channels_bit_exact_vs_scalar` /
//! `turbulence_bitmap_simd_matches_scalar_reference_fullframe` /
//! DA 车道的 preopt 参考对照测试）。
//!
//! # 目标基线
//!
//! aarch64（含 iOS/Android）NEON 是基线能力；x86_64 SSE2 是基线
//! 能力——无需运行时检测。其余架构不编译本 crate（见 compile_error）。

#![no_std]
#![deny(unsafe_code)]

// 测试 harness 需要 std（no_std crate 惯例：cfg(test) 下显式引入）。
#[cfg(test)]
extern crate std;

#[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
compile_error!(
    "svgine-simd 只支持 aarch64/x86_64；本 crate 经 target-gated dependency 挂载，其余架构不应构建"
);

/// 梯度表的 SIMD 布局：`grad_v[lattice][xy][ch]`——每 (lattice, xy)
/// 一条连续 4-float（lane ch = 通道 ch），一个向量加载即取齐 4 通道
/// 的同一分量。值与 core 侧 scalar 布局 `grad[ch][lattice][xy]` 逐位
/// 相同（建表期同一算式直写两份）。
pub type GradTable = [[[f32; 4]; 2]; 256];

#[cfg(target_arch = "aarch64")]
mod arm {
    use core::arch::aarch64::*;

    pub type Raw = float32x4_t;

    /// Safety：aarch64 目标 NEON 为基线能力（无需运行时检测）；
    /// `ps` 指向合法 `[f32; 4]`（`vld1q` 无 16B 对齐要求）。
    #[inline(always)]
    #[allow(unsafe_code)]
    pub fn load(ps: &[f32; 4]) -> Raw {
        unsafe { vld1q_f32(ps.as_ptr()) }
    }

    /// Safety：同 [`load`]（NEON 基线）；`out` 为合法 `[f32; 4]`。
    #[inline(always)]
    #[allow(unsafe_code)]
    pub fn store(out: &mut [f32; 4], v: Raw) {
        unsafe { vst1q_f32(out.as_mut_ptr(), v) }
    }

    /// Safety：NEON 基线。
    #[inline(always)]
    #[allow(unsafe_code)]
    pub fn splat(x: f32) -> Raw {
        unsafe { vdupq_n_f32(x) }
    }

    /// Safety：NEON 基线。逐 lane fadd（IEEE，无收缩）。
    #[inline(always)]
    #[allow(unsafe_code)]
    pub fn add(a: Raw, b: Raw) -> Raw {
        unsafe { vaddq_f32(a, b) }
    }

    /// Safety：NEON 基线。逐 lane fsub（IEEE，无收缩）。
    #[inline(always)]
    #[allow(unsafe_code)]
    pub fn sub(a: Raw, b: Raw) -> Raw {
        unsafe { vsubq_f32(a, b) }
    }

    /// Safety：NEON 基线。逐 lane fmul（IEEE，无收缩）。
    #[inline(always)]
    #[allow(unsafe_code)]
    pub fn mul(a: Raw, b: Raw) -> Raw {
        unsafe { vmulq_f32(a, b) }
    }

    /// Safety：NEON 基线。逐 lane 符号位清除（±0/NaN 与 `f32::abs`
    /// 位型一致）。
    #[inline(always)]
    #[allow(unsafe_code)]
    pub fn abs(a: Raw) -> Raw {
        unsafe { vabsq_f32(a) }
    }

    /// Safety：NEON 基线。frintm = IEEE roundToIntegralTowardNegative
    ///（对 ±0/±inf/nan 全域与 `libm::floorf` 位型一致）。
    #[inline(always)]
    #[allow(unsafe_code)]
    pub fn floor_all(x: f32) -> Raw {
        unsafe { vrndmq_f32(vdupq_n_f32(x)) }
    }

    /// Safety：NEON 基线。提取 lane 0。
    #[inline(always)]
    #[allow(unsafe_code)]
    pub fn get0(v: Raw) -> f32 {
        unsafe { vgetq_lane_f32(v, 0) }
    }
}

#[cfg(target_arch = "x86_64")]
mod x86 {
    use core::arch::x86_64::*;

    pub type Raw = __m128;

    /// Safety：x86_64 目标 SSE2 为基线能力（无需运行时检测）；
    /// `ps` 指向合法 `[f32; 4]`（loadu 无对齐要求）。
    #[inline(always)]
    #[allow(unsafe_code)]
    pub fn load(ps: &[f32; 4]) -> Raw {
        unsafe { _mm_loadu_ps(ps.as_ptr()) }
    }

    /// Safety：同 [`load`]（SSE2 基线）；`out` 为合法 `[f32; 4]`。
    #[inline(always)]
    #[allow(unsafe_code)]
    pub fn store(out: &mut [f32; 4], v: Raw) {
        unsafe { _mm_storeu_ps(out.as_mut_ptr(), v) }
    }

    /// Safety：SSE2 基线。
    #[inline(always)]
    #[allow(unsafe_code)]
    pub fn splat(x: f32) -> Raw {
        unsafe { _mm_set1_ps(x) }
    }

    /// Safety：SSE2 基线。逐 lane addps（IEEE，无收缩）。
    #[inline(always)]
    #[allow(unsafe_code)]
    pub fn add(a: Raw, b: Raw) -> Raw {
        unsafe { _mm_add_ps(a, b) }
    }

    /// Safety：SSE2 基线。逐 lane subps（IEEE，无收缩）。
    #[inline(always)]
    #[allow(unsafe_code)]
    pub fn sub(a: Raw, b: Raw) -> Raw {
        unsafe { _mm_sub_ps(a, b) }
    }

    /// Safety：SSE2 基线。逐 lane mulps（IEEE，无收缩）。
    #[inline(always)]
    #[allow(unsafe_code)]
    pub fn mul(a: Raw, b: Raw) -> Raw {
        unsafe { _mm_mul_ps(a, b) }
    }

    /// Safety：SSE2 基线。逐 lane 位与。
    #[inline(always)]
    #[allow(unsafe_code)]
    pub fn and(a: Raw, b: Raw) -> Raw {
        unsafe { _mm_and_ps(a, b) }
    }

    /// Safety：SSE2 基线。符号位掩码 abs（±0/NaN 与 `f32::abs` 位型
    /// 一致）。
    #[inline(always)]
    pub fn abs(a: Raw) -> Raw {
        // 0x7FFF_FFFF = 除符号位外全 1（运行期构造，避开 MSRV 1.75 的
        // const f32::from_bits 限制）。
        and(a, splat(f32::from_bits(0x7FFF_FFFF)))
    }
}

#[cfg(target_arch = "aarch64")]
use arm as imp;
#[cfg(target_arch = "x86_64")]
use x86 as imp;

#[cfg(target_arch = "x86_64")]
use core::arch::x86_64::*;

#[cfg(target_arch = "aarch64")]
use arm::Raw;
#[cfg(target_arch = "x86_64")]
use x86::Raw;

/// 4×f32 向量（lane ch = 通道 ch）。
///
/// 所有公开运算逐 lane 独立且与 scalar IEEE-754 单精度语义逐位一致
///（见 crate 文档契约）。
#[derive(Clone, Copy)]
pub struct V4(Raw);

impl V4 {
    /// 全 lane 置同一值。
    #[inline(always)]
    pub fn splat(x: f32) -> V4 {
        V4(imp::splat(x))
    }

    /// lane → 通道展开（`[R, G, B, A]`）。
    #[inline(always)]
    pub fn to_array(self) -> [f32; 4] {
        let mut out = [0.0f32; 4];
        imp::store(&mut out, self.0);
        out
    }
}

/// 向量逐 lane 加法。
#[inline(always)]
fn add(a: V4, b: V4) -> V4 {
    V4(imp::add(a.0, b.0))
}

/// 向量逐 lane 减法。
#[inline(always)]
fn sub(a: V4, b: V4) -> V4 {
    V4(imp::sub(a.0, b.0))
}

/// 向量逐 lane 乘法。
#[inline(always)]
fn mul(a: V4, b: V4) -> V4 {
    V4(imp::mul(a.0, b.0))
}

/// 标量广播 × 向量的乘加：`a + s·d`（求值序 sub/mul 场景外共享）。
/// 注意这不是 fused：恒为 mul + add 两条指令，与 scalar `a + s·d`
/// 的 fmul→fadd 序逐位一致。
#[inline(always)]
fn mad(a: V4, s: V4, d: V4) -> V4 {
    add(a, mul(s, d))
}

/// feTurbulence 单 octave 的 4 通道并行求值 + 累加（车道EF）。
///
/// 算式与算序逐项镜像 svgine-core `raster::turb_channel`（车道DA 原文），
/// lane ch 对应通道 ch 的 scalar 链：
///
/// ```text
/// u   = g[b00][0]·frx     + g[b00][1]·fry
/// v   = g[b10][0]·(frx−1) + g[b10][1]·fry
/// a   = u + smx·(v−u)
/// u   = g[b01][0]·frx     + g[b01][1]·(fry−1)
/// v   = g[b11][0]·(frx−1) + g[b11][1]·(fry−1)
/// b   = u + smx·(v−u)
/// c   = a + smy·(b−a)      （abs_mode 时 c = |c|）
/// out = acc + c·ratio
/// ```
///
/// `grad_v` 布局见 [`GradTable`]；每条 lane 的浮点运算序列与 scalar
/// 逐 op 相同（无重结合/无收缩）→ 逐字节等价。
#[inline(always)]
#[allow(clippy::too_many_arguments)]
pub fn turb_channels_vec(
    grad_v: &GradTable,
    b00: usize,
    b10: usize,
    b01: usize,
    b11: usize,
    frx: f32,
    fry: f32,
    smx: f32,
    smy: f32,
    acc: V4,
    ratio: f32,
    abs_mode: bool,
) -> V4 {
    // dot(b, vx, vy) = g[b][0]·vx + g[b][1]·vy（先乘后加，同 scalar）。
    let dot = |b: usize, vx: f32, vy: f32| -> V4 {
        let gx = V4(imp::load(&grad_v[b][0]));
        let gy = V4(imp::load(&grad_v[b][1]));
        add(mul(gx, V4::splat(vx)), mul(gy, V4::splat(vy)))
    };
    let u = dot(b00, frx, fry);
    let v = dot(b10, frx - 1.0, fry);
    let a = mad(u, V4::splat(smx), sub(v, u));
    let u = dot(b01, frx, fry - 1.0);
    let v = dot(b11, frx - 1.0, fry - 1.0);
    let b = mad(u, V4::splat(smx), sub(v, u));
    let mut c = mad(a, V4::splat(smy), sub(b, a));
    if abs_mode {
        c = V4(imp::abs(c.0));
    }
    add(acc, mul(c, V4::splat(ratio)))
}

/// libm::floorf 的**位精确**单值替代（车道EF 内圈热点）。
///
/// 仅 aarch64：frintm 单指令（IEEE roundToIntegralTowardNegative），对
/// 有限值/±inf/quiet-nan（含 ±0 的符号保持）与 `libm::floorf` 逐位一致
///（raster 侧 tests::turb_floor_matches_libm_bit_exact 全域扫描钉死；
/// signalling nan 两家行为不同——engine 全程由算术产生值，不可达）。
/// x86_64 不提供：SSE2 基线无 round 指令（floorps 需 SSE4.1），调用方
/// 回退 libm::floorf。
#[cfg(target_arch = "aarch64")]
#[inline(always)]
pub fn floor_exact(x: f32) -> f32 {
    imp::get0(imp::floor_all(x))
}

// ---- 车道NO（blur-perf）：盒模糊整数内圈（aarch64 NEON / 车道OY x86_64 SSE2）----
//
// 字节级等价契约：每条 lane 承载**一个颜色通道**的 u32 无符号整数
// 运算，lane 间无交互；每条 lane 的算术序列与 raster.rs scalar 参考
// （`box_blur_axis` 滑窗累加 + `BoxDiv::round`）逐 op 一致：
// 加/减为逐 lane u32（累加器值域 65535·4097+2048 < 2²⁹，加法不溢出；
// 减法走 vsq（饱和）与 scalar `u32::saturating_sub` 同义）；舍入
// `q = min((s+half)·m >> sh, 65535)` 以 umull 扩 64 位积再右移，与
// scalar u64 乘移逐位一致（(s+half)·m < 2^60 不失位）。SIMD 路径与
// scalar 路径输出**逐字节一致**（raster 侧 box_blur_axis_byte_equal_
// legacy_* 单测在 aarch64 上对全 win×start×尺寸网格钉死）。

/// 舍入一行 4 通道窗口和：`out[i] = min((s[i]+half)·m >> sh, 65535)`
///（s.len() = out.len() = 4 的倍数；lane = 通道；尾部非 4 倍数标量）。
#[cfg(target_arch = "aarch64")]
#[allow(unsafe_code)]
pub fn box_round_row(s: &[u32], out: &mut [u16], half: u32, m: u32, sh: u32) {
    use core::arch::aarch64::*;
    let n = s.len().min(out.len());
    let nv = n & !3;
    let mut i = 0;
    // Safety：aarch64 NEON 为目标基线能力（无需运行时检测）；全部为
    // 逐 lane 纯数据运算，指针均在切片界内（i+4 ≤ n）。
    unsafe {
        let half_v = vdupq_n_u32(half);
        let m_v = vdupq_n_u32(m);
        let sh_v = vdupq_n_s64(-(sh as i64));
        let max16 = vdupq_n_u32(u16::MAX as u32);
        while i < nv {
            let acc = vld1q_u32(s.as_ptr().add(i));
            let u = vaddq_u32(acc, half_v);
            let pl = vmull_u32(vget_low_u32(u), vget_low_u32(m_v));
            let ph = vmull_high_u32(u, m_v);
            let ql = vshlq_u64(pl, sh_v);
            let qh = vshlq_u64(ph, sh_v);
            let q = vminq_u32(
                vuzp1q_u32(vreinterpretq_u32_u64(ql), vreinterpretq_u32_u64(qh)),
                max16,
            );
            vst1_u16(out.as_mut_ptr().add(i), vmovn_u32(q));
            i += 4;
        }
    }
    while i < n {
        let v = ((s[i] as u64 + half as u64) * m as u64) >> sh;
        out[i] = (v as u32).min(u16::MAX as u32) as u16;
        i += 1;
    }
}

/// 行累加：`sums[i] += row[i]`（u16 → u32 零扩展；4 的倍数长度）。
#[cfg(target_arch = "aarch64")]
#[allow(unsafe_code)]
pub fn acc_add_row(sums: &mut [u32], row: &[u16]) {
    use core::arch::aarch64::*;
    let n = sums.len().min(row.len());
    let nv = n & !3;
    let mut i = 0;
    // Safety：同 box_round_row（NEON 基线，i+4 ≤ n，界内指针）。
    unsafe {
        while i < nv {
            let acc = vld1q_u32(sums.as_ptr().add(i));
            let v = vld1_u16(row.as_ptr().add(i));
            let r = vaddq_u32(acc, vmovl_u16(v));
            vst1q_u32(sums.as_mut_ptr().add(i), r);
            i += 4;
        }
    }
    while i < n {
        sums[i] += row[i] as u32;
        i += 1;
    }
}

/// 行饱和减：`sums[i] = sums[i].saturating_sub(row[i])`（与 scalar
/// `u32::saturating_sub` 同义；4 的倍数长度）。
#[cfg(target_arch = "aarch64")]
#[allow(unsafe_code)]
pub fn acc_sub_row_saturating(sums: &mut [u32], row: &[u16]) {
    use core::arch::aarch64::*;
    let n = sums.len().min(row.len());
    let nv = n & !3;
    let mut i = 0;
    // Safety：同 box_round_row（NEON 基线，i+4 ≤ n，界内指针）。
    unsafe {
        while i < nv {
            let acc = vld1q_u32(sums.as_ptr().add(i));
            let v = vld1_u16(row.as_ptr().add(i));
            let r = vqsubq_u32(acc, vmovl_u16(v));
            vst1q_u32(sums.as_mut_ptr().add(i), r);
            i += 4;
        }
    }
    while i < n {
        sums[i] = sums[i].saturating_sub(row[i] as u32);
        i += 1;
    }
}

/// 水平轴内圈（车道NO）：对每个输出 x ∈ [0,w)：窗口和
/// `s[x][c] = Σ row[(x+lo)..(x+hi)][c]`（下标钳 [0,w]，窗外零），
/// `out = min((s+half)·m >> sh, 65535)` 交错写出。实现：①4 通道并行
/// 前缀和（逐 x 一条 vaddq）；②内部连续 x 段（0 ≤ x+lo 且 x+hi ≤ w）
/// vld4q 取两路窗和 + 舍入 + vst4_u16 交错写出；③两端边界 x 标量。
/// 与 scalar 滑窗参考逐字节一致（P[b]−P[a] 与滑窗累加同值）。
///
/// `p` 为调用方提供的暂存前缀缓冲（长度 ≥ (w+1)·4，内容任意）。
#[cfg(target_arch = "aarch64")]
#[allow(clippy::too_many_arguments)]
#[allow(unsafe_code)]
pub fn box_h_round_row(
    row: &[u16],
    p: &mut [u32],
    out: &mut [u16],
    w: usize,
    lo: i32,
    hi: i32,
    half: u32,
    m: u32,
    sh: u32,
) {
    use core::arch::aarch64::*;
    debug_assert!(p.len() >= (w + 1) * 4);
    // Safety：aarch64 NEON 为目标基线能力（无需运行时检测）；全部为
    // 逐 lane 纯数据运算，指针均在切片界内（逐段注释给出界证明）。
    let (half_v, m_v, sh_v, max16);
    unsafe {
        half_v = vdupq_n_u32(half);
        m_v = vdupq_n_u32(m);
        sh_v = vdupq_n_s64(-(sh as i64));
        max16 = vdupq_n_u32(u16::MAX as u32);
    }
    // ① 前缀和 P[x*4+c] = Σ_{k<x} row[k*4+c]，P[0..4] = 0。
    p[..4].fill(0);
    {
        // Safety：NEON 基线；row.len() ≥ (x+1)*4，p.len() ≥ (x+2)*4。
        let mut acc = unsafe { vdupq_n_u32(0) };
        for x in 0..w {
            // Safety：NEON 基线；row.len() ≥ (x+1)*4，p.len() ≥ (x+2)*4。
            let v = unsafe { vld1_u16(row.as_ptr().add(x * 4)) };
            acc = unsafe { vaddq_u32(acc, vmovl_u16(v)) };
            unsafe { vst1q_u32(p.as_mut_ptr().add((x + 1) * 4), acc) };
        }
    }
    let w_i = w as i32;
    let x0 = ((-lo).max(0)) as usize;
    let x1 = ((w_i - hi).max(0)) as usize;
    let x0 = x0.min(w);
    let x1 = x1.max(x0).min(w);
    // 向量批实际覆盖 [x0, x_cov)（不足一批的尾差回落标量）
    let x_cov = x0 + ((x1 - x0) / 4) * 4;
    // ② 内部区（每 4 个连续 x 一批；b/a 下标随 x 连续 → vld4q 步长 4）
    if x0 + 4 <= x1 {
        let mut x = x0;
        while x + 4 <= x1 {
            // i64 中转：hi 可为负（窗口整体在行首左侧时 x+hi 仍 ≥ 0，
            // 内部区定义保证），避免 debug 溢出检查误伤。
            let b = (x as i64 + hi as i64) as usize;
            let a = (x as i64 + lo as i64) as usize;
            // Safety：P 长度 (w+1)*4；b+3 ≤ w ⇒ 读 (b+3)*4 + (4k+j) ≤
            // (b+4)*4−1 < p.len()；vld4q reg j lane k = P[(b+k)*4+j]
            //（j = 通道，k = x 偏移）。a 侧同（a ≥ 0）。
            let (pb, pa) = unsafe {
                (
                    vld4q_u32(p.as_ptr().add(b * 4)),
                    vld4q_u32(p.as_ptr().add(a * 4)),
                )
            };
            // Safety：NEON 基线，纯 lane 运算。
            let round = |sv: uint32x4_t| -> uint32x4_t {
                unsafe {
                    let u = vaddq_u32(sv, half_v);
                    let pl = vmull_u32(vget_low_u32(u), vget_low_u32(m_v));
                    let ph = vmull_high_u32(u, m_v);
                    let ql = vshlq_u64(pl, sh_v);
                    let qh = vshlq_u64(ph, sh_v);
                    vminq_u32(
                        vuzp1q_u32(vreinterpretq_u32_u64(ql), vreinterpretq_u32_u64(qh)),
                        max16,
                    )
                }
            };
            let qn = [
                // Safety：NEON 基线，纯 lane 减法。
                round(unsafe { vsubq_u32(pb.0, pa.0) }),
                round(unsafe { vsubq_u32(pb.1, pa.1) }),
                round(unsafe { vsubq_u32(pb.2, pa.2) }),
                round(unsafe { vsubq_u32(pb.3, pa.3) }),
            ];
            // Safety：out.len() ≥ w*4 ≥ (x+4)*4。vst4 交错写：
            // mem[(x+k)*4 + j] = qn[j][k]。
            unsafe {
                vst4_u16(
                    out.as_mut_ptr().add(x * 4),
                    uint16x4x4_t(
                        vmovn_u32(qn[0]),
                        vmovn_u32(qn[1]),
                        vmovn_u32(qn[2]),
                        vmovn_u32(qn[3]),
                    ),
                );
            }
            x += 4;
        }
    }
    // ③ 边界 x 标量（窗口含钳制零填充；内部区由向量批覆盖）
    for x in 0..w {
        if x >= x0 && x < x_cov {
            continue;
        }
        let b = (x as i32 + hi).clamp(0, w_i) as usize;
        let a = (x as i32 + lo).clamp(0, w_i) as usize;
        for c in 0..4 {
            let s = p[b * 4 + c] - p[a * 4 + c];
            let v = ((s as u64 + half as u64) * m as u64) >> sh;
            out[x * 4 + c] = (v as u32).min(u16::MAX as u32) as u16;
        }
    }
}

// ---- 车道OY（simd-x86）：盒模糊整数内圈 x86_64 SSE2（NO NEON 同款）----
//
// 字节级等价契约：与上方 NEON 内圈同一条——每条 lane 承载**一个颜色
// 通道**的 u32 无符号整数运算，与 raster.rs scalar 参考（滑窗累加 +
// BoxDiv::round）逐 op 一致；三路径（scalar/NEON/SSE2）同输入逐字节
// 同输出（本 crate tests 标量对照网格 + raster 侧
// box_blur_axis_byte_equal_legacy_* 在两架构各自跑全 win×start×尺寸
// 网格；等价域 = 累加器界 65535·4097+2048 < 2²⁹，见逐 op 注记）。
//
// NEON → SSE2 逐 op 镜像（域内/域外语义均一致）：
// - vaddq_u32（u32 wrap）→ `_mm_add_epi32`（同 wrap；域内无 wrap 时与
//   标量 u64 加法同值）
// - vmull_u32 / vmull_high_u32（32×32→64 扩位积，无失位）→
//   `_mm_mul_epu32` 取 lane 0/2 + `_mm_srli_si128(4)` 错位后再
//   `_mm_mul_epu32` 取 lane 1/3——两半合覆 4 lane，积同为精确 64 位
// - vshlq_u64（负移量 = 逻辑右移）→ `_mm_srli_epi64`
// - vuzp1q_u32 取各 64 位积低 32 字回 lane 序 → `_mm_unpacklo_epi32` +
//   `_mm_unpackhi_epi32` + `_mm_unpacklo_epi64`——同取低 32 字（截断），
//   与标量 `v as u32` 同型
// - vminq_u32（无符号 32 位 min）→ 双侧 xor 符号位后 `_mm_cmpgt_epi32`
//   + 混合（SSE2 无 u32 min/sat 指令；符号翻转比较是全域无符号比较，
//   不依赖累加器界）
// - vmovn_u32（截断收窄）→ shuffle 压缩取低 16 字（min 后每 lane ≤
//   65535，高 16 位恒 0）
// - vqsubq_u32（无符号饱和减）→ sub + 借位掩码 andnot（全域
//   `u32::saturating_sub` 同义，含 sums < row 饱和臂）
// - vmovl_u16（零扩展）→ `_mm_unpacklo_epi16(v, zero)`
//
// 性能注记：AVX2（`_mm256` 扩 8 lane）可再摊薄指令数，但 SSE2 是
// x86_64 基线最小公分母（Windows/老 Intel 宿主零运行时检测），且加宽
// 会引入新的等价证明面——本车道不启用，登记备查（非缺口）。
//
// 域界注记：`s + half` 按位为 u32 wrap（继承 NEON vaddq 位型）——
// `s ≥ 2^32 − half` 的界外垃圾输入与标量 u64 加法分歧，与 NEON 完全
// 同域（raster 累加器界 2²⁹ 内三路径逐字节一致）。

/// 单向量 4 通道舍入（SSE2 内圈共享臂，车道OY）：
/// `q[i] = min(low32((s[i]+half)·m >> sh), 65535)`（lane i = 通道 i）。
/// 每 lane 算序 = [`box_round_row`] 标量尾逐 op（u64 乘移截断 min 同型）。
/// `sh_v` = `_mm_cvtsi32_si128(sh as i32)`（运行期移位量走 PSRLQ 寄存器
/// 形——imm8 形要求编译期常量；sh < 64 由调用方 debug_assert 保证）。
#[cfg(target_arch = "x86_64")]
#[inline(always)]
#[allow(unsafe_code)]
fn round4(
    s: __m128i,
    half_v: __m128i,
    m_v: __m128i,
    sh_v: __m128i,
    max16: __m128i,
    sgn: __m128i,
) -> __m128i {
    // Safety：SSE2 为 x86_64 目标基线能力（无需运行时检测）；纯 lane
    // 运算无内存访问。
    unsafe {
        let u = _mm_add_epi32(s, half_v);
        let pe = _mm_mul_epu32(u, m_v);
        let po = _mm_mul_epu32(_mm_srli_si128(u, 4), _mm_srli_si128(m_v, 4));
        let qe = _mm_srl_epi64(pe, sh_v);
        let qo = _mm_srl_epi64(po, sh_v);
        let t = _mm_unpacklo_epi32(qe, qo);
        let th = _mm_unpackhi_epi32(qe, qo);
        let q = _mm_unpacklo_epi64(t, th);
        let ax = _mm_xor_si128(q, sgn);
        let bx = _mm_xor_si128(max16, sgn);
        // mask = q >u 65535（符号翻转后签名比较）：命中取 65535，否则取 q。
        let mask = _mm_cmpgt_epi32(ax, bx);
        _mm_or_si128(_mm_and_si128(mask, max16), _mm_andnot_si128(mask, q))
    }
}

/// 4×u32 向量压缩为低 8 字节 4×u16（min 后每 lane ≤ 65535，各 u32 高
/// 16 位恒 0；截断语义同 vmovn_u32）。shuffles：u16 字视图
/// `[q0,0,q1,0,q2,0,q3,0]` → lo/hi 各取 `[w0,w2]` → 32 位字 `[0,2]`
/// 拼出低 8 字节 `[q0,q1,q2,q3]`。
#[cfg(target_arch = "x86_64")]
#[inline(always)]
#[allow(unsafe_code)]
fn pack4_u16(q: __m128i) -> __m128i {
    // Safety：SSE2 为 x86_64 目标基线能力；纯 lane 重排无内存访问。
    unsafe {
        let lo = _mm_shufflelo_epi16(q, 0b10_00_10_00);
        let hi = _mm_shufflehi_epi16(lo, 0b10_00_10_00);
        _mm_shuffle_epi32(hi, 0b00_00_10_00)
    }
}

/// 舍入一行 4 通道窗口和：`out[i] = min((s[i]+half)·m >> sh, 65535)`
///（s.len() = out.len() = 4 的倍数；lane = 通道；尾部非 4 倍数标量）。
/// SSE2 版（车道OY），契约见上节。
#[cfg(target_arch = "x86_64")]
#[allow(unsafe_code)]
pub fn box_round_row(s: &[u32], out: &mut [u16], half: u32, m: u32, sh: u32) {
    debug_assert!(sh < 64);
    let n = s.len().min(out.len());
    let nv = n & !3;
    let mut i = 0;
    // Safety：x86_64 SSE2 为目标基线能力（无需运行时检测）；全部为逐
    // lane 纯数据运算，指针均在切片界内（i+4 ≤ n）。
    unsafe {
        let half_v = _mm_set1_epi32(half as i32);
        let m_v = _mm_set1_epi32(m as i32);
        let sh_v = _mm_cvtsi32_si128(sh as i32);
        let max16 = _mm_set1_epi32(u16::MAX as i32);
        let sgn = _mm_set1_epi32(i32::MIN);
        while i < nv {
            let acc = _mm_loadu_si128(s.as_ptr().add(i) as *const __m128i);
            let q = round4(acc, half_v, m_v, sh_v, max16, sgn);
            _mm_storel_epi64(out.as_mut_ptr().add(i) as *mut __m128i, pack4_u16(q));
            i += 4;
        }
    }
    while i < n {
        let v = ((s[i] as u64 + half as u64) * m as u64) >> sh;
        out[i] = (v as u32).min(u16::MAX as u32) as u16;
        i += 1;
    }
}

/// 行累加：`sums[i] += row[i]`（u16 → u32 零扩展；4 的倍数长度）。
/// SSE2 版（车道OY），契约见上节。
#[cfg(target_arch = "x86_64")]
#[allow(unsafe_code)]
pub fn acc_add_row(sums: &mut [u32], row: &[u16]) {
    let n = sums.len().min(row.len());
    let nv = n & !3;
    let mut i = 0;
    // Safety：同 box_round_row（SSE2 基线，i+4 ≤ n，界内指针）。
    unsafe {
        let zero = _mm_setzero_si128();
        while i < nv {
            let acc = _mm_loadu_si128(sums.as_ptr().add(i) as *const __m128i);
            let v = _mm_loadl_epi64(row.as_ptr().add(i) as *const __m128i);
            let r = _mm_add_epi32(acc, _mm_unpacklo_epi16(v, zero));
            _mm_storeu_si128(sums.as_mut_ptr().add(i) as *mut __m128i, r);
            i += 4;
        }
    }
    while i < n {
        sums[i] += row[i] as u32;
        i += 1;
    }
}

/// 行饱和减：`sums[i] = sums[i].saturating_sub(row[i])`（与 scalar
/// `u32::saturating_sub` **全域**同义；4 的倍数长度）。SSE2 版
///（车道OY），契约见上节。
#[cfg(target_arch = "x86_64")]
#[allow(unsafe_code)]
pub fn acc_sub_row_saturating(sums: &mut [u32], row: &[u16]) {
    let n = sums.len().min(row.len());
    let nv = n & !3;
    let mut i = 0;
    // Safety：同 box_round_row（SSE2 基线，i+4 ≤ n，界内指针）。
    unsafe {
        let zero = _mm_setzero_si128();
        let sgn = _mm_set1_epi32(i32::MIN);
        while i < nv {
            let acc = _mm_loadu_si128(sums.as_ptr().add(i) as *const __m128i);
            let v = _mm_loadl_epi64(row.as_ptr().add(i) as *const __m128i);
            let b = _mm_unpacklo_epi16(v, zero);
            // 无符号比较 b >u acc：双侧 xor 符号位后有符号 cmpgt。
            let mask = _mm_cmpgt_epi32(_mm_xor_si128(b, sgn), _mm_xor_si128(acc, sgn));
            // 借位（b > acc）结果清零，否则 a−b（a ≥ b 时无 wrap = 精确）。
            let r = _mm_andnot_si128(mask, _mm_sub_epi32(acc, b));
            _mm_storeu_si128(sums.as_mut_ptr().add(i) as *mut __m128i, r);
            i += 4;
        }
    }
    while i < n {
        sums[i] = sums[i].saturating_sub(row[i] as u32);
        i += 1;
    }
}

/// 水平轴内圈（车道OY SSE2 版，NEON [`box_h_round_row`] 同构）：对每个
/// 输出 x ∈ [0,w)：窗口和 `s[x][c] = Σ row[(x+lo)..(x+hi)][c]`（下标钳
/// [0,w]，窗外零），`out = min((s+half)·m >> sh, 65535)` 写出。实现：
/// ①4 通道并行前缀和（逐 x 一条 `_mm_add_epi32`，顺序累加无重结合）；
/// ②内部连续 x 段（0 ≤ x+lo 且 x+hi ≤ w）每 x 一向量：两路 P 窗差 +
/// [`round4`] 舍入 + 8 字节写出；③两端边界 x 标量。与 scalar 滑窗参考
/// 逐字节一致（P[b]−P[a] 与滑窗累加同值）。
///
/// `p` 为调用方提供的暂存前缀缓冲（长度 ≥ (w+1)·4，内容任意）。
#[cfg(target_arch = "x86_64")]
#[allow(clippy::too_many_arguments)]
#[allow(unsafe_code)]
pub fn box_h_round_row(
    row: &[u16],
    p: &mut [u32],
    out: &mut [u16],
    w: usize,
    lo: i32,
    hi: i32,
    half: u32,
    m: u32,
    sh: u32,
) {
    debug_assert!(p.len() >= (w + 1) * 4);
    debug_assert!(sh < 64);
    // Safety：x86_64 SSE2 为目标基线能力（无需运行时检测）；全部为逐
    // lane 纯数据运算，指针均在切片界内（逐段注释给出界证明）。
    let (half_v, m_v, sh_v, max16, sgn, zero);
    unsafe {
        half_v = _mm_set1_epi32(half as i32);
        m_v = _mm_set1_epi32(m as i32);
        sh_v = _mm_cvtsi32_si128(sh as i32);
        max16 = _mm_set1_epi32(u16::MAX as i32);
        sgn = _mm_set1_epi32(i32::MIN);
        zero = _mm_setzero_si128();
    }
    // ① 前缀和 P[x*4+c] = Σ_{k<x} row[k*4+c]，P[0..4] = 0。
    p[..4].fill(0);
    {
        let mut acc = zero;
        for x in 0..w {
            // Safety：SSE2 基线；row.len() ≥ (x+1)*4，p.len() ≥ (x+2)*4。
            unsafe {
                let v = _mm_loadl_epi64(row.as_ptr().add(x * 4) as *const __m128i);
                acc = _mm_add_epi32(acc, _mm_unpacklo_epi16(v, zero));
                _mm_storeu_si128(p.as_mut_ptr().add((x + 1) * 4) as *mut __m128i, acc);
            }
        }
    }
    let w_i = w as i32;
    let x0 = ((-lo).max(0)) as usize;
    let x1 = ((w_i - hi).max(0)) as usize;
    let x0 = x0.min(w);
    let x1 = x1.max(x0).min(w);
    // 向量批实际覆盖 [x0, x_cov)（尾差回落标量；两路逐字节同值，覆盖
    // 集与 NEON 对齐）。
    let x_cov = x0 + ((x1 - x0) / 4) * 4;
    // ② 内部区（每 x 一向量；b/a 下标随 x 线性、免钳制）
    for x in x0..x_cov {
        // i64 中转：hi 可为负（窗口整体在行首左侧时 x+hi 仍 ≥ 0，内部
        // 区定义保证），避免 debug 溢出检查误伤。
        let b = (x as i64 + hi as i64) as usize;
        let a = (x as i64 + lo as i64) as usize;
        // Safety：内部区保证 a ≥ 0、b ≤ w ⇒ P 读址 [b*4, b*4+4) ⊂
        // [0, (w+1)*4)，a 侧同；out.len() ≥ w*4 且 x ≤ w−hi−1 ⇒ 写址
        // [x*4, x*4+8) ⊂ [0, w*4)。
        unsafe {
            let pb = _mm_loadu_si128(p.as_ptr().add(b * 4) as *const __m128i);
            let pa = _mm_loadu_si128(p.as_ptr().add(a * 4) as *const __m128i);
            let q = round4(_mm_sub_epi32(pb, pa), half_v, m_v, sh_v, max16, sgn);
            _mm_storel_epi64(out.as_mut_ptr().add(x * 4) as *mut __m128i, pack4_u16(q));
        }
    }
    // ③ 边界 x 标量（窗口含钳制零填充；内部区由向量批覆盖）
    for x in 0..w {
        if x >= x0 && x < x_cov {
            continue;
        }
        let b = (x as i32 + hi).clamp(0, w_i) as usize;
        let a = (x as i32 + lo).clamp(0, w_i) as usize;
        for c in 0..4 {
            let s = p[b * 4 + c] - p[a * 4 + c];
            let v = ((s as u64 + half as u64) * m as u64) >> sh;
            out[x * 4 + c] = (v as u32).min(u16::MAX as u32) as u16;
        }
    }
}

#[cfg(test)]
mod tests;
