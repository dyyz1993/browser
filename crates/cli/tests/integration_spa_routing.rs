//! M70.14: SPA hash fragment 路由集成测试。
//!
//! 验证「带 hash fragment 的 URL」在渲染管线中正确处理：
//! - hash（如 `#/?id=docsify`）是客户端路由，服务端 fetch / JS base_url 都应去除。
//! - JS 里用相对路径发 XHR 时，base 不应被 hash 污染。
//!
//! 用 wiremock 起 mock server 模拟 docsify 式场景：
//! 主页面（小 HTML + XHR 加载 README.md 内容）+ README.md 端点。
//! 验收：带 hash 的 URL fetch 后，XHR 拿到 README 内容并渲染进 DOM。

use assert_cmd::Command;
use predicates::prelude::*;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn bin() -> Command {
    Command::cargo_bin("browser").expect("browser binary not found")
}

/// SPA 壳：小 HTML（正文只有 "Loading"），JS 用 XHR 加载 README.md 并塞进 body。
/// 模拟 docsify.js.org 的真实结构。
const SPA_SHELL_HTML: &str = r#"<!doctype html>
<html>
<head><title>SPA Doc</title></head>
<body>
<div id="app">Loading</div>
<script>
(function() {
  var xhr = new XMLHttpRequest();
  xhr.open('GET', 'README.md', true);
  xhr.onload = function() {
    if (xhr.status === 200) {
      document.getElementById('app').innerHTML = xhr.responseText;
    }
  };
  xhr.send();
})();
</script>
</body>
</html>"#;

const README_MD: &str = r#"# Docsify Test

This is the dynamically loaded markdown content.

- Feature one
- Feature two
- Feature three

## Section

More content here."#;

/// 带 hash fragment 的 URL 必须正确渲染 XHR 加载的内容。
/// M70.14 修复点：`#/?id=docsify` 被去除后，相对路径 `README.md` 解析正确。
#[tokio::test]
async fn fetch_url_with_hash_renders_xhr_content() {
    let server = MockServer::start().await;
    let base = server.uri();

    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_string(SPA_SHELL_HTML))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/README.md"))
        .respond_with(ResponseTemplate::new(200).set_body_string(README_MD))
        .mount(&server)
        .await;

    // 关键：URL 带 hash fragment（模拟 docsify.js.org/#/?id=docsify）。
    let url_with_hash = format!("{base}/#/?id=docsify");

    bin()
        .args(["fetch", &url_with_hash, "--format", "markdown"])
        .assert()
        .success()
        // XHR 加载的 README 内容必须出现在输出中
        .stdout(predicate::str::contains("Docsify Test"))
        .stdout(predicate::str::contains(
            "dynamically loaded markdown content",
        ))
        .stdout(predicate::str::contains("Feature one"))
        // "Loading" 占位符应被替换
        .stdout(predicate::str::contains("Loading").not());
}

/// 对照组：不带 hash 的同样 URL 也应正确渲染（保证 hash 去除没有副作用）。
#[tokio::test]
async fn fetch_url_without_hash_renders_same_content() {
    let server = MockServer::start().await;
    let base = server.uri();

    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_string(SPA_SHELL_HTML))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/README.md"))
        .respond_with(ResponseTemplate::new(200).set_body_string(README_MD))
        .mount(&server)
        .await;

    let url_no_hash = format!("{base}/");

    bin()
        .args(["fetch", &url_no_hash, "--format", "markdown"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Docsify Test"))
        .stdout(predicate::str::contains("Feature one"));
}
