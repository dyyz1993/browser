//! M13.3: 注入 `localStorage` 和 `sessionStorage` JS 对象。
//!
//! 让 JS 代码可以直接用 Web 标准 API：
//! ```js
//! localStorage.setItem('token', 'abc');
//! const t = localStorage.getItem('token');  // 'abc'
//! localStorage.removeItem('token');
//! localStorage.clear();
//! const n = localStorage.length();
//! const k = localStorage.key(0);
//! ```
//!
//! 实现：用 boa `ObjectInitializer` 注册方法，方法内部直接转调
//! `__storage*` 全局函数（通过 eval，简单可靠）。

use boa_engine::{object::ObjectInitializer, Context, JsResult, JsValue, NativeFunction};

/// 在 `ctx` 上注册 `localStorage` 和 `sessionStorage` 全局对象。
/// 两个对象共享同一个后端（MVP 爬虫够用）。
pub fn install_storage_globals(ctx: &mut Context) -> JsResult<()> {
    let local = build_storage_object(ctx)?;
    ctx.register_global_property(
        boa_engine::JsString::from("localStorage"),
        local,
        boa_engine::property::Attribute::all(),
    )?;

    let session = build_storage_object(ctx)?;
    ctx.register_global_property(
        boa_engine::JsString::from("sessionStorage"),
        session,
        boa_engine::property::Attribute::all(),
    )?;

    Ok(())
}

fn build_storage_object(ctx: &mut Context) -> JsResult<JsValue> {
    let obj = ObjectInitializer::new(ctx)
        .function(
            NativeFunction::from_fn_ptr(get_item),
            boa_engine::JsString::from("getItem"),
            1,
        )
        .function(
            NativeFunction::from_fn_ptr(set_item),
            boa_engine::JsString::from("setItem"),
            2,
        )
        .function(
            NativeFunction::from_fn_ptr(remove_item),
            boa_engine::JsString::from("removeItem"),
            1,
        )
        .function(
            NativeFunction::from_fn_ptr(clear),
            boa_engine::JsString::from("clear"),
            0,
        )
        .function(
            NativeFunction::from_fn_ptr(key),
            boa_engine::JsString::from("key"),
            1,
        )
        .function(
            NativeFunction::from_fn_ptr(length_fn),
            boa_engine::JsString::from("length"),
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

fn get_item(_this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let key = arg_str(args, 0, ctx).unwrap_or_default();
    let code = format!("__storageGet({:?});", key);
    ctx.eval(boa_engine::Source::from_bytes(&code))
}

fn set_item(_this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let key = arg_str(args, 0, ctx).unwrap_or_default();
    let val = arg_str(args, 1, ctx).unwrap_or_default();
    let code = format!("__storageSet({:?}, {:?}); undefined;", key, val);
    ctx.eval(boa_engine::Source::from_bytes(&code))
}

fn remove_item(_this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let key = arg_str(args, 0, ctx).unwrap_or_default();
    let code = format!("__storageRemove({:?}); undefined;", key);
    ctx.eval(boa_engine::Source::from_bytes(&code))
}

fn clear(_this: &JsValue, _args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    ctx.eval(boa_engine::Source::from_bytes(
        "__storageClear(); undefined;",
    ))
}

fn key(_this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let idx = args
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
        .unwrap_or(-1);
    let code = format!("__storageKey({idx});");
    ctx.eval(boa_engine::Source::from_bytes(&code))
}

fn length_fn(_this: &JsValue, _args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    ctx.eval(boa_engine::Source::from_bytes("__storageLen();"))
}

#[cfg(test)]
mod tests {
    fn str_result(v: boa_engine::JsValue) -> String {
        v.as_string()
            .map(|s| s.to_std_string_escaped())
            .unwrap_or_default()
    }

    fn num_result(v: boa_engine::JsValue) -> i64 {
        v.as_number().map(|n| n as i64).unwrap_or(-1)
    }
    use super::*;
    use crate::bridge::{install, install_storage};
    use browser_storage::new_storage;

    fn setup_ctx() -> Context {
        let mut ctx = Context::default();
        install(&mut ctx);
        let handle = new_storage();
        install_storage(handle);
        install_storage_globals(&mut ctx).expect("install storage globals");
        ctx
    }

    #[test]
    fn set_item_persists_in_get_item() {
        let mut ctx = setup_ctx();
        let r = ctx
            .eval(boa_engine::Source::from_bytes(
                "localStorage.setItem('k', 'v'); localStorage.getItem('k');",
            ))
            .unwrap();
        assert_eq!(str_result(r), "v");
    }

    #[test]
    fn missing_key_returns_null() {
        let mut ctx = setup_ctx();
        let r = ctx
            .eval(boa_engine::Source::from_bytes(
                "localStorage.getItem('nope');",
            ))
            .unwrap();
        assert_eq!(r.display().to_string(), "null");
    }

    #[test]
    fn remove_item_deletes() {
        let mut ctx = setup_ctx();
        let r = ctx
            .eval(boa_engine::Source::from_bytes(
                "localStorage.setItem('k','v'); localStorage.removeItem('k'); localStorage.getItem('k');",
            ))
            .unwrap();
        assert!(r.is_null());
    }

    #[test]
    fn length_returns_count() {
        let mut ctx = setup_ctx();
        let r = ctx
            .eval(boa_engine::Source::from_bytes(
                "localStorage.setItem('a','1'); localStorage.setItem('b','2'); localStorage.length();",
            ))
            .unwrap();
        assert_eq!(num_result(r), 2);
    }

    #[test]
    fn clear_wipes_all() {
        let mut ctx = setup_ctx();
        let r = ctx
            .eval(boa_engine::Source::from_bytes(
                "localStorage.setItem('a','1'); localStorage.clear(); localStorage.length();",
            ))
            .unwrap();
        assert_eq!(num_result(r), 0);
    }

    #[test]
    fn session_storage_shares_with_local() {
        let mut ctx = setup_ctx();
        let r = ctx
            .eval(boa_engine::Source::from_bytes(
                "localStorage.setItem('s','1'); sessionStorage.getItem('s');",
            ))
            .unwrap();
        assert_eq!(str_result(r), "1");
    }
}
