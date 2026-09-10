//! M82: `browser fetch` 工具健壮性 e2e 测试（真实站点扫描问题清单的回归）。
//!
//! 覆盖用户实测暴露的 8 项问题：
//! - P0-1 全局硬超时（挂死：juejin 常驻事件循环 / 纯 JS 死循环）
//! - P0-2 反爬/验证页启发式警告（百度"网络不给力" / 36kr"安全检测"）
//! - P0-3 data: URI 图片默认丢弃 + --inline-images 开关
//! - P1-4 重复长行去噪（GitHub flash 提示 3 遍）
//! - P1-5 --selector 无匹配警告
//! - P1-6 --json 尊重 --format
//! - P2-7 确定性错误（4xx）不盲重 3 次
//! - P2-8 --json network 数组在页面 JS 发 fetch 时有捕获
//!   （设计边界：只记录 JS 发起的 fetch/XHR，外链 <script src> 不算）

use assert_cmd::Command;
use predicates::prelude::*;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn bin() -> Command {
    Command::cargo_bin("browser").expect("browser binary not found")
}

/// P0-1: 纯 JS 死循环 + 全局墙钟预算——进程必须在预算附近返回，绝不挂死。
/// 死循环没有网络/timer 可供协同检查，靠 QuickJS interrupt handler 兜底。
#[tokio::test]
async fn fetch_infinite_js_loop_respects_global_timeout() {
    let html = r#"<!doctype html><html><head><title>Loop</title></head><body>
<p>static content before loop</p>
<script>while (true) { }</script>
</body></html>"#;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/loop"))
        .respond_with(ResponseTemplate::new(200).set_body_string(html))
        .mount(&server)
        .await;
    let url = format!("{}/loop", server.uri());

    bin()
        .args(["fetch", &url, "--format", "text", "--timeout-ms", "1500"])
        .timeout(std::time::Duration::from_secs(30))
        .assert()
        .success()
        // 已渲染的静态部分照常输出（超时返回当前内容，不是空/错误）
        .stdout(predicate::str::contains("static content before loop"))
        // stderr 有明确超时警告
        .stderr(predicate::str::contains("exceeded global budget"));
}

/// P0-2: 36kr 实测样本——HTTP 200 的"安全检测"壳页必须发警告信号。
#[tokio::test]
async fn fetch_anti_bot_page_warns() {
    let html = r#"<!doctype html><html><head><title>人机验证</title></head><body>
<div class="wrap"><h3>正在进行安全检测...</h3><p>请稍候</p></div>
</body></html>"#;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/check"))
        .respond_with(ResponseTemplate::new(200).set_body_string(html))
        .mount(&server)
        .await;
    let url = format!("{}/check", server.uri());

    bin()
        .args(["fetch", &url, "--format", "text", "--json", "--no-js"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"warnings\""))
        .stdout(predicate::str::contains("anti-bot"))
        .stderr(predicate::str::contains("anti-bot"));
}

/// P0-3: data: URI 图片默认丢弃（base64 是 LLM 纯噪声），--inline-images 保留。
#[tokio::test]
async fn fetch_data_uri_img_dropped_by_default() {
    let html = r#"<html><body><article>
<p>real content here</p>
<img src="data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAA" alt="inline">
<img src="/real.png" alt="real">
</article></body></html>"#;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/page"))
        .respond_with(ResponseTemplate::new(200).set_body_string(html))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/real.png"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    let url = format!("{}/page", server.uri());

    bin()
        .args(["fetch", &url, "--format", "markdown", "--no-js"])
        .assert()
        .success()
        .stdout(predicate::str::contains("base64").not())
        .stdout(predicate::str::contains("data:image").not())
        .stdout(predicate::str::contains("![real]("));

    bin()
        .args([
            "fetch",
            &url,
            "--format",
            "markdown",
            "--no-js",
            "--inline-images",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("data:image/png;base64,"));
}

/// P1-4: GitHub flash 实测——同一句长提示重复 3 遍，只保留第一次。
#[tokio::test]
async fn fetch_repeated_long_line_deduped() {
    let flash = "You signed in with another tab or window. Reload to refresh your session.";
    let html = format!(
        r#"<html><body><article><p>Article body intro paragraph.</p></article>
<div class="flash">{flash}</div><div class="flash">{flash}</div><div class="flash">{flash}</div>
</body></html>"#
    );
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/dup"))
        .respond_with(ResponseTemplate::new(200).set_body_string(html))
        .mount(&server)
        .await;
    let url = format!("{}/dup", server.uri());

    let output = bin()
        .args([
            "fetch",
            &url,
            "--format",
            "markdown",
            "--only-main-content=false",
            "--no-js",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8_lossy(&output);
    assert_eq!(
        text.matches(flash).count(),
        1,
        "repeated flash notice should appear exactly once: {text}"
    );
}

/// P1-5: --selector 无匹配时 stderr 警告（不再静默空输出）。
#[tokio::test]
async fn fetch_selector_no_match_warns() {
    let html = r#"<html><body><article><p>content</p></article></body></html>"#;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/page"))
        .respond_with(ResponseTemplate::new(200).set_body_string(html))
        .mount(&server)
        .await;
    let url = format!("{}/page", server.uri());

    bin()
        .args([
            "fetch",
            &url,
            "--format",
            "text",
            "--selector",
            "#content_left",
            "--no-js",
        ])
        .assert()
        .stderr(predicate::str::contains("matched 0 nodes"));
}

/// P1-6: --json 尊重 --format（content.format 记录格式，text 承载该格式内容）。
#[tokio::test]
async fn fetch_json_respects_markdown_format() {
    let html = r#"<html><head><title>Md</title></head><body>
<h1>Heading One</h1><p>paragraph with <a href="/l">link</a>.</p>
</body></html>"#;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/page"))
        .respond_with(ResponseTemplate::new(200).set_body_string(html))
        .mount(&server)
        .await;
    let url = format!("{}/page", server.uri());

    bin()
        .args(["fetch", &url, "--format", "markdown", "--json", "--no-js"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"format\": \"markdown\""))
        // markdown 结构（# 标题 / 链接语法）真实存在于 content.text
        .stdout(predicate::str::contains("# Heading One"))
        .stdout(predicate::str::contains("[link]("));
}

/// P2-7: 404 是确定性错误——立即失败，不盲重 3 次。
#[tokio::test]
async fn fetch_404_does_not_retry() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/missing"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    let url = format!("{}/missing", server.uri());

    bin()
        .args(["fetch", &url, "--no-js"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("non-retryable"))
        // 只打了 attempt 0，没有 attempt 1/2
        .stderr(predicate::str::contains("attempt 1").not());
}

/// P2-8: 页面 JS 发 fetch 时 --json network 数组必须有捕获
/// （设计边界文档化：只记 JS 发起的请求；百度首页实测为空是页面没发 XHR）。
#[tokio::test]
async fn fetch_network_events_captured_from_js() {
    let html = r#"<!doctype html><html><head><title>Net</title></head><body>
<div id="app">loading</div>
<script>
fetch('/api/data').then(function (r) { return r.text(); }).then(function (t) {
  document.getElementById('app').textContent = t;
});
</script>
</body></html>"#;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_string(html))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/data"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .set_body_string(r#"{"msg":"hello"}"#),
        )
        .mount(&server)
        .await;

    bin()
        .args(["fetch", &server.uri(), "--format", "text", "--json"])
        .assert()
        .success()
        // JS fetch 完成后 DOM 已更新
        .stdout(predicate::str::contains("hello"))
        // network 数组捕获了 JS 发起的 /api/data
        .stdout(predicate::str::contains("/api/data"))
        .stdout(predicate::str::contains("\"network\""));
}

/// M83: XHR POST 全链路——method/body/setRequestHeader 透传 + 真实 status。
/// 背景：掘金（axios/XHR）feed 全挂——旧 XHR shim 忽略 body（永远 GET）、
/// status 硬编码 200，POST API 全部静默失败。
#[tokio::test]
async fn fetch_xhr_post_method_body_and_real_status() {
    let html = r#"<!doctype html><html><head><title>XHR</title></head><body>
<div id="out">PENDING</div>
<script>
var xhr = new XMLHttpRequest();
xhr.open('POST', '/api/echo', true);
xhr.setRequestHeader('Content-Type', 'application/json');
xhr.onreadystatechange = function () {
  if (xhr.readyState === 4) {
    document.getElementById('out').textContent =
      'XHR_' + xhr.status + '_' + xhr.responseText;
  }
};
xhr.send(JSON.stringify({name: 'juejin'}));
</script>
</body></html>"#;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_string(html))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/echo"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .set_body_string(r#"{"ok":true,"got":"juejin"}"#),
        )
        .mount(&server)
        .await;

    bin()
        .args(["fetch", &server.uri(), "--format", "text"])
        .assert()
        .success()
        // 真实 status（非硬编码）+ 响应体经 XHR 返回
        .stdout(predicate::str::contains("XHR_200_"))
        .stdout(predicate::str::contains("juejin"));
}

/// M83: XHR 非 2xx 的 status 必须透传（axios 依赖它 reject——旧 shim 永远 200）。
#[tokio::test]
async fn fetch_xhr_404_status_passthrough() {
    let html = r#"<!doctype html><html><head><title>XHR404</title></head><body>
<div id="out">PENDING</div>
<script>
var xhr = new XMLHttpRequest();
xhr.open('GET', '/api/missing', true);
xhr.onload = function () {
  document.getElementById('out').textContent = 'STATUS_' + xhr.status;
};
xhr.send();
</script>
</body></html>"#;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_string(html))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/missing"))
        .respond_with(ResponseTemplate::new(404).set_body_string("nope"))
        .mount(&server)
        .await;

    bin()
        .args(["fetch", &server.uri(), "--format", "text"])
        .assert()
        .success()
        .stdout(predicate::str::contains("STATUS_404"));
}

/// M83: Plugin/MimeType 标准接口 + navigator.plugins——掘金风控 SDK / core-js
/// DOM collections 表裸引用 PluginArray 曾 ReferenceError 断链（juejin 根因①）。
#[test]
fn render_script_plugin_array_interfaces() {
    let html = r#"<!DOCTYPE html><html><body>
<div id="out">FAIL</div>
<script>
var ok = typeof PluginArray === 'function'
  && typeof Plugin === 'function'
  && typeof MimeTypeArray === 'function'
  && typeof MimeType === 'function'
  && navigator.plugins instanceof PluginArray
  && navigator.plugins.length === 0
  && navigator.mimeTypes instanceof MimeTypeArray;
document.getElementById('out').textContent = ok ? 'PLUGIN_OK' : 'PLUGIN_FAIL';
</script></body></html>"#;
    let path = std::env::temp_dir().join("browser_test_plugin_array.html");
    std::fs::write(&path, html).expect("write fixture");
    bin()
        .args(["render-script", path.to_str().unwrap(), "--width", "120"])
        .assert()
        .success()
        .stdout(predicate::str::contains("PLUGIN_OK"));
    let _ = std::fs::remove_file(&path);
}

/// M83 铁证：XHR 路径的请求进 `--json` network 数组——ION 侧验收标准
/// 「network 能显示 XHR 请求」的直接对应测试。juejin network=0 的真因是
/// bdms 风控门卫拦在 axios 拦截器层（XHR 未到 send），不是捕获缺口。
#[tokio::test]
async fn fetch_xhr_requests_appear_in_json_network_array() {
    let html = r#"<!doctype html><html><head><title>XHRNet</title></head><body>
<div id="out">PENDING</div>
<script>
var xhr = new XMLHttpRequest();
xhr.open('GET', '/api/feed', true);
xhr.onload = function () {
  document.getElementById('out').textContent = 'FEED_' + xhr.responseText;
};
xhr.send();
</script>
</body></html>"#;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_string(html))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/feed"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .set_body_string(r#"{"list":["a","b"]}"#),
        )
        .mount(&server)
        .await;

    bin()
        .args(["fetch", &server.uri(), "--format", "text", "--json"])
        .assert()
        .success()
        // XHR 驱动的内容渲染出来
        .stdout(predicate::str::contains("FEED_"))
        // XHR 请求记录在 network 数组（method/status/url 全有）
        .stdout(predicate::str::contains("/api/feed"))
        .stdout(predicate::str::contains("\"method\": \"GET\""));
}
