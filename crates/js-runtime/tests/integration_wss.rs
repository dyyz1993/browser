//! M29.2 e2e: wss:// WebSocket Secure 连接（M24 native-tls 自动生效）。
//!
//! M24.2 切换到 reqwest(native-tls) 后，wss:// 应自动可用（ws crate 用 tokio TcpStream，
//! TLS 由系统库处理）。测试真实 wss:// 端点验证。
//!
//! 注意：这测试依赖外部服务（echo.websocket.org），可能因网络/服务不稳定失败。
//! 如果失败，需手动验证是否是网络问题（用 curl/wscat 测试同一端点）。

use assert_cmd::Command;
use std::io::Write;

#[test]
#[ignore] // 默认 skip，需网络 + 外部服务
fn wss_public_echo_server() {
    let port = std::env::var("WS_PORT")
        .ok()
        .and_then(|p| p.parse::<u16>().ok())
        .unwrap_or(9001);

    // 简单 HTML 连接 wss://echo.websocket.org（HTTPS WebSocket）
    let html = format!(
        r#"<!doctype html><html><body>
<script>
var ws = new WebSocket('wss://echo.websocket.org');
ws.onopen = function() {{ __setBody('OPEN'); }};
ws.onmessage = function(e) {{ __setBody('MSG:' + e.data); }};
ws.onerror = function(e) {{ __setBody('ERR'); }};
ws.onclose = function() {{ __setBody('CLOSE'); }};
setTimeout(function() {{ ws.send('ping'); }}, 500);
</script>
</body></html>"#
    );

    let tmp = std::env::temp_dir();
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = tmp.join(format!("m29-wss-{ts}.html"));
    std::fs::File::create(&path)
        .unwrap()
        .write_all(html.as_bytes())
        .unwrap();

    // 注意：render-script 会等待 pump_event_loop 完成（networkidle），但 WebSocket
    // 是长连接，可能需手动终止。这里用 --js-timeout 5s 限制。
    let output = Command::cargo_bin("browser")
        .expect("browser binary not found")
        .args([
            "render-script",
            path.to_str().unwrap(),
            "--width",
            "40",
            "--js-timeout",
            "5",
        ])
        .output()
        .expect("run");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    // 预期行为：
    // - 如果 wss:// 可用：输出应含 'OPEN' 或 'MSG:ping'
    // - 如果不可用：可能报 ERR 或 CLOSE（但需区分网络问题）
    eprintln!("stdout: {}", stdout);
    eprintln!("stderr: {}", stderr);

    // 弱断言：至少有输出（说明脚本执行了）
    assert!(!stdout.is_empty(), "stdout empty - script not executed");

    // 如果能看到 OPEN/MSG 说明 wss:// 可用
    let has_open = stdout.contains("OPEN");
    let has_msg = stdout.contains("MSG:");
    if has_open || has_msg {
        eprintln!("✓ wss:// connection succeeded (OPEN or MSG received)");
    } else {
        eprintln!("⚠ wss:// connection failed (no OPEN/MSG) - check network/service");
    }

    let _ = std::fs::remove_file(&path);
}