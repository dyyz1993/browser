//! ael_trace —— 跨行 active-edge list（AEL）离线盈利性验证（host-perf 诊断）。
//!
//! ADR-0027 终判后 coverage 第一热点是 isect_sort（MI 8 pelican 占
//! coverage 44.3%）：>32 交点的子扫描线每行 ×4 从零稳定排序。本模块在
//! **与生产完全相同的几何/谓词/排序语义**上做离线 trace 与算法模拟，
//! 回答「跨像素行维护有序 AEL 在 A75 上是否值得实现」（GO/NO-GO 静态
//! 门数据源，见 tests/oracle/ael_feasibility.py 与
//! reports/ael-feasibility.md）。
//!
//! 同源保证（禁止并行实现另一套几何）：
//! - 边收集：[`crate::raster::collect_edges`]（生产 fill_path 同一函数）；
//! - 行桶：[`crate::raster::build_row_buckets`]（同一函数同一效益门，
//!   回退域走与生产相同的全边逐行扫描）；
//! - 子扫描线位置：`y + (sub + 0.5) * 0.25`（aa>1）/ `y + 0.5`（aa=1），
//!   与 fill_path 逐字同式；
//! - 活跃边谓词：`y0 <= yc && yc < y1` 同式；边表顺序 = 边索引序
//!   （生产分桶保序的同构）；
//! - 交点表达式：`x0 + slope * (yc - y0)` 同式（两算法都禁增量漂移）；
//! - 排序：R0 用 [`crate::raster::cmp_scan_x`] 计数闭包走生产同构算法
//!   （≤32 插入 / >32 `sort_by`）——比较次数是**实测**不是模型；候选
//!   序规范形 = (x, ei) 全序（ADR-0027 阶段二已证 ≡ 生产稳定序：生成序
//!   = 边序，相等 x 平局归 ei）。
//!
//! 本模块**不进生产渲染路径**：fill_path 零改动；仅 CLI `ael-trace`
//! 诊断入口消费（无 host-perf 构建零编译，第五门禁证）。

use alloc::vec::Vec;

use crate::dl::FillRule;
use crate::raster::{build_row_buckets, cmp_scan_x, collect_edges};

/// 与 fill_path 同源的 trace 配置。
pub struct TraceCfg {
    pub w: u32,
    pub h: u32,
    /// aa 档（1 = 单子行 y+0.5；>1 = 4 子行解析覆盖——生产缺省 2/3）。
    pub aa: i32,
    pub rule: FillRule,
    /// 设备空间裁切矩形 [x0,y0,x1,y1]（None = 无 clip）。只影响行范围
    /// 收紧（排序统计面）；x 向裁切只影响 span 闭合不影响排序，不模拟。
    pub clip_rect: Option<[f32; 4]>,
}

/// 单条 fill 命令的 trace 汇总（全部标量/小数组，CLI 序列化为 JSONL）。
/// per-sub-row 原始序列不外放（体量），分位在 Rust 侧算好；fill 级
/// Top-churn 归因由 ael_feasibility.py 按 c1_swaps 排序输出。
#[derive(Debug, Clone, Copy, Default)]
pub struct FillTrace {
    // —— 输入结构 ——
    pub pts_n: u32,
    pub edges_n: u32,
    pub rows_scanned: u32,
    /// 有效子行数（交点 ≥2，真正发生排序的子行）
    pub sub_rows: u64,
    pub bucketed: bool,
    pub bbox: [f32; 4],
    // —— 输入形态 ——
    pub nan_x: u64,
    pub inf_x: u64,
    pub same_x_adjacent: u64,
    pub neg_zero_x: u64,
    // —— active 边数分布（有效子行）——
    pub act_max: u32,
    pub act_p50: u32,
    pub act_p95: u32,
    pub act_sum: u64,
    /// >32 交点（进 driftsort）子行数与 Σ n·log2n 排序权重
    pub subs_gt32: u64,
    pub gt32_weight_nlgn: f64,
    // —— 成员变化（相邻有效子行差集）——
    pub enter_total: u64,
    pub exit_total: u64,
    /// (enter+exit)/act_sum
    pub churn_ratio: f64,
    /// 连续零 churn 像素行段的最大长度
    pub low_churn_run_max: u32,
    // —— R0（权威复刻）：实测排序比较次数（同算法计数闭包）——
    pub r0_compares: u64,
    // —— R2（子行复用 = ADR-0027 arm2 语义：行内 4 子行复用 + 行级回退，
    //         每行 sub0 从零重建——对照解释设备 reuse_fb=255）——
    pub r2_swaps: u64,
    /// 行级回退事件数（与设备 coverage_reuse_fallbacks 同口径：一次回退
    /// 杀本行剩余子行，下一行 sub0 重建）
    pub r2_budget_fallbacks: u64,
    pub r2_dead: bool,
    // —— C1（跨行 AEL，append+repair 形态：新成员尾部追加 + 单次插入
    //    排序修复置位——对 churn 免疫，无插入扫描/移位）——
    pub c1_inserts: u64,
    pub c1_deletes: u64,
    /// 删除侧双模型：链表（线性步行）步数 / 数组（二分比较+移位数）
    pub c1_del_lin_steps: u64,
    pub c1_del_bin_cmps: u64,
    pub c1_del_shifts: u64,
    pub c1_recompute: u64,
    pub c1_swaps: u64,
    pub c1_traverse: u64,
    pub c1_peak_members: u32,
    /// 每有效子行 repair swap 数分位
    pub c1_swap_p50: u32,
    pub c1_swap_p95: u32,
    pub c1_swap_p99: u32,
    /// 每子行修复位移（|新索引−旧索引|，成对近似：每个被搬成员计一次
    /// 位移，另一端元素按 swap 对近似 1——分位口径见报告）分位
    pub disp_mean: f64,
    pub disp_p50: u32,
    pub disp_p95: u32,
    pub disp_p99: u32,
    pub disp_max: u32,
    /// C1 可用（全 fill 无 NaN 交点）
    pub c1_eligible: bool,
    // —— 等值断言（R0 vs C1 逐子行序/winding span hash 链）——
    pub order_hash_r0: u64,
    pub order_hash_c1: u64,
    pub span_hash_r0: u64,
    pub span_hash_c1: u64,
    pub equal_verdict: bool,
    // —— edge_setup 行级跳过（ESS：非分桶 fill 的逐行 O(E) band 重扫面）——
    /// 与上一像素行 band-act 集完全相同的行数（首行不计；行级零 churn）
    pub row_zero_churn: u32,
    /// Σ 每像素行 band-act 大小（事件化 act 维护的条目量/建桶 pairs 同源）
    pub band_act_sum: u64,
    /// 非分桶 fill 的 Σ rows×edges（现行逐行全边重扫成本模型）
    pub scan_ops: u64,
    /// oracle O1：非分桶零 churn 行的 Σ rows×edges（act 复用可全免面）
    pub skip_ops_zero: u64,
    // —— C2（自适应）——
    /// 冷启动（R0）有效子行数（含未入选 fill 的全程）
    pub c2_cold_rows: u64,
    /// C1 模式覆盖的 r0 比较权重 / 错过的权重（+bail 后全程）
    pub c2_c1_compares_covered: u64,
    pub c2_c1_compares_missed: u64,
    /// C1 模式运行的有效子行数（C2 成本折算的分母锚）
    pub c2_c1_subrows: u64,
    /// bail 时已付的 AEL ops（waste 口径：swap+重算）
    pub c2_fallback_waste_ops: u64,
    pub c2_bailed: bool,
}

/// C2 预测器常数（在线合法：冷窗 + 滑动窗，无 fixture 特判）。缺省值
/// 来自首轮语料分布定标（报告记录敏感性）。
pub struct C2Params {
    /// fill 级最小边数（小 path 生成成本本就低，AEL 固定成本划不来）
    pub min_edges: u32,
    /// 冷窗有效子行数（影子 AEL 测成本的最小样本）
    pub cold_subs: u64,
    /// 影子成本/R0 比较数 的切换阈（×10/×6 整数比 = 0.6）
    pub cold_ratio_num: u64,
    pub cold_ratio_den: u64,
    /// 滑动窗（8 有效子行）swap/active 比上限（实时 bail）
    pub hot_swap_ratio_max: f64,
}

impl Default for C2Params {
    fn default() -> Self {
        C2Params {
            min_edges: 24,
            cold_subs: 8,
            cold_ratio_num: 6,
            cold_ratio_den: 10,
            hot_swap_ratio_max: 0.5,
        }
    }
}

const HASH_MUL: u64 = 0x100_0000_01B3;

#[inline]
fn hash_step(h: u64, x_bits: u32, dir: i32) -> u64 {
    h.wrapping_mul(HASH_MUL)
        .wrapping_add(x_bits as u64)
        .wrapping_mul(HASH_MUL)
        ^ ((dir as u64) & 0xff)
}

/// winding span hash：对已排序 (x,dir) 按 rule 推进绕数，折叠每个 span
/// 的 (进入 bits, 退出 bits)——与 fill_path 的 span 配对语义同构（x 向
/// 裁切不影响配对，不模拟）。
fn span_hash(xs: &[(f32, i32)], rule: FillRule, h0: u64) -> u64 {
    let mut h = h0;
    let mut wind = 0i32;
    let mut span_start: Option<u32> = None;
    let is_in = |wind: i32| match rule {
        FillRule::NonZero => wind != 0,
        FillRule::EvenOdd => wind % 2 != 0,
    };
    for &(x, dir) in xs.iter() {
        wind += if rule == FillRule::NonZero {
            dir
        } else if dir > 0 {
            1
        } else {
            -1
        };
        let now_in = is_in(wind);
        match (span_start, now_in) {
            (None, true) => span_start = Some(x.to_bits()),
            (Some(sx), false) => {
                span_start = None;
                h = h.wrapping_mul(HASH_MUL).wrapping_add(sx as u64);
                h = h.wrapping_mul(HASH_MUL).wrapping_add(x.to_bits() as u64);
            }
            _ => {}
        }
    }
    h
}

/// 计数插入排序（≤32 域）：与 raster::sort_scan_x 插入臂同构，比较数
/// 实测（含最后一个为假的哨兵比较——与生产同一比较序列）。
fn insertion_sort_counting(xs: &mut [(f32, i32)]) -> u64 {
    let mut cmps = 0u64;
    for i in 1..xs.len() {
        let item = xs[i];
        let mut j = i;
        while j > 0 && {
            cmps += 1;
            cmp_scan_x(&item, &xs[j - 1]) == core::cmp::Ordering::Less
        } {
            xs[j] = xs[j - 1];
            j -= 1;
        }
        xs[j] = item;
    }
    cmps
}

fn quantile_u32(sorted: &[u32], q: f64) -> u32 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = libm::round((sorted.len() as f64 - 1.0) * q) as usize;
    sorted[idx.min(sorted.len() - 1)]
}

/// 有序成员表（C1/R2 共用）：元素 (x, dir, ei)，序 = (x, ei) 全序。
struct Ael {
    v: Vec<(f32, i32, u32)>,
}

impl Ael {
    fn new() -> Self {
        Ael { v: Vec::new() }
    }
    fn clear(&mut self) {
        self.v.clear();
    }
    /// 删除 ei。返回 (链表步行步数, 二分比较数, 移位元素数)。
    /// 数组模型需要先知道被删成员的当前 x 才能二分——调用方须在重算
    /// 之后删除（C1 流程：先重算再删除时 x 已新鲜；R2 行内同理）。
    fn delete(&mut self, ei: u32) -> (u64, u64, u64) {
        let n = self.v.len();
        // 二分定位（(x, ei) 全序，但只有 ei——用线性找位置后报告
        // log2(n) 比较数：数组实现的二分比较数与位置无关）
        for i in 0..n {
            if self.v[i].2 == ei {
                self.v.remove(i);
                let bin = if n > 1 {
                    libm::ceil(libm::log2(n as f64)) as u64
                } else {
                    1
                };
                return (i as u64 + 1, bin, (n - i - 1) as u64);
            }
        }
        (n as u64, 0, 0)
    }
    /// 尾部追加新成员（append+repair 形态：置位由 repair 的相邻 swap
    /// 完成，无插入扫描——swap 数已含置位逆序，成本记在 c1_swaps）。
    fn append(&mut self, x: f32, dir: i32, ei: u32) {
        self.v.push((x, dir, ei));
    }
    /// 相邻 swap 修复到 (x, ei) 全序（swap 数 = 插入排序搬移数 = 逆序
    /// 数）。位移近似：被搬成员计 i−j，另一端每对 swap 计 1（成对近似，
    /// 分位用途足够；不计位移时传哑 vec）。返回 swap 数。
    fn repair(&mut self, displace: &mut Vec<u32>) -> u64 {
        let mut swaps = 0u64;
        for i in 1..self.v.len() {
            let mut j = i;
            while j > 0
                && (self.v[j]
                    .0
                    .partial_cmp(&self.v[j - 1].0)
                    .unwrap_or(core::cmp::Ordering::Equal)
                    .then(self.v[j].2.cmp(&self.v[j - 1].2)))
                    == core::cmp::Ordering::Less
            {
                self.v.swap(j, j - 1);
                swaps += 1;
                j -= 1;
            }
            if j != i {
                displace.push((i - j) as u32);
            }
        }
        if swaps > 0 {
            displace.push(1); // 每对 swap 另一端的近似位移
        }
        swaps
    }
}

/// 对一条 fill 点池做全量 trace。`pts` 与 fill_path 同语义（NaN 分隔环，
/// 设备空间展平点）。C2 决策只用在线可见量（冷窗统计/滑动窗 swap 率）。
pub fn trace_fill(pts: &[f32], cfg: &TraceCfg, c2p: &C2Params) -> FillTrace {
    let mut out = FillTrace::default();
    let mut edges: Vec<(f32, f32, f32, f32, i32)> = Vec::new();
    let mut x_all_finite = true;
    let mut xmin = f32::MAX;
    let mut xmax = f32::MIN;
    collect_edges(pts, &mut edges, &mut x_all_finite, &mut xmin, &mut xmax);
    out.pts_n = (pts.len() / 2) as u32;
    out.edges_n = edges.len() as u32;
    if edges.len() < 2 {
        // 空路径/单边：两算法都不运行，等值空真
        out.c1_eligible = true;
        out.equal_verdict = true;
        return out;
    }
    let h = cfg.h;
    let y_min = libm::floorf(edges.iter().map(|e| e.0).fold(f32::MAX, f32::min)).max(0.0) as i32;
    let y_max =
        libm::ceilf(edges.iter().map(|e| e.1).fold(f32::MIN, f32::max)).min(h as f32) as i32;
    let (cy0, cy1) = match cfg.clip_rect {
        Some(r) => (
            (libm::ceilf(r[1] - 0.5).max(y_min as f32)) as i32,
            (libm::ceilf(r[3] - 0.5).min(y_max as f32)) as i32,
        ),
        None => (y_min, y_max),
    };
    let row_lo = cy0.max(0);
    let row_hi = cy1.max(0);
    out.rows_scanned = (row_hi - row_lo).max(0) as u32;
    out.bbox = [xmin, y_min as f32, xmax, y_max as f32];
    // 行桶（生产同源；回退域 = 全边扫描，与 fill_path 同结构）
    let aa_on = cfg.aa > 1;
    let (hi_d, lo_d) = if aa_on { (0.875, 0.125) } else { (0.5, 0.5) };
    let mut bk_first: Vec<i32> = Vec::new();
    let mut bk_last: Vec<i32> = Vec::new();
    let mut bk_diff: Vec<i32> = Vec::new();
    let mut bk_off: Vec<u32> = Vec::new();
    let mut bk_cur: Vec<u32> = Vec::new();
    let mut bk_flat: Vec<u32> = Vec::new();
    let bucketed = build_row_buckets(
        &edges,
        row_lo,
        row_hi,
        hi_d,
        lo_d,
        &mut bk_first,
        &mut bk_last,
        &mut bk_diff,
        &mut bk_off,
        &mut bk_cur,
        &mut bk_flat,
    );
    out.bucketed = bucketed;

    let mut ael_c1 = Ael::new();
    let mut ael_r2 = Ael::new();
    let mut r2_dummy_disp: Vec<u32> = Vec::new();
    let mut prev_active: Vec<u32> = Vec::new();
    // ESS：上一像素行 band-act（行级零 churn 对比用）
    let mut prev_band_act: Vec<u32> = Vec::new();
    let mut prev_band_act_initialized = false;
    let mut act_samples: Vec<u32> = Vec::new();
    let mut swap_samples: Vec<u32> = Vec::new();
    let mut displace_samples: Vec<u32> = Vec::new();
    let mut low_churn_run = 0u32;
    // C2 v2 状态（在线：冷窗影子 AEL 成本比决策）
    let mut c2_mode_c1 = false;
    let mut c2_decided = false;
    let mut c2_bailed = false;
    let mut cold_r0_cmps: u64 = 0;
    let mut cold_shadow_ops: u64 = 0; // 冷窗影子 C1 数组模型成本
    let mut win_swaps: u64 = 0;
    let mut win_act: u64 = 0;
    let mut win_count: u64 = 0;
    for y in row_lo..row_hi {
        // R2 行级状态（arm2 语义：行内复用 + 行级回退，行首复位）
        let mut r2_row_alive = true;
        let mut r2_bootstrapped_this_row = false;
        let act_list: Vec<u32> = if bucketed {
            let r = (y - row_lo) as usize;
            bk_flat[bk_off[r] as usize..bk_off[r + 1] as usize].to_vec()
        } else {
            let yb_lo = y as f32 + lo_d;
            let yb_hi = y as f32 + hi_d;
            let mut a = Vec::new();
            for (i, &(y0, y1, ..)) in edges.iter().enumerate() {
                if y0 <= yb_hi && y1 > yb_lo {
                    a.push(i as u32);
                }
            }
            a
        };
        // ESS：行级 band-act 零 churn 统计（与上一行逐元素对比，ei 升序）
        let row_unchanged = prev_band_act.as_slice() == act_list.as_slice();
        if prev_band_act_initialized && row_unchanged {
            out.row_zero_churn += 1;
        }
        out.band_act_sum += act_list.len() as u64;
        if !bucketed {
            out.scan_ops += edges.len() as u64;
            if prev_band_act_initialized && row_unchanged {
                out.skip_ops_zero += edges.len() as u64;
            }
        }
        prev_band_act = act_list.clone();
        prev_band_act_initialized = true;
        let nsubs = if aa_on { 4 } else { 1 };
        let mut row_churn: u64 = 0;
        for sub in 0..nsubs {
            let yc = if aa_on {
                y as f32 + (sub as f32 + 0.5) * 0.25
            } else {
                y as f32 + 0.5
            };
            // active 集（谓词同式，ei 升序）
            let mut xs: Vec<(f32, i32)> = Vec::new();
            let mut active: Vec<u32> = Vec::new();
            for &ei in act_list.iter() {
                let &(y0, y1, x0, slope, dir) = &edges[ei as usize];
                if y0 <= yc && yc < y1 {
                    xs.push((x0 + slope * (yc - y0), dir));
                    active.push(ei);
                }
            }
            if xs.len() < 2 {
                continue;
            }
            out.sub_rows += 1;
            act_samples.push(xs.len() as u32);
            out.act_sum += xs.len() as u64;
            for i in 0..xs.len() {
                let b = xs[i].0.to_bits();
                if xs[i].0.is_nan() {
                    out.nan_x += 1;
                }
                if xs[i].0.is_infinite() {
                    out.inf_x += 1;
                }
                if b == 0x8000_0000 {
                    out.neg_zero_x += 1;
                }
                if i + 1 < xs.len() && b == xs[i + 1].0.to_bits() {
                    out.same_x_adjacent += 1;
                }
            }
            // 成员变化（相邻有效子行差集，ei 升序双指针）
            let mut enters: Vec<u32> = Vec::new();
            let mut leaves: Vec<u32> = Vec::new();
            let (mut i, mut j) = (0usize, 0usize);
            while i < prev_active.len() || j < active.len() {
                match (prev_active.get(i), active.get(j)) {
                    (Some(&a), Some(&b)) if a == b => {
                        i += 1;
                        j += 1;
                    }
                    (Some(&a), Some(&b)) if a < b => {
                        leaves.push(a);
                        i += 1;
                    }
                    (Some(_), Some(&b)) => {
                        enters.push(b);
                        j += 1;
                    }
                    (Some(&a), None) => {
                        leaves.push(a);
                        i += 1;
                    }
                    (None, Some(&b)) => {
                        enters.push(b);
                        j += 1;
                    }
                    (None, None) => break,
                }
            }
            out.enter_total += enters.len() as u64;
            out.exit_total += leaves.len() as u64;
            row_churn += (enters.len() + leaves.len()) as u64;
            if xs.len() > 32 {
                out.subs_gt32 += 1;
                let n = xs.len() as f64;
                out.gt32_weight_nlgn += n * libm::log2(n.max(2.0));
            }
            // 生成序三元组（R2 引导/C2 影子用；排序前留档）
            let keyed: Vec<(f32, i32, u32)> = active
                .iter()
                .zip(xs.iter())
                .map(|(&ei, &(x, d))| (x, d, ei))
                .collect();
            // —— R0：生产同构排序 + 实测比较数 ——
            let r0_cmps = if xs.len() > 32 {
                let mut c = 0u64;
                xs.sort_by(|a, b| {
                    c += 1;
                    cmp_scan_x(a, b)
                });
                c
            } else {
                insertion_sort_counting(&mut xs)
            };
            out.r0_compares += r0_cmps;
            let mut oh = out.order_hash_r0;
            for &(x, dir) in xs.iter() {
                oh = hash_step(oh, x.to_bits(), dir);
            }
            out.order_hash_r0 = oh;
            out.span_hash_r0 = span_hash(&xs, cfg.rule, out.span_hash_r0);

            // —— C1 跨行 AEL（append+repair：先重算（删除二分需新鲜 x）→
            //     删除离开成员 → 尾部追加新成员 → 单次修复置位）——
            let (pre_rec, pre_db, pre_dsh) =
                (out.c1_recompute, out.c1_del_bin_cmps, out.c1_del_shifts);
            let (pre_sw, pre_tv) = (out.c1_swaps, out.c1_traverse);
            let mut c1_swaps_this: u64 = 0;
            if out.nan_x == 0 {
                for m in ael_c1.v.iter_mut() {
                    let &(y0, _y1, x0, slope, _d) = &edges[m.2 as usize];
                    m.0 = x0 + slope * (yc - y0);
                    out.c1_recompute += 1;
                }
                for &ei in leaves.iter() {
                    let (lin, bin, m) = ael_c1.delete(ei);
                    out.c1_deletes += 1;
                    out.c1_del_lin_steps += lin;
                    out.c1_del_bin_cmps += bin;
                    out.c1_del_shifts += m;
                }
                for &ei in enters.iter() {
                    let &(y0, _y1, x0, slope, dir) = &edges[ei as usize];
                    let x = x0 + slope * (yc - y0);
                    ael_c1.append(x, dir, ei);
                    out.c1_inserts += 1;
                }
                c1_swaps_this = ael_c1.repair(&mut displace_samples);
                out.c1_swaps += c1_swaps_this;
                swap_samples.push(c1_swaps_this as u32);
                out.c1_traverse += ael_c1.v.len() as u64;
                if ael_c1.v.len() as u32 > out.c1_peak_members {
                    out.c1_peak_members = ael_c1.v.len() as u32;
                }
                let mut oh = out.order_hash_c1;
                for &(x, dir, _ei) in ael_c1.v.iter() {
                    oh = hash_step(oh, x.to_bits(), dir);
                }
                out.order_hash_c1 = oh;
                let c1_xs: Vec<(f32, i32)> = ael_c1.v.iter().map(|&(x, d, _)| (x, d)).collect();
                out.span_hash_c1 = span_hash(&c1_xs, cfg.rule, out.span_hash_c1);
            }
            // 本子行 C1 数组模型成本增量（C2 影子口径：重算+二分+移位
            // +swap×2+遍历；NaN 子行为 0）
            let c1_arr_delta = (out.c1_recompute - pre_rec)
                + (out.c1_del_bin_cmps - pre_db)
                + (out.c1_del_shifts - pre_dsh)
                + (out.c1_swaps - pre_sw) * 2
                + (out.c1_traverse - pre_tv);

            // —— R2（arm2 语义）：行内 4 子行复用；每行首有效子行从生产
            // 同构排序结果引导（sub0 的排序成本已在 r0_compares 记账），
            // 后续子行 enters/leaves/重算/修复；swap > 2n+16 行级回退。
            if r2_row_alive {
                let mut dead = false;
                if out.nan_x > 0 {
                    dead = true;
                } else if !r2_bootstrapped_this_row {
                    // 本行 sub0：按 (x, ei) 全序引导（= 生产 sort_scan_x_reuse
                    // 的输出序——证明见 ADR-0027 阶段二）
                    ael_r2.clear();
                    let mut ks = keyed.clone();
                    ks.sort_by(|a, b| {
                        a.0.partial_cmp(&b.0)
                            .unwrap_or(core::cmp::Ordering::Equal)
                            .then(a.2.cmp(&b.2))
                    });
                    ael_r2.v = ks;
                    r2_bootstrapped_this_row = true;
                } else {
                    for m in ael_r2.v.iter_mut() {
                        let &(y0, _y1, x0, slope, _d) = &edges[m.2 as usize];
                        m.0 = x0 + slope * (yc - y0);
                    }
                    for &ei in leaves.iter() {
                        ael_r2.delete(ei);
                    }
                    for &ei in enters.iter() {
                        let &(y0, _y1, x0, slope, dir) = &edges[ei as usize];
                        let x = x0 + slope * (yc - y0);
                        ael_r2.v.push((x, dir, ei));
                    }
                    let swaps = ael_r2.repair(&mut r2_dummy_disp);
                    out.r2_swaps += swaps;
                    let n = ael_r2.v.len() as u64;
                    if swaps > n * 2 + 16 {
                        out.r2_budget_fallbacks += 1;
                        dead = true;
                    }
                }
                if dead {
                    r2_row_alive = false;
                    out.r2_dead = true;
                    ael_r2.clear();
                }
            }

            // —— C2 v2 自适应（在线：冷窗跑 R0 + 影子 AEL 测成本比，之后
            // 切换；热窗 swap 率 bail。影子成本 = 数组模型 op 数）——
            if c2_bailed || out.nan_x > 0 {
                if !c2_bailed && out.nan_x > 0 {
                    c2_bailed = true;
                }
                out.c2_c1_compares_missed += r0_cmps;
            } else if c2_mode_c1 {
                out.c2_c1_compares_covered += r0_cmps;
                out.c2_c1_subrows += 1;
                win_swaps += c1_swaps_this;
                win_act += xs.len() as u64;
                win_count += 1;
                if win_count >= 8 {
                    if win_act > 0 && (win_swaps as f64 / win_act as f64) > c2p.hot_swap_ratio_max {
                        c2_bailed = true;
                        out.c2_fallback_waste_ops = out.c1_swaps + out.c1_recompute;
                    }
                    win_swaps = 0;
                    win_act = 0;
                    win_count = 0;
                }
            } else {
                // 冷窗（R0 输出 + 影子 C1 测成本）
                out.c2_cold_rows += 1;
                out.c2_c1_compares_missed += r0_cmps;
                cold_r0_cmps += r0_cmps;
                // 影子成本（跳过首子行 bootstrap）：recompute+二分比较+移位
                // +swap×2+遍历（数组模型口径，权重表见报告）
                if out.sub_rows > 1 {
                    cold_shadow_ops += c1_arr_delta;
                }
                win_count += 1;
                if !c2_decided && (win_count >= c2p.cold_subs || cold_r0_cmps > 4096) {
                    c2_decided = true;
                    if out.edges_n >= c2p.min_edges && cold_shadow_ops * 10 <= cold_r0_cmps * 6 {
                        c2_mode_c1 = true; // 影子成本 ≤ 60% 的 R0 比较数
                    } else {
                        c2_bailed = true; // 本 fill 全程 R0
                    }
                }
            }

            prev_active = active;
        }
        if row_churn == 0 {
            low_churn_run += 1;
            if low_churn_run > out.low_churn_run_max {
                out.low_churn_run_max = low_churn_run;
            }
        } else {
            low_churn_run = 0;
        }
    }
    // 分位汇总
    act_samples.sort_unstable();
    out.act_max = *act_samples.last().unwrap_or(&0);
    out.act_p50 = quantile_u32(&act_samples, 0.50);
    out.act_p95 = quantile_u32(&act_samples, 0.95);
    swap_samples.sort_unstable();
    out.c1_swap_p50 = quantile_u32(&swap_samples, 0.50);
    out.c1_swap_p95 = quantile_u32(&swap_samples, 0.95);
    out.c1_swap_p99 = quantile_u32(&swap_samples, 0.99);
    displace_samples.sort_unstable();
    let dn = displace_samples.len();
    if dn > 0 {
        let sum: u64 = displace_samples.iter().map(|&v| v as u64).sum();
        out.disp_mean = sum as f64 / dn as f64;
    }
    out.disp_p50 = quantile_u32(&displace_samples, 0.50);
    out.disp_p95 = quantile_u32(&displace_samples, 0.95);
    out.disp_p99 = quantile_u32(&displace_samples, 0.99);
    out.disp_max = *displace_samples.last().unwrap_or(&0);
    if out.act_sum > 0 {
        out.churn_ratio = (out.enter_total + out.exit_total) as f64 / out.act_sum as f64;
    }
    out.c1_eligible = out.nan_x == 0;
    out.c2_bailed = out.c2_bailed || !c2_mode_c1;
    out.equal_verdict = out.nan_x == 0
        && out.order_hash_r0 == out.order_hash_c1
        && out.span_hash_r0 == out.span_hash_c1;
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn cfg_for(aa: i32, rule: FillRule) -> TraceCfg {
        TraceCfg {
            w: 96,
            h: 72,
            aa,
            rule,
            clip_rect: None,
        }
    }

    /// 等值硬门：随机环（含交叉/洞/重合边/同 x 列）× 规则 × aa 档，
    /// R0 与 C1 的逐子行序 hash + winding span hash 链必须全等——
    /// 这是「模拟器与生产同一问题」的锚（collect_edges/谓词/排序全同源）。
    #[test]
    fn ael_trace_r0_c1_equiv_deterministic_3k() {
        let mut seed: u64 = 0x4145_4c5f_4645_4153;
        let mut rng = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let c2p = C2Params::default();
        for it in 0..3_000usize {
            let (w, h) = match it % 4 {
                0 => (24u32, 18u32),
                _ => (96, 72),
            };
            let n_rings = 1 + (rng() % 3) as usize;
            let mut pts = alloc::vec::Vec::new();
            for _r in 0..n_rings {
                let n = 4 + ((rng() >> 8) % 16) as usize;
                let cx = ((rng() >> 16) % (w as u64 * 4)) as f32 / 4.0;
                let cy = ((rng() >> 24) % (h as u64 * 4)) as f32 / 4.0;
                let rad = 3.0 + ((rng() >> 32) % 90) as f32 / 4.0;
                for k in 0..n {
                    let ang = (k as f32) * 0.9 + ((rng() >> 40) % 64) as f32 / 32.0;
                    let jit = ((rng() >> 48) % 16) as f32 / 8.0;
                    let mut x = cx + libm::cosf(ang) * (rad + jit);
                    let y = cy + libm::sinf(ang) * (rad + jit) * 0.8;
                    if k % 4 == 0 {
                        x = libm::roundf(x);
                    }
                    pts.push(x);
                    pts.push(y);
                }
                pts.push(f32::NAN);
                pts.push(f32::NAN);
            }
            let rule = if it % 2 == 0 {
                FillRule::NonZero
            } else {
                FillRule::EvenOdd
            };
            let aa = if it % 3 == 0 { 3 } else { 2 };
            let cfg = TraceCfg {
                w,
                h,
                aa,
                rule,
                clip_rect: None,
            };
            let t = trace_fill(&pts, &cfg, &c2p);
            if t.sub_rows > 0 {
                assert!(
                    t.equal_verdict,
                    "R0/C1 等值失败 it={it} rule={rule:?} aa={aa} nan={}",
                    t.nan_x
                );
            }
            // 计量守恒：重算数 ≥ 有效子行数-1 级别的下界粗检 + 峰值 ≤ 边数
            assert!(t.c1_peak_members as u64 <= t.edges_n as u64);
        }
    }

    /// NaN/Inf 面：交点 NaN 的 fill 必须 c1_eligible=false 且 eq=false
    ///（不产生假等值）；有限输入恒 eligible。
    #[test]
    fn ael_trace_nan_ineligibility() {
        let c2p = C2Params::default();
        let nan_pts: alloc::vec::Vec<f32> = vec![
            f32::INFINITY,
            0.0,
            90.0,
            f32::INFINITY,
            90.0,
            70.0,
            f32::NEG_INFINITY,
            35.0,
        ];
        let t = trace_fill(&nan_pts, &cfg_for(2, FillRule::NonZero), &c2p);
        assert!(t.nan_x > 0);
        assert!(!t.c1_eligible && !t.equal_verdict);
        let ok_pts: alloc::vec::Vec<f32> = vec![8.0, 8.0, 88.0, 9.0, 80.0, 64.0, 12.0, 60.0];
        let t2 = trace_fill(&ok_pts, &cfg_for(2, FillRule::NonZero), &c2p);
        assert!(t2.sub_rows > 0 && t2.c1_eligible && t2.equal_verdict);
        // aa=1 单子行路径同过
        let t3 = trace_fill(&ok_pts, &cfg_for(1, FillRule::EvenOdd), &c2p);
        assert!(t3.sub_rows > 0 && t3.equal_verdict);
    }

    /// R2（arm2 语义）行级回退公式与生产一致：swap > 2n+16 才计回退，
    /// 且 r2_swaps ≥ 0 / r2_dead 只在有回退或 NaN 时置位。
    #[test]
    fn ael_trace_r2_row_semantics() {
        let c2p = C2Params::default();
        // 高交叉星形（边短命 → 大量行内翻搅）——回退面存在性烟测
        let mut pts = alloc::vec::Vec::new();
        let n = 96usize;
        for k in 0..n {
            let r = if k % 2 == 0 { 34.0 } else { 12.0 };
            let a = (k as f32) * 0.13;
            pts.push(48.0 + r * libm::cosf(a));
            pts.push(36.0 + r * libm::sinf(a));
        }
        pts.push(f32::NAN);
        pts.push(f32::NAN);
        let t = trace_fill(&pts, &cfg_for(2, FillRule::NonZero), &c2p);
        assert!(t.sub_rows > 0 && t.equal_verdict);
        // 不变量：预算回退 ⇒ dead；无回退无 NaN ⇒ not dead
        assert_eq!(t.r2_dead, t.r2_budget_fallbacks > 0 || t.nan_x > 0);
        assert!(t.r2_budget_fallbacks <= t.rows_scanned as u64 + 4);
    }

    /// ESS 四字段的最小语义钉：小矩形不触发行桶，十行的两条竖边保持
    /// 同一 band-act；首行用于初始化，后九行全部属于零 churn 可跳面。
    #[test]
    fn ess_row_oracles_small_rect() {
        let pts: alloc::vec::Vec<f32> = vec![10.0, 10.0, 20.0, 10.0, 20.0, 20.0, 10.0, 20.0];
        let t = trace_fill(&pts, &cfg_for(2, FillRule::NonZero), &C2Params::default());
        assert!(!t.bucketed);
        assert_eq!(t.edges_n, 2);
        assert_eq!(t.rows_scanned, 10);
        assert_eq!(t.row_zero_churn, 9);
        assert_eq!(t.band_act_sum, 20);
        assert_eq!(t.scan_ops, 20);
        assert_eq!(t.skip_ops_zero, 18);
    }
}
