//! M96.5/ADR-0006: V8 152 可选引擎后端（`--features v8`，默认不编译）。
//!
//! v8 crate 152.2.0 = Chrome 152/153 同源 V8——三同源验证实证：
//! etsl=33（QuickJS 226 天花板原生消失）、TypeError 文案逐字符、
//! native toString 单行。数学引擎与 Chrome 153 一致（maths/sumPrecise/
//! bitmask 微差消除——v8eval2 实测）。
//!
//! v8 152 API 重构（vs rusty_v8 0.32）：`scope!` 宏替代 HandleScope
//! 直用、`PinScope` 替代 `&mut HandleScope`、`Context::new` 双参数、
//! `Global<Context>` 跨 scope 用法不变。
//!
//! 完整管线在 run_scripts_v8（scripts.rs）——shim 6 段 + 81 桥 +
//! DCL 派发 + microtask pump。

use std::sync::OnceLock;

use v8::{FunctionCallbackArguments, PinScope, ReturnValue};

/// 进程级 V8 初始化（platform + engine，只能做一次）。
static V8_INIT: OnceLock<()> = OnceLock::new();

pub fn ensure_v8_initialized() {
    V8_INIT.get_or_init(|| {
        let platform = v8::new_default_platform(0, false).make_shared();
        v8::V8::initialize_platform(platform);
        v8::V8::initialize();
        // M96.6: ICU 默认 locale 对齐 shim 的 navigator.language（'zh-CN'，
        // scripts.rs navigator 常量）——Chrome 从浏览器语言推导 Intl locale，
        // fp 采集 `Intl.DateTimeFormat().resolvedOptions().locale` 需要
        // zh-CN（en-US 会成为身份差异项）。timeZone 由 ICU 从宿主
        // TZ 推导（本机 Asia/Shanghai，与 Chrome 一致），无需干预。
        v8::icu::set_default_locale("zh-CN");
    });
}

/// V8 运行器：一个 isolate + 全局 context（桥装进 context global）。
pub struct V8Engine {
    isolate: v8::OwnedIsolate,
    /// 持久 context（桥与 shim 状态跨 eval 调用保留）。
    context: v8::Global<v8::Context>,
}

// ---- 参数工具（FunctionCallbackArguments → Rust 类型） ----

fn arg_f64(scope: &mut PinScope, args: &FunctionCallbackArguments, i: i32) -> f64 {
    args.get(i).to_number(scope).map_or(0.0, |n| n.value())
}

fn arg_string(scope: &mut PinScope, args: &FunctionCallbackArguments, i: i32) -> String {
    let v = args.get(i);
    v.to_string(scope)
        .map_or_else(String::new, |s| s.to_rust_string_lossy(scope))
}

fn ret_str(scope: &mut PinScope, rv: &mut ReturnValue, s: &str) {
    if let Some(ls) = v8::String::new(scope, s) {
        rv.set(ls.into());
    }
}

impl V8Engine {
    pub fn new() -> Option<Self> {
        ensure_v8_initialized();
        let mut isolate = v8::Isolate::new(v8::CreateParams::default());
        let context = {
            v8::scope!(let hs, &mut isolate);
            let ctx = v8::Context::new(hs, Default::default());
            // Global::new 只要 &Isolate——hs（&mut PinScope）自动 Deref 过去，
            // 不能再借 &mut isolate（scope! 宏已持有可变借用）。
            v8::Global::new(hs, ctx)
        };
        Some(V8Engine { isolate, context })
    }

    /// microtask pump（V8 显式策略——Promise .then 回调需要）。
    pub fn pump_microtasks(&mut self) {
        self.isolate.perform_microtask_checkpoint();
    }

    /// eval 一段 JS，返回结果的字符串形式。
    pub fn eval_string(&mut self, js: &str) -> Option<String> {
        let isolate = &mut self.isolate;
        let context = self.context.clone();
        v8::scope!(let hs, isolate);
        let context = v8::Local::new(hs, context);
        let scope = &v8::ContextScope::new(hs, context);
        let code = v8::String::new(scope, js)?;
        let script = v8::Script::compile(scope, code, None)?;
        let result = script.run(scope)?;
        let s = result.to_string(scope)?;
        Some(s.to_rust_string_lossy(scope))
    }

    /// eval 且不取返回值（安装 shim 用）。
    pub fn eval_install(&mut self, js: &str) -> bool {
        let isolate = &mut self.isolate;
        let context = self.context.clone();
        v8::scope!(let hs, isolate);
        let context = v8::Local::new(hs, context);
        let scope = &v8::ContextScope::new(hs, context);
        match v8::String::new(scope, js).and_then(|code| v8::Script::compile(scope, code, None)) {
            Some(script) => script.run(scope).is_some(),
            None => false,
        }
    }

    /// 安装核心桥（M96.2-M96.5：全量 81 桥——qjs_bridge 复用）。
    pub fn install_core_bridges(&mut self) -> bool {
        let isolate = &mut self.isolate;
        let context_global = self.context.clone();
        v8::scope!(let hs, isolate);
        let context = v8::Local::new(hs, context_global);
        // owned ContextScope：Deref/DerefMut 双实现——Function::new 要
        // &mut PinScope（DerefMut），String/Object::set 只要 &PinScope。
        let mut scope = v8::ContextScope::new(hs, context);
        let global = context.global(&scope);

        macro_rules! defn {
            ($name:expr, $body:expr) => {{
                if let Some(f) = v8::Function::new(&mut scope, $body) {
                    if let Some(key) = v8::String::new(&scope, $name) {
                        let _ = global.set(&scope, key.into(), f.into());
                    }
                }
            }};
        }

        use crate::bridge::qjs_bridge as qb;

        // ---- 诊断 ----
        defn!("__ctrace", |s: &mut PinScope,
                           a: FunctionCallbackArguments,
                           _rv: ReturnValue| {
            qb::log(arg_string(s, &a, 0));
        });
        // M96.7: Rust 原生 SHA-256（crypto.subtle.digest fast path——PoW 提速）
        defn!(
            "__sha256Hex",
            |s: &mut PinScope, a: FunctionCallbackArguments, mut rv: ReturnValue| {
                let v = crate::sha256::sha256_hex_latin1(&arg_string(s, &a, 0));
                if let Some(ls) = v8::String::new(s, &v) {
                    rv.set(ls.into());
                }
            }
        );
        // M96.11-diag: fp canvas hash A/B 覆盖值（env 注入，诊断专用）
        if let Ok(v) = std::env::var("BROWSER_FP_CANVAS_OVERRIDE") {
            if let (Some(ls), Some(key)) = (
                v8::String::new(&scope, &v),
                v8::String::new(&scope, "__canvasFpOverride"),
            ) {
                let _ = global.set(&scope, key.into(), ls.into());
            }
        }
        // M96.12-diag: signals 全树 override（终局判别）
        if let Ok(p) = std::env::var("BROWSER_FP_SIGNALS_FILE") {
            if let Ok(s) = std::fs::read_to_string(&p) {
                if let (Some(ls), Some(key)) = (
                    v8::String::new(&scope, &s),
                    v8::String::new(&scope, "__fpSignalsOverride"),
                ) {
                    let _ = global.set(&scope, key.into(), ls.into());
                }
            }
        }
        // M96.12: 二进制安全 fetch 侧信道（wasm 等资产真字节）
        defn!(
            "__fetchB64",
            |_s: &mut PinScope, _a: FunctionCallbackArguments, mut rv: ReturnValue| {
                let v = qb::fetch_b64();
                if let Some(ls) = v8::String::new(_s, &v) {
                    rv.set(ls.into());
                }
            }
        );
        // __hwCores 是**数值**（shim 检查 typeof === 'number'）——Chrome 全核
        {
            let cores = std::process::Command::new("sysctl")
                .args(["-n", "hw.ncpu"])
                .output()
                .ok()
                .and_then(|o| {
                    String::from_utf8_lossy(&o.stdout)
                        .trim()
                        .parse::<f64>()
                        .ok()
                })
                .unwrap_or(8.0);
            let num = v8::Number::new(&scope, cores);
            if let Some(key) = v8::String::new(&scope, "__hwCores") {
                let _ = global.set(&scope, key.into(), num.into());
            }
        }
        defn!(
            "__sysTimezone",
            |s: &mut PinScope, _a: FunctionCallbackArguments, mut rv: ReturnValue| {
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
            |_s: &mut PinScope, _a: FunctionCallbackArguments, mut rv: ReturnValue| {
                let b = v8::Boolean::new(_s, false);
                rv.set(b.into());
            }
        );

        // ---- DOM 核心 ----
        defn!(
            "__createEl",
            |_s: &mut PinScope, a: FunctionCallbackArguments, mut rv: ReturnValue| {
                let id = qb::create_el(arg_string(_s, &a, 0));
                rv.set(v8::Number::new(_s, id).into());
            }
        );
        defn!(
            "__createDetachedEl",
            |_s: &mut PinScope, a: FunctionCallbackArguments, mut rv: ReturnValue| {
                let id = qb::create_detached_el(arg_string(_s, &a, 0));
                rv.set(v8::Number::new(_s, id).into());
            }
        );
        defn!(
            "__appendChild",
            |s: &mut PinScope, a: FunctionCallbackArguments, _rv: ReturnValue| {
                qb::append_child(arg_f64(s, &a, 0), arg_f64(s, &a, 1));
            }
        );
        defn!(
            "__removeChild",
            |s: &mut PinScope, a: FunctionCallbackArguments, _rv: ReturnValue| {
                qb::remove_child(arg_f64(s, &a, 0), arg_f64(s, &a, 1));
            }
        );
        defn!(
            "__insertBefore",
            |s: &mut PinScope, a: FunctionCallbackArguments, _rv: ReturnValue| {
                qb::insert_before(arg_f64(s, &a, 0), arg_f64(s, &a, 1), arg_f64(s, &a, 2));
            }
        );
        defn!("__setText", |s: &mut PinScope,
                            a: FunctionCallbackArguments,
                            _rv: ReturnValue| {
            qb::set_text(arg_f64(s, &a, 0), arg_string(s, &a, 1));
        });
        defn!("__setAttr", |s: &mut PinScope,
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
            |s: &mut PinScope, a: FunctionCallbackArguments, _rv: ReturnValue| {
                qb::remove_attr(arg_f64(s, &a, 0), arg_string(s, &a, 1));
            }
        );
        defn!("__setBody", |_s: &mut PinScope,
                            a: FunctionCallbackArguments,
                            _rv: ReturnValue| {
            qb::set_body(arg_string(_s, &a, 0));
        });
        defn!(
            "__appendBody",
            |_s: &mut PinScope, a: FunctionCallbackArguments, _rv: ReturnValue| {
                qb::append_body(arg_string(_s, &a, 0));
            }
        );
        defn!(
            "__setTitle",
            |_s: &mut PinScope, a: FunctionCallbackArguments, _rv: ReturnValue| {
                qb::set_title(arg_string(_s, &a, 0));
            }
        );

        // ---- DOM 读取族 ----
        defn!(
            "__getText",
            |s: &mut PinScope, a: FunctionCallbackArguments, mut rv: ReturnValue| {
                let v = qb::get_text(arg_f64(s, &a, 0));
                ret_str(s, &mut rv, &v);
            }
        );
        defn!("__getTag", |_s: &mut PinScope,
                           a: FunctionCallbackArguments,
                           mut rv: ReturnValue| {
            let v = qb::get_tag(arg_f64(_s, &a, 0));
            ret_str(_s, &mut rv, &v);
        });
        defn!(
            "__getTagName",
            |_s: &mut PinScope, a: FunctionCallbackArguments, mut rv: ReturnValue| {
                let v = qb::get_tag(arg_f64(_s, &a, 0));
                ret_str(_s, &mut rv, &v);
            }
        );
        defn!(
            "__findTag",
            |_s: &mut PinScope, a: FunctionCallbackArguments, mut rv: ReturnValue| {
                let v = qb::get_tag_by_name(arg_string(_s, &a, 0));
                rv.set(v8::Number::new(_s, v).into());
            }
        );
        defn!(
            "__getAttr",
            |_s: &mut PinScope, a: FunctionCallbackArguments, mut rv: ReturnValue| {
                let v = qb::get_attr(arg_f64(_s, &a, 0), arg_string(_s, &a, 1));
                match v {
                    Some(s) => ret_str(_s, &mut rv, &s),
                    None => rv.set(v8::null(_s).into()),
                }
            }
        );
        defn!(
            "__getElById",
            |_s: &mut PinScope, a: FunctionCallbackArguments, mut rv: ReturnValue| {
                let v = qb::get_el_by_id(arg_string(_s, &a, 0));
                rv.set(v8::Number::new(_s, v).into());
            }
        );
        defn!("__qs", |_s: &mut PinScope,
                       a: FunctionCallbackArguments,
                       mut rv: ReturnValue| {
            let v = qb::qs(arg_string(_s, &a, 0));
            rv.set(v8::Number::new(_s, v).into());
        });
        defn!("__qsAll", |_s: &mut PinScope,
                          a: FunctionCallbackArguments,
                          mut rv: ReturnValue| {
            let v = qb::qs_all(arg_string(_s, &a, 0));
            ret_str(_s, &mut rv, &v);
        });
        defn!(
            "__qsMatch",
            |_s: &mut PinScope, a: FunctionCallbackArguments, mut rv: ReturnValue| {
                let v = qb::qs_match(arg_f64(_s, &a, 0), arg_string(_s, &a, 1));
                rv.set(v8::Boolean::new(_s, v).into());
            }
        );
        defn!(
            "__qsClosest",
            |_s: &mut PinScope, a: FunctionCallbackArguments, mut rv: ReturnValue| {
                let v = qb::qs_closest(arg_f64(_s, &a, 0), arg_string(_s, &a, 1));
                rv.set(v8::Number::new(_s, v).into());
            }
        );
        defn!(
            "__qsCheck",
            |_s: &mut PinScope, a: FunctionCallbackArguments, mut rv: ReturnValue| {
                let v = crate::bridge::qs_syntax_error(&arg_string(_s, &a, 0)).is_none();
                rv.set(v8::Boolean::new(_s, v).into());
            }
        );
        defn!(
            "__offsetWidth",
            |_s: &mut PinScope, a: FunctionCallbackArguments, mut rv: ReturnValue| {
                let v = qb::offset_width(arg_f64(_s, &a, 0));
                rv.set(v8::Number::new(_s, v).into());
            }
        );
        defn!("__allIds", |_s: &mut PinScope,
                           _a: FunctionCallbackArguments,
                           mut rv: ReturnValue| {
            let v = qb::all_ids();
            ret_str(_s, &mut rv, &v);
        });
        defn!(
            "__attrsOf",
            |_s: &mut PinScope, a: FunctionCallbackArguments, mut rv: ReturnValue| {
                let v = qb::attrs_of(arg_f64(_s, &a, 0));
                ret_str(_s, &mut rv, &v);
            }
        );
        defn!(
            "__textData",
            |_s: &mut PinScope, a: FunctionCallbackArguments, mut rv: ReturnValue| {
                let v = qb::text_data(arg_f64(_s, &a, 0));
                ret_str(_s, &mut rv, &v);
            }
        );
        defn!(
            "__visibleBodyTextLen",
            |_s: &mut PinScope, _a: FunctionCallbackArguments, mut rv: ReturnValue| {
                let v = qb::visible_body_text_len();
                rv.set(v8::Number::new(_s, v).into());
            }
        );
        defn!(
            "__getBody",
            |_s: &mut PinScope, _a: FunctionCallbackArguments, mut rv: ReturnValue| {
                let v = qb::get_body();
                rv.set(v8::Number::new(_s, v).into());
            }
        );
        defn!(
            "__getParent",
            |_s: &mut PinScope, a: FunctionCallbackArguments, mut rv: ReturnValue| {
                let v = qb::get_parent(arg_f64(_s, &a, 0));
                rv.set(v8::Number::new(_s, v).into());
            }
        );
        defn!(
            "__children",
            |_s: &mut PinScope, a: FunctionCallbackArguments, mut rv: ReturnValue| {
                let v = qb::children(arg_f64(_s, &a, 0));
                ret_str(_s, &mut rv, &v);
            }
        );
        defn!(
            "__getValue",
            |_s: &mut PinScope, a: FunctionCallbackArguments, mut rv: ReturnValue| {
                let v = qb::get_attr(arg_f64(_s, &a, 0), "value".to_string());
                match v {
                    Some(s) => ret_str(_s, &mut rv, &s),
                    None => rv.set(v8::null(_s).into()),
                }
            }
        );
        defn!(
            "__setValue",
            |s: &mut PinScope, a: FunctionCallbackArguments, _rv: ReturnValue| {
                qb::set_attr(arg_f64(s, &a, 0), "value".to_string(), arg_string(s, &a, 1));
            }
        );
        defn!("__click", |_s: &mut PinScope,
                          _a: FunctionCallbackArguments,
                          _rv: ReturnValue| {});
        defn!("__submit", |_s: &mut PinScope,
                           _a: FunctionCallbackArguments,
                           _rv: ReturnValue| {});

        // ---- fetch 族 ----
        defn!(
            "__fetchSetBody",
            |s: &mut PinScope, a: FunctionCallbackArguments, _rv: ReturnValue| {
                qb::fetch_set_body(arg_string(s, &a, 0));
            }
        );
        defn!(
            "__fetchAppendBody",
            |s: &mut PinScope, a: FunctionCallbackArguments, _rv: ReturnValue| {
                qb::fetch_append_body(arg_string(s, &a, 0));
            }
        );
        defn!(
            "__fetchSync",
            |_s: &mut PinScope, a: FunctionCallbackArguments, mut rv: ReturnValue| {
                let v = qb::fetch_sync(arg_string(_s, &a, 0));
                match v {
                    Some(s) => ret_str(_s, &mut rv, &s),
                    None => rv.set(v8::null(_s).into()),
                }
            }
        );
        defn!(
            "__fetchSyncMethod",
            |_s: &mut PinScope, a: FunctionCallbackArguments, mut rv: ReturnValue| {
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
                    None => rv.set(v8::null(_s).into()),
                }
            }
        );
        defn!(
            "__fetchScriptMimeOk",
            |_s: &mut PinScope, a: FunctionCallbackArguments, mut rv: ReturnValue| {
                let v = crate::scripts::fetch_script_mime_ok(arg_string(_s, &a, 0));
                rv.set(v8::Boolean::new(_s, v).into());
            }
        );
        defn!(
            "__cacheAsset",
            |s: &mut PinScope, a: FunctionCallbackArguments, _rv: ReturnValue| {
                qb::cache_asset(arg_string(s, &a, 0), arg_string(s, &a, 1));
            }
        );

        // ---- parseHtml ----
        defn!(
            "__parseHtml",
            |s: &mut PinScope, a: FunctionCallbackArguments, _rv: ReturnValue| {
                qb::parse_html(arg_f64(s, &a, 0), arg_string(s, &a, 1));
            }
        );

        // ---- storage ----
        defn!(
            "__storageGet",
            |_s: &mut PinScope, a: FunctionCallbackArguments, mut rv: ReturnValue| {
                let v = qb::storage_get(arg_string(_s, &a, 0));
                match v {
                    Some(s) => ret_str(_s, &mut rv, &s),
                    None => rv.set(v8::null(_s).into()),
                }
            }
        );
        defn!(
            "__storageSet",
            |s: &mut PinScope, a: FunctionCallbackArguments, _rv: ReturnValue| {
                qb::storage_set(arg_string(s, &a, 0), arg_string(s, &a, 1));
            }
        );
        defn!(
            "__storageRemove",
            |s: &mut PinScope, a: FunctionCallbackArguments, _rv: ReturnValue| {
                qb::storage_remove(arg_string(s, &a, 0));
            }
        );
        defn!(
            "__storageLen",
            |_s: &mut PinScope, _a: FunctionCallbackArguments, mut rv: ReturnValue| {
                let v = qb::storage_len();
                rv.set(v8::Number::new(_s, v).into());
            }
        );
        defn!(
            "__storageKey",
            |_s: &mut PinScope, a: FunctionCallbackArguments, mut rv: ReturnValue| {
                let v = qb::storage_key(arg_f64(_s, &a, 0));
                match v {
                    Some(s) => ret_str(_s, &mut rv, &s),
                    None => rv.set(v8::null(_s).into()),
                }
            }
        );
        defn!(
            "__storageClear",
            |_s: &mut PinScope, _a: FunctionCallbackArguments, _rv: ReturnValue| {}
        );

        // ---- canvas 2D ----
        defn!("__cvNew", |_s: &mut PinScope,
                          a: FunctionCallbackArguments,
                          mut rv: ReturnValue| {
            let v = crate::canvas2d::cv_new(arg_f64(_s, &a, 0), arg_f64(_s, &a, 1));
            rv.set(v8::Number::new(_s, v).into());
        });
        defn!(
            "__cvResize",
            |_s: &mut PinScope, a: FunctionCallbackArguments, _rv: ReturnValue| {
                crate::canvas2d::cv_resize(
                    arg_f64(_s, &a, 0),
                    arg_f64(_s, &a, 1),
                    arg_f64(_s, &a, 2),
                );
            }
        );
        defn!(
            "__cvBeginPath",
            |_s: &mut PinScope, a: FunctionCallbackArguments, _rv: ReturnValue| {
                crate::canvas2d::cv_begin_path(arg_f64(_s, &a, 0));
            }
        );
        defn!("__cvRect", |_s: &mut PinScope,
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
        defn!("__cvArc", |_s: &mut PinScope,
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
            |s: &mut PinScope, a: FunctionCallbackArguments, _rv: ReturnValue| {
                crate::canvas2d::cv_set_style(
                    &arg_string(s, &a, 0),
                    arg_f64(s, &a, 1),
                    a.get(2).boolean_value(s),
                );
            }
        );
        defn!("__cvFill", |s: &mut PinScope,
                           a: FunctionCallbackArguments,
                           _rv: ReturnValue| {
            crate::canvas2d::cv_fill(arg_f64(s, &a, 0), &arg_string(s, &a, 1));
        });
        defn!(
            "__cvFillRect",
            |_s: &mut PinScope, a: FunctionCallbackArguments, _rv: ReturnValue| {
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
            |s: &mut PinScope, a: FunctionCallbackArguments, _rv: ReturnValue| {
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
            |_s: &mut PinScope, a: FunctionCallbackArguments, mut rv: ReturnValue| {
                let v = crate::canvas2d::cv_to_data_url(arg_f64(_s, &a, 0));
                ret_str(_s, &mut rv, &v);
            }
        );
        defn!(
            "__cvGetImageData",
            |_s: &mut PinScope, a: FunctionCallbackArguments, mut rv: ReturnValue| {
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
            |s: &mut PinScope, a: FunctionCallbackArguments, _rv: ReturnValue| {
                crate::canvas2d::cv_put_image_data(
                    arg_f64(s, &a, 0),
                    &arg_string(s, &a, 1),
                    arg_f64(s, &a, 2),
                    arg_f64(s, &a, 3),
                    arg_f64(s, &a, 4),
                );
            }
        );

        // ---- ws/xhr/location/history/nav ----
        defn!(
            "__wsCreate",
            |_s: &mut PinScope, a: FunctionCallbackArguments, mut rv: ReturnValue| {
                let v = crate::bridge::ws_create(arg_string(_s, &a, 0)) as f64;
                rv.set(v8::Number::new(_s, v).into());
            }
        );
        defn!("__wsSend", |s: &mut PinScope,
                           a: FunctionCallbackArguments,
                           _rv: ReturnValue| {
            crate::bridge::ws_send(arg_f64(s, &a, 0) as u32, arg_string(s, &a, 1));
        });
        defn!("__wsClose", |s: &mut PinScope,
                            a: FunctionCallbackArguments,
                            _rv: ReturnValue| {
            crate::bridge::ws_close(arg_f64(s, &a, 0) as u32);
        });
        defn!(
            "__xhrCreate",
            |_s: &mut PinScope, _a: FunctionCallbackArguments, mut rv: ReturnValue| {
                rv.set(v8::Number::new(_s, 0.0).into());
            }
        );
        defn!("__xhrOpen", |_s: &mut PinScope,
                            _a: FunctionCallbackArguments,
                            _rv: ReturnValue| {});
        defn!("__xhrSend", |_s: &mut PinScope,
                            _a: FunctionCallbackArguments,
                            _rv: ReturnValue| {});
        defn!(
            "__xhrGetResponseText",
            |_s: &mut PinScope, _a: FunctionCallbackArguments, mut rv: ReturnValue| {
                rv.set(v8::null(_s).into());
            }
        );
        defn!(
            "__locationHref",
            |_s: &mut PinScope, _a: FunctionCallbackArguments, mut rv: ReturnValue| {
                let v = qb::location_href();
                ret_str(_s, &mut rv, &v);
            }
        );
        defn!(
            "__locationParts",
            |_s: &mut PinScope, _a: FunctionCallbackArguments, mut rv: ReturnValue| {
                let v = r#"{"href":"about:blank","protocol":"about:","host":"","pathname":"","search":"","hash":""}"#.to_string();
                ret_str(_s, &mut rv, &v);
            }
        );
        defn!(
            "__locationAssign",
            |_s: &mut PinScope, _a: FunctionCallbackArguments, _rv: ReturnValue| {}
        );
        defn!(
            "__locationReplace",
            |_s: &mut PinScope, _a: FunctionCallbackArguments, _rv: ReturnValue| {}
        );
        defn!(
            "__historyPush",
            |_s: &mut PinScope, _a: FunctionCallbackArguments, _rv: ReturnValue| {}
        );
        defn!(
            "__historyReplace",
            |_s: &mut PinScope, _a: FunctionCallbackArguments, _rv: ReturnValue| {}
        );
        defn!(
            "__historyBack",
            |_s: &mut PinScope, _a: FunctionCallbackArguments, _rv: ReturnValue| {}
        );
        defn!(
            "__historyForward",
            |_s: &mut PinScope, _a: FunctionCallbackArguments, _rv: ReturnValue| {}
        );
        defn!(
            "__historyGo",
            |_s: &mut PinScope, _a: FunctionCallbackArguments, _rv: ReturnValue| {}
        );
        defn!(
            "__historyLen",
            |_s: &mut PinScope, _a: FunctionCallbackArguments, mut rv: ReturnValue| {
                rv.set(v8::Number::new(_s, 1.0).into());
            }
        );
        defn!(
            "__historyState",
            |_s: &mut PinScope, _a: FunctionCallbackArguments, mut rv: ReturnValue| {
                rv.set(v8::null(_s).into());
            }
        );
        defn!(
            "__navRecord",
            |s: &mut PinScope, a: FunctionCallbackArguments, _rv: ReturnValue| {
                crate::bridge::record_pending_navigation(&arg_string(s, &a, 0));
            }
        );

        // ---- worker ----
        defn!(
            "__workerRun",
            |s: &mut PinScope, a: FunctionCallbackArguments, mut rv: ReturnValue| {
                let r =
                    crate::engine_quickjs::worker_run(&arg_string(s, &a, 0), &arg_string(s, &a, 1));
                ret_str(s, &mut rv, &r);
            }
        );
        defn!(
            "__workerRunSrc",
            |s: &mut PinScope, a: FunctionCallbackArguments, mut rv: ReturnValue| {
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
        // etsl = 33（v8 152 与 Chrome 153 同源）
        let etsl = e.eval_string("eval.toString().length").unwrap_or_default();
        assert_eq!(etsl, "33", "V8 152 etsl 应为 33");
    }

    #[test]
    fn v8_core_bridges_dom() {
        let tree = browser_html_parser::parse("<html><body></body></html>");
        let _guard = crate::bridge::install_current(tree);
        let mut e = V8Engine::new().expect("v8 init");
        assert!(e.install_core_bridges(), "bridges install");
        let out = e.eval_string(
            "(function(){ var el = __createEl('div'); __setAttr(el,'id','v8t'); return typeof el+':'+el; })()",
        );
        let ok = out
            .as_deref()
            .and_then(|s| s.strip_prefix("number:"))
            .and_then(|n| n.parse::<i64>().ok())
            .is_some_and(|n| n > 0);
        assert!(ok, "bridge should return positive NodeId, got {out:?}");
    }
}
