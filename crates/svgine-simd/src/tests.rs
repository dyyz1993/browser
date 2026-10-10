//! blur 整数内圈三路径字节等价测试（车道OY）。
//!
//! 标量参考在本模块独立实现（滑窗/直算式，与 raster.rs scalar 路径同
//! 算式；box_h 用**滑窗**参考而非前缀差——连 P[b]−P[a] 恒等式一起钉）。
//! `super::box_*` 按 cfg 解析到当前架构的 SIMD 版（aarch64 = NEON，
//! x86_64 = SSE2）——同一份测试体在两架构各自跑，跨架构同数据 =
//! 三路径（scalar/NEON/SSE2）间接互证。
//!
//! 等价域注记（与 NEON 同域，见 lib.rs「域界注记」）：
//! - box_round_row：s + half < 2^32（SIMD 加法为 u32 wrap 位型；
//!   测试值一律 < 2^31，覆盖 raster 累加器界 2^29 加裕量）
//! - acc_add_row：sums + row ≤ u32::MAX（标量 `+=` debug 溢出 panic，
//!   测试域即无溢出域）
//! - acc_sub_row_saturating：全域（饱和语义无溢出点）

use std::vec;
use std::vec::Vec;

/// 确定性伪随机填充 u16（LCG，同 raster::no_fill_u16 口径，覆盖
/// 0..=65535 全值域）。
fn fill_u16(buf: &mut [u16], seed: u32) {
    let mut s = seed | 1;
    for v in buf.iter_mut() {
        s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        *v = (s >> 11) as u16;
    }
}

/// 确定性伪随机 u32（LCG 直出全 32 位域）。
fn lcg_u32(s: &mut u32) -> u32 {
    *s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
    *s
}

/// BoxDiv 魔数（与 raster::BoxDiv::new 同式）。
fn box_div(win: u32) -> (u32, u32, u32) {
    debug_assert!(win >= 1);
    let k = 32 - (win - 1).leading_zeros();
    let sh = 29 + k;
    let m = ((1u64 << sh).div_ceil(win as u64)) as u32;
    (m, sh, win / 2)
}

// ---- 标量参考 ----

fn sc_box_round(s: u32, half: u32, m: u32, sh: u32) -> u16 {
    let v = ((s as u64 + half as u64) * m as u64) >> sh;
    (v as u32).min(u16::MAX as u32) as u16
}

fn sc_box_round_row(s: &[u32], out: &mut [u16], half: u32, m: u32, sh: u32) {
    for (i, o) in out.iter_mut().enumerate() {
        *o = sc_box_round(s[i], half, m, sh);
    }
}

fn sc_acc_add_row(sums: &mut [u32], row: &[u16]) {
    for (s, v) in sums.iter_mut().zip(row.iter()) {
        *s += *v as u32;
    }
}

fn sc_acc_sub_row_saturating(sums: &mut [u32], row: &[u16]) {
    for (s, v) in sums.iter_mut().zip(row.iter()) {
        *s = s.saturating_sub(*v as u32);
    }
}

/// 滑窗 scalar 参考（raster::box_blur_h_row 标量臂同式；独立于被测
/// 函数的前缀差算法）。
#[allow(clippy::too_many_arguments)]
fn sc_box_h_round_row(
    row: &[u16],
    out: &mut [u16],
    w: usize,
    win_lo: i32,
    win_hi: i32,
    half: u32,
    m: u32,
    sh: u32,
) {
    let w_i = w as i32;
    let mut sums = [0u32; 4];
    let lo = win_lo.max(0).min(w_i) as usize;
    let hi = win_hi.min(w_i).max(win_lo.max(0).min(w_i)) as usize;
    for k in lo..hi {
        let kb = k * 4;
        sums[0] += row[kb] as u32;
        sums[1] += row[kb + 1] as u32;
        sums[2] += row[kb + 2] as u32;
        sums[3] += row[kb + 3] as u32;
    }
    for x in 0..w {
        let ob = x * 4;
        out[ob] = sc_box_round(sums[0], half, m, sh);
        out[ob + 1] = sc_box_round(sums[1], half, m, sh);
        out[ob + 2] = sc_box_round(sums[2], half, m, sh);
        out[ob + 3] = sc_box_round(sums[3], half, m, sh);
        let add = x as i32 + win_hi;
        if add >= 0 && add < w_i {
            let ab = add as usize * 4;
            sums[0] += row[ab] as u32;
            sums[1] += row[ab + 1] as u32;
            sums[2] += row[ab + 2] as u32;
            sums[3] += row[ab + 3] as u32;
        }
        let sub = x as i32 + win_lo;
        if sub >= 0 && sub < w_i {
            let sb = sub as usize * 4;
            sums[0] = sums[0].saturating_sub(row[sb] as u32);
            sums[1] = sums[1].saturating_sub(row[sb + 1] as u32);
            sums[2] = sums[2].saturating_sub(row[sb + 2] as u32);
            sums[3] = sums[3].saturating_sub(row[sb + 3] as u32);
        }
    }
}

// ---- 测试 ----

/// box_round_row：全 win × 长度网格，sums 覆盖 [0, 65535·win]。
#[test]
fn box_round_row_byte_equal_scalar_grid() {
    let wins = [1u32, 2, 3, 4, 15, 16, 17, 255, 256, 4096, 4097];
    let lens = [
        1usize, 2, 3, 4, 5, 7, 8, 13, 31, 32, 33, 63, 64, 65, 255, 256, 257,
    ];
    let mut seed = 0x5EED_0001u32;
    for &win in &wins {
        let (m, sh, half) = box_div(win);
        let smax = 65535u64 * win as u64;
        for &n in &lens {
            let s: Vec<u32> = (0..n)
                .map(|_| (lcg_u32(&mut seed) as u64 % (smax + 1)) as u32)
                .collect();
            let mut a = vec![0u16; n];
            let mut b = vec![0u16; n];
            sc_box_round_row(&s, &mut a, half, m, sh);
            super::box_round_row(&s, &mut b, half, m, sh);
            assert_eq!(a, b, "box_round_row win={win} n={n}");
        }
    }
}

/// box_round_row：对抗值（0/1/界值/大数）——min 截断与 u64 乘移在
/// 等价域全域（s < 2^31）逐位对照。
#[test]
fn box_round_row_byte_equal_scalar_adversarial() {
    let wins = [1u32, 3, 17, 256, 4097];
    let mut seed = 0xADDE_0002u32;
    let mut specials: Vec<u32> = vec![0, 1, 2, 65535, 65536, 1 << 29, (1 << 31) - 1];
    // 累加器界邻域 + 界外裕量（< 2^31 域界，见模块注释）。
    for win in wins {
        specials.push(65535 * win);
        specials.push(65535 * win + win / 2 + 1);
    }
    specials.push((1 << 31) - 2);
    for &win in &wins {
        let (m, sh, half) = box_div(win);
        let mut s = specials.clone();
        // 大值伪随机带（贴近 2^31）。
        for _ in 0..64 {
            s.push((1u32 << 30) | (lcg_u32(&mut seed) >> 2));
        }
        let n = s.len();
        let mut a = vec![0u16; n];
        let mut b = vec![0u16; n];
        sc_box_round_row(&s, &mut a, half, m, sh);
        super::box_round_row(&s, &mut b, half, m, sh);
        assert_eq!(a, b, "box_round_row adversarial win={win}");
    }
}

/// acc_add_row：全长度网格（sums 起值 + row ≤ u32::MAX 无溢出域）。
#[test]
fn acc_add_row_byte_equal_scalar() {
    let lens = [1usize, 2, 3, 4, 5, 8, 13, 31, 32, 33, 64, 65, 255, 256, 257];
    let mut seed = 0x5EED_0003u32;
    for &n in &lens {
        let row: Vec<u16> = (0..n).map(|_| (lcg_u32(&mut seed) >> 16) as u16).collect();
        let sums0: Vec<u32> = (0..n)
            .map(|_| lcg_u32(&mut seed) % (u32::MAX - 65535))
            .collect();
        let mut a = sums0.clone();
        let mut b = sums0.clone();
        sc_acc_add_row(&mut a, &row);
        super::acc_add_row(&mut b, &row);
        assert_eq!(a, b, "acc_add_row n={n}");
    }
}

/// acc_sub_row_saturating：对抗值全域——含 sums < row（饱和臂）与
/// ≥ 2^31 大值（符号翻转比较位型）。
#[test]
fn acc_sub_row_saturating_byte_equal_scalar_adversarial() {
    let lens = [1usize, 3, 4, 5, 8, 31, 32, 33, 64, 65, 256];
    let mut seed = 0xADDE_0004u32;
    for &n in &lens {
        let row: Vec<u16> = (0..n).map(|_| (lcg_u32(&mut seed) >> 16) as u16).collect();
        // 混合域：小值 / 大值 / 界值 / 伪随机全 32 位（饱和必然命中）。
        let sums0: Vec<u32> = (0..n)
            .map(|i| match i % 4 {
                0 => lcg_u32(&mut seed) % 65536,
                1 => (1 << 31) + (lcg_u32(&mut seed) >> 1),
                2 => u32::MAX - (lcg_u32(&mut seed) % 3),
                _ => lcg_u32(&mut seed),
            })
            .collect();
        let mut a = sums0.clone();
        let mut b = sums0.clone();
        sc_acc_sub_row_saturating(&mut a, &row);
        super::acc_sub_row_saturating(&mut b, &row);
        assert_eq!(a, b, "acc_sub_row_saturating n={n}");
    }
}

/// box_h_round_row：全 win × start × 宽度网格 vs 滑窗 scalar 参考
///（NO raster 网格同款收窄到 4ch 水平轴；长度含 4 非倍数尾差）。
#[test]
fn box_h_round_row_byte_equal_scalar_grid() {
    let wins = [1u32, 2, 3, 4, 15, 16, 17, 255, 256, 4096, 4097];
    let ws = [
        1usize, 2, 3, 4, 5, 7, 8, 13, 31, 33, 63, 64, 65, 255, 256, 257,
    ];
    let mut seed = 0x5EED_0005u32;
    for &win in &wins {
        let (m, sh, half) = box_div(win);
        for &w in &ws {
            for &start in &[
                -(win as i32),
                -(win as i32 / 2),
                -1,
                0,
                1,
                7,
                win as i32,
                win as i32 + 3,
            ] {
                let row_len = w * 4;
                let mut row = vec![0u16; row_len];
                fill_u16(&mut row, (w * 31 + win as usize) as u32 ^ seed);
                seed = seed.wrapping_add(1);
                let mut a = vec![0u16; row_len];
                let mut b = vec![0u16; row_len];
                let win_hi = start + win as i32;
                sc_box_h_round_row(&row, &mut a, w, start, win_hi, half, m, sh);
                let mut p = vec![0u32; row_len + 4];
                super::box_h_round_row(&row, &mut p, &mut b, w, start, win_hi, half, m, sh);
                assert_eq!(a, b, "box_h_round_row win={win} w={w} start={start}");
            }
        }
    }
}

/// box_h_round_row：vite 实尺寸域（NO vite_dims 网格同款）。
#[test]
fn box_h_round_row_byte_equal_vite_dims() {
    let mut seed = 0x5EED_0006u32;
    for win in [3u32, 5] {
        let (m, sh, half) = box_div(win);
        for start in [-3i32, -2, -1, 0, 1] {
            for w in [267usize, 268, 271, 201, 303, 265, 266, 251] {
                let row_len = w * 4;
                let mut row = vec![0u16; row_len];
                fill_u16(&mut row, (w * 7 + win as usize) as u32 ^ seed);
                seed = seed.wrapping_add(1);
                let mut a = vec![0u16; row_len];
                let mut b = vec![0u16; row_len];
                let win_hi = start + win as i32;
                sc_box_h_round_row(&row, &mut a, w, start, win_hi, half, m, sh);
                let mut p = vec![0u32; row_len + 4];
                super::box_h_round_row(&row, &mut p, &mut b, w, start, win_hi, half, m, sh);
                assert_eq!(a, b, "vite dims win={win} w={w} start={start}");
            }
        }
    }
}
