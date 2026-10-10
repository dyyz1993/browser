//! 自写 f32 十进制解析与超越函数（T1 体积车道AF2，ADR-0009 行为级欠账）。
//!
//! 动机（体积）：std `<f32 as FromStr>`（dec2flt）拉进
//! `POWER_OF_FIVE_128` ≈10.4KB rodata + lemire/DecimalSeq ≈3.6KB .text，
//! 合计 ≈16KB ≈ T1 构建的 12%。SVG 属性数值解析用自写定点路径替代后
//! 该链整体 gc 剥离。sin/cos/tan 自写（f32-only）替代 libm 同名函数，
//! 斩断 rem_pio2f（2.7KB）及其拖进的整条 f64 软浮点链（k_sinf/k_cosf/
//! k_tanf/scalbn/<f64>/__add/muldf3 等 ≈5.6KB）。
//!
//! ## parse_f32 语义登记（相对 std `parse::<f32>`，CONFORMANCE「近似」同步）
//!
//! - 语法同 std：`[+-]?(digits[.digits?] | .digits)([eE][+-]?digits)?`，
//!   整串消费（尾随垃圾 = None）、不接受任何空白（与 std 一致，调用方
//!   已先 trim）。
//! - `"inf"/"infinity"/"nan"` 文本不接受（std 返回 Ok(inf/NaN)）。全部
//!   调用点随后都有 `is_finite` 过滤（style/builder/gradient）或按 SVG
//!   语法本就非法（path/transform/viewBox），行为等价或更严（Chrome 同样
//!   拒绝 inf/nan 属性行）。
//! - 舍入 ≤±1 ulp（|e|≤10 单次幂表乘除；更大指数分段 ≤4 次累计 ≈2ulp，
//!   亚正规下溢区再放宽）。有效数字取 19 位截断；超 19 位的丢弃位仅记
//!   sticky，但**位权必须结算**——结算指数用 `frac_abs`（acc 吸收窗内
//!   sig<19 时经过的小数位数），丢弃的小数位不进除法指数（车道CM 修复
//!   CD 钉死的超长小数丢位缺陷：旧代码 `expl − frac_n` 把丢弃小数位也
//!   计入，每枚多除一次 10，22 位输入差 ×100、32 位差 ×10^11）。丢弃位
//!   相对 f32 的 24bit 尾数 <1ulp，舍入仍 ±1-2ulp。
//! - 上溢 → ±inf、下溢 → ±0（同 std）。
//!
//! ## sin_f32/cos_f32/tan_f32 语义登记
//!
//! - π/2 三段 Cody-Waite 归约（f32-only，无 f64）：DP1=π/2 截 8 位有效
//!   （n·DP1 对 |n|<2^16 精确）、DP2 截 15 位、DP3 补尾。多项式取 Taylor
//!   系数 f32 化（[-π/4,π/4] 极值内截断误差 ≪1ulp）。
//! - 实测（Python 逐位 f32 仿真 + Rust 对 libm 随机对照）：sin/cos 绝对
//!   误差 ≤1.2e-7（=1 ulp@1，含结果自身舍入），tan ≤2 ulp@1；正弦零点
//!   附近 ulp 口径放大但绝对误差 <1e-12（f32 输入本身的量化主导）。
//! - **大角度登记**：|x| ≥ 2^22·π/2 ≈ 6.6e6 rad → 归 0.0（std 继续归约）；
//!   归约绝对误差随 |n| 增（|x|~1e3 rad 时 ≪1e-6）。SVG transform/
//!   弧扁平化角度实际远低于此，像素无感。
//! - 非有限：sin/cos/tan(±inf) = NaN、(NaN) = NaN（与 std/libm 一致）。

/// 十进制字符串 → f32（语法/精度登记见模块注释）。None = 非法。
pub(crate) fn parse_f32(s: &str) -> Option<f32> {
    let b = s.as_bytes();
    let mut i = 0;
    let neg = match b.first() {
        Some(b'-') => {
            i = 1;
            true
        }
        Some(b'+') => {
            i = 1;
            false
        }
        _ => false,
    };

    // 有效数字进 u64（≤19 位），前导零跳过；超额位记 sticky。
    let mut acc: u64 = 0;
    // 吸收窗内（sig<19）经过的小数位数：含被跳过的小数前导零
    // （"0.000001" 的 5 枚零仍占位标度），不含 sig≥19 后的丢弃位。
    let mut frac_abs: i32 = 0;
    let mut sig: u32 = 0; // 已收的有效位（不含前导零）
    let mut exp10: i32 = 0; // 值 = acc * 10^exp10
    let mut dot = false; // 已过小数点
    let mut any = false;
    let mut sticky = false;
    let mut saw_e = false;
    while i < b.len() {
        let c = b[i];
        match c {
            b'0'..=b'9' => {
                any = true;
                if sig >= 19 {
                    if c != b'0' {
                        sticky = true;
                    }
                    // 丢弃位：整数部 = 量级再乘 10；小数部不定标（acc
                    // 已定格，结算走 frac_abs）——车道CM 修复（旧代码
                    // 小数部丢弃位每枚多除一次 10：22 位输入差 ×100、
                    // 32 位解析成 0，见 tests/oracle/reports/
                    // l2-recheck-cd.md）。
                    if !dot {
                        exp10 += 1;
                    }
                } else {
                    if dot {
                        frac_abs += 1;
                    }
                    if acc != 0 || c != b'0' {
                        acc = acc * 10 + (c - b'0') as u64;
                        sig += 1;
                    }
                    // 前导零不计有效位；小数部的前导零已计入 frac_abs。
                }
                i += 1;
            }
            b'.' if !dot => {
                dot = true;
                i += 1;
            }
            b'e' | b'E' => {
                i += 1;
                saw_e = true;
                break;
            }
            _ => return None,
        }
    }
    if !any {
        return None;
    }
    let _ = sticky; // 登记项：midpoint 精确舍入不保证（±1ulp 覆盖）

    // 指数部（[eE] 后）：[+-]?digits+，饱和解析。saw_e 后无指数位
    // （"1e"/"1e+"）= 非法，与 std 一致。
    let mut expl: i32 = 0;
    if saw_e {
        let eneg = match b.get(i) {
            Some(b'-') => {
                i += 1;
                true
            }
            Some(b'+') => {
                i += 1;
                false
            }
            _ => false,
        };
        let mut got = false;
        while i < b.len() {
            match b[i] {
                c @ b'0'..=b'9' => {
                    got = true;
                    expl = expl.saturating_mul(10).saturating_add((c - b'0') as i32);
                    i += 1;
                }
                _ => return None,
            }
        }
        if !got {
            return None;
        }
        if eneg {
            expl = -expl;
        }
    }
    if i != b.len() {
        return None; // 整串消费（如 "1.2.3"、"1 "）
    }

    // 结算指数：小数部只按「acc 吸收窗内」的位数折算（车道CM 修复——
    // 旧代码用全部小数位数 frac，丢弃位每枚多除一次 10）。
    exp10 = exp10.saturating_add(expl.saturating_sub(frac_abs));
    if acc == 0 {
        return Some(if neg { -0.0 } else { 0.0 });
    }

    // 幂表 T[k]=10^k，k≤10 时 f32 精确（5^10=9765625 < 2^24）。
    const T: [f32; 11] = [1.0, 1e1, 1e2, 1e3, 1e4, 1e5, 1e6, 1e7, 1e8, 1e9, 1e10];
    // 指数钳制：acc ≤ 10^19，|e|>70 时结果必为 inf/0（f32 域
    // ~1e±38），与 std 的上溢/下溢判定一致，同时循环次数有界。
    let e0 = exp10.clamp(-70, 70);
    let mut v = acc as f32; // u64→f32：≤0.5ulp（acc<10^19 < 2^63 安全）
    let mut e = e0;
    while e > 10 {
        v *= T[10];
        e -= 10;
        if v.is_infinite() {
            return Some(if neg {
                f32::NEG_INFINITY
            } else {
                f32::INFINITY
            });
        }
    }
    while e < -10 {
        v /= T[10];
        e += 10;
    }
    if e > 0 {
        v *= T[e as usize];
    } else if e < 0 {
        v /= T[(-e) as usize];
    }
    Some(if neg { -v } else { v })
}

/// `<number> | <percentage>` 比率值解析：`"0.5"` → `Some((0.5, false))`、
/// `"50%"` → `Some((50.0, true))`（消费方对 true 除 100）。% 尾缀 ASCII
/// 字节匹配（多字节安全，同 parse_with_abs_unit）。车道OS 抽取——
/// opacity 系（opacity/fill-opacity/stroke-opacity/stop-opacity）双形态
/// （XML 属性位 + CSS 声明位）均须接受 %（Chrome pct 探针四属性两形态
/// 全实证）；旧 parse_f32 拒收 % → 属性位钳 1（50% 变不透明）、CSS 声明
/// 位整条丢弃回退继承（openclipart dolphin：根 style fill-opacity:0.5
/// 被子级 `fill-opacity:100%` 声明掩盖失败 → 全图 50% 褪色）。
pub(crate) fn parse_ratio_or_pct(s: &str) -> Option<(f32, bool)> {
    let t = s.trim();
    match t.strip_suffix('%') {
        Some(num) => parse_f32(num.trim()).map(|n| (n, true)),
        None => parse_f32(t).map(|n| (n, false)),
    }
}

/// 绝对 CSS 长度单位 → 96px 基准系数（车道NF 抽取自 builder 的
/// `abs_unit_suffix` 表——单一事实源供 builder/geom/anim 三处消费）。
/// 单位串须已小写化；`px` = 1.0（用户单位）。
pub(crate) fn abs_unit_factor(unit: &str) -> Option<f32> {
    match unit {
        "px" => Some(1.0),
        "pt" => Some(96.0 / 72.0),
        "pc" => Some(16.0),
        "mm" => Some(96.0 / 25.4),
        "cm" => Some(96.0 / 2.54),
        "in" => Some(96.0),
        "q" => Some(96.0 / 101.6),
        _ => None,
    }
}

/// 带尾缀单位的数值解析：`"12pt"` → `Some((12, 4/3))`（数值段, 单位系数）；
/// 无单位/非法 → None。单位大小写不敏感，尾缀按字节匹配（多字节安全：
/// 命中即 ASCII 边界）。消费方 `parse_f32(num) * k` 得 96px 基准值。
pub(crate) fn parse_with_abs_unit(s: &str) -> Option<(&str, f32)> {
    let b = s.as_bytes();
    let n = b.len();
    if n == 0 {
        return None;
    }
    let last = b[n - 1] | 0x20;
    if last == b'q' {
        return Some((&s[..n - 1], abs_unit_factor("q")?));
    }
    if n >= 2 {
        let u0 = b[n - 2] | 0x20;
        let u1 = last;
        let two = [u0, u1];
        // 两字节均 ASCII（|0x20 后落在字母表）才能安全切片——延续字节
        // ≥0xA0 不在匹配集，命中即合法 char boundary。
        let unit = core::str::from_utf8(&two).ok()?;
        let k = abs_unit_factor(unit)?;
        return Some((&s[..n - 2], k));
    }
    None
}

// ---- sin/cos/tan（f32-only，登记见模块注释）----

/// π/2 三段 Cody-Waite：DP1 截 8 位有效（n·DP1 对 |n|<2^16 精确）、
/// DP2 截 15 位、DP3 补尾（DP1+DP2+DP3 与 π/2 差 1.8e-15）。
/// 位型（f32::from_bits 常量上下文超 MSRV，用最短十进制回环字面量）：
/// 3fc9_0000 / 39fd_a000 / 33a2_2169。
const PI2_DP1: f32 = 1.5703125;
const PI2_DP2: f32 = 0.000_483_751_3;
const PI2_DP3: f32 = 7.549_79e-8;
/// 2/π（最近整舍入用）。
const INV_2PI: f32 = core::f32::consts::FRAC_2_PI;
/// 1.5×2^23 魔术数：加法把 |v|<2^22 的值舍入到最近整（再减回）。
const MAGIC_ROUND: f32 = 12582912.0;

// Taylor 系数（f32 舍入一次；[-π/4,π/4] 截断误差 ≪1ulp）。
const TAYLOR_S1: f32 = -1.0 / 6.0;
const TAYLOR_S2: f32 = 1.0 / 120.0;
const TAYLOR_S3: f32 = -1.0 / 5040.0;
const TAYLOR_S4: f32 = 1.0 / 362880.0;
const TAYLOR_C1: f32 = -0.5;
const TAYLOR_C2: f32 = 1.0 / 24.0;
const TAYLOR_C3: f32 = -1.0 / 720.0;
const TAYLOR_C4: f32 = 1.0 / 40320.0;

/// 归约到 [-π/4,π/4] + 象限 q（n mod 4）→ (sin_p(r), cos_p(r), q)。
/// 非有限输入 → (NaN, NaN, 0)；超大角（登记项）→ (0, 0, 0)。
#[inline]
fn reduce3(x: f32) -> (f32, f32, i32) {
    if !x.is_finite() {
        return (f32::NAN, f32::NAN, 0);
    }
    let nf = (x * INV_2PI + MAGIC_ROUND) - MAGIC_ROUND;
    let n = nf as i32;
    // 车道DI：饱和转换可产出 i32::MIN（负大角，如 rotate(-1e30)），
    // 「n.abs()」对其取负 = debug 态 negate-overflow panic。改为不带
    // 取负的区间判定，语义逐位不变（|n| ≥ 2^22 → 超大角走 (0,0,0)）。
    if n >= 1 << 22 || n <= -(1 << 22) {
        return (0.0, 0.0, 0);
    }
    let nf32 = n as f32;
    let r = ((x - nf32 * PI2_DP1) - nf32 * PI2_DP2) - nf32 * PI2_DP3;
    let z = r * r;
    let sp = r + r * (z * (TAYLOR_S1 + z * (TAYLOR_S2 + z * (TAYLOR_S3 + z * TAYLOR_S4))));
    let cp = 1.0 + z * (TAYLOR_C1 + z * (TAYLOR_C2 + z * (TAYLOR_C3 + z * TAYLOR_C4)));
    (sp, cp, n & 3)
}

/// sin 的 f32-only 实现（libm::sinf 替代，精度登记见模块注释）。
pub(crate) fn sin_f32(x: f32) -> f32 {
    let (s, c, q) = reduce3(x);
    match q {
        0 => s,
        1 => c,
        2 => -s,
        _ => -c,
    }
}

/// cos 的 f32-only 实现（libm::cosf 替代）。
pub(crate) fn cos_f32(x: f32) -> f32 {
    let (s, c, q) = reduce3(x);
    match q {
        0 => c,
        1 => -s,
        2 => -c,
        _ => s,
    }
}

/// tan 的 f32-only 实现（libm::tanf 替代，skewX/skewY 用）。象限选择
/// 后作比（q 偶 = sin/cos，q 奇 = -cos/sin），保证周期 π 与符号正确。
pub(crate) fn tan_f32(x: f32) -> f32 {
    let (s, c, q) = reduce3(x);
    if q & 1 == 0 {
        s / c
    } else {
        -c / s
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // 测试断言场景：None 直接 panic 即失败
mod tests {
    use super::parse_f32;

    // 宿主测试用 std 做正确舍入对照（no_std 纪律仅约束非 test 构建；
    // riscv 交叉 check 时 cfg(test) 不激活）。
    #[cfg(test)]
    extern crate std;

    /// 车道DI：reduce3 超大角哨兵的 debug 态溢出钉死——负大角经
    /// `nf as i32` 饱和转换产出 i32::MIN，旧「n.abs()」取负溢出 panic
    /// （fuzz 二轮 debug 口径实证）；修复后区间判定语义逐位不变。
    #[test]
    fn reduce3_huge_angle_saturates_cleanly() {
        // 正负大角 + 临界值：全部走 (0,0,0) 超大角哨兵，无 panic
        let (s, c, q) = super::reduce3(-1e30);
        assert_eq!((s, c, q), (0.0, 0.0, 0));
        let (s, c, q) = super::reduce3(1e30);
        assert_eq!((s, c, q), (0.0, 0.0, 0));
        let (s, c, q) = super::reduce3(-3.4e38);
        assert_eq!((s, c, q), (0.0, 0.0, 0));
        // 常规角不进哨兵：公共口径 sin(π/2)≈1（reduce3 的象限/多项式
        // 为内部约定，测试只钉外部行为与哨兵位）
        let s = super::sin_f32(std::f32::consts::FRAC_PI_2);
        assert!((s - 1.0).abs() < 1e-5);
        let s2 = super::sin_f32(-1e10);
        assert!(s2.is_finite(), "超大角返回有界值（不 panic 不挂死）");
    }

    fn ulp_diff(a: f32, b: f32) -> u32 {
        let (ab, bb) = (a.to_bits(), b.to_bits());
        if a.is_nan() && b.is_nan() {
            return 0;
        }
        if a == b {
            return 0;
        }
        ab.abs_diff(bb)
    }

    #[test]
    fn parity_with_std() {
        let cases = [
            "0",
            "-0",
            "0.0",
            "-0.0",
            "1",
            "-1",
            "0.5",
            ".5",
            "5.",
            "+.25",
            "3.14159",
            "0.1",
            "123456789",
            "123456789012345678901234567890",
            "1e2",
            "1E2",
            "1e+2",
            "1e-2",
            "2.5e-3",
            "1.1754944e38", // f32::MAX 附近
            "3.4028235e38", // MAX
            "3.4028236e38", // 上溢 → inf
            "1e39",
            "1e-45", // 最小亚正规
            "1e-46", // 亚正规舍入
            "1e-60", // 下溢 → 0
            "1e309999999",
            "-2.25",
            "0.000001",
            "0.000000000000000000000000001",
            "340282350000000000000000000000000000000",
            "00001.500",
            "007",
            "1e0000002",
        ];
        for &s in &cases {
            let mine = parse_f32(s);
            let st: Option<f32> = s.parse().ok();
            assert!(mine.is_some(), "{s} 应可解析");
            let (m, g) = (mine.unwrap(), st.unwrap());
            assert!(
                !m.is_nan() && ulp_diff(m, g) <= 2,
                "{s}: mine={m:?} std={g:?} (bits {:08x} vs {:08x})",
                m.to_bits(),
                g.to_bits()
            );
        }
    }

    #[test]
    fn rejects_like_std_or_stricter() {
        // std 也拒绝：
        for s in [
            "", ".", "-.", "+", "-", "e3", "1e", "1e+", "abc", "1.2.3", "1 2", " 1", "1 ", "0x10",
            "1_000", "--1", "1..2", ".e1", "1e2.5",
        ] {
            assert!(parse_f32(s).is_none(), "{s:?} 应拒绝");
        }
        // std 接受但我方拒绝（登记项）：inf/nan 文本形态——所有调用点
        // 均有 is_finite 过滤或按 SVG 语法非法，行为等价或更严。
        for s in ["inf", "-inf", "infinity", "NaN", "nan", "+inf"] {
            assert!(parse_f32(s).is_none(), "{s:?} 应拒绝（更严登记）");
        }
    }

    #[test]
    fn sign_and_zero_parity() {
        assert!(parse_f32("-0.0").unwrap().is_sign_negative());
        assert!(parse_f32("0").unwrap() == 0.0);
        assert_eq!(parse_f32("-1e-60").unwrap(), 0.0);
        assert!(parse_f32("-1e-60").unwrap().is_sign_negative());
        assert!(parse_f32("-1e39").unwrap().is_infinite());
        assert!(parse_f32("1e39").unwrap().is_infinite());
    }

    /// 车道CM：超长小数丢位修复（CD bisect 钉死，见
    /// tests/oracle/reports/l2-recheck-cd.md）。sig≥19 后的丢弃小数位
    /// 必须按位权折叠（frac_abs 结算）——旧代码把它们计入除法指数，
    /// 22 位输入差 ×100、32 位解析成 ≈0（color-prop-02-f 九枚黑圆根因）。
    #[test]
    fn long_fraction_scale_parity() {
        // CD repro 原值：2 整 + 19 小 = 21 位有效（修复前 ≈0.8627）
        let s = "86.2745098039215686274";
        let v = parse_f32(s).unwrap();
        assert!((86.0..87.0).contains(&v), "量级钉死 {s}: {v:?}");
        assert!(ulp_diff(v, s.parse().unwrap()) <= 2, "{s}");

        // 32 位有效小数（修复前 ≈0 → 黑圆）
        let s = "86.274509803921568627450980392157";
        let v = parse_f32(s).unwrap();
        assert!((86.0..87.0).contains(&v), "量级钉死 {s}: {v:?}");
        assert!(ulp_diff(v, s.parse().unwrap()) <= 2, "{s}");

        // 混合 exp：正 / 负
        let s = "86.2745098039215686274e2";
        let v = parse_f32(s).unwrap();
        assert!((8627.0..8628.0).contains(&v), "量级钉死 {s}: {v:?}");
        assert!(ulp_diff(v, s.parse().unwrap()) <= 2, "{s}");
        let s = "1.000000000000000000000000001e-5";
        let v = parse_f32(s).unwrap();
        assert!(ulp_diff(v, s.parse().unwrap()) <= 2, "{s}");

        // 超长整数部 + 尾随小数：饱和整数后的分数位不得拉动标度
        // （旧公式同根错：".5" 被当除法指数 → 值 ÷10，落 1e19 带）
        let s = "123456789012345678901.5";
        let v = parse_f32(s).unwrap();
        assert!((1.2e20..1.3e20).contains(&v), "量级钉死 {s}: {v:?}");
        assert!(ulp_diff(v, s.parse().unwrap()) <= 2, "{s}");

        // 小数部前导零仍占位标度（吸收窗内跳过的零计入 frac_abs）
        let s = "0.000000000000000000000000123456789";
        let v = parse_f32(s).unwrap();
        assert!(ulp_diff(v, s.parse().unwrap()) <= 2, "{s}");

        // 边界舍入：丢弃尾全零 = 精确值；尾数远低于半 ulp = 不进位
        let s = "0.5000000000000000000000000";
        assert_eq!(parse_f32(s).unwrap(), 0.5, "{s}");
        let s = "1.0000000000000000000000001";
        assert_eq!(parse_f32(s).unwrap(), 1.0, "{s}");

        // 超长 exp：饱和上溢 / 下溢（与 std 同判）
        let v = parse_f32("1.5e309999999").unwrap();
        assert!(v.is_infinite() && v.is_sign_positive());
        let v = parse_f32("-1.5e-309999999").unwrap();
        assert_eq!(v, 0.0);
        assert!(v.is_sign_negative());
    }

    /// 确定性伪随机批量对照（LCG 生成 "整数.小数e指数" 形态），容差
    /// ≤2ulp（登记项 ±1-2ulp；语法/上溢/下溢严格一致）。车道CM 扩展：
    /// 超长整数部/小数部生成档（>19 位有效触发 sticky 丢弃路径）容差
    /// 放宽到 4ulp——丢弃尾 + 大 |exp10| 分段幂表乘除（≤7 次）累计；
    /// 量级错误（本车道修的 bug 形态）是 10^k 级，4ulp 绰绰钉死。
    #[test]
    fn fuzz_vs_std() {
        let mut seed: u64 = 0x5DEECE66D;
        let mut next = || {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (seed >> 33) as u32
        };
        let mut checked = 0u32;
        for _ in 0..20000 {
            let int_len = match next() % 4 {
                0 => next() % 5,
                1 => 5 + next() % 8, // 跨 19 位吸收窗的超长整数部
                _ => next() % 3,
            };
            let frac_len = match next() % 4 {
                0 => next() % 6,
                1 => 6 + next() % 15,
                2 => 21 + next() % 20, // sig≥19 后仍有丢弃小数位
                _ => next() % 3,
            };
            let mut s = std::string::String::new();
            if next() % 8 == 0 {
                s.push('-');
            }
            for _ in 0..int_len {
                s.push((b'0' + (next() % 10) as u8) as char);
            }
            if frac_len > 0 {
                s.push('.');
                for _ in 0..frac_len {
                    s.push((b'0' + (next() % 10) as u8) as char);
                }
            }
            if next() % 3 == 0 && (int_len + frac_len) > 0 {
                s.push('e');
                if next() % 4 == 0 {
                    s.push('-');
                }
                s.push_str(&std::format!("{}", next() % 45));
            }
            if !s.chars().any(|c| c.is_ascii_digit()) {
                continue;
            }
            // 有效位数（小数点前拼小数点后、剥前导零）定容差档
            let sig_len = s
                .split(['e', 'E'])
                .next()
                .unwrap_or("")
                .chars()
                .filter(|c| c.is_ascii_digit())
                .skip_while(|c| *c == '0')
                .count();
            let tol = if sig_len > 19 { 4 } else { 2 };
            let mine = match parse_f32(&s) {
                Some(v) => v,
                None => panic!("{s:?} 应可解析"),
            };
            let g: f32 = s.parse().unwrap();
            assert!(
                mine == g || ulp_diff(mine, g) <= tol,
                "{s:?}: mine={mine:?} std={g:?} ({} ulp > {tol})",
                ulp_diff(mine, g)
            );
            checked += 1;
        }
        assert!(checked > 10000, "fuzz 样本过少: {checked}");
    }

    // ---- sin/cos/tan 对 libm 的对照（宿主测试；容差 = 登记项）----

    /// 登记口径：|差| ≤ 4 ulp@1（实测 sin/cos ≤1、tan ≤2）；tan 在
    /// ±π/2 邻域发散到 ~1e7 量级，容差按值缩放。
    fn close(a: f32, b: f32) -> bool {
        a == b || (a - b).abs() <= 4.8e-7 * a.abs().max(b.abs()).max(1.0)
    }

    #[test]
    fn trig_vs_libm_sampled() {
        // 典型 SVG 角度（度）+ 弧度采样 + 归约边界
        let degs = [
            0.0, 15.0, 30.0, 45.0, 60.0, 90.0, 120.0, 180.0, 270.0, 360.0, -90.0, -45.0, 720.0,
            1080.0, -2025.0, 44.9, 89.99, 359.99,
        ];
        for &d in &degs {
            let r = d * core::f32::consts::PI / 180.0;
            assert!(close(crate::fnum::sin_f32(r), libm::sinf(r)), "sin({d}deg)");
            assert!(close(crate::fnum::cos_f32(r), libm::cosf(r)), "cos({d}deg)");
            // 极点邻域（|x|≈(2k+1)π/2）：tan ~1e8 量级且对归约残差
            // 二阶敏感（Δtan=Δr/r²；f32 输入自身 1ulp 就能让真值倍增）
            // ——只比有限性、符号与相对 1e-5（skewX(90°) 本就是退化
            // 输入，登记）。
            let my_t = crate::fnum::tan_f32(r);
            let ref_t = libm::tanf(r);
            assert!(
                my_t.is_finite() == ref_t.is_finite()
                    && my_t.is_sign_negative() == ref_t.is_sign_negative()
                    && (my_t.abs() < 1e6
                        || (my_t - ref_t).abs() <= 1e-5 * my_t.abs().max(ref_t.abs())),
                "tan({d}deg): {my_t} vs {ref_t}"
            );
        }
        let mut seed: u64 = 0x9E3779B97F4A7C15;
        let mut next = move || {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (seed >> 33) as u32
        };
        for _ in 0..50000 {
            let x = (next() as f32 / u32::MAX as f32) * 400.0 - 200.0;
            assert!(close(crate::fnum::sin_f32(x), libm::sinf(x)), "sin({x})");
            assert!(close(crate::fnum::cos_f32(x), libm::cosf(x)), "cos({x})");
            let my_t = crate::fnum::tan_f32(x);
            let ref_t = libm::tanf(x);
            assert!(
                (my_t.abs() < 1e6 && ref_t.abs() < 1e6 && close(my_t, ref_t))
                    || (my_t.abs() >= 1e6
                        && ref_t.abs() >= 1e6
                        && my_t.is_sign_negative() == ref_t.is_sign_negative()
                        && (my_t - ref_t).abs() <= 1e-5 * my_t.abs().max(ref_t.abs())),
                "tan({x}): {my_t} vs {ref_t}"
            );
        }
    }

    #[test]
    fn trig_specials() {
        // 常数位型钉死（clippy --fix 缩写字面量后的回环保障）
        assert_eq!(super::PI2_DP1.to_bits(), 0x3fc9_0000);
        assert_eq!(super::PI2_DP2.to_bits(), 0x39fd_a000);
        assert_eq!(super::PI2_DP3.to_bits(), 0x33a2_2169);
        assert!(crate::fnum::sin_f32(f32::NAN).is_nan());
        assert!(crate::fnum::sin_f32(f32::INFINITY).is_nan());
        assert!(crate::fnum::cos_f32(f32::NEG_INFINITY).is_nan());
        assert!(crate::fnum::tan_f32(f32::NAN).is_nan());
        assert_eq!(crate::fnum::sin_f32(0.0), 0.0);
        // sin(±0) = +0（IEEE (-0)+(+0)=+0 吞符号；std 保留符号——
        // 零点符号位无像素意义，登记）
        assert_eq!(crate::fnum::sin_f32(-0.0), 0.0);
        // 超大角归零（登记项；std 继续归约返回 [-1,1] 内某值）
        assert_eq!(crate::fnum::sin_f32(1e9), 0.0);
        assert_eq!(crate::fnum::cos_f32(-1e9), 0.0);
    }

    /// 车道NF（unit-audit）：绝对单位系数表 + 带尾缀解析。单一事实源
    /// 供 builder(abs_unit_suffix)/geom(parse_transform_css)/anim(
    /// strip_css_unit) 三处消费。
    #[test]
    fn abs_unit_factor_and_parse_with_suffix() {
        let f = super::abs_unit_factor;
        assert_eq!(f("px"), Some(1.0));
        assert!((f("pt").unwrap() - 96.0 / 72.0).abs() < 1e-6);
        assert_eq!(f("pc"), Some(16.0));
        assert!((f("mm").unwrap() - 96.0 / 25.4).abs() < 1e-6);
        assert!((f("cm").unwrap() - 96.0 / 2.54).abs() < 1e-6);
        assert_eq!(f("in"), Some(96.0));
        assert!((f("q").unwrap() - 96.0 / 101.6).abs() < 1e-6);
        assert_eq!(f("em"), None);
        assert_eq!(f("vw"), None);

        let p = super::parse_with_abs_unit;
        let (num, k) = p("18pt").unwrap();
        assert_eq!(num, "18");
        assert!((k - 4.0 / 3.0).abs() < 1e-6);
        let (num, _) = p("12.7MM").unwrap();
        assert_eq!(num, "12.7");
        // 单字节单位 q
        let (num, k) = p("101.6q").unwrap();
        assert_eq!(num, "101.6");
        assert!((k - 96.0 / 101.6).abs() < 1e-6);
        // 无单位 / 空 / 多字节尾 → None
        assert!(p("18").is_none());
        assert!(p("").is_none());
        assert!(p("1中").is_none());
        // 负号保留在数字段
        let (num, _) = p("-3in").unwrap();
        assert_eq!(num, "-3");
    }

    /// 车道OS：`<number> | <percentage>` 比率解析——第二返回值 = 是否
    /// % 形态（消费方对 true 除 100）。数值精度走 parse_f32 家族
    /// （指数/前导符号/空白容忍同源）；% 尾缀 ASCII 字节匹配，多字节
    /// 安全（同 parse_with_abs_unit 的「1中」→ None 口径）。
    #[test]
    fn ratio_or_pct_two_forms() {
        let r = super::parse_ratio_or_pct;
        // 纯 number 形态：pct=false
        assert_eq!(r("0.5"), Some((0.5, false)));
        assert_eq!(r("1"), Some((1.0, false)));
        assert_eq!(r(" -0.25 "), Some((-0.25, false)));
        assert_eq!(r("1e-1"), Some((0.1, false)));
        // % 形态：pct=true，数值原样（除 100 是消费方职责）
        assert_eq!(r("50%"), Some((50.0, true)));
        assert_eq!(r(" 12.5% "), Some((12.5, true)));
        assert_eq!(r("-25%"), Some((-25.0, true)));
        assert_eq!(r("150%"), Some((150.0, true)));
        assert_eq!(r("100%"), Some((100.0, true)));
        // 非法 / 空 / 裸 % / 多字节尾
        assert_eq!(r("abc"), None);
        assert_eq!(r("abc%"), None);
        assert_eq!(r(""), None);
        assert_eq!(r("%"), None);
        assert_eq!(r("1中"), None);
        assert_eq!(
            r("1 %"),
            Some((1.0, true)),
            "% 前空格被 trim 容忍（宽松口径，钉死现状）"
        );
    }
}
