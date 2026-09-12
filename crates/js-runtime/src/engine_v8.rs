//! M95/ADR-0006: V8 可选引擎后端（`--features v8`，默认不编译）。
//!
//! 与 Chrome 同源（同引擎/同 Skia/同 ICU）——设计目标：一次消解
//! etsl/toSourceError/canvasFingerprint 三个跨引擎差异（M94 系列
//! 攻坚中 QuickJS 的结构性天花板）。
//!
//! M96.1: 真实初始化 + eval pipeline（API 序列从 /tmp/v8eval 实测
//! 验证版移植）。进程级 V8 platform 只初始化一次（OnceLock）；
//! isolate 每次运行创建。不调用 `V8::dispose()`（unsafe——宪法
//! unsafe 只进 cli crate；进程退出由 OS 回收，deno 同样做法）。

use std::sync::OnceLock;

/// 进程级 V8 初始化（platform + engine，只能做一次）。
static V8_INIT: OnceLock<()> = OnceLock::new();

pub fn ensure_v8_initialized() {
    V8_INIT.get_or_init(|| {
        let platform = rusty_v8::SharedRef::from(rusty_v8::new_default_platform(0, false));
        rusty_v8::V8::initialize_platform(platform);
        rusty_v8::V8::initialize();
    });
}

/// V8 运行器：一个 isolate + 全局 context。
pub struct V8Engine {
    isolate: rusty_v8::OwnedIsolate,
}

impl V8Engine {
    pub fn new() -> Option<Self> {
        ensure_v8_initialized();
        Some(V8Engine {
            isolate: rusty_v8::Isolate::new(Default::default()),
        })
    }

    /// eval 一段 JS，返回结果的字符串形式（对齐 eval_string 语义）。
    pub fn eval_string(&mut self, js: &str) -> Option<String> {
        let mut hs = rusty_v8::HandleScope::new(&mut self.isolate);
        let context = rusty_v8::Context::new(&mut hs);
        let mut cs = rusty_v8::ContextScope::new(&mut hs, context);
        let code = rusty_v8::String::new(&mut cs, js)?;
        let script = rusty_v8::Script::compile(&mut cs, code, None)?;
        let result = script.run(&mut cs)?;
        let s = result.to_string(&mut cs)?;
        Some(s.to_rust_string_lossy(&mut cs))
    }

    /// eval 且不取返回值（安装 shim 用）。
    pub fn eval_install(&mut self, js: &str) -> bool {
        let mut hs = rusty_v8::HandleScope::new(&mut self.isolate);
        let context = rusty_v8::Context::new(&mut hs);
        let mut cs = rusty_v8::ContextScope::new(&mut hs, context);
        match rusty_v8::String::new(&mut cs, js)
            .and_then(|code| rusty_v8::Script::compile(&mut cs, code, None))
        {
            Some(script) => script.run(&mut cs).is_some(),
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v8_pipeline_eval() {
        let mut e = V8Engine::new().expect("v8 init");
        assert_eq!(
            e.eval_string("'v8-alive ' + (1+1)").as_deref(),
            Some("v8-alive 2")
        );
        // QuickJS 天花板项的同源验证：V8 原生函数 toString 单行（etsl 语义）
        let ts = e.eval_string("eval.toString()").unwrap_or_default();
        assert!(
            ts.contains("native code") && !ts.contains('\n'),
            "V8 native fn toString 应为单行 native 格式，got: {ts}"
        );
        // TypeError 文案（toSourceError 语义）
        let msg = e
            .eval_string("try { null.usdfsh } catch(t) { t.toString() }")
            .unwrap_or_default();
        assert!(
            msg.contains("Cannot read properties of null (reading 'usdfsh')"),
            "V8 TypeError 文案应与 Chrome 同源，got: {msg}"
        );
    }
}
