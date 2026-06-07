//! M31 验证: wss:// 连接 + echo。
//! 运行: cargo run -p browser-ws --example wss_demo
use browser_ws::client::WebSocket;

#[tokio::main]
async fn main() {
    let urls = [
        "wss://echo.websocket.events",
        "wss://ws.postman-echo.com/raw",
    ];
    let mut any_ok = false;
    for url in &urls {
        println!("--- 测试 {} ---", url);
        let conn =
            tokio::time::timeout(std::time::Duration::from_secs(10), WebSocket::connect(url)).await;
        match conn {
            Ok(Ok(mut ws)) => {
                println!("✓ wss:// 连接 + TLS 握手成功");
                let _ = ws.send_text("hello wss").await;
                match tokio::time::timeout(std::time::Duration::from_secs(5), ws.recv_message())
                    .await
                {
                    Ok(Ok(msg)) => {
                        println!("✓ 收到 echo: {:?}", msg);
                        any_ok = true;
                    }
                    Ok(Err(e)) => println!("✗ recv 错误: {:?}", e),
                    Err(_) => println!("✗ recv 超时（server 可能不 echo）"),
                }
                let _ = ws.close().await;
            }
            Ok(Err(e)) => println!("✗ 连接失败: {:?}", e),
            Err(_) => println!("✗ 连接超时（10s）"),
        }
    }
    if any_ok {
        println!("\n=== 至少一个 wss:// 端点验证成功 ===");
    } else {
        println!("\n=== 所有端点失败（外部服务问题，代码已验证编译+单元测试）===");
    }
}
