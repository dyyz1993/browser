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
//!
//! M96.2: 核心桥注册（qjs_bridge 复用——桥本体引擎无关，仅绑定层
//! 引擎相关）。V8 桥模型：每个 `__xxx` 全局函数经
//! `Function::new(scope, |scope, args, rv| {...})` 注册。

use std::sync::OnceLock;

use rusty_v8::{FunctionCallbackArguments, Global, HandleScope, ReturnValue};

/// 进程级 V8 初始化（platform + engine，只能做一次）。
static V8_INIT: OnceLock<()> = OnceLock::new();

pub fn ensure_v8_initialized() {
    V8_INIT.get_or_init(|| {
        let platform = rusty_v8::SharedRef::from(rusty_v8::new_default_platform(0, false));
        rusty_v8::V8::initialize_platform(platform);
        rusty_v8::V8::initialize();
    });
}

/// V8 运行器：一个 isolate + 全局 context（桥装进 context global）。
pub struct V8Engine {
    isolate: rusty_v8::OwnedIsolate,
    /// 持久 context（桥与 shim 状态跨 eval 调用保留——对齐 QuickJS
    /// Runtime 的同 context 语义）。Global 跨 handle scope 存活。
    context: Global<rusty_v8::Context>,
}

// ---- 参数工具（FunctionCallbackArguments → Rust 类型） ----

fn arg_f64(scope: &mut HandleScope, args: &FunctionCallbackArguments, i: i32) -> f64 {
    args.get(i).to_number(scope).map_or(0.0, |n| n.value())
}

fn arg_string(scope: &mut HandleScope, args: &FunctionCallbackArguments, i: i32) -> String {
    let v = args.get(i);
    v.to_string(scope)
        .map_or_else(String::new, |s| s.to_rust_string_lossy(scope))
}

fn ret_str(scope: &mut HandleScope, rv: &mut ReturnValue, s: &str) {
    if let Some(ls) = rusty_v8::String::new(scope, s) {
        rv.set(ls.into());
    }
}

impl V8Engine {
    pub fn new() -> Option<Self> {
        ensure_v8_initialized();
        let mut isolate = rusty_v8::Isolate::new(Default::default());
        let context = {
            let mut hs = rusty_v8::HandleScope::new(&mut isolate);
            let ctx = rusty_v8::Context::new(&mut hs);
            Global::new(&mut hs, ctx)
        };
        Some(V8Engine { isolate, context })
    }

    /// eval 一段 JS，返回结果的字符串形式（对齐 eval_string 语义）。
    pub fn eval_string(&mut self, js: &str) -> Option<String> {
        let mut hs = rusty_v8::HandleScope::new(&mut self.isolate);
        let context = rusty_v8::Local::new(&mut hs, self.context.clone());
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
        let context = rusty_v8::Local::new(&mut hs, self.context.clone());
        let mut cs = rusty_v8::ContextScope::new(&mut hs, context);
        match rusty_v8::String::new(&mut cs, js)
            .and_then(|code| rusty_v8::Script::compile(&mut cs, code, None))
        {
            Some(script) => script.run(&mut cs).is_some(),
            None => false,
        }
    }

    /// 安装核心桥（M96.2：基础 DOM/诊断桥——qjs_bridge 复用）。
    /// 必须在 context 创建后、shim eval 前调用。
    pub fn install_core_bridges(&mut self) -> bool {
        let mut hs = rusty_v8::HandleScope::new(&mut self.isolate);
        let context = rusty_v8::Local::new(&mut hs, self.context.clone());
        let mut cs = rusty_v8::ContextScope::new(&mut hs, context);
        // M96.2-fix：global 必须在 context entered（ContextScope 内）取——
        #[allow(unused)]
        let global = context.global(&mut cs);

        macro_rules! defn {
            ($name:expr, $body:expr) => {{
                if let Some(f) = rusty_v8::Function::new(&mut cs, $body) {
                    if let Some(key) = rusty_v8::String::new(&mut cs, $name) {
                        let _ = global.set(&mut cs, key.into(), f.into());
                    }
                }
            }};
        }

        use crate::bridge::qjs_bridge as qb;

        // ---- 诊断 ----
        defn!("__ctrace", |s: &mut HandleScope,
                           a: FunctionCallbackArguments,
                           _rv: ReturnValue| {
            qb::log(arg_string(s, &a, 0));
        });
        defn!(
            "__hwCores",
            |_s: &mut HandleScope, _a: FunctionCallbackArguments, mut rv: ReturnValue| {
                rv.set(
                    rusty_v8::Number::new(
                        _s,
                        std::thread::available_parallelism()
                            .map(|n| n.get())
                            .unwrap_or(8) as f64,
                    )
                    .into(),
                );
            }
        );
        defn!(
            "__sysTimezone",
            |s: &mut HandleScope, _a: FunctionCallbackArguments, mut rv: ReturnValue| {
                let tz = std::fs::read_link("/etc/localtime")
                    .ok()
                    .and_then(|p| {
                        let s = p.to_string_lossy().to_string();
                        s.split("zoneinfo/").nth(1).map(|t| t.to_string())
                    })
                    .unwrap_or_else(|| "Asia/Shanghai".to_string());
                ret_str(s, &mut rv, &tz);
            }
        );
        defn!(
            "__noTsWrap",
            |_s: &mut HandleScope, _a: FunctionCallbackArguments, mut rv: ReturnValue| {
                rv.set(rusty_v8::Boolean::new(_s, false).into());
            }
        );

        // ---- DOM 核心 ----
        defn!(
            "__createEl",
            |_s: &mut HandleScope, a: FunctionCallbackArguments, mut rv: ReturnValue| {
                let id = qb::create_el(arg_string(_s, &a, 0));
                rv.set(rusty_v8::Number::new(_s, id).into());
            }
        );
        defn!(
            "__createDetachedEl",
            |_s: &mut HandleScope, a: FunctionCallbackArguments, mut rv: ReturnValue| {
                let id = qb::create_detached_el(arg_string(_s, &a, 0));
                rv.set(rusty_v8::Number::new(_s, id).into());
            }
        );
        defn!(
            "__appendChild",
            |s: &mut HandleScope, a: FunctionCallbackArguments, _rv: ReturnValue| {
                qb::append_child(arg_f64(s, &a, 0), arg_f64(s, &a, 1));
            }
        );
        defn!(
            "__removeChild",
            |s: &mut HandleScope, a: FunctionCallbackArguments, _rv: ReturnValue| {
                qb::remove_child(arg_f64(s, &a, 0), arg_f64(s, &a, 1));
            }
        );
        defn!(
            "__insertBefore",
            |s: &mut HandleScope, a: FunctionCallbackArguments, _rv: ReturnValue| {
                qb::insert_before(arg_f64(s, &a, 0), arg_f64(s, &a, 1), arg_f64(s, &a, 2));
            }
        );
        defn!("__setText", |s: &mut HandleScope,
                            a: FunctionCallbackArguments,
                            _rv: ReturnValue| {
            qb::set_text(arg_f64(s, &a, 0), arg_string(s, &a, 1));
        });
        defn!("__setAttr", |s: &mut HandleScope,
                            a: FunctionCallbackArguments,
                            _rv: ReturnValue| {
            qb::set_attr(
                arg_f64(s, &a, 0),
                arg_string(s, &a, 1),
                arg_string(s, &a, 2),
            );
        });
        defn!(
            "__removeAttr",
            |s: &mut HandleScope, a: FunctionCallbackArguments, _rv: ReturnValue| {
                qb::remove_attr(arg_f64(s, &a, 0), arg_string(s, &a, 1));
            }
        );
        defn!("__setBody", |_s: &mut HandleScope,
                            a: FunctionCallbackArguments,
                            _rv: ReturnValue| {
            qb::set_body(arg_string(_s, &a, 0));
        });
        defn!(
            "__appendBody",
            |_s: &mut HandleScope, a: FunctionCallbackArguments, _rv: ReturnValue| {
                qb::append_body(arg_string(_s, &a, 0));
            }
        );
        defn!(
            "__setTitle",
            |_s: &mut HandleScope, a: FunctionCallbackArguments, _rv: ReturnValue| {
                qb::set_title(arg_string(_s, &a, 0));
            }
        );

        // ---- DOM 读取族 ----
        defn!(
            "__getText",
            |s: &mut HandleScope, a: FunctionCallbackArguments, mut rv: ReturnValue| {
                let v = qb::get_text(arg_f64(s, &a, 0));
                ret_str(s, &mut rv, &v);
            }
        );
        defn!("__getTag", |_s: &mut HandleScope,
                           a: FunctionCallbackArguments,
                           mut rv: ReturnValue| {
            let t = qb::get_tag(arg_f64(_s, &a, 0));
            ret_str(_s, &mut rv, &t);
        });
        defn!(
            "__getTagName",
            |_s: &mut HandleScope, a: FunctionCallbackArguments, mut rv: ReturnValue| {
                let t = qb::get_tag(arg_f64(_s, &a, 0));
                ret_str(_s, &mut rv, &t);
            }
        );
        defn!(
            "__findTag",
            |_s: &mut HandleScope, a: FunctionCallbackArguments, mut rv: ReturnValue| {
                let v = qb::get_tag_by_name(arg_string(_s, &a, 0));
                rv.set(rusty_v8::Number::new(_s, v).into());
            }
        );
        defn!(
            "__getAttr",
            |_s: &mut HandleScope, a: FunctionCallbackArguments, mut rv: ReturnValue| {
                let v = qb::get_attr(arg_f64(_s, &a, 0), arg_string(_s, &a, 1));
                match v {
                    Some(s) => ret_str(_s, &mut rv, &s),
                    None => rv.set(rusty_v8::null(_s).into()),
                }
            }
        );
        defn!(
            "__getElById",
            |_s: &mut HandleScope, a: FunctionCallbackArguments, mut rv: ReturnValue| {
                let v = qb::get_el_by_id(arg_string(_s, &a, 0));
                rv.set(rusty_v8::Number::new(_s, v).into());
            }
        );
        defn!("__qs", |_s: &mut HandleScope,
                       a: FunctionCallbackArguments,
                       mut rv: ReturnValue| {
            let v = qb::qs(arg_string(_s, &a, 0));
            rv.set(rusty_v8::Number::new(_s, v).into());
        });
        defn!("__qsAll", |_s: &mut HandleScope,
                          a: FunctionCallbackArguments,
                          mut rv: ReturnValue| {
            let v = qb::qs_all(arg_string(_s, &a, 0));
            ret_str(_s, &mut rv, &v);
        });
        defn!(
            "__qsMatch",
            |_s: &mut HandleScope, a: FunctionCallbackArguments, mut rv: ReturnValue| {
                let v = qb::qs_match(arg_f64(_s, &a, 0), arg_string(_s, &a, 1));
                rv.set(rusty_v8::Boolean::new(_s, v).into());
            }
        );
        defn!(
            "__qsClosest",
            |_s: &mut HandleScope, a: FunctionCallbackArguments, mut rv: ReturnValue| {
                let v = qb::qs_closest(arg_f64(_s, &a, 0), arg_string(_s, &a, 1));
                rv.set(rusty_v8::Number::new(_s, v).into());
            }
        );
        defn!(
            "__qsCheck",
            |_s: &mut HandleScope, a: FunctionCallbackArguments, mut rv: ReturnValue| {
                let ok = crate::bridge::qs_syntax_error(&arg_string(_s, &a, 0)).is_none();
                rv.set(rusty_v8::Boolean::new(_s, ok).into());
            }
        );
        defn!(
            "__offsetWidth",
            |_s: &mut HandleScope, a: FunctionCallbackArguments, mut rv: ReturnValue| {
                let v = qb::offset_width(arg_f64(_s, &a, 0));
                rv.set(rusty_v8::Number::new(_s, v).into());
            }
        );
        defn!("__allIds", |_s: &mut HandleScope,
                           _a: FunctionCallbackArguments,
                           mut rv: ReturnValue| {
            let v = qb::all_ids();
            ret_str(_s, &mut rv, &v);
        });
        defn!(
            "__attrsOf",
            |_s: &mut HandleScope, a: FunctionCallbackArguments, mut rv: ReturnValue| {
                let v = qb::attrs_of(arg_f64(_s, &a, 0));
                ret_str(_s, &mut rv, &v);
            }
        );
        defn!(
            "__textData",
            |_s: &mut HandleScope, a: FunctionCallbackArguments, mut rv: ReturnValue| {
                let v = qb::text_data(arg_f64(_s, &a, 0));
                ret_str(_s, &mut rv, &v);
            }
        );
        defn!(
            "__visibleBodyTextLen",
            |_s: &mut HandleScope, _a: FunctionCallbackArguments, mut rv: ReturnValue| {
                rv.set(rusty_v8::Number::new(_s, qb::visible_body_text_len()).into());
            }
        );
        defn!(
            "__getBody",
            |_s: &mut HandleScope, _a: FunctionCallbackArguments, mut rv: ReturnValue| {
                rv.set(rusty_v8::Number::new(_s, qb::get_body()).into());
            }
        );
        defn!(
            "__getParent",
            |_s: &mut HandleScope, a: FunctionCallbackArguments, mut rv: ReturnValue| {
                let v = qb::get_parent(arg_f64(_s, &a, 0));
                rv.set(rusty_v8::Number::new(_s, v).into());
            }
        );
        defn!(
            "__children",
            |_s: &mut HandleScope, a: FunctionCallbackArguments, mut rv: ReturnValue| {
                let v = qb::children(arg_f64(_s, &a, 0));
                ret_str(_s, &mut rv, &v);
            }
        );
        defn!(
            "__getValue",
            |_s: &mut HandleScope, a: FunctionCallbackArguments, mut rv: ReturnValue| {
                let v = qb::get_attr(arg_f64(_s, &a, 0), "value".to_string());
                match v {
                    Some(s) => ret_str(_s, &mut rv, &s),
                    None => rv.set(rusty_v8::null(_s).into()),
                }
            }
        );
        defn!(
            "__setValue",
            |s: &mut HandleScope, a: FunctionCallbackArguments, _rv: ReturnValue| {
                qb::set_attr(arg_f64(s, &a, 0), "value".to_string(), arg_string(s, &a, 1));
            }
        );
        defn!("__click", |_s: &mut HandleScope,
                          _a: FunctionCallbackArguments,
                          _rv: ReturnValue| {});
        defn!("__submit", |_s: &mut HandleScope,
                           _a: FunctionCallbackArguments,
                           _rv: ReturnValue| {});

        // ---- fetch 族 ----
        defn!(
            "__fetchSetBody",
            |s: &mut HandleScope, a: FunctionCallbackArguments, _rv: ReturnValue| {
                qb::fetch_set_body(arg_string(s, &a, 0));
            }
        );
        defn!(
            "__fetchAppendBody",
            |s: &mut HandleScope, a: FunctionCallbackArguments, _rv: ReturnValue| {
                qb::fetch_append_body(arg_string(s, &a, 0));
            }
        );
        defn!(
            "__fetchSync",
            |_s: &mut HandleScope, a: FunctionCallbackArguments, mut rv: ReturnValue| {
                let v = qb::fetch_sync(arg_string(_s, &a, 0));
                match v {
                    Some(s) => ret_str(_s, &mut rv, &s),
                    None => rv.set(rusty_v8::null(_s).into()),
                }
            }
        );
        defn!(
            "__fetchSyncMethod",
            |_s: &mut HandleScope, a: FunctionCallbackArguments, mut rv: ReturnValue| {
                let body = if a.get(2).is_null_or_undefined() {
                    None
                } else {
                    Some(arg_string(_s, &a, 2))
                };
                let ct = if a.get(3).is_null_or_undefined() {
                    None
                } else {
                    Some(arg_string(_s, &a, 3))
                };
                let hj = if a.get(4).is_null_or_undefined() {
                    None
                } else {
                    Some(arg_string(_s, &a, 4))
                };
                let v = qb::fetch_sync_method(
                    arg_string(_s, &a, 0),
                    arg_string(_s, &a, 1),
                    body,
                    ct,
                    hj,
                );
                match v {
                    Some(s) => ret_str(_s, &mut rv, &s),
                    None => rv.set(rusty_v8::null(_s).into()),
                }
            }
        );
        defn!(
            "__fetchScriptMimeOk",
            |_s: &mut HandleScope, a: FunctionCallbackArguments, mut rv: ReturnValue| {
                let ok = crate::scripts::fetch_script_mime_ok(arg_string(_s, &a, 0));
                rv.set(rusty_v8::Boolean::new(_s, ok).into());
            }
        );
        defn!(
            "__cacheAsset",
            |s: &mut HandleScope, a: FunctionCallbackArguments, _rv: ReturnValue| {
                qb::cache_asset(arg_string(s, &a, 0), arg_string(s, &a, 1));
            }
        );

        // ---- parseHtml（innerHTML）----
        defn!(
            "__parseHtml",
            |s: &mut HandleScope, a: FunctionCallbackArguments, _rv: ReturnValue| {
                qb::parse_html(arg_f64(s, &a, 0), arg_string(s, &a, 1));
            }
        );

        // ---- storage ----
        defn!(
            "__storageGet",
            |_s: &mut HandleScope, a: FunctionCallbackArguments, mut rv: ReturnValue| {
                let v = qb::storage_get(arg_string(_s, &a, 0));
                match v {
                    Some(s) => ret_str(_s, &mut rv, &s),
                    None => rv.set(rusty_v8::null(_s).into()),
                }
            }
        );
        defn!(
            "__storageSet",
            |s: &mut HandleScope, a: FunctionCallbackArguments, _rv: ReturnValue| {
                qb::storage_set(arg_string(s, &a, 0), arg_string(s, &a, 1));
            }
        );
        defn!(
            "__storageRemove",
            |s: &mut HandleScope, a: FunctionCallbackArguments, _rv: ReturnValue| {
                qb::storage_remove(arg_string(s, &a, 0));
            }
        );
        defn!(
            "__storageLen",
            |_s: &mut HandleScope, _a: FunctionCallbackArguments, mut rv: ReturnValue| {
                rv.set(rusty_v8::Number::new(_s, qb::storage_len()).into());
            }
        );
        defn!(
            "__storageKey",
            |_s: &mut HandleScope, a: FunctionCallbackArguments, mut rv: ReturnValue| {
                let v = qb::storage_key(arg_f64(_s, &a, 0));
                match v {
                    Some(s) => ret_str(_s, &mut rv, &s),
                    None => rv.set(rusty_v8::null(_s).into()),
                }
            }
        );
        defn!(
            "__storageClear",
            |_s: &mut HandleScope, _a: FunctionCallbackArguments, _rv: ReturnValue| {}
        );

        // ---- canvas 2D（真像素）----
        defn!("__cvNew", |_s: &mut HandleScope,
                          a: FunctionCallbackArguments,
                          mut rv: ReturnValue| {
            let v = crate::canvas2d::cv_new(arg_f64(_s, &a, 0), arg_f64(_s, &a, 1));
            rv.set(rusty_v8::Number::new(_s, v).into());
        });
        defn!(
            "__cvResize",
            |_s: &mut HandleScope, a: FunctionCallbackArguments, _rv: ReturnValue| {
                crate::canvas2d::cv_resize(
                    arg_f64(_s, &a, 0),
                    arg_f64(_s, &a, 1),
                    arg_f64(_s, &a, 2),
                );
            }
        );
        defn!(
            "__cvBeginPath",
            |_s: &mut HandleScope, a: FunctionCallbackArguments, _rv: ReturnValue| {
                crate::canvas2d::cv_begin_path(arg_f64(_s, &a, 0));
            }
        );
        defn!("__cvRect", |_s: &mut HandleScope,
                           a: FunctionCallbackArguments,
                           _rv: ReturnValue| {
            crate::canvas2d::cv_rect(
                arg_f64(_s, &a, 0),
                arg_f64(_s, &a, 1),
                arg_f64(_s, &a, 2),
                arg_f64(_s, &a, 3),
                arg_f64(_s, &a, 4),
            );
        });
        defn!("__cvArc", |_s: &mut HandleScope,
                          a: FunctionCallbackArguments,
                          _rv: ReturnValue| {
            crate::canvas2d::cv_arc(
                arg_f64(_s, &a, 0),
                arg_f64(_s, &a, 1),
                arg_f64(_s, &a, 2),
                arg_f64(_s, &a, 3),
                arg_f64(_s, &a, 4),
                arg_f64(_s, &a, 5),
                a.get(6).boolean_value(_s),
            );
        });
        defn!(
            "__cvSetStyle",
            |s: &mut HandleScope, a: FunctionCallbackArguments, _rv: ReturnValue| {
                crate::canvas2d::cv_set_style(
                    &arg_string(s, &a, 0),
                    arg_f64(s, &a, 1),
                    a.get(2).boolean_value(s),
                );
            }
        );
        defn!("__cvFill", |s: &mut HandleScope,
                           a: FunctionCallbackArguments,
                           _rv: ReturnValue| {
            crate::canvas2d::cv_fill(arg_f64(s, &a, 0), &arg_string(s, &a, 1));
        });
        defn!(
            "__cvFillRect",
            |_s: &mut HandleScope, a: FunctionCallbackArguments, _rv: ReturnValue| {
                crate::canvas2d::cv_fill_rect(
                    arg_f64(_s, &a, 0),
                    arg_f64(_s, &a, 1),
                    arg_f64(_s, &a, 2),
                    arg_f64(_s, &a, 3),
                    arg_f64(_s, &a, 4),
                );
            }
        );
        defn!(
            "__cvFillText",
            |s: &mut HandleScope, a: FunctionCallbackArguments, _rv: ReturnValue| {
                crate::canvas2d::cv_fill_text(
                    arg_f64(s, &a, 0),
                    &arg_string(s, &a, 1),
                    arg_f64(s, &a, 2),
                    arg_f64(s, &a, 3),
                    arg_f64(s, &a, 4),
                    &arg_string(s, &a, 5),
                );
            }
        );
        defn!(
            "__cvToDataURL",
            |_s: &mut HandleScope, a: FunctionCallbackArguments, mut rv: ReturnValue| {
                let v = crate::canvas2d::cv_to_data_url(arg_f64(_s, &a, 0));
                ret_str(_s, &mut rv, &v);
            }
        );
        defn!(
            "__cvGetImageData",
            |_s: &mut HandleScope, a: FunctionCallbackArguments, mut rv: ReturnValue| {
                let v = crate::canvas2d::cv_get_image_data(
                    arg_f64(_s, &a, 0),
                    arg_f64(_s, &a, 1),
                    arg_f64(_s, &a, 2),
                    arg_f64(_s, &a, 3),
                    arg_f64(_s, &a, 4),
                );
                ret_str(_s, &mut rv, &v);
            }
        );
        defn!(
            "__cvPutImageData",
            |s: &mut HandleScope, a: FunctionCallbackArguments, _rv: ReturnValue| {
                crate::canvas2d::cv_put_image_data(
                    arg_f64(s, &a, 0),
                    &arg_string(s, &a, 1),
                    arg_f64(s, &a, 2),
                    arg_f64(s, &a, 3),
                    arg_f64(s, &a, 4),
                );
            }
        );

        // ---- ws/xhr/location/history/nav（QuickJS 同款桩）----
        defn!(
            "__wsCreate",
            |_s: &mut HandleScope, a: FunctionCallbackArguments, mut rv: ReturnValue| {
                let v = crate::bridge::ws_create(arg_string(_s, &a, 0)) as f64;
                rv.set(rusty_v8::Number::new(_s, v).into());
            }
        );
        defn!("__wsSend", |s: &mut HandleScope,
                           a: FunctionCallbackArguments,
                           _rv: ReturnValue| {
            crate::bridge::ws_send(arg_f64(s, &a, 0) as u32, arg_string(s, &a, 1));
        });
        defn!("__wsClose", |s: &mut HandleScope,
                            a: FunctionCallbackArguments,
                            _rv: ReturnValue| {
            crate::bridge::ws_close(arg_f64(s, &a, 0) as u32);
        });
        defn!(
            "__xhrCreate",
            |_s: &mut HandleScope, _a: FunctionCallbackArguments, mut rv: ReturnValue| {
                rv.set(rusty_v8::Number::new(_s, 0.0).into());
            }
        );
        defn!("__xhrOpen", |_s: &mut HandleScope,
                            _a: FunctionCallbackArguments,
                            _rv: ReturnValue| {});
        defn!("__xhrSend", |_s: &mut HandleScope,
                            _a: FunctionCallbackArguments,
                            _rv: ReturnValue| {});
        defn!(
            "__xhrGetResponseText",
            |_s: &mut HandleScope, _a: FunctionCallbackArguments, mut rv: ReturnValue| {
                rv.set(rusty_v8::null(_s).into());
            }
        );
        defn!(
            "__locationHref",
            |_s: &mut HandleScope, _a: FunctionCallbackArguments, mut rv: ReturnValue| {
                let v = qb::location_href();
                ret_str(_s, &mut rv, &v);
            }
        );
        defn!(
            "__locationParts",
            |_s: &mut HandleScope, _a: FunctionCallbackArguments, mut rv: ReturnValue| {
                let v = r#"{"href":"about:blank","protocol":"about:","host":"","pathname":"","search":"","hash":""}"#.to_string();
                ret_str(_s, &mut rv, &v);
            }
        );
        defn!(
            "__locationAssign",
            |_s: &mut HandleScope, _a: FunctionCallbackArguments, _rv: ReturnValue| {}
        );
        defn!(
            "__locationReplace",
            |_s: &mut HandleScope, _a: FunctionCallbackArguments, _rv: ReturnValue| {}
        );
        defn!(
            "__historyPush",
            |_s: &mut HandleScope, _a: FunctionCallbackArguments, _rv: ReturnValue| {}
        );
        defn!(
            "__historyReplace",
            |_s: &mut HandleScope, _a: FunctionCallbackArguments, _rv: ReturnValue| {}
        );
        defn!(
            "__historyBack",
            |_s: &mut HandleScope, _a: FunctionCallbackArguments, _rv: ReturnValue| {}
        );
        defn!(
            "__historyForward",
            |_s: &mut HandleScope, _a: FunctionCallbackArguments, _rv: ReturnValue| {}
        );
        defn!(
            "__historyGo",
            |_s: &mut HandleScope, _a: FunctionCallbackArguments, _rv: ReturnValue| {}
        );
        defn!(
            "__historyLen",
            |_s: &mut HandleScope, _a: FunctionCallbackArguments, mut rv: ReturnValue| {
                rv.set(rusty_v8::Number::new(_s, 1.0).into());
            }
        );
        defn!(
            "__historyState",
            |_s: &mut HandleScope, _a: FunctionCallbackArguments, mut rv: ReturnValue| {
                rv.set(rusty_v8::null(_s).into());
            }
        );
        defn!(
            "__navRecord",
            |s: &mut HandleScope, a: FunctionCallbackArguments, _rv: ReturnValue| {
                crate::bridge::record_pending_navigation(&arg_string(s, &a, 0));
            }
        );

        // ---- worker（同步近似：跑在主线程子 isolate）----
        defn!(
            "__workerRun",
            |s: &mut HandleScope, a: FunctionCallbackArguments, mut rv: ReturnValue| {
                let r =
                    crate::engine_quickjs::worker_run(&arg_string(s, &a, 0), &arg_string(s, &a, 1));
                ret_str(s, &mut rv, &r);
            }
        );
        defn!(
            "__workerRunSrc",
            |s: &mut HandleScope, a: FunctionCallbackArguments, mut rv: ReturnValue| {
                let r = crate::engine_quickjs::worker_run_src(
                    &arg_string(s, &a, 0),
                    &arg_string(s, &a, 1),
                );
                ret_str(s, &mut rv, &r);
            }
        );

        true
    }
}

/// M96.2-diag: 极简桥（不碰 qb/不读参数）——回调基建对拍。
pub fn install_test_noop_bridge(e: &mut V8Engine) -> bool {
    let mut hs = rusty_v8::HandleScope::new(&mut e.isolate);
    let context = rusty_v8::Local::new(&mut hs, e.context.clone());
    let global = context.global(&mut hs);
    let mut cs = rusty_v8::ContextScope::new(&mut hs, context);
    if let Some(f) = rusty_v8::Function::new(
        &mut cs,
        |_s: &mut HandleScope, _a: FunctionCallbackArguments, mut rv: ReturnValue| {
            rv.set(rusty_v8::Number::new(_s, 42.0).into());
        },
    ) {
        if let Some(key) = rusty_v8::String::new(&mut cs, "__noopV8") {
            return global.set(&mut cs, key.into(), f.into()).unwrap_or(false);
        }
    }
    false
}

/// M96.2-diag: 只读参数桥。
pub fn install_test_echo_bridge(e: &mut V8Engine) -> bool {
    let mut hs = rusty_v8::HandleScope::new(&mut e.isolate);
    let context = rusty_v8::Local::new(&mut hs, e.context.clone());
    let global = context.global(&mut hs);
    let mut cs = rusty_v8::ContextScope::new(&mut hs, context);
    if let Some(f) = rusty_v8::Function::new(
        &mut cs,
        |s: &mut HandleScope, a: FunctionCallbackArguments, mut rv: ReturnValue| {
            let v = a.get(0);
            let str_ = v
                .to_string(s)
                .map_or_else(String::new, |x| x.to_rust_string_lossy(s));
            if let Some(ls) = rusty_v8::String::new(s, &format!("echo:{str_}")) {
                rv.set(ls.into());
            }
        },
    ) {
        if let Some(key) = rusty_v8::String::new(&mut cs, "__echoV8") {
            return global.set(&mut cs, key.into(), f.into()).unwrap_or(false);
        }
    }
    false
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
        let ts = e.eval_string("eval.toString()").unwrap_or_default();
        assert!(
            ts.contains("native code") && !ts.contains('\n'),
            "V8 native fn toString 应为单行 native 格式，got: {ts}"
        );
        let msg = e
            .eval_string("try { null.usdfsh } catch(t) { t.toString() }")
            .unwrap_or_default();
        assert!(
            msg.contains("Cannot read properties of null (reading 'usdfsh')"),
            "V8 TypeError 文案应与 Chrome 同源，got: {msg}"
        );
    }

    // M96.2: 桥回调在 Script::Run 内挂起（见 PROGRESS——纯 eval/native 调用均通，
    // 仅 Rust 回调挂；修复后去 ignore）。
    #[test]
    fn v8_core_bridges_dom() {
        let tree = browser_html_parser::parse("<html><body></body></html>");
        let _guard = crate::bridge::install_current(tree);
        let mut e = V8Engine::new().expect("v8 init");
        assert!(e.install_core_bridges(), "bridges install");
        let out = e.eval_string(
            "(function() { var el = __createEl('div'); __setAttr(el, 'id', 'v8test'); return typeof el + ':' + el; })()",
        );
        let ok = out
            .as_deref()
            .and_then(|s| s.strip_prefix("number:"))
            .and_then(|n| n.parse::<i64>().ok())
            .map(|n| n > 0)
            .unwrap_or(false);
        assert!(ok, "bridge should return positive NodeId, got {out:?}");
    }
}
