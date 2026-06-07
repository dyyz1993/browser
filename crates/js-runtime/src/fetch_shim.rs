//! M19.1: 注入全局 `fetch` 函数（Web 标准 Promise-based API）。
//!
//! 让 JS 代码可以用现代 SPA 的标准模式：
//! ```js
//! const res = await fetch('/api/data');
//! const text = await res.text();
//! // 或
//! fetch('/api/data').then(res => res.text()).then(text => render(text));
//! ```
//!
//! 实现策略（复用 M16 Promise + M17 纯 JS shim 模式）：
//! - `fetch(url)` 返回一个 Promise（异步语义，符合 Web 标准）。
//! - Promise executor 内部：同步调 `__fetchSync(url)` 拿原始响应，
//!   然后用 `setTimeout(resolve, 0)` 让 resolve 走 event loop（macrotask）。
//! - Response 对象：`{ ok, status, statusText, text: () => Promise }`。
//!
//! 这是 MVP：
//! - 不支持 POST/PUT 等非 GET 方法（爬虫场景 GET 足够）
//! - 不支持 request headers / body（cookie jar 自动带）
//! - Response.text() 返回 Promise（标准语义），内部立即 resolve

use boa_engine::{Context, JsResult};

/// 在 `ctx` 上注册全局 `fetch` 函数。
pub fn install_fetch(ctx: &mut Context) -> JsResult<()> {
    let js = r#"
// __fetchSync 返回 "status\nbody" 编码（或 "" 失败）
function __parseFetchResponse(raw) {
    if (!raw) {
        return { ok: false, status: 0, statusText: 'network error', __body: '' };
    }
    var nl = raw.indexOf('\n');
    var status, body;
    if (nl < 0) {
        status = parseInt(raw, 10) || 0;
        body = '';
    } else {
        status = parseInt(raw.substring(0, nl), 10) || 0;
        body = raw.substring(nl + 1);
    }
    return {
        ok: status >= 200 && status < 300,
        status: status,
        statusText: status === 200 ? 'OK' : ('status ' + status),
        __body: body
    };
}

// Response 对象：标准 fetch Response 的子集
function __FetchResponse(raw) {
    var parsed = __parseFetchResponse(raw);
    this.ok = parsed.ok;
    this.status = parsed.status;
    this.statusText = parsed.statusText;
    this.__body = parsed.__body;
}
__FetchResponse.prototype.text = function() {
    // 标准：text() 返回 Promise。立即 resolve（body 已在构造时拿到）。
    var body = this.__body;
    return new Promise(function(resolve) { resolve(body); });
};
__FetchResponse.prototype.json = function() {
    var body = this.__body;
    return new Promise(function(resolve, reject) {
        try {
            resolve(JSON.parse(body));
        } catch (e) {
            reject(e);
        }
    });
};

// 全局 fetch：返回 Promise<Response>
globalThis.fetch = function(input) {
    var url = (typeof input === 'string') ? input
        : (input && input.url) ? input.url
        : String(input);
    return new Promise(function(resolve, reject) {
        // 同步 fetch（__fetchSync 带 cookie jar + networkidle 计数）
        var raw = __fetchSync(url);
        // setTimeout(0) 让 resolve 走 macrotask（符合 Web 标准：
        // fetch 总是异步 resolve，即使响应已就绪）
        setTimeout(function() {
            if (raw === '') {
                // 网络错误：reject（标准行为）
                reject(new TypeError('Failed to fetch ' + url));
            } else {
                resolve(new __FetchResponse(raw));
            }
        }, 0);
    });
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
        let _ = install_fetch(&mut ctx);
        ctx
    }

    #[test]
    fn fetch_is_a_function() {
        let mut ctx = setup_ctx();
        let r = ctx
            .eval(boa_engine::Source::from_bytes("typeof fetch"))
            .unwrap();
        assert_eq!(r.display().to_string(), "\"function\"");
    }

    #[test]
    fn fetch_returns_promise() {
        let mut ctx = setup_ctx();
        let r = ctx
            .eval(boa_engine::Source::from_bytes(
                "var p = fetch('http://localhost:1/x'); typeof p.then === 'function';",
            ))
            .unwrap();
        // fetch 必须返回 Promise（有 .then 方法）
        assert_eq!(r.display().to_string(), "true");
    }
}
