//! M66: BoaEngine —— boa 引擎的 JsEngine trait 实现。
//!
//! 封装 boa::Context 的创建逻辑（含可选的 HttpModuleLoader for ESM）。
//! bridge.rs 的 install() 和所有 shim 的 install_xxx() 仍然直接操作
//! boa::Context（通过 engine.ctx_mut() 获取）。

use std::rc::Rc;

use boa_engine::Context;

use crate::engine::JsEngine;
use crate::esm_loader::HttpModuleLoader;

/// M66: boa 引擎封装。
pub struct BoaEngine {
    ctx: Context,
    has_esm: bool,
}

impl BoaEngine {
    /// 创建引擎。如果有 origin URL 且页面含 ESM module 脚本，
    /// 用 HttpModuleLoader 创建 Context（支持 import/export/import.meta）。
    pub fn new(esm_origin: Option<&str>) -> Self {
        let (ctx, has_esm) = if let Some(origin) = esm_origin {
            let loader = Rc::new(HttpModuleLoader::new(origin));
            let ctx = Context::builder()
                .module_loader(loader)
                .build()
                .unwrap_or_else(|_| Context::default());
            (ctx, true)
        } else {
            (Context::default(), false)
        };
        Self { ctx, has_esm }
    }
}

impl JsEngine for BoaEngine {
    fn ctx_mut(&mut self) -> &mut Context {
        &mut self.ctx
    }

    fn supports_esm(&self) -> bool {
        self.has_esm
    }

    fn name(&self) -> &'static str {
        "boa"
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}
