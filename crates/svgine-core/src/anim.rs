//! 动画引擎（M5/M6 落地）：SMIL 时序模型 + CSS keyframes。
//!
//! 宪法 2 的落地：不读系统时钟，文档时间轴由宿主注入/推进；
//! 引擎只产出「t 时刻属性覆盖表」，与渲染完全解耦，
//! 使 `render(doc, t)` 保持纯函数、可在任意 t 采样 golden 帧。
//!
//! 本文件是 **M5 纯模型层**（不接 builder、不解析 `<animate>` 元素）：
//! 时钟值 / begin-end 列表 / 区间状态机 / calcMode 插值器均为纯数据结构 +
//! 纯函数，独立可测；M5 集成期把 builder 解析出的属性喂进来即可。
//!
//! 本车道的已知简化（M5 集成时升级）：
//! - syncbase/eventbase begin **只解析不求值**（求值需文档级时间依赖图）；
//! - `repeatDur` / `media` 标记显式忽略；
//! - `paced` 按线性近似（真 paced 需路径弧长表，M5 集成几何后升级）；
//! - 颜色插值在 sRGB 域逐通道线性（与 Chrome 的 SMIL 颜色插值一致）。

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::css;

/// 文档时间轴。f64 簿记、每帧一次，不进像素热路径（ADR-0005）。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Timeline {
    pub t_ms: f64,
}

impl Timeline {
    /// 宿主驱动的时间推进（MCU 每帧调用；支持暂停/变速由宿主控制 dt）。
    pub fn advance(&mut self, dt_ms: f64) {
        self.t_ms += dt_ms;
    }
}

// ---------------------------------------------------------------------------
// 1. 时钟值解析（SMIL 3.0 Timing §clock-val）
// ---------------------------------------------------------------------------

/// 解析 SMIL/SVG clock-value → 秒。
///
/// 支持的语法（白名单制，其余一律 `None`）：
/// - 完整时钟 `h:mm:ss(.f)` / 部分时钟 `mm:ss(.f)`（分/秒必须 < 60，
///   SMIL 语法如此；`2:30.5` = 150.5s，`12:34` = 754s，`1:02:03` = 3723s）
/// - 时间计数：`0.5`（无单位 = 秒）、`2s`、`500ms`、`3min`、`2h`
///   （单位后缀不区分大小写）
/// - 前置符号 `+`/`-`（offset 值语义；冒号形式按 SMIL 本不带符号，宽松接受）
pub fn parse_clock(s: &str) -> Option<f64> {
    let s = s.trim();
    let (neg, rest) = match s.as_bytes().first() {
        Some(b'-') => (true, &s[1..]),
        Some(b'+') => (false, &s[1..]),
        _ => (false, s),
    };
    let v = if rest.contains(':') {
        parse_colon_clock(rest)?
    } else {
        parse_timecount(rest)?
    };
    Some(if neg { -v } else { v })
}

/// `h:mm:ss(.f)`（3 段）或 `mm:ss(.f)`（2 段）。splitn(3) 保证 4+ 段落入 c 段被拒。
fn parse_colon_clock(s: &str) -> Option<f64> {
    let mut it = s.splitn(3, ':');
    let a = it.next().unwrap_or("");
    let b = it.next();
    let c = it.next();
    match (b, c) {
        // mm:ss(.f)
        (Some(sec), None) => {
            let m = parse_uint_bounded(a, 59)?;
            let s = parse_seconds_bounded(sec)?;
            Some(m as f64 * 60.0 + s)
        }
        // h:mm:ss(.f)
        (Some(min), Some(sec)) => {
            if sec.contains(':') {
                return None; // 4+ 段
            }
            let h = parse_uint_bounded(a, u64::MAX)?; // 小时无上界
            let m = parse_uint_bounded(min, 59)?;
            let s = parse_seconds_bounded(sec)?;
            Some(h as f64 * 3600.0 + m as f64 * 60.0 + s)
        }
        // "12:" / ":30" 等残缺形式
        _ => None,
    }
}

/// 时间计数：`数字[.小数][单位]`。单位 ∈ {s, ms, min, h}（不区分大小写）。
fn parse_timecount(s: &str) -> Option<f64> {
    let split_at = s.find(|c: char| c.is_ascii_alphabetic()).unwrap_or(s.len());
    let (num, unit) = s.split_at(split_at);
    let mult = match unit.to_ascii_lowercase().as_str() {
        "" | "s" => 1.0,
        "ms" => 0.001,
        "min" => 60.0,
        "h" => 3600.0,
        _ => return None,
    };
    let v = parse_unsigned_decimal(num)?;
    Some(v * mult)
}

/// 无符号十进制数（`Digit+ ("." Digit+)?`；拒绝符号/指数/多点）。parse 安全。
fn parse_unsigned_decimal(s: &str) -> Option<f64> {
    let (int_part, frac_part) = match s.split_once('.') {
        Some((i, f)) => (i, Some(f)),
        None => (s, None),
    };
    let digits = |d: &str| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit());
    if !digits(int_part) {
        return None;
    }
    if let Some(f) = frac_part {
        if !digits(f) {
            return None;
        }
    }
    s.parse::<f64>().ok()
}

/// 秒分量（时钟形式最后一段）：无符号十进制且 < 60。
fn parse_seconds_bounded(s: &str) -> Option<f64> {
    let v = parse_unsigned_decimal(s)?;
    if v < 60.0 {
        Some(v)
    } else {
        None
    }
}

/// 纯数字整数且 ≤ max。
fn parse_uint_bounded(s: &str, max: u64) -> Option<u64> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let v = s.parse::<u64>().ok()?;
    if v <= max {
        Some(v)
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// 2. begin/end 列表解析
// ---------------------------------------------------------------------------

/// begin/end 列表的时间基（syncbase）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Basis {
    Begin,
    End,
    Repeat(u32),
}

/// begin/end 列表单项。
///
/// 本车道 Event/Sync **只解析不求值**（求值需文档级时间依赖图，M5 集成）。
#[derive(Clone, Debug, PartialEq)]
pub enum BeginSpec {
    /// `±clock`：相对文档开始的偏移秒。
    Offset(f64),
    /// `indefinite`。
    Indefinite,
    /// 事件基：`click`（target=None，作用于动画元素自身）/ `rect01.click`。
    /// 车道HK：携带可选带符号偏移（SMIL event-value `event ± offset`，
    /// 如 `btn.click+2s` / `click-0.5s`）——解析期即收下，求值时事件
    /// 时刻加偏移（`hit_test::dispatch_event`）。
    Event {
        target: Option<String>,
        event: String,
        offset: f64,
    },
    /// 同步基：`anim1.begin+0.5s` / `anim2.repeat(2)` / `anim3.end`。
    Sync {
        target: String,
        basis: Basis,
        offset: f64,
    },
}

/// 解析 begin/end 列表中的**单个**条目。
///
/// 不支持/显式拒绝的形式（返回 `None`）：
/// - 事件名后跟不可解析的偏移（`click+` / `click+x`）；
/// - `wallclock(...)` / `accesskey(...)` / `media`。
pub fn parse_begin_item(item: &str) -> Option<BeginSpec> {
    let item = item.trim();
    if item.is_empty() || item.eq_ignore_ascii_case("media") {
        return None;
    }
    if item.eq_ignore_ascii_case("indefinite") {
        return Some(BeginSpec::Indefinite);
    }
    // 1) offset：能按 clock-value 解析的都算（含符号/单位）
    if let Some(v) = parse_clock(item) {
        return Some(BeginSpec::Offset(v));
    }
    // 2) 裸事件名 ±offset **先于**带 '.' 形态（车道HK）：事件名字符集
    //    不含 '.'，但 offset 的十进制小数点（`click-0.5s`）会被下方
    //    split_once('.') 误切成 target="click-0" + event="5s"——裸形态
    //    整体匹配成功即短路，歧义消解。含 '.' 的 item 在此必失败
    //    （is_event_name 拒点），落回带 target 分支。
    if let Some((event, offset)) = parse_event_tail(item) {
        return Some(BeginSpec::Event {
            target: None,
            event: event.to_string(),
            offset,
        });
    }
    // 3) 带 '.'：先试 syncbase，再退 eventbase（`btn.click` /
    //    `btn.click+2s`）
    if let Some((id, tail)) = item.split_once('.') {
        if id.is_empty() {
            return None;
        }
        if let Some(sync) = parse_syncbase(id, tail) {
            return Some(sync);
        }
        let (event, offset) = parse_event_tail(tail)?;
        return Some(BeginSpec::Event {
            target: Some(id.to_string()),
            event: event.to_string(),
            offset,
        });
    }
    None
}

/// 事件名 + 可选带符号 offset（车道HK）：`click` / `click+2s` /
/// `click-0.5s`。事件名字符集（ASCII 字母数字/下划线）不含 `+`/`-`，
/// 故首个 `+`/`-` 即 offset 起点；其余部分必须**整体**可解析为
/// ±clock-value（`click+` / `click+x` 拒绝），offset 语义 = 事件时刻
/// 加偏移（SMIL eventbase `event-value ::= ID-ref-value ( "±" offset-value )?`）。
fn parse_event_tail(s: &str) -> Option<(&str, f64)> {
    let split_at = s.find(['+', '-']).unwrap_or(s.len());
    let (name, rest) = s.split_at(split_at);
    if !is_event_name(name) {
        return None;
    }
    let offset = if rest.is_empty() {
        0.0
    } else {
        parse_clock(rest)?
    };
    Some((name, offset))
}

/// `begin|end|repeat(n)` + 可选 ±offset。`None` = 不是 syncbase 形式。
fn parse_syncbase(id: &str, tail: &str) -> Option<BeginSpec> {
    let (basis, rest) = sync_basis(tail)?;
    // rest 为空 → offset 0；否则必须是可解析的 ±offset
    // （"beginner" 这类残缺词在此被拒，随后按 event 名重判）
    let offset = if rest.is_empty() {
        0.0
    } else {
        parse_clock(rest)?
    };
    Some(BeginSpec::Sync {
        target: id.to_string(),
        basis,
        offset,
    })
}

/// 剥离 `begin|end|repeat(n)` 前缀 → (basis, 余下的 offset 子串)。
fn sync_basis(tail: &str) -> Option<(Basis, &str)> {
    if let Some(r) = tail.strip_prefix("begin") {
        Some((Basis::Begin, r))
    } else if let Some(r) = tail.strip_prefix("end") {
        Some((Basis::End, r))
    } else if let Some(r) = tail.strip_prefix("repeat(") {
        let close = r.find(')')?;
        let n: u32 = r[..close].parse().ok()?;
        Some((Basis::Repeat(n), &r[close + 1..]))
    } else {
        None
    }
}

/// 事件名白名单字符集：非空、ASCII 字母数字/下划线。
fn is_event_name(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// 分号分隔的 begin/end 列表；不可解析项**跳过**（真实世界 SVG 常带杂质）。
pub fn parse_begin_list(s: &str) -> Vec<BeginSpec> {
    s.split(';').filter_map(parse_begin_item).collect()
}

/// 取列表中最早的 Offset。简化时轴的 begin 解析：仅 Offset 生效，
/// Event/Sync/Indefinite 的求值留给 M5 文档时间图。
pub fn earliest_offset(specs: &[BeginSpec]) -> Option<f64> {
    let mut best: Option<f64> = None;
    for spec in specs {
        if let BeginSpec::Offset(o) = spec {
            best = Some(match best {
                Some(b) if b <= *o => b,
                _ => *o,
            });
        }
    }
    best
}

// ---------------------------------------------------------------------------
// 3. Interval 状态机（简化时轴，仅 Offset begin 生效）
// ---------------------------------------------------------------------------

/// repeatCount 属性值（小数形式按 SVG 1.1 只收非负整数）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RepeatCount {
    Count(u32),
    Indefinite,
}

/// fill 属性值：活动期结束后冻住末值还是还原（移除）。
/// Default = Remove（SMIL fill 属性缺省值）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Fill {
    #[default]
    Remove,
    Freeze,
}

/// restart 属性值。纯函数渲染下的**静态采样语义**（事件驱动的重启不可
/// 实现——引擎只吃 t）：多 begin 实例在 t 时刻开出的活动区间如何取舍。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Restart {
    /// 缺省：新实例总是重启活动期——t 取**最新的已开始实例**的区间状态。
    #[default]
    Always,
    /// 活动期内的新实例被忽略（实例区间互不重叠）——t 取最早未结束的
    /// 实例；全部已结束时取最后一个有效实例的结束态。
    WhenNotActive,
    /// 首个实例之后忽略全部后续。
    Never,
}

/// restart 解析（SMIL 枚举值大小写敏感；缺省 always 由调用方兜底）。
pub fn parse_restart(s: &str) -> Option<Restart> {
    match s.trim() {
        "always" => Some(Restart::Always),
        "whenNotActive" => Some(Restart::WhenNotActive),
        "never" => Some(Restart::Never),
        _ => None,
    }
}

/// 简化时轴配置。`media` 标记显式忽略；`repeatDur` 车道CJ 已收。
///
/// begin 实例不变量：`begin_instances` 升序且首元素 == `begin`；空表 =
/// 单实例（求值回落到 `begin`）。`begin` 保留为兼容字段/单实例快路径。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Timing {
    /// 最早（首个）begin 实例的文档秒。
    pub begin: f64,
    /// 全部已解析的 Offset begin 实例（升序）。空 = 单实例。
    pub begin_instances: Vec<f64>,
    /// begin 实例与活动期重叠时的取舍（缺省 [`Restart::Always`]）。
    pub restart: Restart,
    /// simple duration 秒；`None` = indefinite（永不结束）。
    pub dur: Option<f64>,
    /// None = 只播一轮（活动时长 = dur）。
    pub repeat_count: Option<RepeatCount>,
    /// `repeatDur`（秒，车道CJ）：活动总时长上限，与 repeatCount 推出的
    /// 总时长取小（SMIL Timing §repeatDur）。截断落在迭代中段时 freeze
    /// 值取截断点的局部相位（`repeat_dur mod dur`；恰为整轮倍数时取
    /// dur = 末值，与 Count(n) 用尽语义一致）。None = 未指定。
    pub repeat_dur: Option<f64>,
    pub fill: Fill,
}

impl Timing {
    /// freeze（活动期用尽）时 accumulate=sum 的等效迭代号（0 基，车道CJ
    /// 收敛点——builder 的 `frozen_iter` 委托此处）：冻结相位在整轮倍数
    /// （local==dur）→ total/dur − 1（Count(n) → n−1）；中段截断 →
    /// floor(total/dur)；无界活动期（永不冻结）→ 0。
    pub fn frozen_iter(&self) -> u32 {
        let Some(dur) = self.dur else {
            return 0;
        };
        if !dur.is_finite() || dur <= 0.0 {
            return 0;
        }
        let total = self.active_total();
        if !total.is_finite() {
            return 0;
        }
        let local = Self::repeat_dur_cut_local(total, dur);
        if local >= dur {
            (total / dur - 1.0).max(0.0) as u32
        } else {
            libm::floor(total / dur).max(0.0) as u32
        }
    }

    /// 生效的 repeatDur：None = 未指定/非法（负/NaN/+inf 视同未指定）。
    fn eff_repeat_dur(&self) -> Option<f64> {
        match self.repeat_dur {
            Some(d) if d.is_finite() && d >= 0.0 => Some(d),
            _ => None,
        }
    }

    /// repeatDur 截断点的本地采样秒（截断在迭代中段 → `rd mod dur`；
    /// 恰为整轮倍数 → dur（末值，与 Count(n) 用尽同语义）；rd=0 → 0）。
    /// 调用方保证 `dur` 有限且 > 0。
    fn repeat_dur_cut_local(rd: f64, dur: f64) -> f64 {
        if rd <= 0.0 {
            return 0.0;
        }
        let m = rd % dur;
        if m == 0.0 {
            dur
        } else {
            m
        }
    }
}

/// t 时刻的活动相位。
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ActiveState {
    /// 尚未 begin（含 t 为 NaN 的防御分支）。
    Inactive,
    /// 活动期内：`local_t` 是当前迭代的本地秒（`elapsed mod dur`），`iter` 从 0 计。
    Active { local_t: f64, iter: u32 },
    /// 活动期已结束且 fill=freeze。`final_t` 为冻结的本地采样秒
    /// （progress = final_t / dur）：正常用尽 = dur（末值），
    /// repeatCount=0 / dur<=0 = 0（首值）。
    /// 字段名 `final_t`——`final` 是 Rust 关键字。
    /// 注意：accumulate=Sum 的冻结值需配合 repeat_count 推算迭代数，M5 集成处理。
    Frozen { final_t: f64 },
}

impl Timing {
    /// 纯函数：文档时间 `t`（秒）→ 活动相位。
    ///
    /// 多 begin 实例求值（M6）：`begin_instances` 的每个 Offset 实例
    /// 开一个活动区间，按 [`Restart`] 取舍后交 [`Timing::interval_state_at`]
    /// 求值。单实例（`begin_instances` 为空/单元素）三种 restart 同归一
    /// 区间——与历史单 begin 行为逐字节等价。
    pub fn state_at(&self, t: f64) -> ActiveState {
        if t.is_nan() {
            return ActiveState::Inactive;
        }
        let insts: &[f64] = if self.begin_instances.is_empty() {
            core::slice::from_ref(&self.begin)
        } else {
            &self.begin_instances
        };
        let begin = match self.restart {
            // always：最新的已开始实例（升序倒序首个 ≤ t）
            Restart::Always => match insts.iter().rev().find(|&&b| b <= t) {
                Some(&b) => b,
                None => return ActiveState::Inactive,
            },
            // never：仅首个实例
            Restart::Never => match insts.first() {
                Some(&b) => b,
                None => return ActiveState::Inactive,
            },
            // whenNotActive：新实例只在活动期外生效——升序重建互不重叠的
            // 有效区间（开火时仍在前一区间内 → 该实例被忽略），取 t 前
            // 最后一个有效实例求值
            Restart::WhenNotActive => {
                let total = self.active_total();
                let mut cur: Option<f64> = None;
                let mut last_end = f64::NEG_INFINITY;
                for &b in insts {
                    if b > t {
                        break; // 升序：后续实例更晚，一并出局
                    }
                    if b >= last_end {
                        // 开火时元素不在活动期 → 新有效区间
                        //（indefinite → last_end=+inf，此后实例全被忽略）
                        cur = Some(b);
                        last_end = b + total;
                    }
                }
                match cur {
                    Some(b) => b,
                    None => return ActiveState::Inactive, // t 早于全部实例
                }
            }
        };
        self.interval_state_at(begin, t)
    }

    /// 单实例区间求值（begin 实例 `begin` 开出的活动区间在 t 的相位）。
    /// 历史单 begin `state_at` 主体逐字节保留。
    fn interval_state_at(&self, begin: f64, t: f64) -> ActiveState {
        let elapsed = t - begin;
        if elapsed < 0.0 {
            return ActiveState::Inactive;
        }
        let dur = match self.dur {
            Some(d) if d.is_finite() => d,
            // indefinite dur：永久 active，local_t 直增（repeatCount 无从
            // 用尽）；repeatDur 指定时活动期在 repeatDur 处截止（车道CJ）
            _ => {
                if let Some(rd) = self.eff_repeat_dur() {
                    if elapsed >= rd {
                        return ended(self.fill, rd);
                    }
                }
                return ActiveState::Active {
                    local_t: elapsed,
                    iter: 0,
                };
            }
        };
        if dur <= 0.0 {
            // 0/负 dur：活动时长为 0，begin 即结束
            return ended(self.fill, 0.0);
        }
        // 活动总时长（SMIL 3.0 §3.4：repeatDur 单独指定 = 简单时长循环
        // 填充至 repeatDur——dur=10s/repeatDur=25s → 2.5 轮；两者同给 =
        // 取短者；车道CJ）
        let total = match (self.repeat_count, self.eff_repeat_dur()) {
            (Some(RepeatCount::Count(n)), Some(rd)) => (dur * (n as f64)).min(rd),
            (Some(RepeatCount::Count(n)), None) => dur * (n as f64),
            (Some(RepeatCount::Indefinite), Some(rd)) => rd,
            (Some(RepeatCount::Indefinite), None) => f64::INFINITY,
            (None, Some(rd)) => rd,
            (None, None) => dur,
        };
        if elapsed >= total {
            // 冻结相位 = 活动期终点的本地秒：整轮倍数 → dur（末值，与
            // Count(n) 用尽同语义）；中段截断 → 截断点局部相位；total=0 → 0
            let final_t = match self.repeat_count {
                Some(RepeatCount::Count(0)) => 0.0,
                _ => Self::repeat_dur_cut_local(total, dur),
            };
            return ended(self.fill, final_t);
        }
        let iter_f = libm::floor(elapsed / dur);
        let iter = if iter_f > u32::MAX as f64 {
            u32::MAX
        } else {
            iter_f as u32
        };
        ActiveState::Active {
            local_t: elapsed - iter as f64 * dur,
            iter,
        }
    }

    /// 单实例活动总时长（whenNotActive 区间重建用）——与
    /// `interval_state_at` 的 total 同式（SMIL §3.4，车道CJ）：
    /// Count(n) → dur*n；Indefinite repeat → +inf；`None`/非有限 dur →
    /// +inf；repeatDur 与结果取小；repeatDur 单独指定（无 repeatCount）
    /// = 活动期即 repeatDur；dur ≤ 0 → 0。
    fn active_total(&self) -> f64 {
        match self.dur {
            Some(d) if d.is_finite() && d > 0.0 => {
                match (self.repeat_count, self.eff_repeat_dur()) {
                    (Some(RepeatCount::Count(n)), Some(rd)) => (d * (n as f64)).min(rd),
                    (Some(RepeatCount::Count(n)), None) => d * (n as f64),
                    (Some(RepeatCount::Indefinite), Some(rd)) => rd,
                    (Some(RepeatCount::Indefinite), None) => f64::INFINITY,
                    (None, Some(rd)) => rd,
                    (None, None) => d,
                }
            }
            Some(d) if d.is_finite() => 0.0, // dur ≤ 0
            // dur 缺省/indefinite：repeatDur 界定活动期
            _ => self.eff_repeat_dur().unwrap_or(f64::INFINITY),
        }
    }
}

fn ended(fill: Fill, final_t: f64) -> ActiveState {
    match fill {
        Fill::Freeze => ActiveState::Frozen { final_t },
        Fill::Remove => ActiveState::Inactive,
    }
}

// ---------------------------------------------------------------------------
// 4. calcMode 插值器
// ---------------------------------------------------------------------------

/// calcMode 属性值。`Paced` 本车道按 `Linear` 近似（真 paced 需路径弧长表）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CalcMode {
    Linear,
    Discrete,
    Paced,
    Spline,
}

/// keySplines 单段控制点 (x1 y1 x2 y2)；SMIL 约束 x ∈ [0,1]（y 可越界过冲）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct KeySpline {
    pub x1: f64,
    pub y1: f64,
    pub x2: f64,
    pub y2: f64,
}

/// calcMode 插值器：progress ∈ [0,1] → values 上的插值。
///
/// - keyTimes 缺省 → 均匀分布；提供时必须与 values 等长、首 0、
///   （linear/spline/paced 下）末 1、单调不降，否则 `None`（SMIL: 文档错误；
///   discrete 末值允许 < 1——末值保持到结束）
/// - **Discrete**：第 i 段返回 values[i]（缺省 keyTimes 时 i = floor(p·n)）
/// - **Linear / Paced**：分段线性（Paced 为本车道的线性近似，见模块 doc）
/// - **Spline**：每段用 keySplines[i] 三次贝塞尔缓动 y(x)；
///   splines 缺失/长度 ≠ 段数/x 越界 → 整体回退线性（Chrome 对无效
///   keySplines 的近似行为）
/// - values 空 → None；单值 → 恒为该值；p 越界夹到 [0,1]，NaN → None
pub fn sample(
    values: &[f64],
    key_times: Option<&[f64]>,
    mode: CalcMode,
    key_splines: Option<&[KeySpline]>,
    progress: f64,
) -> Option<f64> {
    if values.is_empty() || progress.is_nan() {
        return None;
    }
    let v0 = *values.first()?;
    if values.len() == 1 {
        return Some(v0);
    }
    let p = progress.clamp(0.0, 1.0);
    let kt = match key_times {
        Some(kt) => {
            if !validate_key_times(kt, values.len(), mode) {
                return None;
            }
            Some(kt)
        }
        None => None,
    };
    if mode == CalcMode::Discrete {
        return Some(discrete_value(values, kt, p));
    }
    let splines = if mode == CalcMode::Spline {
        match key_splines {
            Some(sp) if sp.len() == values.len() - 1 && sp.iter().all(valid_spline) => Some(sp),
            _ => None, // 回退线性
        }
    } else {
        None
    };
    let (i, local) = locate_segment(values.len(), kt, p);
    let e = match splines {
        Some(sp) => spline_eval(&sp[i], local),
        None => local,
    };
    Some(values[i] + (values[i + 1] - values[i]) * e)
}

/// keyTimes 结构校验：等长、首 0、末 ≤ 1（非 discrete 还须 == 1）、单调不降。
fn validate_key_times(kt: &[f64], n_values: usize, mode: CalcMode) -> bool {
    if kt.len() != n_values {
        return false;
    }
    let first = match kt.first() {
        Some(v) => *v,
        None => return false,
    };
    if first.abs() > 1e-9 {
        return false;
    }
    let last = match kt.last() {
        Some(v) => *v,
        None => return false,
    };
    if (mode != CalcMode::Discrete && (last - 1.0).abs() > 1e-9) || last > 1.0 + 1e-9 {
        return false;
    }
    kt.windows(2).all(|w| w[0] <= w[1]) // NaN 在此一并拒绝
}

/// 定位 p 落在第 i 段，返回 (i, 段内局部进度)。n ≥ 2。
fn locate_segment(n: usize, kt: Option<&[f64]>, p: f64) -> (usize, f64) {
    let last = n - 1;
    match kt {
        Some(kt) => {
            let mut i = last - 1;
            while i > 0 && kt[i] > p {
                i -= 1; // kt[0]=0 ≤ p，循环必停在 ≥ 0
            }
            let denom = kt[i + 1] - kt[i];
            let local = if denom > 0.0 {
                ((p - kt[i]) / denom).clamp(0.0, 1.0)
            } else {
                1.0 // 零长段：瞬间跳到段末值
            };
            (i, local)
        }
        None => {
            let seg = last as f64;
            let i = (libm::floor(p * seg) as usize).min(last - 1);
            (i, p * seg - i as f64)
        }
    }
}

/// Discrete 取值：keyTimes 时取最大 i 使 kt[i] ≤ p；缺省时 i = floor(p·n)。
fn discrete_value(values: &[f64], kt: Option<&[f64]>, p: f64) -> f64 {
    let n = values.len();
    let idx = match kt {
        Some(kt) => kt.iter().rposition(|&k| k <= p).unwrap_or(0),
        None => (libm::floor(p * n as f64) as usize).min(n - 1),
    };
    values[idx]
}

/// keySpline 有效性：分量有限且 x1/x2 ∈ [0,1]（SMIL 约束，保证 x(u) 单调可逆）。
fn valid_spline(sp: &KeySpline) -> bool {
    sp.x1.is_finite()
        && sp.y1.is_finite()
        && sp.x2.is_finite()
        && sp.y2.is_finite()
        && sp.x1 >= 0.0
        && sp.x1 <= 1.0
        && sp.x2 >= 0.0
        && sp.x2 <= 1.0
}

/// 三次贝塞尔 y(x)：P0=(0,0)、P3=(1,1)。
///
/// x1/x2 ∈ [0,1] 保证 x(u) 单调 → 固定 24 轮二分求逆：
/// |Δu| ≤ 2⁻²⁴ ≈ 6e-8，|Δy| ≤ 3·max(1,|y1|,|y2|)·6e-8，常规样条 < 2e-7，
/// 远小于 1e-4 精度门限。
fn spline_eval(sp: &KeySpline, x: f64) -> f64 {
    let (mut lo, mut hi) = (0.0f64, 1.0f64);
    for _ in 0..24 {
        let mid = 0.5 * (lo + hi);
        if bez(sp, mid, false) < x {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    bez(sp, 0.5 * (lo + hi), true)
}

/// 贝塞尔分量：is_y=false → x(u)，否则 y(u)。
fn bez(sp: &KeySpline, u: f64, is_y: bool) -> f64 {
    let (c1, c2) = if is_y { (sp.y1, sp.y2) } else { (sp.x1, sp.x2) };
    let inv = 1.0 - u;
    3.0 * inv * inv * u * c1 + 3.0 * inv * u * u * c2 + u * u * u
}

/// CSS keyframes 轨迹采样（timing function 版，车道BN）：p 定位关键帧段
/// （offsets 与 values 等长、首 0 末 1 单调——同 `sample` 的 keyTimes
/// 校验），段内局部进度过 timing 样条后线性插值。无 keySplines Vec
/// 分配（SMIL `sample` 的 Spline 路径要按段构造切片，此处直接单样条
/// 求值——CSS 简写 timing 恒全程同一曲线）。
pub fn sample_kf_timed(values: &[f64], offsets: &[f64], timing: CssTiming, p: f64) -> Option<f64> {
    if values.is_empty() || p.is_nan() || offsets.len() != values.len() {
        return None;
    }
    let v0 = *values.first()?;
    if values.len() == 1 {
        return Some(v0);
    }
    if !validate_key_times(offsets, values.len(), CalcMode::Spline) {
        return None;
    }
    let p = p.clamp(0.0, 1.0);
    let (i, local) = locate_segment(values.len(), Some(offsets), p);
    let e = match timing.spline() {
        Some(sp) => spline_eval(&sp, local),
        None => local,
    };
    Some(values[i] + (values[i + 1] - values[i]) * e)
}

// ---------------------------------------------------------------------------
// 5. attributeName / additive / accumulate / 杂项解析
// ---------------------------------------------------------------------------

/// additive 属性：Replace（默认，覆盖基底）/ Sum（叠加到基底）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Additive {
    Replace,
    Sum,
}

/// accumulate 属性：None（默认，迭代间不复利）/ Sum（每迭代叠加上一轮末值）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Accumulate {
    None,
    Sum,
}

/// additive 解析（SMIL 枚举值大小写敏感，白名单小写）。
pub fn parse_additive(s: &str) -> Option<Additive> {
    match s.trim() {
        "replace" => Some(Additive::Replace),
        "sum" => Some(Additive::Sum),
        _ => None,
    }
}

/// accumulate 解析。
pub fn parse_accumulate(s: &str) -> Option<Accumulate> {
    match s.trim() {
        "none" => Some(Accumulate::None),
        "sum" => Some(Accumulate::Sum),
        _ => None,
    }
}

/// fill 解析。
pub fn parse_fill(s: &str) -> Option<Fill> {
    match s.trim() {
        "remove" => Some(Fill::Remove),
        "freeze" => Some(Fill::Freeze),
        _ => None,
    }
}

/// calcMode 解析（SMIL 默认 linear，缺省/非法由 M5 集成层兜底）。
pub fn parse_calc_mode(s: &str) -> Option<CalcMode> {
    match s.trim() {
        "linear" => Some(CalcMode::Linear),
        "discrete" => Some(CalcMode::Discrete),
        "paced" => Some(CalcMode::Paced),
        "spline" => Some(CalcMode::Spline),
        _ => None,
    }
}

/// repeatCount 解析：非负整数或 indefinite。
pub fn parse_repeat_count(s: &str) -> Option<RepeatCount> {
    let s = s.trim();
    if s == "indefinite" {
        return Some(RepeatCount::Indefinite);
    }
    s.parse::<u32>().ok().map(RepeatCount::Count)
}

/// attributeName：非空即收。命名空间前缀（`xlink:href`）原样保留，
/// 前缀 → URI 的解析在 M5 集成（需要文档 ns 上下文）。
pub fn parse_attribute_name(s: &str) -> Option<String> {
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

/// keyTimes 属性：分号分隔浮点列表（结构解析；语义校验在 [`sample`]）。
pub fn parse_key_times(s: &str) -> Option<Vec<f64>> {
    s.split(';').map(|p| p.trim().parse::<f64>().ok()).collect()
}

/// keySplines 属性：分号分隔、每段 4 个数 (x1 y1 x2 y2)。
/// 任一段畸形或 x ∉ [0,1] → 整体 None（SMIL：属性整体忽略）。
pub fn parse_key_splines(s: &str) -> Option<Vec<KeySpline>> {
    let mut out = Vec::new();
    for seg in s.split(';') {
        let n: Vec<f64> = seg
            .split_whitespace()
            .map(|t| t.parse::<f64>().ok())
            .collect::<Option<Vec<_>>>()?;
        if n.len() != 4 {
            return None;
        }
        let sp = KeySpline {
            x1: n[0],
            y1: n[1],
            x2: n[2],
            y2: n[3],
        };
        if !valid_spline(&sp) {
            return None;
        }
        out.push(sp);
    }
    if out.is_empty() {
        return None;
    }
    Some(out)
}

// ---------------------------------------------------------------------------
// 6. 颜色插值
// ---------------------------------------------------------------------------

/// 直链 RGBA（动画插值用；与 style::Color 解耦，M5 集成时做转换）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

/// sRGB 域逐通道线性插值（与 Chrome 的 SMIL 颜色插值一致：
/// 不做 gamma 线性化、不预乘 alpha），四舍五入到 u8。
/// t 夹 [0,1]，NaN 按 0。
pub fn lerp_rgba(a: Rgba, b: Rgba, t: f64) -> Rgba {
    let t = if t.is_nan() { 0.0 } else { t.clamp(0.0, 1.0) };
    Rgba {
        r: lerp_u8(a.r, b.r, t),
        g: lerp_u8(a.g, b.g, t),
        b: lerp_u8(a.b, b.b, t),
        a: lerp_u8(a.a, b.a, t),
    }
}

fn lerp_u8(x: u8, y: u8, t: f64) -> u8 {
    let v = f64::from(x) + (f64::from(y) - f64::from(x)) * t;
    (v + 0.5).clamp(0.0, 255.0) as u8
}

// ---------------------------------------------------------------------------
// 7. CSS keyframes（M5b：@keyframes 解析 + animation 简写 + 迭代求值）
// ---------------------------------------------------------------------------
//
// 已知简化（见 docs/CONFORMANCE.md）：
// - timing function 关键词子集已落地（车道BN：linear/ease/ease-in/
//   ease-out/ease-in-out，映射 SMIL Spline 逐帧段求值）；step-* 与
//   cubic-bezier() 仍忽略（回落 Linear/初始值）；帧内
//   `animation-timing-function` 声明不识别（登记近似）；
// - `animation` 长属性（animation-name/...）与逗号分隔多动画只取首个；
// - `animation-play-state: paused` 不支持（解析期丢弃）。

/// 一帧关键帧：offset ∈ [0,1] + 声明块（属性名已小写化，同帧同名属性
/// 后者胜——采样侧取最后一个）。`0%`/`from` → 0.0，`100%`/`to` → 1.0。
#[derive(Clone, Debug, PartialEq)]
pub struct Keyframe {
    pub offset: f64,
    pub decls: Vec<(String, String)>,
}

/// `<style>` 文本中的全部 `@keyframes` 块（文档序）。
/// 每块 = (动画名, 帧列表按 offset 升序；同 offset 帧按出现序合并)。
pub fn parse_keyframes(text: &str) -> Vec<(String, Vec<Keyframe>)> {
    let cleaned = strip_css_comments(text);
    let mut out: Vec<(String, Vec<Keyframe>)> = Vec::new();
    let mut rest = cleaned.as_str();
    while let Some(at) = find_at_keyframes(rest) {
        rest = &rest[at + "@keyframes".len()..];
        // 动画名：到 '{' 为止（trim；CSS 标识符不允许 '{'）
        let Some(brace) = rest.find('{') else { break };
        let name = rest[..brace].trim();
        if name.is_empty() {
            rest = skip_rule_block(&rest[brace..]);
            continue;
        }
        let body_end = match matching_brace(&rest[brace..]) {
            Some(e) => brace + e,
            None => rest.len(), // 未闭合：尽力取到结尾
        };
        let body = &rest[brace + 1..body_end];
        out.push((String::from(name), parse_kf_body(body)));
        rest = if body_end < rest.len() {
            &rest[body_end + 1..]
        } else {
            ""
        };
    }
    out
}

/// 从 `<style>` 文本中剥除全部 `@keyframes` 块（注释一并剥除）。
/// 用途：剩余规则交给 css::parse 前的净化——@keyframes 的嵌套花括号会
/// 让朴素按 `}` 分块的规则解析器把**紧随其后的那条规则的 selector**
/// 切坏而整条丢弃（s12-05 的 .draw 即中招）。
pub fn strip_keyframes(text: &str) -> String {
    let cleaned = strip_css_comments(text);
    let mut out = String::with_capacity(cleaned.len());
    let mut rest = cleaned.as_str();
    while let Some(at) = find_at_keyframes(rest) {
        out.push_str(&rest[..at]);
        let after = &rest[at + "@keyframes".len()..];
        // 块从首个 '{' 开始（跳过名字），配对 '}' 结束
        rest = match after.find('{') {
            Some(brace) => skip_rule_block(&after[brace..]),
            None => "",
        };
    }
    out.push_str(rest);
    out
}

/// 关键帧块体：`selector { decls }` 重复。selector 为逗号分隔的
/// `NN%` / `from` / `to` 列表；帧内声明复用 css::parse_decls。
fn parse_kf_body(body: &str) -> Vec<Keyframe> {
    let mut frames: Vec<Keyframe> = Vec::new();
    let mut rest = body;
    while let Some(brace) = rest.find('{') {
        let end = match matching_brace(&rest[brace..]) {
            Some(e) => brace + e,
            None => rest.len(),
        };
        let decls = css::parse_decls(&rest[brace + 1..end]);
        for sel in rest[..brace].split(',') {
            let Some(offset) = parse_kf_offset(sel) else {
                continue;
            };
            match frames.iter_mut().find(|f| (f.offset - offset).abs() < 1e-9) {
                Some(f) => f.decls.extend(decls.iter().cloned()), // 同 offset 合并
                None => frames.push(Keyframe {
                    offset,
                    decls: decls.clone(),
                }),
            }
        }
        rest = if end < rest.len() {
            &rest[end + 1..]
        } else {
            ""
        };
    }
    frames.sort_by(|a, b| {
        a.offset
            .partial_cmp(&b.offset)
            .unwrap_or(core::cmp::Ordering::Equal)
    });
    frames
}

/// 帧选择器：`from`/`to`/`50%`/`12.5%` → [0,1]。其余（畸形）→ None。
fn parse_kf_offset(sel: &str) -> Option<f64> {
    let t = sel.trim();
    if t.eq_ignore_ascii_case("from") {
        return Some(0.0);
    }
    if t.eq_ignore_ascii_case("to") {
        return Some(1.0);
    }
    let pct = t.strip_suffix('%')?.trim();
    let v = parse_unsigned_decimal(pct)?;
    if v.is_finite() {
        Some(v / 100.0)
    } else {
        None
    }
}

/// `@keyframes` 大小写不敏感定位（含 '@' 前的空白容忍由调用方截断保证）。
fn find_at_keyframes(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let pat = b"@keyframes";
    let mut i = 0usize;
    while i + pat.len() <= bytes.len() {
        if bytes[i..i + pat.len()].eq_ignore_ascii_case(pat) {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// 从 `s`（以 '{' 开头）跳过整个平衡花括号块，返回其后余部。
fn skip_rule_block(s: &str) -> &str {
    match matching_brace(s) {
        Some(e) if e + 1 < s.len() => &s[e + 1..],
        _ => "",
    }
}

/// `s[0] == '{'` 时找到配对的 '}'（嵌套 +1；文本声明内无花括号），
/// 返回 '}' 的下标；未闭合 → None。
fn matching_brace(s: &str) -> Option<usize> {
    let mut depth = 0i32;
    for (i, b) in s.bytes().enumerate() {
        match b {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// 剥 `/* ... */`（与 css::strip_comments 同款语义；未闭合取到结尾）。
fn strip_css_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("/*") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        match after.find("*/") {
            Some(end) => rest = &after[end + 2..],
            None => rest = "",
        }
    }
    out.push_str(rest);
    out
}

/// animation 方向。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CssDirection {
    Normal,
    Reverse,
    Alternate,
    AlternateReverse,
}

/// animation timing function 关键词子集（车道BN）。`linear` 无缓动；
/// 其余四个是 CSS 规范的固定 cubic-bezier 预设。`steps()`/帧内
/// `animation-timing-function` 声明仍不识别（前者回落 Linear、后者被
/// 忽略——登记近似）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum CssTiming {
    Linear,
    /// CSS 初始值（简写缺省 timing 即此）。
    #[default]
    Ease,
    EaseIn,
    EaseOut,
    EaseInOut,
}

impl CssTiming {
    /// timing 关键词 → 枚举（大小写不敏感；`ease` 是 `ease-in` 等的
    /// 前缀，匹配须全词等号）。
    pub fn parse(tok: &str) -> Option<CssTiming> {
        Some(match tok.to_ascii_lowercase().as_str() {
            "linear" => CssTiming::Linear,
            "ease" => CssTiming::Ease,
            "ease-in" => CssTiming::EaseIn,
            "ease-out" => CssTiming::EaseOut,
            "ease-in-out" => CssTiming::EaseInOut,
            _ => return None,
        })
    }

    /// 对应的 cubic-bezier 控制点（CSS timing function 预设表）；
    /// `None` = linear（无缓动）。
    pub fn spline(self) -> Option<KeySpline> {
        const fn sp(x1: f64, y1: f64, x2: f64, y2: f64) -> KeySpline {
            KeySpline { x1, y1, x2, y2 }
        }
        match self {
            CssTiming::Linear => None,
            CssTiming::Ease => Some(sp(0.25, 0.1, 0.25, 1.0)),
            CssTiming::EaseIn => Some(sp(0.42, 0.0, 1.0, 1.0)),
            CssTiming::EaseOut => Some(sp(0.0, 0.0, 0.58, 1.0)),
            CssTiming::EaseInOut => Some(sp(0.42, 0.0, 0.58, 1.0)),
        }
    }
}

/// `animation` 简写解析产物。已知简化：play-state / step-* 忽略；
/// 逗号多动画取首段（解析期其余计告警由 builder 负责）。
#[derive(Clone, Debug, PartialEq)]
pub struct CssAnimationSpec {
    /// keyframes 名（与 @keyframes 块名精确匹配，大小写敏感）。
    pub name: String,
    /// 单次迭代时长秒（CSS 不可为负；0 = 无效动画由调用方丢弃）。
    pub duration: f64,
    /// 延迟秒（可为负——负延迟直接跳入迭代中段，本模型支持）。
    pub delay: f64,
    /// None = infinite。
    pub iteration_count: Option<u32>,
    pub direction: CssDirection,
    /// fill=forwards/both → 结束后保持末态。
    pub fill_forwards: bool,
    /// timing function（车道BN）：作用于相邻关键帧之间（CSS 模型——
    /// 不是整迭代一次缓动），采样层映射到 SMIL Spline 路径逐段求值。
    pub timing: CssTiming,
}

/// `animation` 简写解析：时间值第 1 个 = duration、第 2 个 = delay；
/// 裸数字 = 迭代数；`infinite` = 无限；方向/fill/状态关键字归类；
/// 其余非关键字 token = 动画名（首个胜出）。`none` / 无名 / 逗号 → None
/// （逗号列表只取首段：其余段丢弃，调用方按需计告警）。
pub fn parse_animation_shorthand(value: &str) -> Option<CssAnimationSpec> {
    let first = value.split(',').next().unwrap_or("");
    let mut name: Option<String> = None;
    let mut times: Vec<f64> = Vec::new();
    // 车道IE：CSS animation-iteration-count 初始值 = **1**（非无限）——
    // 旧缺省 None 被采样端当无限循环，`animation: fade 1s forwards` 这类
    // 省略计数的常见形态在 t>dur 后回卷重播（Chrome 钉扎探针 ie4 实证
    // held 终态 vs 我方 t mod dur 帧）。显式 `infinite` 才置 None。
    let mut iteration: Option<u32> = Some(1);
    let mut infinite = false;
    let mut direction = CssDirection::Normal;
    let mut fill_forwards = false;
    let mut timing = CssTiming::Ease; // CSS animation-timing-function 初始值
    for tok in first.split_whitespace() {
        // 时间值：`[-]N[.M](s|ms)`
        if let Some(t) = parse_css_time(tok) {
            if times.len() < 2 {
                times.push(t);
            }
            continue;
        }
        if let Some(t) = CssTiming::parse(tok) {
            timing = t;
            continue;
        }
        match tok.to_ascii_lowercase().as_str() {
            "none" => return None,
            "infinite" => infinite = true,
            "normal" => direction = CssDirection::Normal,
            "reverse" => direction = CssDirection::Reverse,
            "alternate" => direction = CssDirection::Alternate,
            "alternate-reverse" => direction = CssDirection::AlternateReverse,
            "forwards" | "both" => fill_forwards = true,
            "backwards" | "running" | "paused" | "step-start" | "step-end" => {}
            _ => {
                // 裸数字 = 迭代数（CSS 允许小数；本模型取整下限）
                if let Some(v) = parse_unsigned_decimal(tok) {
                    let n = libm::floor(v);
                    if n.is_finite() && n >= 0.0 && n <= u32::MAX as f64 {
                        iteration = Some(n as u32);
                    }
                    continue;
                }
                if name.is_none() {
                    name = Some(String::from(tok));
                }
            }
        }
    }
    let name = name?;
    if infinite {
        iteration = None;
    }
    Some(CssAnimationSpec {
        name,
        duration: times.first().copied().unwrap_or(0.0),
        delay: times.get(1).copied().unwrap_or(0.0),
        iteration_count: iteration,
        direction,
        fill_forwards,
        timing,
    })
}

/// CSS 时间 token：数字 + s/ms 后缀（大小写不敏感；负值合法）。
fn parse_css_time(tok: &str) -> Option<f64> {
    let mult = if tok.ends_with("ms") {
        0.001
    } else if tok.ends_with('s') || tok.ends_with('S') {
        1.0
    } else {
        return None;
    };
    let num = tok[..tok.len() - if tok.ends_with("ms") { 2 } else { 1 }].trim();
    let neg = num.starts_with('-');
    let v = parse_unsigned_decimal(num.trim_start_matches(['-', '+']))?;
    Some(if neg { -v * mult } else { v * mult })
}

/// CSS 动画的迭代求值结果。
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CssAnimPhase {
    /// 尚处 delay 之前（未开始，无覆盖）。
    Before,
    /// 活动期：本迭代进度 [0,1]（已按方向翻转）。
    Active { progress: f64 },
    /// 迭代用尽且 fill=forwards/both：保持的末态进度。
    Held { progress: f64 },
    /// 迭代用尽且无 fill：无覆盖。
    Ended,
}

/// 文档时间 t（秒）→ CSS 动画相位（纯函数；duration ≤ 0 → Ended 防御）。
pub fn css_anim_phase(spec: &CssAnimationSpec, t: f64) -> CssAnimPhase {
    if t.is_nan() || spec.duration <= 0.0 {
        return CssAnimPhase::Ended;
    }
    if t < spec.delay {
        return CssAnimPhase::Before;
    }
    let iter_f = (t - spec.delay) / spec.duration;
    let iter = libm::floor(iter_f);
    let frac = iter_f - iter;
    let reversed = match spec.direction {
        CssDirection::Normal => false,
        CssDirection::Reverse => true,
        CssDirection::Alternate => (iter as i64) % 2 != 0,
        CssDirection::AlternateReverse => (iter as i64) % 2 == 0,
    };
    let progress = if reversed { 1.0 - frac } else { frac };
    match spec.iteration_count {
        Some(n) if iter_f >= n as f64 => {
            // 末迭代（第 n−1 轮）的方向决定 held 值
            let last_reversed = match spec.direction {
                CssDirection::Normal => false,
                CssDirection::Reverse => true,
                CssDirection::Alternate => (n as i64 - 1) % 2 != 0,
                CssDirection::AlternateReverse => (n as i64 - 1) % 2 == 0,
            };
            if spec.fill_forwards {
                CssAnimPhase::Held {
                    progress: if last_reversed { 0.0 } else { 1.0 },
                }
            } else {
                CssAnimPhase::Ended
            }
        }
        _ => CssAnimPhase::Active {
            progress: progress.clamp(0.0, 1.0),
        },
    }
}

/// CSS 关键帧 transform 的可插值子集（SVG 语境 px == 用户单位）。
/// `translateX(-90px)` / `translateY(5px)` / `translate(10px, 20px)` /
/// `rotate(360deg)`；`rotate` 角度制。多函数串联 / 不识别 → None
/// （调用方按离散帧处理或跳过）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CssXf {
    Translate(f64, f64),
    Rotate(f64),
}

/// 解析单个 CSS transform 函数（首尾空白容忍；单位 px/deg，纯数按
/// CSS 语法在 translate 分量上不合法、rotate 上按 deg 宽容处理；
/// 无单位零值合法——CSS `<length>`/`<angle>` 允许零免单位，Chrome
/// 接受 `translateX(0)`，s6-05 slide 关键帧即此形态）。
pub fn parse_css_xf(v: &str) -> Option<CssXf> {
    let t = v.trim();
    let (fname, arg) = t.split_once('(')?;
    let arg = arg.strip_suffix(')')?.trim();
    let nums: Vec<f64> = arg
        .split(|c: char| c.is_ascii_whitespace() || c == ',')
        .filter(|s| !s.is_empty())
        .map(strip_css_unit)
        .collect::<Option<Vec<_>>>()?;
    match fname.trim() {
        "translateX" if nums.len() == 1 => Some(CssXf::Translate(nums[0], 0.0)),
        "translateY" if nums.len() == 1 => Some(CssXf::Translate(0.0, nums[0])),
        "translate" if nums.len() == 1 => Some(CssXf::Translate(nums[0], 0.0)),
        "translate" if nums.len() == 2 => Some(CssXf::Translate(nums[0], nums[1])),
        "rotate" if nums.len() == 1 => Some(CssXf::Rotate(nums[0])),
        _ => None,
    }
}

/// `Numpx` / `Numdeg` / `Numpt` 等绝对单位 → f64（车道NF：追加绝对
/// 长度单位 96px 基准换算——CSS keyframes transform 的 translate 分量
/// 接受全 CSS 长度，Chrome 同口径；deg 系数 1。rad/grad/turn、em 等
/// 相对单位登记不支持 → None。分量必须带单位；无单位在 CSS transform
/// 函数参数中非法 → None；例外：无单位零合法——`translateX(0)` /
/// `rotate(0)`，CSS 零值免单位）。
fn strip_css_unit(tok: &str) -> Option<f64> {
    let t = tok.trim();
    if parse_unsigned_decimal(t) == Some(0.0) {
        return Some(0.0);
    }
    for suffix in ["px", "deg"] {
        if let Some(num) = t.strip_suffix(suffix) {
            let neg = num.starts_with('-');
            return parse_unsigned_decimal(num.trim_start_matches(['-', '+'])).map(|v| {
                if neg {
                    -v
                } else {
                    v
                }
            });
        }
    }
    // 车道NF：其余绝对单位（pt/pc/in/mm/cm/q），96px 基准换算为 px。
    // 系数来自 fnum（尾缀按字节判多字节安全）；keyframes 插值 f64，
    // 换算量化差在亚像素以下。exponent 形态（1e2px）不支持——同
    // parse_unsigned_decimal 的 SMIL 语法域。
    if let Some((num, k)) = crate::fnum::parse_with_abs_unit(t) {
        let neg = num.starts_with('-');
        return crate::fnum::parse_f32(num.trim_start_matches(['-', '+']))
            .filter(|v| v.is_finite())
            .map(|v| {
                let px = f64::from(v) * f64::from(k);
                if neg {
                    -px
                } else {
                    px
                }
            });
    }
    None
}

/// 同变体插值；异变体 → None（调用方按离散处理）。
pub fn lerp_css_xf(a: CssXf, b: CssXf, t: f64) -> Option<CssXf> {
    let t = t.clamp(0.0, 1.0);
    match (a, b) {
        (CssXf::Translate(ax, ay), CssXf::Translate(bx, by)) => {
            Some(CssXf::Translate(ax + (bx - ax) * t, ay + (by - ay) * t))
        }
        (CssXf::Rotate(a), CssXf::Rotate(b)) => Some(CssXf::Rotate(a + (b - a) * t)),
        _ => None,
    }
}

/// 抽取某属性在各帧的 (offset, 值原文) 轨迹（同帧同名后者胜）。
/// 返回按 offset 升序（帧列表本身有序，逐帧追加即有序）。
pub fn kf_prop_track<'a>(frames: &'a [Keyframe], prop: &str) -> Vec<(f64, &'a str)> {
    let mut out = Vec::new();
    for f in frames {
        // 同帧内取最后一个同名声明
        if let Some((_, v)) = f.decls.iter().rev().find(|(n, _)| n == prop) {
            out.push((f.offset, v.as_str()));
        }
    }
    out
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    // ---- Timeline（既有骨架回归）----

    #[test]
    fn timeline_advances_by_host() {
        let mut t = Timeline::default();
        assert_eq!(t.t_ms, 0.0);
        t.advance(16.0);
        t.advance(16.0);
        assert_eq!(t.t_ms, 32.0);
    }

    // ---- 1. 时钟值解析 ----

    #[test]
    fn clock_parses_units_and_sign() {
        assert_eq!(parse_clock("2s"), Some(2.0));
        assert_eq!(parse_clock("0.5"), Some(0.5));
        assert_eq!(parse_clock("-3s"), Some(-3.0));
        assert_eq!(parse_clock("+1s"), Some(1.0));
        assert_eq!(parse_clock("3min"), Some(180.0));
        assert_eq!(parse_clock("2h"), Some(7200.0));
        assert_eq!(parse_clock("1500ms"), Some(1.5));
        assert_eq!(parse_clock("  12.5s  "), Some(12.5)); // 整体空白容忍
        let ms = parse_clock("500ms").unwrap_or(f64::NAN);
        assert!((ms - 0.5).abs() < 1e-12);
    }

    #[test]
    fn clock_parses_colon_forms() {
        assert_eq!(parse_clock("12:34"), Some(754.0)); // mm:ss
        assert_eq!(parse_clock("2:30.5"), Some(150.5)); // mm:ss.f
        assert_eq!(parse_clock("1:02:03"), Some(3723.0)); // h:mm:ss
        assert_eq!(parse_clock("0:00:00.25"), Some(0.25));
        assert_eq!(parse_clock("00:00:10"), Some(10.0));
    }

    #[test]
    fn clock_rejects_garbage() {
        for bad in [
            "",
            " ",
            "s",
            "1x",
            "1 s",
            "1:2:3:4",
            "1:",
            ":30",
            "1e5",
            "1.2.3",
            "12:60",
            "1:60:00",
            "-",
            "--3s",
            "1.",
            "indefinite",
            "media",
        ] {
            assert_eq!(parse_clock(bad), None, "should reject {bad:?}");
        }
    }

    // ---- 2. begin/end 列表 ----

    #[test]
    fn begin_list_parses_mixed_forms() {
        let specs = parse_begin_list(
            "0s; click; rect01.click; anim1.begin+0.5s; anim2.repeat(2); anim3.end; indefinite",
        );
        assert_eq!(specs.len(), 7);
        assert_eq!(specs[0], BeginSpec::Offset(0.0));
        assert_eq!(
            specs[1],
            BeginSpec::Event {
                target: None,
                event: "click".to_string(),
                offset: 0.0,
            }
        );
        assert_eq!(
            specs[2],
            BeginSpec::Event {
                target: Some("rect01".to_string()),
                event: "click".to_string(),
                offset: 0.0,
            }
        );
        assert_eq!(
            specs[3],
            BeginSpec::Sync {
                target: "anim1".to_string(),
                basis: Basis::Begin,
                offset: 0.5,
            }
        );
        assert_eq!(
            specs[4],
            BeginSpec::Sync {
                target: "anim2".to_string(),
                basis: Basis::Repeat(2),
                offset: 0.0,
            }
        );
        assert_eq!(
            specs[5],
            BeginSpec::Sync {
                target: "anim3".to_string(),
                basis: Basis::End,
                offset: 0.0,
            }
        );
        assert_eq!(specs[6], BeginSpec::Indefinite);
    }

    #[test]
    fn begin_list_skips_unsupported_items() {
        // media / 残缺偏移（nonsense+bad）/ wallclock / 空项全部跳过，
        // 只留合法项（车道HK：rect01.click+2s 由「拒绝」升级为「收下」）
        let specs = parse_begin_list("media; nonsense+bad; ; wallclock(x); click+; click+x; 0.5s");
        assert_eq!(specs, vec![BeginSpec::Offset(0.5)]);
        // 大小写不敏感的 indefinite
        assert_eq!(parse_begin_list("Indefinite"), vec![BeginSpec::Indefinite]);
    }

    #[test]
    fn eventbase_offset_forms_parse() {
        // 车道HK：事件基 ±offset 全形态（SMIL event-value）
        assert_eq!(
            parse_begin_item("btn.click+2s"),
            Some(BeginSpec::Event {
                target: Some("btn".to_string()),
                event: "click".to_string(),
                offset: 2.0,
            })
        );
        assert_eq!(
            parse_begin_item("click-0.5s"),
            Some(BeginSpec::Event {
                target: None,
                event: "click".to_string(),
                offset: -0.5,
            })
        );
        // 偏移走完整 clock-value 语法（含冒号钟/单位大小写不敏感）
        assert_eq!(
            parse_begin_item("b.mouseover+0:01.5"),
            Some(BeginSpec::Event {
                target: Some("b".to_string()),
                event: "mouseover".to_string(),
                offset: 1.5,
            })
        );
        // 事件名内部不受 offset 分割误伤（下划线/数字合法字符）
        assert_eq!(
            parse_begin_item("focusin"),
            Some(BeginSpec::Event {
                target: None,
                event: "focusin".to_string(),
                offset: 0.0,
            })
        );
    }

    #[test]
    fn begin_sync_offset_variants() {
        assert_eq!(
            parse_begin_item("a.begin"),
            Some(BeginSpec::Sync {
                target: "a".to_string(),
                basis: Basis::Begin,
                offset: 0.0,
            })
        );
        assert_eq!(
            parse_begin_item("b.end-2s"),
            Some(BeginSpec::Sync {
                target: "b".to_string(),
                basis: Basis::End,
                offset: -2.0,
            })
        );
        assert_eq!(
            parse_begin_item("c.repeat(3)+1s"),
            Some(BeginSpec::Sync {
                target: "c".to_string(),
                basis: Basis::Repeat(3),
                offset: 1.0,
            })
        );
        assert_eq!(
            parse_begin_item("d.repeat(0)"),
            Some(BeginSpec::Sync {
                target: "d".to_string(),
                basis: Basis::Repeat(0),
                offset: 0.0,
            })
        );
        // repeat 带非数字 → 整项拒绝
        assert_eq!(parse_begin_item("e.repeat(x)"), None);
    }

    #[test]
    fn earliest_offset_picks_min_offset() {
        let specs = parse_begin_list("3s; click; -1s; indefinite; anim1.begin");
        assert_eq!(earliest_offset(&specs), Some(-1.0));
        assert_eq!(earliest_offset(&[]), None);
        let no_offset = parse_begin_list("click; indefinite");
        assert_eq!(earliest_offset(&no_offset), None);
    }

    // ---- 3. Interval 状态机 ----

    #[test]
    fn state_inactive_before_begin() {
        let tm = Timing {
            begin: 2.0,
            dur: Some(3.0),
            repeat_count: None,
            fill: Fill::Remove,
            ..Timing::default()
        };
        assert_eq!(tm.state_at(1.0), ActiveState::Inactive);
        assert_eq!(tm.state_at(1.999), ActiveState::Inactive);
        assert_eq!(tm.state_at(f64::NAN), ActiveState::Inactive); // NaN 防御
    }

    #[test]
    fn state_active_first_iteration() {
        let tm = Timing {
            begin: 2.0,
            dur: Some(3.0),
            repeat_count: None,
            fill: Fill::Remove,
            ..Timing::default()
        };
        assert_eq!(
            tm.state_at(2.0),
            ActiveState::Active {
                local_t: 0.0,
                iter: 0
            }
        );
        assert_eq!(
            tm.state_at(3.5),
            ActiveState::Active {
                local_t: 1.5,
                iter: 0
            }
        );
    }

    #[test]
    fn state_active_mid_repeat() {
        let tm = Timing {
            begin: 1.0,
            dur: Some(2.0),
            repeat_count: Some(RepeatCount::Count(3)),
            fill: Fill::Remove,
            ..Timing::default()
        };
        // elapsed=5 → 第 3 轮内 1 秒
        assert_eq!(
            tm.state_at(6.0),
            ActiveState::Active {
                local_t: 1.0,
                iter: 2
            }
        );
        // elapsed=4 → 恰好第 3 轮起点
        assert_eq!(
            tm.state_at(5.0),
            ActiveState::Active {
                local_t: 0.0,
                iter: 2
            }
        );
        // elapsed=2.5 → 第 2 轮中段
        assert_eq!(
            tm.state_at(3.5),
            ActiveState::Active {
                local_t: 0.5,
                iter: 1
            }
        );
    }

    #[test]
    fn state_exhausted_freezes_at_end() {
        let tm = Timing {
            begin: 1.0,
            dur: Some(2.0),
            repeat_count: Some(RepeatCount::Count(3)),
            fill: Fill::Freeze,
            ..Timing::default()
        };
        // elapsed=6 = 2s×3 恰好用尽 → 冻在末值（progress 1）
        assert_eq!(tm.state_at(7.0), ActiveState::Frozen { final_t: 2.0 });
        assert_eq!(tm.state_at(100.0), ActiveState::Frozen { final_t: 2.0 });
    }

    #[test]
    fn state_exhausted_remove_returns_inactive() {
        let tm = Timing {
            begin: 1.0,
            dur: Some(2.0),
            repeat_count: Some(RepeatCount::Count(3)),
            fill: Fill::Remove,
            ..Timing::default()
        };
        assert_eq!(tm.state_at(7.0), ActiveState::Inactive);
        assert_eq!(tm.state_at(100.0), ActiveState::Inactive);
    }

    #[test]
    fn state_indefinite_dur_runs_forever() {
        let tm = Timing {
            begin: 0.0,
            dur: None,
            repeat_count: None,
            fill: Fill::Freeze,
            ..Timing::default()
        };
        assert_eq!(
            tm.state_at(1000.0),
            ActiveState::Active {
                local_t: 1000.0,
                iter: 0
            }
        );
        // dur=None 时 repeatCount 无从用尽
        let tm2 = Timing {
            dur: None,
            repeat_count: Some(RepeatCount::Count(2)),
            ..tm
        };
        assert_eq!(
            tm2.state_at(123.5),
            ActiveState::Active {
                local_t: 123.5,
                iter: 0
            }
        );
    }

    #[test]
    fn state_repeat_indefinite_never_ends() {
        let tm = Timing {
            begin: 0.0,
            dur: Some(2.0),
            repeat_count: Some(RepeatCount::Indefinite),
            fill: Fill::Freeze,
            ..Timing::default()
        };
        assert_eq!(
            tm.state_at(5.0),
            ActiveState::Active {
                local_t: 1.0,
                iter: 2
            }
        );
        // 1e9 = 2s × 5亿轮整 → 恰处新一轮起点
        assert_eq!(
            tm.state_at(1e9),
            ActiveState::Active {
                local_t: 0.0,
                iter: 500_000_000
            }
        );
    }

    #[test]
    fn state_single_run_without_repeat() {
        // repeatCount=None：活动时长 = dur，只播一轮
        let freeze = Timing {
            begin: 0.0,
            dur: Some(2.0),
            repeat_count: None,
            fill: Fill::Freeze,
            ..Timing::default()
        };
        assert_eq!(
            freeze.state_at(1.0),
            ActiveState::Active {
                local_t: 1.0,
                iter: 0
            }
        );
        assert_eq!(freeze.state_at(2.0), ActiveState::Frozen { final_t: 2.0 });
        let remove = Timing {
            fill: Fill::Remove,
            ..freeze
        };
        assert_eq!(remove.state_at(2.0), ActiveState::Inactive);
    }

    #[test]
    fn state_zero_repeat_and_zero_dur_freeze_at_start() {
        let tm = Timing {
            begin: 1.0,
            dur: Some(2.0),
            repeat_count: Some(RepeatCount::Count(0)),
            fill: Fill::Freeze,
            ..Timing::default()
        };
        assert_eq!(tm.state_at(1.0), ActiveState::Frozen { final_t: 0.0 });
        assert_eq!(tm.state_at(50.0), ActiveState::Frozen { final_t: 0.0 });
        let zero_dur = Timing {
            begin: 1.0,
            dur: Some(0.0),
            repeat_count: None,
            fill: Fill::Freeze,
            ..Timing::default()
        };
        assert_eq!(zero_dur.state_at(1.0), ActiveState::Frozen { final_t: 0.0 });
    }

    // ---- 3b. 多 begin 实例 × restart（M6：静态采样语义） ----
    //
    // begin="0s;9s"（实例区间 [b, b+5)，fill=freeze）：
    //   always       —— t 取最新已开始实例；
    //   whenNotActive —— 新实例只在活动期外生效（区间不重叠重建）；
    //   never        —— 只认首个实例。
    fn multi_begin(restart: Restart, dur: f64) -> Timing {
        Timing {
            begin: 0.0,
            begin_instances: vec![0.0, 9.0],
            restart,
            dur: Some(dur),
            repeat_count: None,
            repeat_dur: None,
            fill: Fill::Freeze,
        }
    }

    #[test]
    fn restart_always_takes_latest_started_instance() {
        let tm = multi_begin(Restart::Always, 5.0);
        // t<9：最新已开始 = 实例 0
        assert_eq!(
            tm.state_at(4.0),
            ActiveState::Active {
                local_t: 4.0,
                iter: 0
            }
        );
        // t=9.5：实例 9 重启活动期（local 从 0 重计）
        assert_eq!(
            tm.state_at(9.5),
            ActiveState::Active {
                local_t: 0.5,
                iter: 0
            }
        );
        // t=15：实例 9 已用尽（9+5=14）→ 冻末值
        assert_eq!(tm.state_at(15.0), ActiveState::Frozen { final_t: 5.0 });
        // t 早于全部实例
        assert_eq!(tm.state_at(-0.5), ActiveState::Inactive);
    }

    #[test]
    fn restart_when_not_active_ignores_overlapping_instances() {
        // dur=20：实例 0 的区间 [0,20) 在 9s 处仍活动 → 实例 9 被忽略
        let tm = multi_begin(Restart::WhenNotActive, 20.0);
        assert_eq!(
            tm.state_at(9.5),
            ActiveState::Active {
                local_t: 9.5,
                iter: 0
            },
            "活动期内的新实例不重启（s12-04 语义）"
        );
        // 用尽后冻在实例 0 的末值（t=25 > 20）
        assert_eq!(tm.state_at(25.0), ActiveState::Frozen { final_t: 20.0 });
        // dur=5：区间 [0,5) 与 [9,14) 不重叠——实例 9 正常开新区间
        let tm2 = multi_begin(Restart::WhenNotActive, 5.0);
        assert_eq!(
            tm2.state_at(9.5),
            ActiveState::Active {
                local_t: 0.5,
                iter: 0
            }
        );
        // 两区间之间的空档（t=7）：无活动区间 → 最后一有效实例的结束态
        assert_eq!(tm2.state_at(7.0), ActiveState::Frozen { final_t: 5.0 });
        // remove：空档期复原
        let rm = Timing {
            fill: Fill::Remove,
            ..multi_begin(Restart::WhenNotActive, 5.0)
        };
        assert_eq!(rm.state_at(7.0), ActiveState::Inactive);
    }

    #[test]
    fn restart_never_first_instance_exclusive() {
        let tm = multi_begin(Restart::Never, 5.0);
        assert_eq!(
            tm.state_at(4.0),
            ActiveState::Active {
                local_t: 4.0,
                iter: 0
            }
        );
        // t=9.5：实例 0 已结束（freeze 冻末值），实例 9 永不生效
        assert_eq!(tm.state_at(9.5), ActiveState::Frozen { final_t: 5.0 });
        assert_eq!(tm.state_at(100.0), ActiveState::Frozen { final_t: 5.0 });
        // repeat=indefinite 首实例独占后永不结束
        let inf = Timing {
            repeat_count: Some(RepeatCount::Indefinite),
            fill: Fill::Remove,
            ..multi_begin(Restart::Never, 2.0)
        };
        assert_eq!(
            inf.state_at(9.5),
            ActiveState::Active {
                local_t: 1.5,
                iter: 4
            }
        );
    }

    /// 单实例等价性红线：`begin_instances` 空/单元素 + 任意 restart 都与
    /// 历史「begin 单值」行为一致（全部语料的既有形态）。
    #[test]
    fn single_instance_equivalence_across_restart_modes() {
        let legacy = Timing {
            begin: 2.0,
            begin_instances: Vec::new(),
            restart: Restart::Always,
            dur: Some(3.0),
            repeat_count: None,
            repeat_dur: None,
            fill: Fill::Freeze,
        };
        for restart in [Restart::Always, Restart::WhenNotActive, Restart::Never] {
            for insts in [Vec::new(), vec![2.0]] {
                let tm = Timing {
                    begin_instances: insts,
                    restart,
                    ..legacy.clone()
                };
                assert_eq!(tm.state_at(1.0), legacy.state_at(1.0), "t<begin");
                assert_eq!(
                    tm.state_at(3.5),
                    ActiveState::Active {
                        local_t: 1.5,
                        iter: 0
                    }
                );
                assert_eq!(tm.state_at(6.0), legacy.state_at(6.0), "用尽冻结");
                assert_eq!(tm.state_at(f64::NAN), ActiveState::Inactive);
            }
        }
    }

    #[test]
    fn parse_restart_enum() {
        assert_eq!(parse_restart("always"), Some(Restart::Always));
        assert_eq!(parse_restart("whenNotActive"), Some(Restart::WhenNotActive));
        assert_eq!(parse_restart("never"), Some(Restart::Never));
        assert_eq!(parse_restart(" never "), Some(Restart::Never)); // 空白容忍
                                                                    // SMIL 枚举大小写敏感
        assert_eq!(parse_restart("Always"), None);
        assert_eq!(parse_restart("whennotactive"), None);
        assert_eq!(parse_restart(""), None);
    }

    // ---- 3c. repeatDur（车道CJ：活动总时长上限） ----

    #[test]
    fn repeat_dur_cuts_mid_iteration_freeze_at_cut_phase() {
        // s12-04 形态：dur=2s、repeatDur=3s、fill=freeze。
        // [0,2) 第 0 轮；[2,3) 第 1 轮前半；t≥3 冻在 local=1s（3 mod 2）。
        let tm = Timing {
            begin: 0.0,
            dur: Some(2.0),
            repeat_dur: Some(3.0),
            fill: Fill::Freeze,
            ..Timing::default()
        };
        assert_eq!(
            tm.state_at(1.0),
            ActiveState::Active {
                local_t: 1.0,
                iter: 0
            }
        );
        assert_eq!(
            tm.state_at(2.5),
            ActiveState::Active {
                local_t: 0.5,
                iter: 1
            },
            "截断窗内仍在第 2 轮推进"
        );
        assert_eq!(tm.state_at(3.0), ActiveState::Frozen { final_t: 1.0 });
        assert_eq!(tm.state_at(100.0), ActiveState::Frozen { final_t: 1.0 });
        // remove：截断后复原
        let rm = Timing {
            fill: Fill::Remove,
            ..tm
        };
        assert_eq!(rm.state_at(3.0), ActiveState::Inactive);
    }

    #[test]
    fn repeat_dur_exact_multiple_freezes_at_iteration_end() {
        // rd=4s = 2×2s 整轮倍数 → 冻在末值（与 Count(2) 用尽同语义）
        let tm = Timing {
            begin: 0.0,
            dur: Some(2.0),
            repeat_dur: Some(4.0),
            fill: Fill::Freeze,
            ..Timing::default()
        };
        assert_eq!(tm.state_at(4.0), ActiveState::Frozen { final_t: 2.0 });
        assert_eq!(
            tm.state_at(3.9),
            ActiveState::Active {
                local_t: 1.9,
                iter: 1
            }
        );
    }

    #[test]
    fn repeat_dur_shorter_than_dur_freezes_mid_first_round() {
        let tm = Timing {
            begin: 0.5,
            dur: Some(2.0),
            repeat_dur: Some(0.5),
            fill: Fill::Freeze,
            ..Timing::default()
        };
        assert_eq!(
            tm.state_at(0.75),
            ActiveState::Active {
                local_t: 0.25,
                iter: 0
            }
        );
        assert_eq!(tm.state_at(1.0), ActiveState::Frozen { final_t: 0.5 });
    }

    #[test]
    fn repeat_dur_zero_freezes_at_start_and_invalid_ignored() {
        // rd=0：begin 即冻首值
        let tm = Timing {
            dur: Some(2.0),
            repeat_dur: Some(0.0),
            fill: Fill::Freeze,
            ..Timing::default()
        };
        assert_eq!(tm.state_at(0.1), ActiveState::Frozen { final_t: 0.0 });
        // 负/NaN/+inf 视同未指定（走既有 repeatCount 语义）
        for bad in [-1.0, f64::NAN, f64::INFINITY] {
            let tm = Timing {
                dur: Some(2.0),
                repeat_count: Some(RepeatCount::Count(3)),
                repeat_dur: Some(bad),
                fill: Fill::Freeze,
                ..Timing::default()
            };
            assert_eq!(tm.state_at(6.5), ActiveState::Frozen { final_t: 2.0 });
        }
    }

    #[test]
    fn repeat_dur_caps_repeat_count_total() {
        // Count(5)×dur=1s = 5s 总长，rd=2.5s 截断 → 冻 local 0.5
        let tm = Timing {
            dur: Some(1.0),
            repeat_count: Some(RepeatCount::Count(5)),
            repeat_dur: Some(2.5),
            fill: Fill::Freeze,
            ..Timing::default()
        };
        assert_eq!(tm.state_at(2.5), ActiveState::Frozen { final_t: 0.5 });
        // rd 大于总时长：不延长（Count 语义不变）
        let tm = Timing {
            dur: Some(1.0),
            repeat_count: Some(RepeatCount::Count(2)),
            repeat_dur: Some(9.0),
            fill: Fill::Freeze,
            ..Timing::default()
        };
        assert_eq!(tm.state_at(2.0), ActiveState::Frozen { final_t: 1.0 });
        assert_eq!(tm.state_at(9.5), ActiveState::Frozen { final_t: 1.0 });
    }

    #[test]
    fn repeat_dur_with_indefinite_dur_bounds_activity() {
        // dur=indefinite + rd=3s：活动期 3s 后冻（local=rd）
        let tm = Timing {
            dur: None,
            repeat_dur: Some(3.0),
            fill: Fill::Freeze,
            ..Timing::default()
        };
        assert_eq!(
            tm.state_at(2.0),
            ActiveState::Active {
                local_t: 2.0,
                iter: 0
            }
        );
        assert_eq!(tm.state_at(3.0), ActiveState::Frozen { final_t: 3.0 });
    }

    #[test]
    fn repeat_dur_respected_in_when_not_active_reconstruction() {
        // whenNotActive 区间重建用 active_total：rd 截断后空档的新 begin
        // 实例应能开新区间（dur=5, rd=3 → 区间 [0,3) 与 [9,…) 不重叠）
        let tm = Timing {
            begin: 0.0,
            begin_instances: vec![0.0, 9.0],
            restart: Restart::WhenNotActive,
            dur: Some(5.0),
            repeat_dur: Some(3.0),
            fill: Fill::Freeze,
            ..Timing::default()
        };
        assert_eq!(
            tm.state_at(9.5),
            ActiveState::Active {
                local_t: 0.5,
                iter: 0
            },
            "rd 截断后实例 9 不再与实例 0 区间重叠"
        );
    }

    // ---- 4. calcMode 插值器 ----

    #[test]
    fn sample_linear_uniform_keytimes() {
        assert_eq!(
            sample(&[0.0, 10.0], None, CalcMode::Linear, None, 0.5),
            Some(5.0)
        );
        assert_eq!(
            sample(&[0.0, 10.0, 20.0], None, CalcMode::Linear, None, 0.75),
            Some(15.0)
        );
        // p 越界夹取
        assert_eq!(
            sample(&[0.0, 10.0], None, CalcMode::Linear, None, -3.0),
            Some(0.0)
        );
        assert_eq!(
            sample(&[0.0, 10.0], None, CalcMode::Linear, None, 5.0),
            Some(10.0)
        );
    }

    #[test]
    fn sample_linear_explicit_keytimes() {
        let kt = [0.0, 0.25, 1.0];
        let v = [0.0, 10.0, 20.0];
        assert_eq!(
            sample(&v, Some(&kt), CalcMode::Linear, None, 0.25),
            Some(10.0)
        );
        // (0.625-0.25)/0.75 = 0.5 → 10 + 10*0.5
        assert_eq!(
            sample(&v, Some(&kt), CalcMode::Linear, None, 0.625),
            Some(15.0)
        );
        assert_eq!(
            sample(&v, Some(&kt), CalcMode::Linear, None, 0.0),
            Some(0.0)
        );
        assert_eq!(
            sample(&v, Some(&kt), CalcMode::Linear, None, 1.0),
            Some(20.0)
        );
    }

    #[test]
    fn sample_discrete_step_points() {
        let v = [10.0, 20.0, 30.0];
        assert_eq!(sample(&v, None, CalcMode::Discrete, None, 0.0), Some(10.0));
        assert_eq!(
            sample(&v, None, CalcMode::Discrete, None, 0.333333),
            Some(10.0)
        );
        assert_eq!(
            sample(&v, None, CalcMode::Discrete, None, 0.3333334),
            Some(20.0)
        );
        assert_eq!(
            sample(&v, None, CalcMode::Discrete, None, 0.666666),
            Some(20.0)
        );
        assert_eq!(
            sample(&v, None, CalcMode::Discrete, None, 0.6666667),
            Some(30.0)
        );
        assert_eq!(sample(&v, None, CalcMode::Discrete, None, 1.0), Some(30.0));
    }

    #[test]
    fn sample_discrete_with_keytimes() {
        let kt = [0.0, 0.5, 1.0];
        let v = [1.0, 2.0, 3.0];
        assert_eq!(
            sample(&v, Some(&kt), CalcMode::Discrete, None, 0.49),
            Some(1.0)
        );
        assert_eq!(
            sample(&v, Some(&kt), CalcMode::Discrete, None, 0.5),
            Some(2.0)
        );
        assert_eq!(
            sample(&v, Some(&kt), CalcMode::Discrete, None, 0.99),
            Some(2.0)
        );
        assert_eq!(
            sample(&v, Some(&kt), CalcMode::Discrete, None, 1.0),
            Some(3.0)
        );
        // discrete 允许末 keyTime < 1（末值保持到结束）
        let kt2 = [0.0, 0.2];
        let v2 = [5.0, 7.0];
        assert_eq!(
            sample(&v2, Some(&kt2), CalcMode::Discrete, None, 0.1),
            Some(5.0)
        );
        assert_eq!(
            sample(&v2, Some(&kt2), CalcMode::Discrete, None, 0.5),
            Some(7.0)
        );
        assert_eq!(
            sample(&v2, Some(&kt2), CalcMode::Discrete, None, 1.0),
            Some(7.0)
        );
    }

    #[test]
    fn keytimes_validation_rejects_bad_lists() {
        let v = [0.0, 10.0, 20.0];
        // 首 ≠ 0
        assert_eq!(
            sample(&v, Some(&[0.1, 0.5, 1.0]), CalcMode::Linear, None, 0.5),
            None
        );
        // 末 ≠ 1（linear）
        assert_eq!(
            sample(&v, Some(&[0.0, 0.5, 0.9]), CalcMode::Linear, None, 0.5),
            None
        );
        // 长度不匹配
        assert_eq!(
            sample(&v, Some(&[0.0, 0.2]), CalcMode::Linear, None, 0.1),
            None
        );
        // 非单调
        let bad = [0.0, 0.5, 0.3];
        assert_eq!(sample(&v, Some(&bad), CalcMode::Linear, None, 0.5), None);
        // NaN
        let nan_kt = [0.0, f64::NAN, 1.0];
        assert_eq!(sample(&v, Some(&nan_kt), CalcMode::Linear, None, 0.5), None);
        // discrete 末 > 1 仍拒绝
        assert_eq!(
            sample(
                &[1.0, 2.0],
                Some(&[0.0, 1.5]),
                CalcMode::Discrete,
                None,
                0.5
            ),
            None
        );
    }

    #[test]
    fn spline_ease_in_out_center() {
        // CSS ease-in-out：中心对称 → p=0.5 处 ≈ 0.5（精度门限 1e-4）
        let sp = [KeySpline {
            x1: 0.42,
            y1: 0.0,
            x2: 0.58,
            y2: 1.0,
        }];
        let v = sample(&[0.0, 1.0], None, CalcMode::Spline, Some(&sp), 0.5).unwrap_or(f64::NAN);
        assert!((v - 0.5).abs() < 1e-4, "ease-in-out mid {v}");
    }

    #[test]
    fn spline_identity_equals_linear() {
        let sp = [KeySpline {
            x1: 0.0,
            y1: 0.0,
            x2: 1.0,
            y2: 1.0,
        }];
        for p in [0.0, 0.1, 0.25, 0.5, 0.75, 0.9, 1.0] {
            let lin = sample(&[0.0, 10.0], None, CalcMode::Linear, None, p).unwrap_or(f64::NAN);
            let spl =
                sample(&[0.0, 10.0], None, CalcMode::Spline, Some(&sp), p).unwrap_or(f64::NAN);
            assert!(
                (lin - spl).abs() < 1e-6,
                "identity spline at {p}: {lin} vs {spl}"
            );
        }
    }

    #[test]
    fn spline_slow_ends_fast_middle() {
        // (0.9,0,0.1,1)：中心对称、两端慢中间快
        let sp = [KeySpline {
            x1: 0.9,
            y1: 0.0,
            x2: 0.1,
            y2: 1.0,
        }];
        let at =
            |p: f64| sample(&[0.0, 1.0], None, CalcMode::Spline, Some(&sp), p).unwrap_or(f64::NAN);
        assert!(at(0.25) < 0.10, "slow start: {}", at(0.25));
        assert!(at(0.75) > 0.90, "slow end: {}", at(0.75));
        assert!((at(0.5) - 0.5).abs() < 1e-4, "symmetric mid: {}", at(0.5));
    }

    #[test]
    fn spline_falls_back_to_linear() {
        let v = [0.0, 10.0];
        // splines 缺失 / 长度不符 / x 越界 → 均回退线性
        assert_eq!(sample(&v, None, CalcMode::Spline, None, 0.5), Some(5.0));
        let empty: [KeySpline; 0] = [];
        assert_eq!(
            sample(&v, None, CalcMode::Spline, Some(&empty), 0.5),
            Some(5.0)
        );
        let bad = [KeySpline {
            x1: -0.1,
            y1: 0.0,
            x2: 1.0,
            y2: 1.0,
        }];
        assert_eq!(
            sample(&v, None, CalcMode::Spline, Some(&bad), 0.5),
            Some(5.0)
        );
    }

    #[test]
    fn paced_approximates_linear() {
        let v = [0.0, 10.0, 20.0];
        let kt = [0.0, 0.25, 1.0];
        for p in [0.0, 0.3, 0.625, 1.0] {
            let lin = sample(&v, Some(&kt), CalcMode::Linear, None, p).unwrap_or(f64::NAN);
            let paced = sample(&v, Some(&kt), CalcMode::Paced, None, p).unwrap_or(f64::NAN);
            assert_eq!(lin, paced);
        }
    }

    #[test]
    fn sample_degenerate_inputs() {
        assert_eq!(sample(&[], None, CalcMode::Linear, None, 0.5), None);
        assert_eq!(
            sample(&[42.0], None, CalcMode::Linear, None, 0.9),
            Some(42.0)
        );
        assert_eq!(
            sample(&[42.0], None, CalcMode::Discrete, None, 0.0),
            Some(42.0)
        );
        assert_eq!(
            sample(&[0.0, 1.0], None, CalcMode::Linear, None, f64::NAN),
            None
        );
    }

    // ---- 5. 杂项解析 ----

    #[test]
    fn parse_anim_enums() {
        assert_eq!(parse_additive("replace"), Some(Additive::Replace));
        assert_eq!(parse_additive(" sum "), Some(Additive::Sum));
        assert_eq!(parse_additive("SUM"), None); // 枚举值大小写敏感
        assert_eq!(parse_accumulate("none"), Some(Accumulate::None));
        assert_eq!(parse_accumulate("sum"), Some(Accumulate::Sum));
        assert_eq!(parse_accumulate("x"), None);
        assert_eq!(parse_fill("remove"), Some(Fill::Remove));
        assert_eq!(parse_fill("freeze"), Some(Fill::Freeze));
        assert_eq!(parse_calc_mode("linear"), Some(CalcMode::Linear));
        assert_eq!(parse_calc_mode("discrete"), Some(CalcMode::Discrete));
        assert_eq!(parse_calc_mode("paced"), Some(CalcMode::Paced));
        assert_eq!(parse_calc_mode("spline"), Some(CalcMode::Spline));
        assert_eq!(parse_calc_mode("easing"), None);
        assert_eq!(parse_repeat_count("3"), Some(RepeatCount::Count(3)));
        assert_eq!(
            parse_repeat_count("indefinite"),
            Some(RepeatCount::Indefinite)
        );
        assert_eq!(parse_repeat_count("2.5"), None);
        assert_eq!(parse_repeat_count("-1"), None);
        assert_eq!(
            parse_attribute_name(" xlink:href "),
            Some("xlink:href".to_string())
        );
        assert_eq!(parse_attribute_name("width"), Some("width".to_string()));
        assert_eq!(parse_attribute_name("  "), None);
    }

    #[test]
    fn parse_key_lists() {
        assert_eq!(parse_key_times("0; 0.5; 1"), Some(vec![0.0, 0.5, 1.0]));
        assert_eq!(parse_key_times("0;x"), None);
        assert_eq!(parse_key_times(""), None);
        let sp = parse_key_splines("0.42 0 0.58 1; 0 0 1 1");
        assert_eq!(sp.as_ref().map(|v| v.len()), Some(2));
        assert_eq!(
            sp,
            Some(vec![
                KeySpline {
                    x1: 0.42,
                    y1: 0.0,
                    x2: 0.58,
                    y2: 1.0
                },
                KeySpline {
                    x1: 0.0,
                    y1: 0.0,
                    x2: 1.0,
                    y2: 1.0
                },
            ])
        );
        assert_eq!(parse_key_splines("1 2"), None); // 段内数量 ≠ 4
        assert_eq!(parse_key_splines("-0.1 0 1 1"), None); // x 越界
        assert_eq!(parse_key_splines(""), None);
    }

    // ---- 6. 颜色插值 ----

    #[test]
    fn lerp_rgba_midpoint_and_edges() {
        let red = Rgba {
            r: 255,
            g: 0,
            b: 0,
            a: 255,
        };
        let blue = Rgba {
            r: 0,
            g: 0,
            b: 255,
            a: 255,
        };
        // 中点四舍五入：255→0 的 0.5 处 = 127.5 → 128
        assert_eq!(
            lerp_rgba(red, blue, 0.5),
            Rgba {
                r: 128,
                g: 0,
                b: 128,
                a: 255
            }
        );
        assert_eq!(lerp_rgba(red, blue, 0.0), red);
        assert_eq!(lerp_rgba(red, blue, 1.0), blue);
        // t 越界/NaN 防御
        assert_eq!(lerp_rgba(red, blue, -5.0), red);
        assert_eq!(lerp_rgba(red, blue, 5.0), blue);
        assert_eq!(lerp_rgba(red, blue, f64::NAN), red);
        // alpha 通道同规则：200→100 的中点 = 150
        let x = Rgba {
            r: 0,
            g: 0,
            b: 0,
            a: 200,
        };
        let y = Rgba {
            r: 0,
            g: 0,
            b: 0,
            a: 100,
        };
        assert_eq!(
            lerp_rgba(x, y, 0.5),
            Rgba {
                r: 0,
                g: 0,
                b: 0,
                a: 150
            }
        );
    }

    // ---- 7. CSS keyframes ----

    /// s12-05 同款 @keyframes 文本：多选择器帧、from/to、排序与合并。
    #[test]
    fn keyframes_parses_multi_selector_and_keywords() {
        let kfs = parse_keyframes(
            "@keyframes c12-fill { 0%, 100% { fill: #2E86C1; } 50% { fill: #E94560; } }",
        );
        assert_eq!(kfs.len(), 1);
        assert_eq!(kfs[0].0, "c12-fill");
        let fr = &kfs[0].1;
        assert_eq!(fr.len(), 3); // 0.0 / 0.5 / 1.0（0% 与 100% 是两个 offset）
        assert_eq!(fr[0].offset, 0.0);
        assert_eq!(
            fr[0].decls,
            vec![(String::from("fill"), String::from("#2E86C1"))]
        );
        assert_eq!(fr[1].offset, 0.5);
        assert_eq!(fr[1].decls[0].1, "#E94560");
        assert_eq!(fr[2].offset, 1.0);
    }

    #[test]
    fn keyframes_from_to_and_comments() {
        let kfs = parse_keyframes(
            "/* c */ @KEYFRAMES clip { from { transform: translateX(-90px) } /* m */ to { transform: translateX(60px) } }",
        );
        assert_eq!(kfs.len(), 1);
        assert_eq!(kfs[0].0, "clip"); // 大小写不敏感 at-rule、名保留
        let fr = &kfs[0].1;
        assert_eq!(fr.len(), 2);
        assert_eq!(fr[0].offset, 0.0);
        assert_eq!(fr[0].decls[0].1, "translateX(-90px)");
        assert_eq!(fr[1].offset, 1.0);
    }

    /// 多块 @keyframes 与普通规则混排：只收 @keyframes，帧内声明解析复用
    /// css::parse_decls（小写化、尾分号可省）。
    #[test]
    fn keyframes_multiple_blocks_skip_normal_rules() {
        let kfs = parse_keyframes(
            ".a { fill: red } @keyframes k1 { 10% { STROKE-WIDTH: 2 } } @keyframes k2 { to { opacity: 0.3; } } .b { fill: blue }",
        );
        assert_eq!(kfs.len(), 2);
        assert_eq!(kfs[0].0, "k1");
        assert_eq!(kfs[0].1[0].offset, 0.1);
        assert_eq!(
            kfs[0].1[0].decls,
            vec![(String::from("stroke-width"), String::from("2"))]
        );
        assert_eq!(kfs[1].0, "k2");
    }

    /// 同帧多声明 + 同名属性后者覆盖（kf_prop_track 取最后）。
    #[test]
    fn kf_prop_track_last_decl_wins() {
        let kfs =
            parse_keyframes("@keyframes k { 0% { fill: red; fill: blue } 50% { fill: green } }");
        let track = kf_prop_track(&kfs[0].1, "fill");
        assert_eq!(track.len(), 2);
        assert_eq!(track[0], (0.0, "blue"));
        assert_eq!(track[1], (0.5, "green"));
        assert!(kf_prop_track(&kfs[0].1, "stroke").is_empty());
    }

    /// animation 简写：名/时长/迭代/方向/fill/timing 全字段。
    #[test]
    fn animation_shorthand_full_parse() {
        let sp = parse_animation_shorthand("c12-clip 2.2s ease-in-out infinite alternate")
            .unwrap_or_else(|| panic!("应解析成功"));
        assert_eq!(sp.name, "c12-clip");
        assert_eq!(sp.duration, 2.2);
        assert_eq!(sp.delay, 0.0);
        assert_eq!(sp.iteration_count, None); // infinite
        assert_eq!(sp.direction, CssDirection::Alternate);
        assert_eq!(sp.timing, CssTiming::EaseInOut);
        assert!(!sp.fill_forwards);

        let sp2 = parse_animation_shorthand("spin 2s linear 3 forwards")
            .unwrap_or_else(|| panic!("应解析成功"));
        assert_eq!(sp2.iteration_count, Some(3));
        assert!(sp2.fill_forwards);
        assert_eq!(sp2.direction, CssDirection::Normal);
        assert_eq!(sp2.timing, CssTiming::Linear);

        // 延迟在第二个时间值；首个时间值才是 duration
        let sp3 = parse_animation_shorthand("k 1s 0.5s 2").unwrap_or_else(|| panic!("应解析成功"));
        assert_eq!(sp3.duration, 1.0);
        assert_eq!(sp3.delay, 0.5);
        assert_eq!(sp3.iteration_count, Some(2));
        // 缺省 timing = CSS 初始值 ease
        assert_eq!(sp3.timing, CssTiming::Ease);
    }

    /// timing 关键词解析 + cubic-bezier 预设表（车道BN）。
    #[test]
    fn css_timing_parse_and_spline_table() {
        assert_eq!(CssTiming::parse("LINEAR"), Some(CssTiming::Linear));
        assert_eq!(CssTiming::parse("ease"), Some(CssTiming::Ease));
        assert_eq!(CssTiming::parse("Ease-In"), Some(CssTiming::EaseIn));
        assert_eq!(CssTiming::parse("ease-out"), Some(CssTiming::EaseOut));
        assert_eq!(CssTiming::parse("ease-in-out"), Some(CssTiming::EaseInOut));
        assert_eq!(CssTiming::parse("cubic-bezier(.2,0,.8,1)"), None); // 不识别回落 Linear
        assert_eq!(CssTiming::parse("step-start"), None);
        // 预设控制点 = CSS 规范固定表
        assert_eq!(CssTiming::Linear.spline(), None);
        let sp = |t: CssTiming| t.spline().expect("预设必有样条");
        assert_eq!(
            sp(CssTiming::Ease),
            KeySpline {
                x1: 0.25,
                y1: 0.1,
                x2: 0.25,
                y2: 1.0
            }
        );
        assert_eq!(
            sp(CssTiming::EaseIn),
            KeySpline {
                x1: 0.42,
                y1: 0.0,
                x2: 1.0,
                y2: 1.0
            }
        );
        assert_eq!(
            sp(CssTiming::EaseOut),
            KeySpline {
                x1: 0.0,
                y1: 0.0,
                x2: 0.58,
                y2: 1.0
            }
        );
        assert_eq!(
            sp(CssTiming::EaseInOut),
            KeySpline {
                x1: 0.42,
                y1: 0.0,
                x2: 0.58,
                y2: 1.0
            }
        );
    }

    /// ease-in-out 预设采样数值（车道BN 定位依据）：s6-05 pulse 在
    /// t=1s（2.5s 循环进度 0.667，50%→100% 段局部 1/3）Chrome 实测
    /// opacity ≈ 0.4255（本函数级断言钉住采样数学，端到端由差分验收）。
    #[test]
    fn css_ease_in_out_sample_values() {
        let vals = [1.0, 0.25, 1.0];
        let offs = [0.0, 0.5, 1.0];
        let at = |p: f64| {
            sample_kf_timed(&vals, &offs, CssTiming::EaseInOut, p).expect("合法轨迹可采样")
        };
        // p=1/3：第一段局部 0.667 → spline(0.667)=0.768225 → op = 1-0.75·0.768225 ≈ 0.4238
        let v = at(1.0 / 3.0);
        assert!((v - 0.4238).abs() < 1e-3, "p=1/3 → {v}");
        // p=2/3：第二段局部 0.333 → spline(0.333)=0.231775 → op = 0.25+0.75·0.231775 ≈ 0.4238
        let v = at(2.0 / 3.0);
        assert!((v - 0.4238).abs() < 1e-3, "p=2/3 → {v}");
        // 端点过 (0,0)/(1,1)；p=0.5 恰在 50% 关键帧上 → 帧值 0.25
        assert!((at(0.0) - 1.0).abs() < 1e-9);
        assert!((at(1.0) - 1.0).abs() < 1e-9);
        assert!((at(0.5) - 0.25).abs() < 1e-9, "p=0.5 关键帧值");
        // linear 同轨对照：p=2/3 → 0.25+0.75·0.333 = 0.5（旧行为）
        let lin =
            sample_kf_timed(&vals, &offs, CssTiming::Linear, 2.0 / 3.0).expect("合法轨迹可采样");
        assert!((lin - 0.5).abs() < 1e-9);
        // 结构校验：offsets 长度失配 / 首非 0 / 末非 1 → None（同 sample 的
        // keyTimes 校验语义，轨迹整条放弃）
        assert!(sample_kf_timed(&vals, &[0.0, 0.5], CssTiming::Ease, 0.5).is_none());
        assert!(sample_kf_timed(&vals, &[0.1, 0.5, 1.0], CssTiming::Ease, 0.5).is_none());
        assert!(sample_kf_timed(&vals, &[0.0, 0.5, 0.9], CssTiming::Ease, 0.5).is_none());
    }

    /// animation 简写边界：none / 缺名 / 空串 → None；逗号多动画取首段。
    #[test]
    fn animation_shorthand_edge_cases() {
        assert!(parse_animation_shorthand("none").is_none());
        assert!(parse_animation_shorthand("2s linear").is_none()); // 无名
        assert!(parse_animation_shorthand("").is_none());
        let sp = parse_animation_shorthand("k1 1s, k2 2s").unwrap_or_else(|| panic!("应解析成功"));
        assert_eq!(sp.name, "k1");
        assert_eq!(sp.duration, 1.0);
    }

    /// 车道IE：省略迭代计数的简写缺省 = **1 次**（CSS 初始值），非无限
    /// ——旧缺省 None（无限）使 `animation: fade 1s forwards` 在 t>dur
    /// 回卷重播（Chrome 钉扎探针 ie4：held 终态 vs 我方 t mod dur 帧，
    /// 40.5% 差分）。显式 infinite 才是无限；显式数字照常生效。
    #[test]
    fn animation_shorthand_default_iteration_is_one() {
        let dflt = parse_animation_shorthand("fade 1s forwards").expect("合法简写");
        assert_eq!(dflt.iteration_count, Some(1), "缺省迭代数 = 1");
        assert!(dflt.fill_forwards);
        // 缺省 1 次 + forwards：t=1.5s → Held(1.0)（修复前 = Active 0.5 回卷）
        assert_eq!(
            css_anim_phase(&dflt, 1.5),
            CssAnimPhase::Held { progress: 1.0 }
        );
        // 显式 infinite → None（采样端无限循环）
        let inf = parse_animation_shorthand("k 2s linear infinite").expect("合法简写");
        assert_eq!(inf.iteration_count, None);
        assert_eq!(
            css_anim_phase(&inf, 2.5),
            CssAnimPhase::Active { progress: 0.25 }
        );
        // 显式数字照常
        let n3 = parse_animation_shorthand("k 1s 3").expect("合法简写");
        assert_eq!(n3.iteration_count, Some(3));
    }

    /// 迭代求值：正向 / alternate 奇偶翻转 / 用尽 Ended / forwards Held /
    /// delay 前_before / duration 0 防御。
    #[test]
    fn css_anim_phase_directions_and_fill() {
        let mk = |dir: CssDirection, iter: Option<u32>, fill: bool, delay: f64| CssAnimationSpec {
            name: String::from("k"),
            duration: 1.0,
            delay,
            iteration_count: iter,
            direction: dir,
            fill_forwards: fill,
            timing: CssTiming::Linear,
        };
        // 正向无限：t=0.25 → 0.25；t=2.5（第 3 轮中点）→ 0.5
        let norm = mk(CssDirection::Normal, None, false, 0.0);
        assert_eq!(
            css_anim_phase(&norm, 0.25),
            CssAnimPhase::Active { progress: 0.25 }
        );
        assert_eq!(
            css_anim_phase(&norm, 2.5),
            CssAnimPhase::Active { progress: 0.5 }
        );
        // alternate：偶轮正向、奇轮反向（t=1.25 → 第 2 轮 frac 0.25 → 0.75）
        let alt = mk(CssDirection::Alternate, None, false, 0.0);
        assert_eq!(
            css_anim_phase(&alt, 0.25),
            CssAnimPhase::Active { progress: 0.25 }
        );
        assert_eq!(
            css_anim_phase(&alt, 1.25),
            CssAnimPhase::Active { progress: 0.75 }
        );
        assert_eq!(
            css_anim_phase(&alt, 1.0),
            CssAnimPhase::Active { progress: 1.0 }
        );
        // 用尽无 fill → Ended；forwards → Held(progress 1)
        let n2 = mk(CssDirection::Normal, Some(2), false, 0.0);
        assert_eq!(
            css_anim_phase(&n2, 1.5),
            CssAnimPhase::Active { progress: 0.5 }
        );
        assert_eq!(css_anim_phase(&n2, 2.0), CssAnimPhase::Ended);
        let n2f = mk(CssDirection::Normal, Some(2), true, 0.0);
        assert_eq!(
            css_anim_phase(&n2f, 5.0),
            CssAnimPhase::Held { progress: 1.0 }
        );
        // alternate 用尽 forwards：末轮（第 2 轮，反向）末态 = 0
        let altf = mk(CssDirection::Alternate, Some(2), true, 0.0);
        assert_eq!(
            css_anim_phase(&altf, 9.0),
            CssAnimPhase::Held { progress: 0.0 }
        );
        // delay 之前 → Before；delay 后 elapsed=0.25 → 迭代进度 0.25
        let dl = mk(CssDirection::Normal, None, false, 0.5);
        assert_eq!(css_anim_phase(&dl, 0.4), CssAnimPhase::Before);
        assert_eq!(
            css_anim_phase(&dl, 0.75),
            CssAnimPhase::Active { progress: 0.25 }
        );
        // 负 delay（车道CJ 钉死）：t=0 直接跳入迭代中段（-0.5s → 第 2 轮
        // 半程）。alternate 下奇轮反向。
        let neg = mk(CssDirection::Normal, None, false, -0.5);
        assert_eq!(
            css_anim_phase(&neg, 0.0),
            CssAnimPhase::Active { progress: 0.5 }
        );
        let negalt = mk(CssDirection::Alternate, None, false, -1.5);
        // t=0 → iter_f=1.5：第 2 轮（奇，反向）frac 0.5 → progress 0.5
        assert_eq!(
            css_anim_phase(&negalt, 0.0),
            CssAnimPhase::Active { progress: 0.5 }
        );
        // iteration-count 小数（车道CJ 登记：解析期 floor——2.5 轮按 2 轮）
        assert!(parse_animation_shorthand("k 1s 2.5").is_some());
        // duration 0 / NaN 防御
        let zero = CssAnimationSpec {
            duration: 0.0,
            ..norm.clone()
        };
        assert_eq!(css_anim_phase(&zero, 1.0), CssAnimPhase::Ended);
        assert_eq!(css_anim_phase(&norm, f64::NAN), CssAnimPhase::Ended);
    }

    /// CSS transform 函数子集解析与插值。
    #[test]
    fn css_xf_parse_and_lerp() {
        // 车道NF：keyframes transform 分量的绝对单位（96px 基准换算；
        // f32 系数有 ulp 级量化，断言用 1e-4 容差）。
        let tx = |v: &str| -> Option<(f64, f64)> {
            match parse_css_xf(v) {
                Some(CssXf::Translate(x, y)) => Some((x, y)),
                _ => None,
            }
        };
        let (x, y) = tx("translateX(12pt)").unwrap_or_default();
        assert!((x - 16.0).abs() < 1e-4 && y == 0.0, "{x}");
        let (x, y) = tx("translate(0.25in, 25.4mm)").unwrap_or_default();
        assert!((x - 24.0).abs() < 1e-3 && (y - 96.0).abs() < 1e-2);
        let (x, y) = tx("translateY(-6PT)").unwrap_or_default();
        assert!(x == 0.0 && (y + 8.0).abs() < 1e-4, "{y}");
        assert_eq!(
            parse_css_xf("translateX(-90px)"),
            Some(CssXf::Translate(-90.0, 0.0))
        );
        assert_eq!(parse_css_xf("rotate(360deg)"), Some(CssXf::Rotate(360.0)));
        assert_eq!(
            parse_css_xf("translate(10px, 20px)"),
            Some(CssXf::Translate(10.0, 20.0))
        );
        assert_eq!(
            parse_css_xf("translateY(5px)"),
            Some(CssXf::Translate(0.0, 5.0))
        );
        assert_eq!(parse_css_xf("scale(2)"), None);
        assert_eq!(parse_css_xf("translateX(1)"), None); // 分量必须带单位
                                                         // 无单位零合法（CSS 零值免单位；s6-05 slide `translateX(0)`）
        assert_eq!(
            parse_css_xf("translateX(0)"),
            Some(CssXf::Translate(0.0, 0.0))
        );
        assert_eq!(
            parse_css_xf("translateX(0.0)"),
            Some(CssXf::Translate(0.0, 0.0))
        );
        assert_eq!(parse_css_xf("rotate(0)"), Some(CssXf::Rotate(0.0)));
        assert_eq!(
            parse_css_xf("translate(0, 10px)"),
            Some(CssXf::Translate(0.0, 10.0))
        );
        assert_eq!(
            lerp_css_xf(
                CssXf::Translate(-90.0, 0.0),
                CssXf::Translate(60.0, 0.0),
                0.5
            ),
            Some(CssXf::Translate(-15.0, 0.0))
        );
        assert_eq!(
            lerp_css_xf(CssXf::Rotate(0.0), CssXf::Rotate(360.0), 0.25),
            Some(CssXf::Rotate(90.0))
        );
        // 异变体不可插值
        assert_eq!(
            lerp_css_xf(CssXf::Translate(0.0, 0.0), CssXf::Rotate(90.0), 0.5),
            None
        );
    }
}
