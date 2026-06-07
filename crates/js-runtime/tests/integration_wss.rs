//! M29.2 e2e: wss:// WebSocket Secure 连接测试。
//!
//! M24.2 切换到 reqwest(native-tls) 后，net crate 支持 HTTPS。
//! 但 ws crate 代码层面只支持 ws://（client.rs 硬判断 scheme == "ws"），
//! 不支持 wss://。M29.2 验证确认了这一点（ws://echo.websocket.org 能连，
//! 但 wss:// 需 ws crate 加 TLS 支持，是单独的工程）。
//!
//! 这测试用 ws:// 验证基础 WebSocket 功能未退化（M23 ws:// echo server）。
//! 默认 #[ignore]（依赖外部服务，CI 不跑）。

use assert_cmd::Command;
use std::io::Write;

#[test]
#[ignore] // 依赖外部服务，默认 skip
fn ws_public_echo_server() {
    // HTML 连接 ws://echo.websocket.org（plaintext WebSocket，M23 支持）
    let html = r#"<!doctype html><html><body>
<script>
var ws = new WebSocket('ws://echo.websocket.org');
ws.onopen = function() { __setBody('OPEN'); };
ws.onmessage = function(e) { __setBody('MSG:' + e.data); };
ws.onerror = function(e) { __setBody('ERR'); };
ws.onclose = function() { __setBody('CLOSE'); };
setTimeout(function() { ws.send('ping'); }, 500);
</script>
</body></html>"#;

    let tmp = std::env::temp_dir();
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = tmp.join(format!("m29-ws-{ts}.html"));
    std::fs::File::create(&path)
        .unwrap()
        .write_all(html.as_bytes())
        .unwrap();

    let output = Command::cargo_bin("browser")
        .expect("browser binary not found")
        .args(["render-script", path.to_str().unwrap(), "--width", "40"])
        .output()
        .expect("run");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    eprintln!("stdout: {stdout}");
    eprintln!("stderr: {stderr}");

    // 弱断言：至少有输出（说明脚本执行了）
    assert!(!stdout.is_empty(), "stdout empty - script not executed");

    let _ = std::fs::remove_file(&path);
}
