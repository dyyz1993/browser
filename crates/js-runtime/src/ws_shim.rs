//! M23.5: 注入 `WebSocket` 全局构造器（标准 Web API）。
//!
//! 让 JS 代码可以直接用 Web 标准 API：
//! ```js
//! var ws = new WebSocket('ws://localhost:8080/echo');
//! ws.onopen = function() { ws.send('hello'); };
//! ws.onmessage = function(e) { console.log('recv: ' + e.data); };
//! ws.onclose = function() { console.log('closed'); };
//! ```
//!
//! 实现策略（复用 M23.4 WsManager + M16 event loop）：
//! - `new WebSocket(url)` 内部调 `__wsCreate(url)` 拿 Rust id，
//!   存到 `this.__wsId`，并注册到全局 `__wsInstances[id]` 表。
//! - `send(data)` 调 `__wsSend(id, data)`（队列到后台线程）。
//! - `close()` 调 `__wsClose(id)`（队列关闭）。
//! - 后台线程收到 Open/Text/Closed/Error 事件 → pump_event_loop
//!   每轮 drain → eval `__wsDispatchEvent(id, type, data)` →
//!   分派到对应实例的 onopen/onmessage/onclose/onerror。
//!
//! 这是纯 JS 原型（参考 M17 xhr_shim），避开 boa native fn 的
//! `this` 绑定问题：所有方法在 JS 语义下 `this` 正确传递。

use boa_engine::{Context, JsResult};

/// 在 `ctx` 上注册全局 `WebSocket` 构造器 + `__wsDispatchEvent` 分派器。
///
/// 实现纯 JS eval（避开 native fn this 绑定丢失，参考 M17 教训）。
pub fn install_websocket(ctx: &mut Context) -> JsResult<()> {
    let js = r#"
// 全局实例表：__wsId → WebSocket 实例（Rust dispatch 用）
var __wsInstances = {};

function WebSocket(url) {
    this.__wsId = __wsCreate(url);
    this.url = url;
    this.readyState = 0; // CONNECTING
    this.onopen = null;
    this.onmessage = null;
    this.onclose = null;
    this.onerror = null;
    this.binaryType = 'blob';
    __wsInstances[this.__wsId] = this;
}
WebSocket.CONNECTING = 0;
WebSocket.OPEN = 1;
WebSocket.CLOSING = 2;
WebSocket.CLOSED = 3;
WebSocket.prototype.send = function(data) {
    __wsSend(this.__wsId, String(data));
};
WebSocket.prototype.close = function() {
    this.readyState = 2; // CLOSING
    __wsClose(this.__wsId);
};

// Rust drain 事件后 eval 这个。type: 'open'|'message'|'close'|'error'
// data: 'open'/'error' 时是消息字符串；'message' 时是文本载荷；
//       'close' 时是 reason 字符串。
function __wsDispatchEvent(id, type, data) {
    var ws = __wsInstances[id];
    if (!ws) return;
    if (type === 'open') {
        ws.readyState = 1; // OPEN
        if (typeof ws.onopen === 'function') {
            ws.onopen.call(ws, { type: 'open' });
        }
    } else if (type === 'message') {
        if (typeof ws.onmessage === 'function') {
            ws.onmessage.call(ws, { type: 'message', data: data });
        }
    } else if (type === 'close') {
        ws.readyState = 3; // CLOSED
        if (typeof ws.onclose === 'function') {
            ws.onclose.call(ws, { type: 'close', reason: data || '' });
        }
    } else if (type === 'error') {
        if (typeof ws.onerror === 'function') {
            ws.onerror.call(ws, { type: 'error', message: data || '' });
        }
    }
}
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
        // 注册 __ws* 桥（install 里注册了，这里只需 ws_shim）
        let _ = install_websocket(&mut ctx);
        ctx
    }

    #[test]
    fn websocket_constructor_exists() {
        let mut ctx = setup_ctx();
        let r = ctx
            .eval(boa_engine::Source::from_bytes("typeof WebSocket"))
            .unwrap();
        assert_eq!(r.display().to_string(), "\"function\"");
    }

    #[test]
    fn websocket_constants_defined() {
        let mut ctx = setup_ctx();
        let r = ctx
            .eval(boa_engine::Source::from_bytes(
                "WebSocket.CONNECTING + ',' + WebSocket.OPEN + ',' + WebSocket.CLOSING + ',' + WebSocket.CLOSED",
            ))
            .unwrap();
        assert_eq!(r.display().to_string(), "\"0,1,2,3\"");
    }

    #[test]
    fn dispatch_event_function_exists() {
        let mut ctx = setup_ctx();
        let r = ctx
            .eval(boa_engine::Source::from_bytes("typeof __wsDispatchEvent"))
            .unwrap();
        assert_eq!(r.display().to_string(), "\"function\"");
    }

    #[test]
    fn instances_table_is_object() {
        let mut ctx = setup_ctx();
        let r = ctx
            .eval(boa_engine::Source::from_bytes("typeof __wsInstances"))
            .unwrap();
        assert_eq!(r.display().to_string(), "\"object\"");
    }
}
