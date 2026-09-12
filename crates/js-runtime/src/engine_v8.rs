//! M95/ADR-0006: V8 可选引擎后端（`--features v8`，默认不编译）。
//!
//! 与 Chrome 同源（同引擎/同 Skia/同 ICU）——设计目标：一次消解
//! etsl/toSourceError/canvasFingerprint 三个跨引擎差异（M94 系列
//! 攻坚中 QuickJS 的结构性天花板）。
//!
//! 架构：镜像 `engine_quickjs.rs` 的桥模式——5400 行 JS shim
//! （scripts.rs 常量）引擎无关直接复用；`__*` 全局桥经 rusty_v8
//! Function API 注册。本文件为 M96 渐进集成骨架。

#![allow(dead_code)]

/// V8 引擎句柄（进程级 platform + 每次运行 isolate）。
pub struct V8Engine {
    // M96: isolate_handle + 上下文
    _private: (),
}

impl V8Engine {
    pub fn new() -> Option<Self> {
        // M96 骨架：platform/isolate 初始化（参照 /tmp/v8eval 验证过的
        // API 序列——SharedRef platform / Isolate / HandleScope / Context）
        Some(V8Engine { _private: () })
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn placeholder() {
        // M96: 真实测试随桥层落地
    }
}
