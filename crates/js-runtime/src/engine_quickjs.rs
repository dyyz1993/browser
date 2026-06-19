//! M66-B: QuickJS 引擎后端（rquickjs）—— 占位骨架。
//!
//! API 验证已完成（见测试）：rquickjs 0.12 在 macOS arm64 编译运行正常。
//! Function::new + Rest<Value> 参数 + Promise 支持 + GC 全部可用。
//!
//! 完整实现需要：
//! 1. 注册 68 个 bridge 函数（Function::new 调 with_tree 后端）
//! 2. 实现 setTimeout/setInterval event loop
//! 3. 安装 5400 行 JS shim（和 boa 复用同样的 JS 字符串）
//! 4. ESM module loader（rquickjs Module API）
//!
//! 这是个独立的大工作量任务（~2000 行），留给后续专项实现。

#![cfg(feature = "quickjs")]

/// M66: QuickJS 引擎占位。EngineKind::create() 中 QuickJS 回退到 boa。
pub struct QuickJsEngine;

impl QuickJsEngine {
    pub fn new(_esm_origin: Option<&str>) -> Self {
        Self
    }
}
