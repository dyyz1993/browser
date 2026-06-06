//! `browser-eventloop` — 纯算法的事件循环 timer 后端（M16.1）。
//!
//! 设计动机：见 `docs/decisions/0002-boa-settimeout-vs-deno-core.md`。
//! 本 crate 只做**时间管理 + id 生成**，不碰 boa 引擎（避免依赖方向倒置）。
//! `JsObject` 回调的存储由 js-runtime 层的 thread-local slot 负责（M16.2）。
//!
//! 三大职责：
//! 1. `TimerWheel::schedule(deadline) -> TimerId`：注册一个未来触发的 timer
//! 2. `TimerWheel::cancel(id)`：取消（用于 clearTimeout）
//! 3. `TimerWheel::drain_due(now) -> Vec<TimerId>`：取出所有已到期的 timer id，
//!    按注册顺序返回（保证 FIFO，符合 HTML spec 对 timer 顺序的要求）
//!
//! 调用方（js-runtime event loop）拿到 due ids 后，按顺序执行对应回调。

use std::time::Instant;

/// Timer 的唯一标识。`schedule` 返回，`cancel` / `__clearTimeout` 用。
/// 用 `NonZero` 让 `Option<TimerId>` 走 niche 优化（0 = None）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TimerId(u64);

impl TimerId {
    /// 给测试用的公共访问器（生产代码用 `schedule` 返回值）。
    #[must_use]
    pub const fn raw(self) -> u64 {
        self.0
    }

    /// 从原始 `u64` 重建 `TimerId`（JS 桥跨边界传递后还原）。
    /// M16.2: `setTimeout` 返回 number 给 JS，`clearTimeout` 接收 number 后还原。
    #[must_use]
    pub const fn from_raw(n: u64) -> Self {
        Self(n)
    }
}

/// 单个 timer 记录：到期时刻 + 注册序号（FIFO 稳定排序用）。
#[derive(Debug, Clone, Copy)]
struct TimerEntry {
    deadline: Instant,
    seq: u64,
}

/// 时间轮 timer 后端。纯算法，无 IO / 无回调。
///
/// 内部用 `Vec<(TimerId, TimerEntry)>` + lazy cancel（cancel 只打标，
/// `drain_due` 时跳过），避免 HashMap rehash + 避免删除时移动元素。
#[derive(Debug, Default)]
pub struct TimerWheel {
    /// 下一个分配的 id（从 1 开始，0 保留给 niche）。
    next_id: u64,
    /// 下一个分配的 seq（保证 FIFO：相同 deadline 时按 seq 升序）。
    next_seq: u64,
    /// 活跃 timer 列表（未 cancel 的 + 已 cancel 但未清理的）。
    timers: Vec<(TimerId, TimerEntry)>,
    /// 已 cancel 的 id 集合（drain_due 时跳过 + 顺便清理）。
    cancelled: Vec<TimerId>,
}

impl TimerWheel {
    /// 新建空时间轮。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 注册一个 timer，`delay_ms` 毫秒后到期。返回 `TimerId`。
    ///
    /// `delay_ms` 为 0 或溢出时按 HTML spec：clamp 到"立即到期"（now）。
    pub fn schedule(&mut self, delay_ms: u64, now: Instant) -> TimerId {
        self.next_id = self.next_id.checked_add(1).expect("timer id overflow");
        let id = TimerId(self.next_id);
        let deadline = now.checked_add(std::time::Duration::from_millis(delay_ms));
        let deadline = deadline.unwrap_or(now); // 溢出 → 立即到期
        self.next_seq = self.next_seq.checked_add(1).expect("timer seq overflow");
        let entry = TimerEntry {
            deadline,
            seq: self.next_seq,
        };
        self.timers.push((id, entry));
        id
    }

    /// 取消一个 timer。已触发或不存在或已 cancel 的 id 都安全调用（幂等）。
    pub fn cancel(&mut self, id: TimerId) {
        // 仅当该 id 仍活跃时才打 cancel 标记，否则忽略。
        if self.timers.iter().any(|(tid, _)| *tid == id) {
            self.cancelled.push(id);
        }
    }

    /// 返回所有**已到期且未取消**的 timer id，按 FIFO 顺序。
    /// 取出的 timer 从内部移除（一次性），避免重复触发。
    pub fn drain_due(&mut self, now: Instant) -> Vec<TimerId> {
        // 1. 按到期顺序排：deadline 升序，同 deadline 按 seq 升序（FIFO）。
        self.timers.sort_by_key(|(_, e)| (e.deadline, e.seq));

        // 2. 分区：到期 + 未取消 → 返回；到期 + 已取消 → 丢弃；未到期 → 保留。
        let mut due = Vec::new();
        let mut remaining = Vec::new();
        for (id, entry) in self.timers.drain(..) {
            if now < entry.deadline {
                // 未到期 → 保留
                remaining.push((id, entry));
            } else if self.cancelled.contains(&id) {
                // 到期但已 cancel → 丢弃（不返回、不保留）
            } else {
                // 到期且未取消 → 返回
                due.push(id);
            }
        }
        // 3. 清理 cancelled 里已处理的 id（避免 cancelled 无限增长）。
        self.cancelled
            .retain(|c| remaining.iter().any(|(tid, _)| tid == c));
        self.timers = remaining;
        due
    }

    /// 当前活跃（未触发、未取消）的 timer 数量。测试 + 爬虫 networkidle 判断用。
    #[must_use]
    pub fn pending(&self) -> usize {
        self.timers
            .iter()
            .filter(|(id, _)| !self.cancelled.contains(id))
            .count()
    }

    /// 是否没有任何活跃 timer。networkidle 算法（M18）的核心信号。
    #[must_use]
    pub fn is_idle(&self) -> bool {
        self.pending() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn schedule_returns_distinct_increasing_ids() {
        let mut w = TimerWheel::new();
        let now = Instant::now();
        let a = w.schedule(100, now);
        let b = w.schedule(100, now);
        let c = w.schedule(100, now);
        assert_eq!(a.raw(), 1);
        assert_eq!(b.raw(), 2);
        assert_eq!(c.raw(), 3);
    }

    #[test]
    fn drain_due_empty_when_no_timers() {
        let mut w = TimerWheel::new();
        let now = Instant::now();
        assert!(w.drain_due(now).is_empty());
        assert!(w.is_idle());
    }

    #[test]
    fn drain_due_returns_zero_delay_immediately() {
        let mut w = TimerWheel::new();
        let now = Instant::now();
        let id = w.schedule(0, now);
        let due = w.drain_due(now);
        assert_eq!(due, vec![id]);
        assert!(w.is_idle());
    }

    #[test]
    fn drain_due_skips_not_yet_due() {
        let mut w = TimerWheel::new();
        let now = Instant::now();
        let id = w.schedule(1000, now); // 1s 后
        let due = w.drain_due(now);
        assert!(due.is_empty(), "未来 timer 不应被取出");
        assert_eq!(w.pending(), 1);
        // 推进时间 → 到期
        let later = now + Duration::from_millis(1000);
        let due = w.drain_due(later);
        assert_eq!(due, vec![id]);
        assert!(w.is_idle());
    }

    #[test]
    fn drain_due_fifo_order_for_same_deadline() {
        // 同一 deadline，按注册顺序触发（HTML spec 要求）。
        let mut w = TimerWheel::new();
        let now = Instant::now();
        let a = w.schedule(100, now);
        let b = w.schedule(100, now);
        let c = w.schedule(100, now);
        let later = now + Duration::from_millis(100);
        let due = w.drain_due(later);
        assert_eq!(due, vec![a, b, c]);
    }

    #[test]
    fn drain_due_orders_by_deadline_then_seq() {
        // 不同 deadline：先到期的先返回。
        let mut w = TimerWheel::new();
        let now = Instant::now();
        let late = w.schedule(500, now);
        let early = w.schedule(100, now);
        let mid = w.schedule(300, now);
        // 在三个都已到期的时间点 drain
        let far = now + Duration::from_millis(1000);
        let due = w.drain_due(far);
        assert_eq!(due, vec![early, mid, late]);
    }

    #[test]
    fn cancel_prevents_callback() {
        let mut w = TimerWheel::new();
        let now = Instant::now();
        let keep = w.schedule(100, now);
        let drop_ = w.schedule(100, now);
        w.cancel(drop_);
        let later = now + Duration::from_millis(100);
        let due = w.drain_due(later);
        assert_eq!(due, vec![keep], "被 cancel 的 timer 不应出现在结果里");
        assert!(w.is_idle());
    }

    #[test]
    fn cancel_is_idempotent() {
        let mut w = TimerWheel::new();
        let now = Instant::now();
        let id = w.schedule(100, now);
        w.cancel(id);
        w.cancel(id); // 二次 cancel 不应 panic
        w.cancel(id);
        let later = now + Duration::from_millis(100);
        assert!(w.drain_due(later).is_empty());
    }

    #[test]
    fn cancel_nonexistent_id_is_safe() {
        let mut w = TimerWheel::new();
        w.cancel(TimerId(9999)); // 不存在的 id
        assert_eq!(w.pending(), 0);
    }

    #[test]
    fn cancelled_timer_is_removed_on_drain() {
        // drain 后 cancelled 列表应清理，避免无限增长。
        let mut w = TimerWheel::new();
        let now = Instant::now();
        let a = w.schedule(100, now);
        let b = w.schedule(200, now);
        w.cancel(a);
        let t1 = now + Duration::from_millis(100);
        w.drain_due(t1); // a 被 cancel 丢弃，b 未到期保留
        assert_eq!(w.pending(), 1);
        // b 仍可正常触发
        let t2 = now + Duration::from_millis(200);
        let due = w.drain_due(t2);
        assert_eq!(due, vec![b]);
    }

    #[test]
    fn drain_due_is_one_shot() {
        // 同一 timer 不会触发两次。
        let mut w = TimerWheel::new();
        let now = Instant::now();
        let id = w.schedule(100, now);
        let later = now + Duration::from_millis(100);
        assert_eq!(w.drain_due(later), vec![id]);
        // 再次 drain（时间再推进）不应重复返回
        let far = now + Duration::from_millis(1000);
        assert!(w.drain_due(far).is_empty());
    }

    #[test]
    fn pending_counts_only_active_timers() {
        let mut w = TimerWheel::new();
        let now = Instant::now();
        let _a = w.schedule(100, now);
        let b = w.schedule(100, now);
        let _c = w.schedule(100, now);
        w.cancel(b);
        assert_eq!(w.pending(), 2, "pending 不应计入 cancelled 的");
        assert!(!w.is_idle());
    }

    #[test]
    fn mix_of_active_and_cancelled_timers() {
        let mut w = TimerWheel::new();
        let now = Instant::now();
        let a = w.schedule(100, now);
        let b = w.schedule(200, now);
        let c = w.schedule(300, now);
        w.cancel(b);
        let t1 = now + Duration::from_millis(150);
        assert_eq!(w.drain_due(t1), vec![a]);
        let t3 = now + Duration::from_millis(350);
        // b 已 cancel 丢弃，c 到期返回
        assert_eq!(w.drain_due(t3), vec![c]);
        assert!(w.is_idle());
    }

    #[test]
    fn new_timer_after_drain_gets_unique_id() {
        let mut w = TimerWheel::new();
        let now = Instant::now();
        let first = w.schedule(0, now);
        w.drain_due(now);
        let second = w.schedule(0, now);
        assert_ne!(first, second, "drain 后新 timer 应有更大 id");
    }
}
