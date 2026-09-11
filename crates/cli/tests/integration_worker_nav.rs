//! M93: Web Worker + 文档导航闭环 集成测试。
//!
//! 覆盖 Anubis PoW 挑战所需的四段能力（真实场景逆向自 nitter.tiekoetter.com）：
//! 1. Worker 消息往返——`new Worker(url)` + `postMessage` → 子 Context 计算 →
//!    `onmessage({data})` 回投 → DOM 更新。
//! 2. 文档导航——`location.replace(X)` 触发重新 fetch + 换树 + 重跑脚本。
//! 3. 重定向 + cookie 链——导航目标 302 + Set-Cookie → 逐跳 cookie 传递
//!    → 受保护页可见（Anubis pass-challenge 语义）。
//! 4. URL polyfill 的 searchParams 回写——`new URL(...).searchParams.set(...)`
//!    后 href 必须带 query（Anubis 的 v() 构造器依赖；此前 query 全丢）。

use assert_cmd::Command;
use predicates::prelude::*;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn bin() -> Command {
    Command::cargo_bin("browser").expect("browser binary not found")
}

const WORKER_PAGE: &str = r#"<!doctype html>
<html><body><div id="out">pending</div><script>
var w = new Worker('/worker.js');
w.onmessage = function(e) {
    document.getElementById('out').textContent = 'got:' + e.data.val;
};
w.postMessage({x: 2});
</script></body></html>"#;

const WORKER_JS: &str = r#"addEventListener('message', function(e) {
    Promise.resolve().then(function() {
        postMessage({val: e.data.x * 21});
    });
});
"#;

#[tokio::test]
async fn worker_message_roundtrip_updates_dom() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/worker-page"))
        .respond_with(ResponseTemplate::new(200).set_body_string(WORKER_PAGE))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/worker.js"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(WORKER_JS)
                .insert_header("content-type", "text/javascript"),
        )
        .mount(&server)
        .await;
    let url = format!("{}/worker-page", server.uri());

    bin()
        .args(["fetch", &url, "--format", "text"])
        .assert()
        .success()
        .stdout(predicate::str::contains("got:42"));
}

const NAV_SOURCE: &str = r#"<!doctype html>
<html><body><div>NAV-INTERMEDIATE-PAGE</div><script>
setTimeout(function() { location.replace('/nav-target'); }, 10);
</script></body></html>"#;

const NAV_TARGET: &str = r#"<!doctype html>
<html><body><div>NAV-FINAL-CONTENT-OK</div></body></html>"#;

#[tokio::test]
async fn location_replace_refetches_and_rerenders() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/nav-source"))
        .respond_with(ResponseTemplate::new(200).set_body_string(NAV_SOURCE))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/nav-target"))
        .respond_with(ResponseTemplate::new(200).set_body_string(NAV_TARGET))
        .mount(&server)
        .await;
    let url = format!("{}/nav-source", server.uri());

    bin()
        .args(["fetch", &url, "--format", "text"])
        .assert()
        .success()
        .stdout(predicate::str::contains("NAV-FINAL-CONTENT-OK"))
        // 中间页内容必须被换掉（导航 = 新文档替换旧文档）
        .stdout(predicate::str::contains("NAV-INTERMEDIATE-PAGE").not());
}

/// Anubis 语义：挑战页解完 → location.replace(pass-challenge) →
/// 302 + Set-Cookie → 受保护页（校验 cookie 才给真身）。
const CHALLENGE_PAGE: &str = r#"<!doctype html>
<html><body><div>Checking your browser</div><script>
setTimeout(function() {
    location.replace('/pass-challenge?id=abc&response=0000dead&redir=/protected');
}, 10);
</script></body></html>"#;

#[tokio::test]
async fn navigation_redirect_chain_carries_cookie() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/anubis-challenge"))
        .respond_with(ResponseTemplate::new(200).set_body_string(CHALLENGE_PAGE))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/pass-challenge"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("location", "/protected")
                .insert_header("set-cookie", "anubis_auth=passed; Path=/"),
        )
        .mount(&server)
        .await;
    // /protected 只有带 cookie 的请求才有 mock 响应——cookie 丢失 = 404 = 测试失败
    // （此流程 jar 里只有 pass-challenge 设的一个 cookie，精确匹配）
    Mock::given(method("GET"))
        .and(path("/protected"))
        .and(header("cookie", "anubis_auth=passed"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<html><body><div>SECRET-TIMELINE-CONTENT</div></body></html>"),
        )
        .mount(&server)
        .await;
    let url = format!("{}/anubis-challenge", server.uri());

    bin()
        .args(["fetch", &url, "--format", "text"])
        .assert()
        .success()
        .stdout(predicate::str::contains("SECRET-TIMELINE-CONTENT"));
}

/// Anubis main.mjs 的 v()：`new URL(base) + searchParams.set(...) + toString()`。
/// 修复前 searchParams 是构造时快照，query 全丢（pass-challenge 无参数 → 无效）。
const URL_SYNC_PAGE: &str = r#"<!doctype html>
<html><body><div id="out"></div><script>
var t = new URL('https://x.test/api/pass-challenge');
t.searchParams.set('id', 'abc123');
t.searchParams.set('response', '0000ab');
t.searchParams.set('nonce', '42');
document.getElementById('out').textContent = t.toString();
</script></body></html>"#;

#[tokio::test]
async fn url_searchparams_set_syncs_href() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/url-sync"))
        .respond_with(ResponseTemplate::new(200).set_body_string(URL_SYNC_PAGE))
        .mount(&server)
        .await;
    let url = format!("{}/url-sync", server.uri());

    bin()
        .args(["fetch", &url, "--format", "text"])
        .assert()
        .success()
        .stdout(predicate::str::contains("id=abc123"))
        .stdout(predicate::str::contains("response=0000ab"))
        .stdout(predicate::str::contains("nonce=42"));
}

/// pushState/replaceState（SPA 路由）不得触发文档级重新 fetch——
/// 只有 href 赋值/assign/replace 才算文档导航。
const SPA_ROUTER_PAGE: &str = r#"<!doctype html>
<html><body><div id="out">SPA-SHELL</div><script>
history.pushState(null, '', '/client-route?q=1');
document.getElementById('out').textContent = 'SPA-RENDERED';
</script></body></html>"#;

#[tokio::test]
async fn pushstate_does_not_trigger_refetch() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/spa"))
        .respond_with(ResponseTemplate::new(200).set_body_string(SPA_ROUTER_PAGE))
        .mount(&server)
        .await;
    // /client-route 无 mock——若 pushState 误触发重新 fetch，会 404 且输出变化
    let url = format!("{}/spa", server.uri());

    bin()
        .args(["fetch", &url, "--format", "text"])
        .assert()
        .success()
        .stdout(predicate::str::contains("SPA-RENDERED"));
}
