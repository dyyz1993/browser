//! M66: JS 引擎抽象层。
//!
//! 让 boa 和 QuickJS 可共存、可切换。
//!
//! 架构设计：
//! - **JS shim 层（5400 行纯 JS 字符串）完全引擎无关**——任何引擎 eval 同样的 JS。
//! - **bridge 层**：每个引擎有自己的 bridge 实现（boa 用 NativeFn，QuickJS 用 Function::new）。
//!   但 bridge 调用的 Rust 后端（thread_local DOM/storage/nav）完全共享。
//! - **scripts.rs 编排层**：通过 trait 调用引擎，不关心底层是 boa 还是 QuickJS。
//!
//! trait 方法：
//! - `install_and_run`：安装 bridge + shim + 执行脚本（每个引擎自己的实现）
//! - 接收一个 `Box<dyn BridgeHost>` trait 对象，提供引擎无关的 bridge 后端调用

use boa_engine::Context;

/// M66: JS 引擎抽象。
///
/// boa 后端直接暴露 Context（bridge.rs 直接操作）。
/// QuickJS 后端（M66-B）会重构为不依赖此 trait 的独立实现路径。
pub trait JsEngine {
    /// 获取底层 boa Context（bridge 函数注册 + eval 需要）。
    /// 仅 boa 后端实现。QuickJS 后端不实现此方法（调用时 panic）。
    fn ctx_mut(&mut self) -> &mut Context;

    /// 创建一个带 module loader 的引擎（ESM 支持用）。
    fn supports_esm(&self) -> bool {
        false
    }

    /// 引擎名称（用于 --profile 日志）。
    fn name(&self) -> &'static str;
}

/// M66: 默认引擎选择器。根据 CLI flag 创建对应引擎。
pub enum EngineKind {
    Boa,
    #[allow(dead_code)]
    QuickJs,
}

impl EngineKind {
    #[must_use]
    pub fn parse_str(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "quickjs" | "qjs" => Self::QuickJs,
            _ => Self::Boa,
        }
    }

    /// 创建引擎实例。如果需要 ESM 支持，传入 origin URL。
    pub fn create(&self, esm_origin: Option<&str>) -> Box<dyn JsEngine> {
        match self {
            Self::Boa => Box::new(crate::engine_boa::BoaEngine::new(esm_origin)),
            Self::QuickJs => {
                // M66-B: QuickJS 后端尚未实现，回退到 boa。
                eprintln!("[js-runtime] QuickJS engine not yet implemented, falling back to boa");
                Box::new(crate::engine_boa::BoaEngine::new(esm_origin))
            }
        }
    }
}
