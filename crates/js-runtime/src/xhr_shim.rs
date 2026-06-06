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

use boa_engine::{Context, JsResult};

/// 在 `ctx` 上注册全局 `XMLHttpRequest` 构造器。
///
/// 实现（纯 JS 原型，避开 native fn 的 this 绑定问题）：
/// - `install` 时 eval 一段 JS，定义 XMLHttpRequest 为构造器函数，
///   prototype 上挂 open/send/getResponseText。
/// - send 内部调 `__xhrSend(id)` 同步 fetch，然后用 setTimeout(0)
///   触发 onload（闭包捕获 self，this 绑定正确）。
/// - 这样 `this` 在 JS 语义下正确传递，boa 不会丢失。
pub fn install_xml_http_request(ctx: &mut Context) -> JsResult<()> {
    let js = r#"
function XMLHttpRequest() {
    this.__xhrId = __xhrCreate();
    this.onload = null;
    this.responseText = '';
}
XMLHttpRequest.prototype.open = function(method, url) {
    __xhrOpen(this.__xhrId, method, url);
};
XMLHttpRequest.prototype.send = function() {
    __xhrSend(this.__xhrId);
    this.responseText = __xhrGetResponseText(this.__xhrId);
    var self = this;
    setTimeout(function() {
        if (typeof self.onload === 'function') {
            self.responseText = __xhrGetResponseText(self.__xhrId);
            self.onload.call(self);
        }
    }, 0);
};
XMLHttpRequest.prototype.getResponseText = function() {
    return __xhrGetResponseText(this.__xhrId);
};
"#;
    ctx.eval(boa_engine::Source::from_bytes(js))?;
    Ok(())
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
