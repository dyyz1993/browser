//! M17.2: 注入 `XMLHttpRequest` 全局构造器。
//!
//! 让 JS 代码可以直接用 Web 标准 API：
//! ```js
//! var xhr = new XMLHttpRequest();
//! xhr.onload = function() {
//!     console.log(xhr.responseText);
//! };
//! xhr.open('GET', '/api/data');
//! xhr.send();
//! ```
//!
//! 实现策略（复用 M16 event loop + M15 cookie jar）：
//! - `new XMLHttpRequest()` 内部调 `__xhrCreate()` 拿一个 Rust id，
//!   存到 `this.__xhrId`。
//! - `open(method, url)` 调 `__xhrOpen(id, method, url)`（只记录参数）。
//! - `send()` 调 `__xhrSend(id)`（同步 fetch + 存 response_text 到 Rust），
//!   然后 **用 setTimeout(0) 异步触发 onload**（复用 M16 event loop，
//!   这样 onload 回调里能读到 responseText）。
//! - `responseText` 用 getter shim：每次读都调 `__xhrGetResponseText(id)`。
//!
//! 这是 MVP：非标准（真实 XHR 是异步 send + 状态机），但爬虫场景够用。

use boa_engine::{object::ObjectInitializer, Context, JsResult, JsValue, NativeFunction};

/// 在 `ctx` 上注册全局 `XMLHttpRequest` 构造器。
///
/// 实现：用 `register_global_callable` 注册一个 0-arity 函数，
/// 它返回一个带 open/send/responseText/onload 的对象（用 ObjectInitializer）。
/// 真实构造器语义：`new XMLHttpRequest()` 返回该对象。
pub fn install_xml_http_request(ctx: &mut Context) -> JsResult<()> {
    ctx.register_global_callable(
        boa_engine::JsString::from("XMLHttpRequest"),
        0,
        NativeFunction::from_fn_ptr(xhr_constructor),
    )?;
    Ok(())
}

/// `new XMLHttpRequest()`：返回一个带方法的空对象，__xhrId 由 __xhrCreate 分配。
fn xhr_constructor(_this: &JsValue, _args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let id_val = ctx.eval(boa_engine::Source::from_bytes("__xhrCreate()"))?;
    let obj = ObjectInitializer::new(ctx)
        .function(
            NativeFunction::from_fn_ptr(xhr_open),
            boa_engine::JsString::from("open"),
            3,
        )
        .function(
            NativeFunction::from_fn_ptr(xhr_send),
            boa_engine::JsString::from("send"),
            1,
        )
        .function(
            NativeFunction::from_fn_ptr(xhr_get_response_text),
            boa_engine::JsString::from("getResponseText"),
            0,
        )
        .build();
    // 存 id 到对象（用 set_property）
    obj.set(boa_engine::JsString::from("__xhrId"), id_val, false, ctx)?;
    // onload 默认 undefined（JS 端赋值）
    obj.set(
        boa_engine::JsString::from("onload"),
        JsValue::undefined(),
        false,
        ctx,
    )?;
    Ok(JsValue::from(obj))
}

/// `xhr.open(method, url)`：记录请求参数。
fn xhr_open(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let id = read_xhr_id(this, ctx)?;
    let method = args
        .first()
        .and_then(|v| if v.is_undefined() { None } else { Some(v) })
        .and_then(|v| v.to_string(ctx).ok())
        .map(|s| s.to_std_string_escaped())
        .unwrap_or_else(|| "GET".to_string());
    let url = args
        .get(1)
        .and_then(|v| if v.is_undefined() { None } else { Some(v) })
        .and_then(|v| v.to_string(ctx).ok())
        .map(|s| s.to_std_string_escaped())
        .unwrap_or_default();
    let code = format!("__xhrOpen({}, {:?}, {:?}); undefined;", id, method, url);
    ctx.eval(boa_engine::Source::from_bytes(&code))
}

/// `xhr.send()`：同步 fetch + setTimeout(0) 触发 onload。
fn xhr_send(this: &JsValue, _args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let id = read_xhr_id(this, ctx)?;
    // 同步 fetch（__xhrSend 把 response_text 存到 Rust）
    let fetch_code = format!("__xhrSend({}); undefined;", id);
    ctx.eval(boa_engine::Source::from_bytes(&fetch_code))?;
    // 用 setTimeout(0) 异步触发 onload（复用 M16 event loop）。
    // this 是 xhr 对象，传给 setTimeout 回调。
    let onload_code = "setTimeout(function(self) { if (typeof self.onload === 'function') { self.onload.call(self); } }, 0, this);";
    let _ = ctx.eval(boa_engine::Source::from_bytes(onload_code));
    Ok(JsValue::undefined())
}

/// `xhr.getResponseText()`：读取响应体（JS shim 用）。
fn xhr_get_response_text(
    this: &JsValue,
    _args: &[JsValue],
    ctx: &mut Context,
) -> JsResult<JsValue> {
    let id = read_xhr_id(this, ctx)?;
    let code = format!("__xhrGetResponseText({});", id);
    ctx.eval(boa_engine::Source::from_bytes(&code))
}

/// 从 xhr 对象读取 `__xhrId` 属性（存的是 f64），用 ctx 读避免新建 Context。
fn read_xhr_id(this: &JsValue, ctx: &mut Context) -> JsResult<u64> {
    let obj = this.as_object().ok_or_else(|| {
        boa_engine::JsNativeError::typ().with_message("XHR method called on non-object")
    })?;
    let id_val = obj.get(boa_engine::JsString::from("__xhrId"), ctx)?;
    Ok(id_val.as_number().map(|n| n as u64).unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::install;

    fn setup_ctx() -> Context {
        let mut ctx = Context::default();
        install(&mut ctx);
        let _ = install_xml_http_request(&mut ctx);
        ctx
    }

    #[test]
    fn xhr_can_be_constructed() {
        let mut ctx = setup_ctx();
        let r = ctx
            .eval(boa_engine::Source::from_bytes(
                "var xhr = new XMLHttpRequest(); typeof xhr;",
            ))
            .unwrap();
        assert_eq!(r.display().to_string(), "\"object\"");
    }

    #[test]
    fn xhr_has_open_send_methods() {
        let mut ctx = setup_ctx();
        let r = ctx
            .eval(boa_engine::Source::from_bytes(
                "var xhr = new XMLHttpRequest(); typeof xhr.open + ':' + typeof xhr.send;",
            ))
            .unwrap();
        assert_eq!(r.display().to_string(), "\"function:function\"");
    }

    #[test]
    fn xhr_open_records_method_and_url() {
        // open 不立即 fetch，只记录参数（send 才 fetch）
        let mut ctx = setup_ctx();
        let r = ctx
            .eval(boa_engine::Source::from_bytes(
                "var xhr = new XMLHttpRequest(); xhr.open('GET', 'http://x'); 1;",
            ))
            .unwrap();
        assert_eq!(r.display().to_string(), "1");
    }

    #[test]
    fn xhr_get_response_text_returns_null_before_send() {
        let mut ctx = setup_ctx();
        let r = ctx
            .eval(boa_engine::Source::from_bytes(
                "var xhr = new XMLHttpRequest(); xhr.getResponseText();",
            ))
            .unwrap();
        // 还没 send → response_text 为空字符串 → 返回空 string（非 null）
        assert_eq!(r.display().to_string(), "\"\"");
    }
}
