//! M66-B: QuickJS 引擎后端（rquickjs）。
//!
//! 用 rquickjs 绑定 QuickJS（Bellard 的轻量 JS 引擎），作为 boa 的替代。
//! 5400 行 JS shim 完全复用（和 boa eval 同样的 JS 字符串）。
//! Rust bridge 函数用 Function::new 注册，调同样的 thread_local 后端。

#![cfg(feature = "quickjs")]

use rquickjs::function::Rest;
use rquickjs::{Context, Ctx, Function, Runtime, Value};

use crate::bridge;

/// M66: QuickJsEngine 的 JsEngine trait 包装器。
/// ctx_mut() 会 panic——scripts.rs 通过 name() == "quickjs" 检测后走专用路径。
pub struct QuickJsEngineWrapper {
    inner: QuickJsEngine,
}

impl QuickJsEngineWrapper {
    pub fn new(esm_origin: Option<&str>) -> Self {
        Self {
            inner: QuickJsEngine::new(esm_origin),
        }
    }

    pub fn engine(&mut self) -> &mut QuickJsEngine {
        &mut self.inner
    }
}

impl crate::engine::JsEngine for QuickJsEngineWrapper {
    fn ctx_mut(&mut self) -> &mut boa_engine::Context {
        panic!("QuickJS engine does not support ctx_mut() — use QuickJsEngine::eval() directly");
    }

    fn supports_esm(&self) -> bool {
        false // TODO: rquickjs Module API
    }

    fn name(&self) -> &'static str {
        "quickjs"
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

/// M66: QuickJS 引擎封装。
pub struct QuickJsEngine {
    rt: Runtime,
    ctx: Context,
}

impl QuickJsEngine {
    pub fn new(_esm_origin: Option<&str>) -> Self {
        let rt = Runtime::new().expect("QuickJS runtime");
        let ctx = Context::full(&rt).expect("QuickJS context");
        let mut engine = Self { rt, ctx };
        engine.install_bridge();
        engine
    }

    /// 注册所有 bridge 函数 + Web API 桩。
    fn install_bridge(&mut self) {
        self.ctx
            .with(|ctx: Ctx| {
                let g = ctx.globals();

                // === 日志 ===
                let _ = g.set(
                    "__log",
                    Function::new(ctx.clone(), |args: Rest<Value>| {
                        for v in &args.0 {
                            if let Some(s) = v.as_string() {
                                if let Ok(s) = s.to_string() {
                                    eprintln!("[js] {s}");
                                }
                            }
                        }
                    })
                    .unwrap(),
                );

                // === DOM bridge ===
                let _ = g.set("__createEl", Function::new(ctx.clone(), |tag: String| bridge::qjs_bridge::create_el(tag)).unwrap());
                let _ = g.set("__appendChild", Function::new(ctx.clone(), |p: f64, c: f64| bridge::qjs_bridge::append_child(p, c)).unwrap());
                let _ = g.set("__insertBefore", Function::new(ctx.clone(), |p: f64, c: f64, r: f64| bridge::qjs_bridge::insert_before(p, c, r)).unwrap());
                let _ = g.set("__removeChild", Function::new(ctx.clone(), |p: f64, c: f64| bridge::qjs_bridge::remove_child(p, c)).unwrap());
                let _ = g.set("__setText", Function::new(ctx.clone(), |id: f64, t: String| bridge::qjs_bridge::set_text(id, t)).unwrap());
                let _ = g.set("__getText", Function::new(ctx.clone(), |id: f64| bridge::qjs_bridge::get_text(id)).unwrap());
                let _ = g.set("__getTag", Function::new(ctx.clone(), |id: f64| bridge::qjs_bridge::get_tag(id)).unwrap());
                let _ = g.set("__getTagName", Function::new(ctx.clone(), |id: f64| bridge::qjs_bridge::get_tag(id)).unwrap());
                let _ = g.set("__getAttr", Function::new(ctx.clone(), |id: f64, k: String| bridge::qjs_bridge::get_attr(id, k)).unwrap());
                let _ = g.set("__setAttr", Function::new(ctx.clone(), |id: f64, k: String, v: String| bridge::qjs_bridge::set_attr(id, k, v)).unwrap());
                let _ = g.set("__removeAttr", Function::new(ctx.clone(), |id: f64, k: String| bridge::qjs_bridge::remove_attr(id, k)).unwrap());
                let _ = g.set("__getElById", Function::new(ctx.clone(), |id: String| bridge::qjs_bridge::get_el_by_id(id)).unwrap());
                let _ = g.set("__qs", Function::new(ctx.clone(), |s: String| bridge::qjs_bridge::qs(s)).unwrap());
                let _ = g.set("__qsAll", Function::new(ctx.clone(), |s: String| bridge::qjs_bridge::qs_all(s)).unwrap());
                let _ = g.set("__getBody", Function::new(ctx.clone(), |_: f64| bridge::qjs_bridge::get_body()).unwrap());
                let _ = g.set("__setTitle", Function::new(ctx.clone(), |t: String| bridge::qjs_bridge::set_title(t)).unwrap());
                let _ = g.set("__getParent", Function::new(ctx.clone(), |id: f64| bridge::qjs_bridge::get_parent(id)).unwrap());
                let _ = g.set("__children", Function::new(ctx.clone(), |id: f64| bridge::qjs_bridge::children(id)).unwrap());
                let _ = g.set("__getValue", Function::new(ctx.clone(), |id: f64| bridge::qjs_bridge::get_attr(id, "value".to_string()).unwrap_or_default()).unwrap());
                let _ = g.set("__setValue", Function::new(ctx.clone(), |id: f64, v: String| bridge::qjs_bridge::set_attr(id, "value".to_string(), v)).unwrap());
                let _ = g.set("__click", Function::new(ctx.clone(), |_: f64| {}).unwrap());
                let _ = g.set("__submit", Function::new(ctx.clone(), |_: f64| {}).unwrap());

                // === setBody / appendBody / fetchSetBody / fetchAppendBody ===
                let _ = g.set("__setBody", Function::new(ctx.clone(), |html: String| {
                    bridge::qjs_bridge::set_attr(bridge::qjs_bridge::get_body(), "innerHTML".to_string(), html);
                }).unwrap());
                let _ = g.set("__appendBody", Function::new(ctx.clone(), |html: String| {
                    // 简化：设 innerHTML（和 setBody 一样，爬虫够用）
                    bridge::qjs_bridge::set_attr(bridge::qjs_bridge::get_body(), "innerHTML".to_string(), html);
                }).unwrap());

                // === Fetch ===
                let _ = g.set("__fetchSync", Function::new(ctx.clone(), |url: String| bridge::qjs_bridge::fetch_sync(url)).unwrap());
                let _ = g.set("__fetchSyncMethod", Function::new(ctx.clone(), |url: String, method: String, body: Option<String>, ct: Option<String>| {
                    bridge::qjs_bridge::fetch_sync_method(url, method, body, ct)
                }).unwrap());

                // === Storage ===
                let _ = g.set("__storageGet", Function::new(ctx.clone(), |k: String| bridge::qjs_bridge::storage_get(k)).unwrap());
                let _ = g.set("__storageSet", Function::new(ctx.clone(), |k: String, v: String| bridge::qjs_bridge::storage_set(k, v)).unwrap());
                let _ = g.set("__storageRemove", Function::new(ctx.clone(), |k: String| bridge::qjs_bridge::storage_remove(k)).unwrap());
                let _ = g.set("__storageClear", Function::new(ctx.clone(), || {}).unwrap());
                let _ = g.set("__storageLen", Function::new(ctx.clone(), || 0i32).unwrap());
                let _ = g.set("__storageKey", Function::new(ctx.clone(), |_: f64| Option::<String>::None).unwrap());

                // === Location ===
                let _ = g.set("__locationHref", Function::new(ctx.clone(), || bridge::qjs_bridge::location_href()).unwrap());
                let _ = g.set("__locationReplace", Function::new(ctx.clone(), |_: String| {}).unwrap());
                let _ = g.set("__locationAssign", Function::new(ctx.clone(), |_: String| {}).unwrap());

                // === Timer（简化版——QuickJS event loop 后续完善）===
                let _ = g.set("setTimeout", Function::new(ctx.clone(), |_cb: Value, _delay: f64| 0i32).unwrap());
                let _ = g.set("clearTimeout", Function::new(ctx.clone(), |_: f64| {}).unwrap());
                let _ = g.set("setInterval", Function::new(ctx.clone(), |_cb: Value, _delay: f64| 0i32).unwrap());
                let _ = g.set("clearInterval", Function::new(ctx.clone(), |_: f64| {}).unwrap());

                // === History（桩）===
                let _ = g.set("__historyPush", Function::new(ctx.clone(), |_: String, _: String, _: String| {}).unwrap());
                let _ = g.set("__historyReplace", Function::new(ctx.clone(), |_: String, _: String, _: String| {}).unwrap());
                let _ = g.set("__historyBack", Function::new(ctx.clone(), |_: f64| {}).unwrap());
                let _ = g.set("__historyForward", Function::new(ctx.clone(), |_: f64| {}).unwrap());
                let _ = g.set("__historyGo", Function::new(ctx.clone(), |_: f64| {}).unwrap());
                let _ = g.set("__historyLen", Function::new(ctx.clone(), || 1i32).unwrap());
                let _ = g.set("__historyState", Function::new(ctx.clone(), || Option::<String>::None).unwrap());
                // __locationParts 返回 URL 解析对象。rquickjs 的 Function 闭包不能返回
                // 非 'static 的 JS Value，所以返回 JSON 字符串，JS shim 解析。
                let _ = g.set("__locationParts", Function::new(ctx.clone(), || {
                    r#"{"href":"about:blank","protocol":"about:","host":"","pathname":"","search":"","hash":""}"#.to_string()
                }).unwrap());

                // === XHR（桩——后续完善）===
                let _ = g.set("__xhrCreate", Function::new(ctx.clone(), || 0i32).unwrap());
                let _ = g.set("__xhrOpen", Function::new(ctx.clone(), |_: f64, _: String, _: String| {}).unwrap());
                let _ = g.set("__xhrSend", Function::new(ctx.clone(), |_: f64| {}).unwrap());
                let _ = g.set("__xhrGetResponseText", Function::new(ctx.clone(), |_: f64| Option::<String>::None).unwrap());

                // === WebSocket（桩）===
                let _ = g.set("__wsCreate", Function::new(ctx.clone(), |_: String| 0i32).unwrap());
                let _ = g.set("__wsSend", Function::new(ctx.clone(), |_: f64, _: String| {}).unwrap());
                let _ = g.set("__wsClose", Function::new(ctx.clone(), |_: f64| {}).unwrap());

                // === HTML 解析 ===
                let _ = g.set("__parseHtml", Function::new(ctx.clone(), |_target: f64, _html: String| {}).unwrap());

                // === makeElement（JS shim 工厂，返回 undefined 占位——shim JS 自己创建）===
                // JS shim 的 __makeElement 需要返回一个带 __nodeId 的对象。
                // QuickJS 版本让 JS shim 自己处理（返回 undefined，shim 有 fallback）。

                Ok::<(), rquickjs::Error>(())
            })
            .ok();
    }

    /// Eval JS 字符串（复用 5400 行 JS shim）。
    pub fn eval(&mut self, js: &str) -> Result<(), String> {
        self.ctx
            .with(|ctx: Ctx| ctx.eval::<(), _>(js))
            .map_err(|e: rquickjs::Error| format!("{e:?}"))?;
        Ok(())
    }

    /// 运行微任务队列。
    pub fn run_jobs(&mut self) {
        // rquickjs 微任务在 ctx.with 闭包退出时自动 drain。
    }

    /// 手动触发 GC。
    pub fn gc(&mut self) {
        let _ = self.rt.run_gc();
    }
}

impl Drop for QuickJsEngine {
    fn drop(&mut self) {
        let _ = self.rt.run_gc();
    }
}
