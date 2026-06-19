//! M66: JS 引擎抽象层。
//!
//! 目标：让 boa 和 QuickJS 可共存、可切换。
//!
//! 设计原则：
//! - bridge.rs 的 49 个 NativeFn 保持 boa 原生签名（`fn(&JsValue, &[JsValue],
//!   &mut Context) -> JsResult<JsValue>`），因为它们需要 boa 的 Context 做 eval
//!   和 JsObject 存储。
//! - trait 只抽象"引擎生命周期"：创建 + 安装 bridge + 安装 shim + eval + run_jobs + gc。
//! - 通过泛型 `E: JsEngine` 而非 `dyn JsEngine` 避免动态分发的 downcast 需求。
//!
//! 架构：
//! ```text
//! ┌─────────────────────────────────────────────┐
//!  │  scripts.rs（编排层）                        │
//!  │  fn run_scripts<E: JsEngine>(engine: E)     │
//!  ├─────────────────────────────────────────────┤
//!  │  bridge.rs（boa 原生 bridge）                │  ← 保持不变
//!  │  install(ctx: &mut boa::Context)            │
//!  ├─────────────────────────────────────────────┤
//!  │  engine.rs（trait）                         │
//!  │  trait JsEngine { fn ctx_mut(&mut self)     │
//!  │    -> &mut boa::Context; ... }              │
//!  ├─────────────────────────────────────────────┤
//!  │  engine_boa.rs    │  engine_quickjs.rs      │
//!  │  BoaEngine        │  QuickJsEngine (TODO)   │
//!  └─────────────────────────────────────────────┘
//! ```

use boa_engine::Context;

/// M66: JS 引擎抽象。
///
/// 当前只有 BoaEngine 实现。QuickJsEngine 待 M66-B 阶段实现。
///
/// 设计：trait 提供 `ctx_mut() -> &mut Context`（boa Context），
/// 因为 bridge.rs 的所有 `__*` 函数都需要 boa Context。
/// QuickJS 后端实现时，如果 API 不兼容，会重构 bridge 层。
pub trait JsEngine {
    /// 获取底层 boa Context（bridge 函数注册 + eval 需要）。
    fn ctx_mut(&mut self) -> &mut Context;

    /// 创建一个带 module loader 的引擎（ESM 支持用）。
    /// 默认实现返回 false（不支持 ESM）。
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
    QuickJs, // M66-B 阶段实现
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
