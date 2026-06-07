//! M31 e2e: wss:// WebSocket Secure 连接测试。
//!
//! M31 ws crate 加 native-tls 支持，wss:// 已可用。用 postman-echo
//! 真实 wss:// echo server 验证（echo.websocket.events 已停服）。
//! 默认 #[ignore]（依赖外部服务，CI 不跑）。
//!
//! 运行: cargo test -p browser-js-runtime --test integration_wss -- --ignored

use assert_cmd::Command;
use std::io::Write;

#[test]
#[ignore] // 依赖外部服务，默认 skip
fn wss_postman_echo() {
    // HTML 连接 wss://ws.postman-echo.com/raw（M31 已支持 wss://）
    let html = r#"<!doctype html><html><body>
<script>
var ws = new WebSocket('wss://ws.postman-echo.com/raw');
ws.onopen = function() { __setBody('OPEN'); ws.send('hello wss'); };
ws.onmessage = function(e) { __appendBody(' MSG:' + e.data); };
ws.onerror = function(e) { __setBody('ERR'); };
ws.onclose = function() { __appendBody(' CLOSE'); };
</script>
</body></html>"#;

    let tmp = std::env::temp_dir();
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = tmp.join(format!("m31-wss-{ts}.html"));
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

    assert!(!stdout.is_empty(), "stdout empty - script not executed");
    assert!(
        stdout.contains("OPEN"),
        "expected wss:// connection to open"
    );

    let _ = std::fs::remove_file(&path);
}
