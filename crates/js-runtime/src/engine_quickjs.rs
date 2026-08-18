//! M66-B: QuickJS 引擎后端（rquickjs）。
//!
//! 用 rquickjs 绑定 QuickJS（Bellard 的轻量 JS 引擎），作为 boa 的替代。
//! 5400 行 JS shim 完全复用（和 boa eval 同样的 JS 字符串）。
//! Rust bridge 函数用 Function::new 注册，调同样的 thread_local 后端。

use rquickjs::function::Rest;
use rquickjs::module::{Declared, Module};
use rquickjs::{Context, Ctx, Function, Runtime, Value};

use crate::bridge;

/// M66: HTTP Resolver —— 把相对路径解析成绝对 URL。
/// 注意：resolve 用的是 trait 传入的 `base` 参数，不存自身状态；
/// 结构体保留是因为 rquickjs 的 set_loader 需要具体类型实例。
pub struct HttpResolver;

impl rquickjs_core::loader::Resolver for HttpResolver {
    fn resolve<'js>(
        &mut self,
        _ctx: &Ctx<'js>,
        base: &str,
        name: &str,
        _attributes: Option<rquickjs_core::loader::ImportAttributes<'js>>,
    ) -> rquickjs_core::Result<String> {
        if name.starts_with("http://") || name.starts_with("https://") {
            return Ok(name.to_string());
        }
        // M76: 绝对路径（以 / 开头）——用 base URL 的 origin 拼成完整 URL。
        // Vite 的 import "/@fs/..." 和 "/@vite/client" 是以 / 开头的绝对路径。
        if name.starts_with('/') {
            if let Ok(base_url) = url::Url::parse(base) {
                if let Ok(full) = base_url.join(name) {
                    return Ok(full.to_string());
                }
            }
            return Ok(name.to_string());
        }
        let base_dir = base.rfind('/').map(|i| &base[..i]).unwrap_or(base);
        if let Some(stripped) = name.strip_prefix("./") {
            Ok(format!("{base_dir}/{stripped}"))
        } else if let Some(stripped) = name.strip_prefix("../") {
            let parent = base_dir
                .rfind('/')
                .map(|i| &base_dir[..i])
                .unwrap_or(base_dir);
            Ok(format!("{parent}/{stripped}"))
        } else {
            Ok(format!("{base_dir}/{name}"))
        }
    }
}

/// M66: HTTP Loader —— 用 fetch_sync 拉取远程 JS chunk，声明为 Module。
pub struct HttpLoader;

impl rquickjs_core::loader::Loader for HttpLoader {
    fn load<'js>(
        &mut self,
        ctx: &Ctx<'js>,
        name: &str,
        _attributes: Option<rquickjs_core::loader::ImportAttributes<'js>>,
    ) -> rquickjs_core::Result<Module<'js, Declared>> {
        let trace = std::env::var("BROWSER_TRACE_SCRIPTS").is_ok();
        eprintln!("[loader] fetch: {name}");
        let source = crate::bridge::fetch_sync(name).map_err(|e| {
            if trace {
                eprintln!("[loader] fetch_sync failed: {name}: {e}");
            }
            rquickjs_core::Error::new_loading(&format!("{name}: {e}"))
        })?;
        // M76: QuickJS 的 import.meta.env 不可赋（invalid assignment）。
        // 替换 import.meta.env → __vite_env__ 模块级变量 + 替换 import.meta.hot。
        let source = if source.contains("import.meta.env") {
            let patched = source.replace("import.meta.env", "__vite_env__");
            format!(
                "if(typeof __vite_env__==='undefined')var __vite_env__={{MODE:'production',DEV:false,PROD:true,SSR:false,BASE_URL:'/'}};
{patched}"
            )
        } else if source.contains("import.meta.hot") {
            format!(
                "if(typeof import.meta.hot==='undefined')import.meta.hot={{accept:function(){{}},dispose:function(){{}},on:function(){{}},decline:function(){{}},invalidate:function(){{}},data:{{}}}};
{source}"
            )
        } else {
            source
        };
        // M76bis: Vite React preamble 桩——模块级注入
        // @vitejs/plugin-react 注入的检测代码若找不到此 flag 则抛错
        let preamble_stub = "window.__vite_plugin_react_preamble_installed__=true;";
        let source = format!("{preamble_stub}{source}");
        // M76: UMD/CJS 模块（如 React/vendor）没有 export 语句 → Module::declare 创建空导出。
        // 加 export{}; 使其成为合法 ESM 模块（不导出任何东西，代码原地执行）。
        // M76fin: CJS 模块（无 export/import）可直接 eval 并捕获 namespace
        let is_cjs = !source.contains("export") && !source.contains("import");
        let wrapped = if is_cjs {
            format!("{source}\nexport{{}};")
        } else if wrapped_needs_named_exports(&source) {
            add_named_exports_to_source(source)
        } else {
            source
        };
        let declared = Module::declare(ctx.clone(), name, wrapped.as_bytes());
        match &declared {
            Ok(module) => {
                if trace {
                    eprintln!("[loader] declared OK: {name}");
                }
                if is_cjs {
                    // CJS 模块无依赖，可直接 eval 捕获 namespace
                    let _ = (|| -> Result<(), rquickjs::Error> {
                        let (evaluated, _promise) = module.clone().eval()?;
                        if let Ok(ns) = evaluated.namespace() {
                            let _ = ctx.catch();
                            let name_esc = name.replace('\\', "\\\\").replace('"', "\\\"");
                            let js = format!(
                                r#"if(typeof window.__vite_ns_registry__==='undefined')window.__vite_ns_registry__={{}};window.__vite_ns_registry__["{}"]=arguments[0]"#,
                                name_esc
                            );
                            if let Ok(f) =
                                ctx.eval::<rquickjs::Function, _>(format!("(function(ns){{{js}}})"))
                            {
                                let _ = f.call::<_, ()>((ns,));
                            }
                        }
                        Ok(())
                    })();
                }
            }
            Err(e) => {
                eprintln!("[loader] Module::declare FAILED: {name}: {e}");
            }
        }
        declared
    }
}

/// M77: 检测模块是否需要为 Vite CJS→ESM wrapper 加命名导出。
/// 条件：有 `export default <call>()` 且无别的 `export` 语句。
fn wrapped_needs_named_exports(source: &str) -> bool {
    if !source.contains("export default ") {
        return false;
    }
    if !source.contains("exports.") {
        return false;
    }
    // 检查是否有非 default 的 export 语句（忽略 export default 自身）
    // 检查行首的 `export`（非字符串内的 `"export`）
    let mut other_export = false;
    for line in source.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("export ") && !trimmed.starts_with("export default ") {
            other_export = true;
            break;
        }
    }
    !other_export
}

/// M77: 为 CJS→ESM wrapper 添加命名导出（解析 `exports.XXX =` 模式）。
fn add_named_exports_to_source(source: String) -> String {
    // 收集 CJS callback 内 `exports.XXX =` 的 export 名
    let mut export_names: Vec<String> = Vec::new();
    for line in source.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("exports.") {
            let name_end = rest.find(|c: char| !c.is_alphanumeric() && c != '_' && c != '$');
            if let Some(end) = name_end {
                let name = &rest[..end];
                if !name.is_empty() && !export_names.iter().any(|n| n == name) {
                    export_names.push(name.to_string());
                }
            }
        }
    }
    if export_names.is_empty() {
        return source;
    }
    // 找到 `export default <expr>` 行，替换成两行版本 + 命名导出
    let export_line_marker = "export default ";
    if let Some(line_start) = source.find(export_line_marker) {
        // 找到行尾
        let after_marker = line_start + export_line_marker.len();
        let line_end = source[after_marker..]
            .find('\n')
            .map(|i| after_marker + i)
            .unwrap_or(source.len());
        // 提取表达式的值（去除分号末尾）
        let expr = source[after_marker..line_end]
            .trim_end()
            .trim_end_matches(';');
        let prefix = &source[..line_start];
        let suffix = &source[line_end..];
        let mut named_exports: String = String::new();
        for name in &export_names {
            named_exports.push_str(&format!("export const {name} = __vite_ns__.{name};\n"));
        }
        format!(
            "{prefix}var __vite_ns__ = {expr};\nexport default __vite_ns__;\n{named_exports}{suffix}"
        )
    } else {
        source
    }
}

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
    #[cfg(feature = "boa")]
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
    pub fn new(esm_origin: Option<&str>) -> Self {
        let rt = Runtime::new().expect("QuickJS runtime");
        let base = esm_origin.unwrap_or("about:blank").to_string();
        // M66: 注册 HTTP Module Loader（ESM import 支持）。
        // base 仅用于日志/loader 上下文，不存到结构体（resolver 用 trait 参数）。
        let _ = base;
        rt.set_loader(HttpResolver, HttpLoader);
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

                // === M74: 结构化采集桥 ===
                let _ = g.set(
                    "__captureConsoleEvent",
                    Function::new(ctx.clone(), |level: String, text: String| {
                        bridge::capture_console_event(&level, &text);
                    })
                    .unwrap(),
                );
                let _ = g.set(
                    "__captureJsError",
                    Function::new(ctx.clone(), |message: String, stack: String| {
                        let s = if stack.is_empty() { None } else { Some(stack.as_str()) };
                        bridge::capture_js_error(&message, s);
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
                // M67: __findTag(tag) -> f64 —— 按标签名找第一个匹配节点 NodeId（对齐 boa get_tag 字符串模式）。
                // document.title/head getter 用它读真实 DOM（找 <title>/<head> 节点）。
                let _ = g.set("__findTag", Function::new(ctx.clone(), |tag: String| bridge::qjs_bridge::get_tag_by_name(tag)).unwrap());
                let _ = g.set("__getAttr", Function::new(ctx.clone(), |id: f64, k: String| bridge::qjs_bridge::get_attr(id, k)).unwrap());
                let _ = g.set("__setAttr", Function::new(ctx.clone(), |id: f64, k: String, v: String| bridge::qjs_bridge::set_attr(id, k, v)).unwrap());
                let _ = g.set("__removeAttr", Function::new(ctx.clone(), |id: f64, k: String| bridge::qjs_bridge::remove_attr(id, k)).unwrap());
                let _ = g.set("__getElById", Function::new(ctx.clone(), |id: String| bridge::qjs_bridge::get_el_by_id(id)).unwrap());
                let _ = g.set("__qs", Function::new(ctx.clone(), |s: String| bridge::qjs_bridge::qs(s)).unwrap());
                let _ = g.set("__qsAll", Function::new(ctx.clone(), |s: String| bridge::qjs_bridge::qs_all(s)).unwrap());
                let _ = g.set("__qsMatch", Function::new(ctx.clone(), |id: f64, s: String| bridge::qjs_bridge::qs_match(id, s)).unwrap());
                let _ = g.set("__qsClosest", Function::new(ctx.clone(), |id: f64, s: String| bridge::qjs_bridge::qs_closest(id, s)).unwrap());
                // M78: __qsCheck(sel) —— querySelector 语法校（非法选择器抛 SYNTAX_ERR 前置）。
                let _ = g.set("__qsCheck", Function::new(ctx.clone(), |s: String| crate::bridge::qs_syntax_error(&s).is_none()).unwrap());
                // M78: __offsetWidth(id) —— 经 css-engine mini 级联取元素 width（WPT :lang 测试）。
                let _ = g.set("__offsetWidth", Function::new(ctx.clone(), |id: f64| bridge::qjs_bridge::offset_width(id)).unwrap());
                // M78: __allIds() —— window 命名访问（WPT 裸引用元素 id）。
                let _ = g.set("__allIds", Function::new(ctx.clone(), bridge::qjs_bridge::all_ids).unwrap());
                // M78: __fetchScriptMimeOk(url) —— 动态 script MIME 强制（WPT block-mime）。
                let _ = g.set("__fetchScriptMimeOk", Function::new(ctx.clone(), |u: String| crate::scripts::fetch_script_mime_ok(u)).unwrap());
                // M78: __attrsOf(id) —— element.attributes（NamedNodeMap）反射。
                let _ = g.set("__attrsOf", Function::new(ctx.clone(), |id: f64| bridge::qjs_bridge::attrs_of(id)).unwrap());
                // M78.10: __textData(id) —— 文本节点自身 data（innerText getter）。
                let _ = g.set("__textData", Function::new(ctx.clone(), |id: f64| bridge::qjs_bridge::text_data(id)).unwrap());
                // M78: __visibleBodyTextLen() —— DOM 稳定检测的"可见文本"（跳过 script/style）。
                let _ = g.set("__visibleBodyTextLen", Function::new(ctx.clone(), bridge::qjs_bridge::visible_body_text_len).unwrap());
                let _ = g.set("__getBody", Function::new(ctx.clone(), |_: f64| bridge::qjs_bridge::get_body()).unwrap());
                let _ = g.set("__setTitle", Function::new(ctx.clone(), |t: String| bridge::qjs_bridge::set_title(t)).unwrap());
                let _ = g.set("__getParent", Function::new(ctx.clone(), |id: f64| bridge::qjs_bridge::get_parent(id)).unwrap());
                let _ = g.set("__children", Function::new(ctx.clone(), |id: f64| bridge::qjs_bridge::children(id)).unwrap());
                let _ = g.set("__getValue", Function::new(ctx.clone(), |id: f64| bridge::qjs_bridge::get_attr(id, "value".to_string()).unwrap_or_default()).unwrap());
                let _ = g.set("__setValue", Function::new(ctx.clone(), |id: f64, v: String| bridge::qjs_bridge::set_attr(id, "value".to_string(), v)).unwrap());
                let _ = g.set("__click", Function::new(ctx.clone(), |_: f64| {}).unwrap());
                let _ = g.set("__submit", Function::new(ctx.clone(), |_: f64| {}).unwrap());

                // === setBody / appendBody / fetchSetBody / fetchAppendBody ===
                // M66-fix: 用 qjs_bridge::set_body（走 set_body_inner_html，清空子节点+插文本），
                // 而非 set_attr("innerHTML", ...)（后者只改 attribute，渲染仍读旧子节点）。
                let _ = g.set("__setBody", Function::new(ctx.clone(), |html: String| {
                    bridge::qjs_bridge::set_body(html);
                }).unwrap());
                // __appendBody 追加文本到 <body> 末尾（不清空），与 boa append_body_text 一致。
                let _ = g.set("__appendBody", Function::new(ctx.clone(), |html: String| {
                    bridge::qjs_bridge::append_body(html);
                }).unwrap());
                let _ = g.set("__fetchSetBody", Function::new(ctx.clone(), |url: String| {
                    bridge::qjs_bridge::fetch_set_body(url);
                }).unwrap());
                let _ = g.set("__fetchAppendBody", Function::new(ctx.clone(), |url: String| {
                    bridge::qjs_bridge::fetch_append_body(url);
                }).unwrap());

                // === Fetch ===
                let _ = g.set("__fetchSync", Function::new(ctx.clone(), |url: String| bridge::qjs_bridge::fetch_sync(url)).unwrap());
                let _ = g.set("__fetchSyncMethod", Function::new(ctx.clone(), |url: String, method: String, body: Option<String>, ct: Option<String>| {
                    bridge::qjs_bridge::fetch_sync_method(url, method, body, ct)
                }).unwrap());

                // === WebSocket（复用 boa 的后台线程 WsManager）===
                let _ = g.set("__wsCreate", Function::new(ctx.clone(), |url: String| bridge::ws_create(url) as f64).unwrap());
                let _ = g.set("__wsSend", Function::new(ctx.clone(), |id: f64, data: String| bridge::ws_send(id as u32, data)).unwrap());
                let _ = g.set("__wsClose", Function::new(ctx.clone(), |id: f64| bridge::ws_close(id as u32)).unwrap());

                // === Storage ===
                let _ = g.set("__storageGet", Function::new(ctx.clone(), |k: String| bridge::qjs_bridge::storage_get(k)).unwrap());
                let _ = g.set("__storageSet", Function::new(ctx.clone(), |k: String, v: String| bridge::qjs_bridge::storage_set(k, v)).unwrap());
                let _ = g.set("__storageRemove", Function::new(ctx.clone(), |k: String| bridge::qjs_bridge::storage_remove(k)).unwrap());
                let _ = g.set("__storageClear", Function::new(ctx.clone(), || {}).unwrap());
                let _ = g.set("__storageLen", Function::new(ctx.clone(), || 0i32).unwrap());
                let _ = g.set("__storageKey", Function::new(ctx.clone(), |_: f64| Option::<String>::None).unwrap());

                // === Location ===
                let _ = g.set("__locationHref", Function::new(ctx.clone(), bridge::qjs_bridge::location_href).unwrap());
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

                // === HTML 解析（html5ever → 真实 DOM 子节点）===
                let _ = g.set(
                    "__parseHtml",
                    Function::new(ctx.clone(), |target: f64, html: String| {
                        bridge::qjs_bridge::parse_html(target, html);
                    })
                    .unwrap(),
                );

                // === M69: 动态 script 执行队列 ===
                // appendChild(scriptEl) 的 JS shim 检测到 script 标签后调
                // __enqueueDynamicScript(code) 入队；Rust 侧 run_scripts_quickjs 的
                // pump 循环每轮用 drain_dynamic_scripts() 取出 eval_safe。
                let _ = g.set(
                    "__enqueueDynamicScript",
                    Function::new(ctx.clone(), |code: String| {
                        bridge::enqueue_dynamic_script(code);
                    })
                    .unwrap(),
                );

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

    /// M66: 执行有静态 import 的 ESM module（需要 set_loader 预先注册）。
    /// 用 Module::declare + eval + catch 全部在 ctx.with 闭包内完成。
    pub fn eval_module_with_imports(&mut self, name: &str, source: &str) -> Result<(), String> {
        self.ctx.with(|ctx: Ctx| {
            // M77: 改用 Module::evaluate（一步完成 declare + eval + promise resolve）。
            // 之前 Module::declare + module.eval() + promise.finish 的顺序在某些 QuickJS
            // 版本上会导致模块 eval 时报 "cannot read property '0' of undefined"——
            // 可能是作用域竞争。Module::evaluate 是原始逻辑的合体。
            match Module::evaluate(ctx.clone(), name, source) {
                Ok(_promise) => match _promise.finish::<()>() {
                    Ok(()) => Ok(()),
                    Err(e) => {
                        eprintln!("[js-runtime] module error ({name}): {e}");
                        let _ = ctx.catch();
                        Err(format!("{e}"))
                    }
                },
                Err(e) => Err(format!("module evaluate: {e:?}")),
            }
        })
    }

    /// M66: 安全 eval——用 CatchResultExt 捕获错误，不泄漏 GC 对象。
    /// CaughtError 在 with 闭包内 drop（安全释放 JS 值）。
    pub fn eval_safe(&mut self, js: &str) -> Result<(), String> {
        use rquickjs::CatchResultExt;
        self.ctx
            .with(|ctx: Ctx| match ctx.eval::<(), _>(js).catch(&ctx) {
                Ok(()) => Ok(()),
                Err(e) => Err(format!("{e}")),
            })
    }

    /// M71.4: 执行**用户 script**（非 shim）。
    ///
    /// 与 `eval_safe` 的关键区别：关闭 strict 模式（`EvalOptions{strict:false}`）。
    /// 原因：rquickjs 的 `ctx.eval()` 默认 `strict: true`，导致用户 script 里
    /// 的**裸赋值未声明变量**（如 SvelteKit 的 `__sveltekit_xxx = {...}`）抛
    /// ReferenceError，整段 script 中断。真实浏览器是 sloppy mode，裸赋值会
    /// 自动创建 globalThis 属性。
    ///
    /// 仅对用户 script 关闭 strict；shim 安装代码（eval_safe）保持 strict。
    /// CaughtError 在 with 闭包内 drop（GC 安全）。
    pub fn eval_user_script(&mut self, js: &str) -> Result<(), String> {
        use rquickjs::context::EvalOptions;
        use rquickjs::CatchResultExt;
        self.ctx.with(|ctx: Ctx| {
            let mut opts = EvalOptions::default();
            opts.strict = false;
            match ctx.eval_with_options::<(), _>(js, opts).catch(&ctx) {
                Ok(()) => Ok(()),
                Err(e) => Err(format!("{e}")),
            }
        })
    }

    /// M66: 带整数返回值的 eval。
    pub fn eval_i32(&mut self, js: &str) -> Option<i32> {
        self.ctx.with(|ctx: Ctx| ctx.eval::<i32, _>(js).ok())
    }

    /// M66: 带布尔返回值的 eval。
    pub fn eval_js_bool(&mut self, js: &str) -> Option<bool> {
        self.ctx.with(|ctx: Ctx| ctx.eval::<bool, _>(js).ok())
    }

    /// M66: 带字符串返回值的 eval。
    pub fn eval_string(&mut self, js: &str) -> Option<String> {
        self.ctx.with(|ctx: Ctx| ctx.eval::<String, _>(js).ok())
    }

    /// M67: eval 单表达式，返回对齐 boa `display()` 格式的结果字符串。
    ///
    /// 供 CDP `eval_in_tree_engine` 的 QuickJS 分支用。CDP evaluate 的返回类型
    /// 不定（number/bool/string/undefined/null/object），用 JS 层统一格式化：
    /// - string → JSON.stringify 包引号（正确转义换行/引号），模拟 boa display
    /// - undefined / null / number / bool → String(r) 原样字面量
    ///
    /// 用 `CatchResultExt::catch` 捕获错误，CaughtError 在 `with` 闭包内 drop（GC 安全）。
    pub fn eval_display_string(&mut self, js: &str) -> Result<String, String> {
        use rquickjs::CatchResultExt;
        // 用 IIFE 在 JS 层格式化结果，避免 rquickjs Value 跨闭包取值的复杂性。
        // 注意：expr 原样注入到 return 后，不转义（CDP evaluate 的 expr 本就是 JS 代码）。
        let wrapper = format!(
            "(function() {{ var r = (function(){{ return ({EXPR}); }})(); \
             return typeof r === 'string' ? JSON.stringify(r) : String(r); }})()",
            EXPR = js
        );
        self.ctx.with(
            |ctx: Ctx| match ctx.eval::<String, _>(wrapper.as_str()).catch(&ctx) {
                Ok(s) => Ok(s),
                Err(e) => Err(format!("{e}")),
            },
        )
    }

    /// M66: 执行 ESM module 源码（支持 import/export/import.meta）。
    pub fn eval_module(&mut self, name: &str, source: &str) -> Result<(), String> {
        self.ctx.with(
            |ctx: Ctx| match Module::declare(ctx.clone(), name, source) {
                Ok(module) => match module.eval() {
                    Ok((_module, promise)) => {
                        let _ = promise.finish::<()>();
                        Ok(())
                    }
                    Err(e) => Err(format!("module eval: {e:?}")),
                },
                Err(e) => Err(format!("module declare: {e:?}")),
            },
        )
    }

    /// 运行微任务队列。
    /// M66-fix: rquickjs 的 Promise microtask（.then 回调）不会自动 drain，
    /// 必须显式调用 ctx.execute_pending_job() 直到返回 false。
    /// 否则 `Promise.resolve().then(fn)` 里的 fn 永远不执行，
    /// 导致 setTimeout 回调拿不到 then 里准备的数据（integration_timer_spa 失败）。
    pub fn run_jobs(&mut self) {
        self.ctx.with(|ctx: Ctx| {
            // drain 所有 pending microtask（上限 1000 防御死循环）
            let mut guard = 0;
            while ctx.execute_pending_job() {
                guard += 1;
                if guard > 1000 {
                    break;
                }
            }
        });
    }

    /// 手动触发 GC。
    pub fn gc(&mut self) {
        self.rt.run_gc();
    }
}

impl Drop for QuickJsEngine {
    fn drop(&mut self) {
        self.rt.run_gc();
    }
}
