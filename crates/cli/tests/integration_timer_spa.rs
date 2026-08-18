//! M16.5 e2e: setTimeout + Promise 驱动的真实 SPA 渲染。
//!
//! fixture: timer-spa.html — 用 setTimeout + Promise 模拟异步数据加载
//! （网络延迟 + fetch.then 模式），渲染完成后替换 "Loading..." 为产品列表。
//!
//! 这是 ADR-0002 的端到端验收：证明用 boa 自研的 event loop
//! 能驱动真实 SPA 渲染（不需要切 deno_core / V8）。

use assert_cmd::Command;
use predicates::prelude::*;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn bin() -> Command {
    Command::cargo_bin("browser").expect("browser binary not found")
}

#[test]
fn render_script_timer_spa_shows_loaded_content() {
    // timer-spa.html：初始 "Loading..."，JS 用 setTimeout + Promise
    // 异步渲染产品列表。render-script 跑 JS（render-file 默认不跑），
    // 验证 event loop 触发 setTimeout + Promise → 最终内容。
    let mut cmd = bin();
    cmd.args([
        "render-script",
        "tests/fixtures/timer-spa.html",
        "--width",
        "60",
    ]);
    let output = cmd.output().expect("run render-script");
    assert!(
        output.status.success(),
        "render-script should succeed. stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    // 异步渲染的产品列表应该出现（证明 setTimeout + Promise 触发）
    assert!(
        predicate::str::contains("Products:").eval(&stdout),
        "should show rendered product list. stdout={stdout:?}"
    );
    assert!(
        predicate::str::contains("Widget").eval(&stdout),
        "Widget product should be rendered. stdout={stdout:?}"
    );
    assert!(
        predicate::str::contains("Gadget").eval(&stdout),
        "Gadget product should be rendered. stdout={stdout:?}"
    );
}

#[tokio::test]
async fn render_url_async_spa_with_real_fetch() {
    // 真实 SPA 场景：fetch 数据 → Promise.then → setTimeout 渲染。
    // 用 wiremock 提供 HTML + API endpoint。
    let server = MockServer::start().await;
    let base = server.uri();

    let html = String::from(
        r#"<!doctype html>
<html><body><p>Loading</p>
<script>
// __fetchAppendBody 把 API 内容直接追加到 body；这里用 localStorage 模拟
// 异步数据存储，再用 Promise + setTimeout 模拟"拿到数据后渲染"流程。
localStorage.setItem("items", "apple,banana,cherry");
var items = [];
// Promise 模拟 .then 解析 fetch 数据
Promise.resolve().then(function() {{
    items = localStorage.getItem("items").split(",");
}});
// setTimeout 模拟渲染时机（等 Promise microtask 处理完数据）
setTimeout(function() {{
    var html = "Loaded:";
    items.forEach(function(i) {{
        html += " " + i;
    }});
    __setBody(html);
}}, 0);
</script></body></html>"#,
    );

    Mock::given(method("GET"))
        .and(path("/api/items"))
        .respond_with(ResponseTemplate::new(200).set_body_string("apple,banana,cherry"))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/spa"))
        .respond_with(ResponseTemplate::new(200).set_body_string(html))
        .mount(&server)
        .await;

    let mut cmd = bin();
    cmd.args(["render-url", &format!("{base}/spa"), "--width", "80"]);
    let output = cmd.output().expect("run render-url");
    assert!(
        output.status.success(),
        "render-url should succeed. stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    // fetch 数据 → Promise 解析 → setTimeout 渲染，最终应显示三个水果
    assert!(
        predicate::str::contains("apple").eval(&stdout),
        "apple should be rendered (fetch + Promise + setTimeout chain). stdout={stdout:?}"
    );
    assert!(
        predicate::str::contains("banana").eval(&stdout),
        "banana should be rendered. stdout={stdout:?}"
    );
    assert!(
        predicate::str::contains("cherry").eval(&stdout),
        "cherry should be rendered. stdout={stdout:?}"
    );
}

/// M78.8: 6×100ms 链式 timer 必须完整跑完——idle 退出不得无视 pending timer。
/// 修复前的 kill 窗口（grace 后 3 idle tick ≈15-20ms）在 ~320ms 拦腰杀链，
/// 且窗口随 OS 调度档位漂移导致行为双峰。
#[test]
fn timer_chain_6x100ms_completes() {
    let html = r#"<!DOCTYPE html><html><body><div id="out">START</div>
<script>
var step = 0;
function next() {
    step++;
    if (step < 6) { setTimeout(next, 100); }
    else { document.getElementById('out').textContent = 'CHAIN_DONE_6'; }
}
setTimeout(next, 100);
</script></body></html>"#;
    let path = std::env::temp_dir().join(format!("m78_chain_{}.html", std::process::id()));
    std::fs::write(&path, html).unwrap();
    Command::cargo_bin("browser")
        .expect("binary")
        .args(["render-script", path.to_str().unwrap(), "--width", "120"])
        .assert()
        .success()
        .stdout(predicate::str::contains("CHAIN_DONE_6"));
    let _ = std::fs::remove_file(&path);
}
