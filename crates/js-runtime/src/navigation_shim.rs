//! M14.3: 注入 `history` 和 `location` JS 对象。
//!
//! 让 JS 代码可直接用 Web 标准 API：
//! ```js
//! history.pushState({page:1}, '', '/page1')
//! history.replaceState(null, '', '/home')
//! history.back() / forward() / go(-1)
//! history.length
//! history.state
//! location.href                  // 读当前 URL
//! location.replace('/new-url')   // 百度反爬用的 location.replace
//! location.assign('/new-url')
//! location.pathname / host / protocol / ...
//! ```
//!
//! 实现：用 boa ObjectInitializer 注册方法，方法内部直接 eval
//! `__history*` / `__location*` 全局函数（同 storage_shim 模式）。

use boa_engine::{object::ObjectInitializer, Context, JsResult, JsValue, NativeFunction};

/// 在 `ctx` 上注册 `history` 和 `location` 全局对象。
pub fn install_navigation_globals(ctx: &mut Context) -> JsResult<()> {
    let history = build_history_object(ctx)?;
    ctx.register_global_property(
        boa_engine::JsString::from("history"),
        history,
        boa_engine::property::Attribute::all(),
    )?;

    let location = build_location_object(ctx)?;
    ctx.register_global_property(
        boa_engine::JsString::from("location"),
        location,
        boa_engine::property::Attribute::all(),
    )?;

    Ok(())
}

fn build_history_object(ctx: &mut Context) -> JsResult<JsValue> {
    let obj = ObjectInitializer::new(ctx)
        .function(
            NativeFunction::from_fn_ptr(history_push),
            boa_engine::JsString::from("pushState"),
            3,
        )
        .function(
            NativeFunction::from_fn_ptr(history_replace),
            boa_engine::JsString::from("replaceState"),
            3,
        )
        .function(
            NativeFunction::from_fn_ptr(history_back),
            boa_engine::JsString::from("back"),
            0,
        )
        .function(
            NativeFunction::from_fn_ptr(history_forward),
            boa_engine::JsString::from("forward"),
            0,
        )
        .function(
            NativeFunction::from_fn_ptr(history_go),
            boa_engine::JsString::from("go"),
            1,
        )
        .function(
            NativeFunction::from_fn_ptr(history_length),
            boa_engine::JsString::from("length"),
            0,
        )
        .function(
            NativeFunction::from_fn_ptr(history_state),
            boa_engine::JsString::from("state"),
            0,
        )
        .build();
    Ok(JsValue::from(obj))
}

fn build_location_object(ctx: &mut Context) -> JsResult<JsValue> {
    let obj = ObjectInitializer::new(ctx)
        .function(
            NativeFunction::from_fn_ptr(location_href_get),
            boa_engine::JsString::from("href"),
            0,
        )
        .function(
            NativeFunction::from_fn_ptr(location_replace),
            boa_engine::JsString::from("replace"),
            1,
        )
        .function(
            NativeFunction::from_fn_ptr(location_assign),
            boa_engine::JsString::from("assign"),
            1,
        )
        .function(
            NativeFunction::from_fn_ptr(location_pathname),
            boa_engine::JsString::from("pathname"),
            0,
        )
        .function(
            NativeFunction::from_fn_ptr(location_host),
            boa_engine::JsString::from("host"),
            0,
        )
        .function(
            NativeFunction::from_fn_ptr(location_hostname),
            boa_engine::JsString::from("hostname"),
            0,
        )
        .function(
            NativeFunction::from_fn_ptr(location_protocol),
            boa_engine::JsString::from("protocol"),
            0,
        )
        .function(
            NativeFunction::from_fn_ptr(location_search),
            boa_engine::JsString::from("search"),
            0,
        )
        .function(
            NativeFunction::from_fn_ptr(location_hash),
            boa_engine::JsString::from("hash"),
            0,
        )
        .build();
    Ok(JsValue::from(obj))
}

fn arg_str(args: &[JsValue], idx: usize, ctx: &mut Context) -> Option<String> {
    args.get(idx)
        .and_then(|v| {
            if v.is_undefined() || v.is_null() {
                None
            } else {
                Some(v)
            }
        })
        .and_then(|v| v.to_string(ctx).ok())
        .map(|s| s.to_std_string_escaped())
}

// --- history.* ---

fn history_push(_this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    // 真实 API: pushState(stateObj, title, url)。我们把 stateObj 转成字符串。
    let state = arg_str(args, 0, ctx).unwrap_or_else(|| "null".to_string());
    let url = arg_str(args, 2, ctx).unwrap_or_default();
    let code = format!("__historyPush({:?}, '', {:?}); undefined;", state, url);
    ctx.eval(boa_engine::Source::from_bytes(&code))
}

fn history_replace(_this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let state = arg_str(args, 0, ctx).unwrap_or_else(|| "null".to_string());
    let url = arg_str(args, 2, ctx).unwrap_or_default();
    let code = format!("__historyReplace({:?}, '', {:?}); undefined;", state, url);
    ctx.eval(boa_engine::Source::from_bytes(&code))
}

fn history_back(_this: &JsValue, _args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    ctx.eval(boa_engine::Source::from_bytes("__historyBack();"))
}

fn history_forward(_this: &JsValue, _args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    ctx.eval(boa_engine::Source::from_bytes("__historyForward();"))
}

fn history_go(_this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let n = args
        .first()
        .and_then(|v| {
            if v.is_undefined() || v.is_null() {
                None
            } else {
                Some(v)
            }
        })
        .and_then(|v| v.to_number(ctx).ok())
        .map(|n| n as i64)
        .unwrap_or(0);
    let code = format!("__historyGo({n});");
    ctx.eval(boa_engine::Source::from_bytes(&code))
}

fn history_length(_this: &JsValue, _args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    ctx.eval(boa_engine::Source::from_bytes("__historyLen();"))
}

fn history_state(_this: &JsValue, _args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    ctx.eval(boa_engine::Source::from_bytes("__historyState();"))
}

// --- location.* ---

fn location_href_get(_this: &JsValue, _args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    ctx.eval(boa_engine::Source::from_bytes("__locationHref();"))
}

fn location_replace(_this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let url = arg_str(args, 0, ctx).unwrap_or_default();
    let code = format!("__locationReplace({:?}); undefined;", url);
    ctx.eval(boa_engine::Source::from_bytes(&code))
}

fn location_assign(_this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let url = arg_str(args, 0, ctx).unwrap_or_default();
    let code = format!("__locationAssign({:?}); undefined;", url);
    ctx.eval(boa_engine::Source::from_bytes(&code))
}

fn location_pathname(_this: &JsValue, _args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    ctx.eval(boa_engine::Source::from_bytes(
        "__locationParts().pathname;",
    ))
}

fn location_host(_this: &JsValue, _args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    ctx.eval(boa_engine::Source::from_bytes("__locationParts().host;"))
}

/// M62: location.hostname（host 去掉端口；React/base.js 等用 hostname 比较）。
fn location_hostname(_this: &JsValue, _args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    ctx.eval(boa_engine::Source::from_bytes(
        "(function() { var h = __locationParts().host || ''; return h.split(':')[0]; })();",
    ))
}

fn location_protocol(_this: &JsValue, _args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    ctx.eval(boa_engine::Source::from_bytes(
        "__locationParts().protocol;",
    ))
}

fn location_search(_this: &JsValue, _args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    ctx.eval(boa_engine::Source::from_bytes("__locationParts().search;"))
}

fn location_hash(_this: &JsValue, _args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    ctx.eval(boa_engine::Source::from_bytes("__locationParts().hash;"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::{install, install_navigation};
    use browser_navigation::new_navigation;

    fn setup_ctx(initial_url: &str) -> Context {
        let mut ctx = Context::default();
        install(&mut ctx);
        let handle = new_navigation(initial_url);
        install_navigation(handle);
        install_navigation_globals(&mut ctx).expect("install nav globals");
        ctx
    }

    fn str_result(v: boa_engine::JsValue) -> String {
        v.as_string()
            .map(|s| s.to_std_string_escaped())
            .unwrap_or_default()
    }

    fn num_result(v: boa_engine::JsValue) -> i64 {
        v.as_number().map(|n| n as i64).unwrap_or(-1)
    }

    #[test]
    fn history_length_starts_at_1() {
        let mut ctx = setup_ctx("https://example.com/");
        let r = ctx
            .eval(boa_engine::Source::from_bytes("history.length();"))
            .unwrap();
        assert_eq!(num_result(r), 1);
    }

    #[test]
    fn push_state_advances_length_and_href() {
        let mut ctx = setup_ctx("https://example.com/");
        let r = ctx
            .eval(boa_engine::Source::from_bytes(
                "history.pushState({p:1}, '', '/page1'); history.length();",
            ))
            .unwrap();
        assert_eq!(num_result(r), 2);
        let href = ctx
            .eval(boa_engine::Source::from_bytes("location.href();"))
            .unwrap();
        assert_eq!(str_result(href), "/page1");
    }

    #[test]
    fn replace_state_does_not_increase_length() {
        let mut ctx = setup_ctx("https://example.com/");
        let r = ctx
            .eval(boa_engine::Source::from_bytes(
                "history.replaceState(null, '', '/replaced'); history.length();",
            ))
            .unwrap();
        assert_eq!(num_result(r), 1);
        let href = ctx
            .eval(boa_engine::Source::from_bytes("location.href();"))
            .unwrap();
        assert_eq!(str_result(href), "/replaced");
    }

    #[test]
    fn history_state_round_trip() {
        let mut ctx = setup_ctx("https://example.com/");
        let r = ctx
            .eval(boa_engine::Source::from_bytes(
                "history.pushState({k:'v'}, '', '/p'); history.state();",
            ))
            .unwrap();
        // state 是 [object Object] 的字符串形式（MVP 简化）
        assert!(!str_result(r).is_empty());
    }

    #[test]
    fn location_replace_updates_href() {
        let mut ctx = setup_ctx("https://example.com/");
        let _ = ctx.eval(boa_engine::Source::from_bytes(
            "location.replace('/new-url');",
        ));
        let href = ctx
            .eval(boa_engine::Source::from_bytes("location.href();"))
            .unwrap();
        assert_eq!(str_result(href), "/new-url");
    }

    #[test]
    fn location_pathname_returns_parsed_part() {
        let mut ctx = setup_ctx("https://example.com/foo/bar?q=1#h");
        let p = ctx
            .eval(boa_engine::Source::from_bytes("location.pathname();"))
            .unwrap();
        assert_eq!(str_result(p), "/foo/bar");
    }

    #[test]
    fn location_host_returns_hostname_and_port() {
        let mut ctx = setup_ctx("https://example.com:8080/");
        let h = ctx
            .eval(boa_engine::Source::from_bytes("location.host();"))
            .unwrap();
        assert_eq!(str_result(h), "example.com:8080");
    }

    #[test]
    fn location_protocol_returns_scheme_with_colon() {
        let mut ctx = setup_ctx("https://example.com/");
        let p = ctx
            .eval(boa_engine::Source::from_bytes("location.protocol();"))
            .unwrap();
        assert_eq!(str_result(p), "https:");
    }
}
