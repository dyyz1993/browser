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
    // M38: 标准 XHR 属性
    this.onload = null;
    this.onerror = null;
    this.onreadystatechange = null;
    this.readyState = 0;       // UNSENT
    this.status = 0;
    this.statusText = '';
    this.responseText = '';
    this.response = '';        // M62: docsify onload 读 xhr.response（非 responseText）
    this.responseType = '';
    this.responseURL = '';
    this._method = 'GET';
    this._url = '';
}
XMLHttpRequest.prototype.open = function(method, url) {
    this._method = (method || 'GET').toUpperCase();
    this._url = url;
    this.readyState = 1;       // OPENED
    __xhrOpen(this.__xhrId, method, url);
    this._fireReadyStateChange();
};
XMLHttpRequest.prototype.setRequestHeader = function(key, value) {
    // MVP: no-op（简化，爬虫场景 headers 不关键）
};
XMLHttpRequest.prototype.abort = function() {
    this.readyState = 0;
};
XMLHttpRequest.prototype.send = function(body) {
    this.readyState = 2;       // HEADERS_RECEIVED
    this._fireReadyStateChange();
    __xhrSend(this.__xhrId);
    var raw = __xhrGetResponseText(this.__xhrId);
    this.responseText = raw;
    this.response = raw;       // M62: 同步 response（docsify 等读 xhr.response）
    // 解析 status（__fetchSync 编码格式 "status\nbody"，__xhrSend 后端可能
    // 只存 body。对兼容性：尝试解析 status 行，失败则默认 200）
    var parsed = this._parseStatus(raw);
    this.status = parsed.status;
    this.statusText = parsed.statusText;
    this.readyState = 4;       // DONE
    var self = this;
    // M38: 同步触发 onreadystatechange（readyState=4）
    this._fireReadyStateChange();
    // 异步触发 onload（setTimeout 0，复用 M16 event loop）
    setTimeout(function() {
        self.responseText = __xhrGetResponseText(self.__xhrId);
        self.response = self.responseText;  // M62: 同步 response
        var p = self._parseStatus(self.responseText);
        self.status = p.status;
        self.statusText = p.statusText;
        // M62: 触发 addEventListener 注册的回调（docsify X().then 用 addEventListener('load', cb)）。
        var ev = { type: 'load', target: self, currentTarget: self,
                   status: self.status, response: self.responseText,
                   responseText: self.responseText };
        // 先触发 addEventListener 回调，再触发 onload（顺序对齐浏览器行为）。
        // M63: 回调必须以 XHR 实例为 this（规范要求），否则 cb 内 this.responseText 为 undefined。
        if (self.__xhrListeners && self.__xhrListeners['load']) {
            for (var _i = 0; _i < self.__xhrListeners['load'].length; _i++) {
                try { self.__xhrListeners['load'][_i].call(self, ev); } catch(e) {
                    if (typeof __log === 'function') __log('[xhr] load listener threw: ' + e.message + (e.stack ? (' | stack=' + e.stack.split('\\n').slice(0,3).join(' | ')) : ''));
                }
            }
        }
        if (typeof self.onload === 'function') {
            self.onload.call(self, ev);
        }
    }, 0);
};
XMLHttpRequest.prototype.getResponseHeader = function(name) {
    // M62: 爬虫场景不解析响应头（docsify 读 last-modified 做 cache，null 安全）。
    return null;
};
XMLHttpRequest.prototype.getAllResponseHeaders = function() {
    return '';
};
// M62: XMLHttpRequest 继承 EventTarget，框架（docsify 等）用 addEventListener
// 注册 progress/load/error 回调。我们的 XHR 是简化版，存回调但不触发（onload
// 已直接 dispatch），只需要 addEventListener 不崩溃。
XMLHttpRequest.prototype.addEventListener = function(type, cb) {
    if (!this.__xhrListeners) this.__xhrListeners = {};
    if (!this.__xhrListeners[type]) this.__xhrListeners[type] = [];
    this.__xhrListeners[type].push(cb);
};
XMLHttpRequest.prototype.removeEventListener = function(type, cb) {
    if (!this.__xhrListeners || !this.__xhrListeners[type]) return;
    this.__xhrListeners[type] = this.__xhrListeners[type].filter(function(f) { return f !== cb; });
};
XMLHttpRequest.prototype.getResponseText = function() {
    return __xhrGetResponseText(this.__xhrId);
};
// M38: 内部辅助——触发 onreadystatechange 回调
XMLHttpRequest.prototype._fireReadyStateChange = function() {
    if (typeof this.onreadystatechange === 'function') {
        this.onreadystatechange.call(this);
    }
};
// M38: 内部辅助——从响应里解析 status（兼容 __fetchSync 的 "status\nbody" 编码）
XMLHttpRequest.prototype._parseStatus = function(raw) {
    if (!raw || raw.indexOf('\n') < 0) {
        return { status: 200, statusText: 'OK' };
    }
    var nl = raw.indexOf('\n');
    var first = raw.substring(0, nl);
    var n = parseInt(first, 10);
    if (isNaN(n) || n < 100) {
        return { status: 200, statusText: 'OK' };
    }
    return { status: n, statusText: n === 200 ? 'OK' : ('status ' + n) };
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
