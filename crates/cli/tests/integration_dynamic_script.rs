//! M69: 动态 script 执行集成测试。
//!
//! 验证 `document.createElement("script") + appendChild(s)` 触发 JS 执行：
//! - inline script（textContent）
//! - 外链 script（src，需本地 HTTP server）
//! - onload/onerror 回调时序
//! - 多层链式加载（模拟 webpack runtime→chunk）
//! - 非 script 元素不误触发
//!
//! 这是 webpack/vite 等前端工程化站点的核心加载机制。修复前 appendChild 只做
//! DOM 树移动，不触发 script 执行，导致所有动态加载 chunk 的 SPA 渲染失败。

use assert_cmd::Command;
use predicates::prelude::*;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Once;
use std::thread;

fn bin() -> Command {
    Command::cargo_bin("browser").expect("browser binary not found")
}

fn fixture_path(name: &str) -> String {
    format!("../../tests/fixtures/spa/{name}.html")
}

// ===== 本地 HTTP server（供外链 script 测试）=====

static mut SERVER_PORT: u16 = 0;
static SERVER_INIT: Once = Once::new();

/// 启动一次性 HTTP server，返回 base URL（如 http://127.0.0.1:PORT）。
/// server 线程在整个测试进程生命周期内存活。
fn server_base_url() -> String {
    SERVER_INIT.call_once(|| {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind failed");
        let port = listener.local_addr().expect("local_addr").port();
        // safety: SERVER_INIT.call_once 保证单线程写入
        unsafe {
            SERVER_PORT = port;
        }
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                // 读请求（忽略内容）
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf);
                let req = String::from_utf8_lossy(&buf);
                // 解析 GET 行：GET /path HTTP/1.1
                let path = req
                    .lines()
                    .next()
                    .and_then(|l| l.split_whitespace().nth(1))
                    .unwrap_or("/");
                let (status, body, content_type) = serve_file(path);
                let resp = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(resp.as_bytes());
                let _ = stream.write_all(body.as_bytes());
            }
        });
    });
    // safety: SERVER_INIT 已执行，SERVER_PORT 已初始化
    let port = unsafe { SERVER_PORT };
    format!("http://127.0.0.1:{port}")
}

/// 根据 path 返回 (status, body, content_type)。404 返回空 body。
fn serve_file(path: &str) -> (&'static str, String, &'static str) {
    match path {
        "/" | "/index.html" => (
            "200 OK",
            "<!DOCTYPE html><html><head></head><body><div id=\"app\">INIT</div>\
             <script>window.__entry = true;</script></body></html>"
                .to_string(),
            "text/html",
        ),
        "/external.html" => (
            "200 OK",
            "<!DOCTYPE html><html><head></head><body><div id=\"app\">INIT</div><script>\n\
             var s = document.createElement('script');\n\
             s.src = '/standalone.js';\n\
             document.head.appendChild(s);\n\
             setTimeout(function() {\n\
               document.getElementById('app').innerHTML = window.__standalone ? 'EXTERNAL_OK' : 'EXTERNAL_FAIL';\n\
             }, 50);\n\
             </script></body></html>"
                .to_string(),
            "text/html",
        ),
        "/chain.html" => (
            "200 OK",
            "<!DOCTYPE html><html><head></head><body><div id=\"app\">INIT</div><script>\n\
             var s = document.createElement('script');\n\
             s.src = '/chunk-a.js';\n\
             document.head.appendChild(s);\n\
             </script></body></html>"
                .to_string(),
            "text/html",
        ),
        "/onerror.html" => (
            "200 OK",
            "<!DOCTYPE html><html><head></head><body><div id=\"app\">INIT</div><script>\n\
             var s = document.createElement('script');\n\
             s.src = '/does-not-exist.js';\n\
             s.onerror = function() {\n\
               document.getElementById('app').innerHTML = 'ONERROR_FIRED';\n\
             };\n\
             document.head.appendChild(s);\n\
             </script></body></html>"
                .to_string(),
            "text/html",
        ),
        "/chunk-a.js" => (
            "200 OK",
            "window.__chunkA = true;\nvar s = document.createElement('script');\ns.src = '/chunk-b.js';\ndocument.head.appendChild(s);"
                .to_string(),
            "application/javascript",
        ),
        "/chunk-b.js" => (
            "200 OK",
            "window.__chunkB = true;\ndocument.getElementById('app').innerHTML = 'CHAIN_DONE';"
                .to_string(),
            "application/javascript",
        ),
        "/standalone.js" => (
            "200 OK",
            "window.__standalone = true;".to_string(),
            "application/javascript",
        ),
        _ => ("404 Not Found", String::new(), "text/plain"),
    }
}

// ===== 测试用例 =====

/// inline 动态 script（textContent）：appendChild 后应执行内联代码。
#[test]
fn dyn_script_inline_executes_on_appendchild() {
    bin()
        .args([
            "render-script",
            &fixture_path("dyn-script-inline"),
            "--width",
            "80",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("DYNAMIC_INLINE_OK"))
        // BEFORE 占位应被替换
        .stdout(predicate::str::contains("BEFORE").not());
}

/// inline 动态 script 的 onload 回调在 script eval 之后触发。
#[test]
fn dyn_script_onload_fires_after_eval() {
    bin()
        .args([
            "render-script",
            &fixture_path("dyn-script-onload"),
            "--width",
            "80",
        ])
        .assert()
        .success()
        // onload 读到的 window.__loaded 应是 eval 后的值
        .stdout(predicate::str::contains("ONLOAD_AFTER_EVAL"));
}

/// 非 script 元素（div）的 appendChild 不触发 eval。
/// div 的 textContent 是 JS 代码字符串——作为**文本**渲染出来是正常的（显示），
/// 但**不应被执行**。验收点：app div 的内容保持 UNCHANGED（JS 代码没真的跑）。
#[test]
fn dyn_script_div_appendchild_does_not_eval() {
    bin()
        .args([
            "render-script",
            &fixture_path("dyn-script-noscript"),
            "--width",
            "80",
        ])
        .assert()
        .success()
        // app div 保持 UNCHANGED——证明 div 的 textContent 没被执行
        .stdout(predicate::str::contains("UNCHANGED"));
}

/// 外链动态 script（src）：appendChild 触发 fetch + eval。
#[test]
fn dyn_script_external_src_executes() {
    let base = server_base_url();
    bin()
        .args([
            "render-url",
            &format!("{base}/external.html"),
            "--width",
            "80",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("EXTERNAL_OK"));
}

/// 多层链式加载：chunk-a 动态加载 chunk-b，验证两者都执行（模拟 webpack）。
#[test]
fn dyn_script_chained_loading_like_webpack() {
    let base = server_base_url();
    // chunk-a 设 window.__chunkA 并加载 chunk-b
    // chunk-b 设 window.__chunkB 并改 innerHTML 为 CHAIN_DONE
    bin()
        .args(["render-url", &format!("{base}/chain.html"), "--width", "80"])
        .assert()
        .success()
        // 两层都执行后 app 显示 CHAIN_DONE
        .stdout(predicate::str::contains("CHAIN_DONE"));
}

/// 外链 script 加载失败（404）触发 onerror 回调。
#[test]
fn dyn_script_onerror_fires_on_404() {
    let base = server_base_url();
    bin()
        .args([
            "render-url",
            &format!("{base}/onerror.html"),
            "--width",
            "80",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("ONERROR_FIRED"));
}
