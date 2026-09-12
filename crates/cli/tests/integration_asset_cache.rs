//! M93.5: fetch() 静态资产 GET 的缓存去重 集成测试。
//!
//! 真实场景（Anubis 挑战页，逆向自 nitter.tiekoetter.com 的突发限流实测）：
//! main.mjs 用页面 `fetch()` 预取 worker 源码（sha256.mjs），随后 Worker 实现
//! （`worker_run` → `bridge::fetch_sync`）再次发网络请求取同一 URL——同一个
//! URL 每页发两次请求，在有限流配额的站点直接消耗双倍配额。
//!
//! M93.5 修复：页面 fetch() 成功的"静态资产形状"GET 响应（js/mjs/css/版本
//! 参数 URL，见 `bridge::is_static_asset_url`）经 `__cacheAsset` 写入
//! SCRIPT_CACHE；Worker 源码加载（fetch_sync 的 SCRIPT_CACHE 只读查询，
//! PERF-M80）天然命中，零二次网络请求。
//!
//! 铁证方式：wiremock `Mock::expect(1)`——/worker.js 若被请求第二次，
//! MockServer 关闭校验时 panic，测试失败。

use assert_cmd::Command;
use predicates::prelude::*;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn bin() -> Command {
    Command::cargo_bin("browser").expect("browser binary not found")
}

/// Worker 源码（与任务规格一致：收到 {n} 回 {got: n+1}）。
const WORKER_JS: &str = r#"addEventListener('message', function(e) {
    postMessage({got: e.data.n + 1});
});
"#;

/// 页面：先 fetch('/worker.js') 预取（模拟 Anubis main.mjs），消费响应后
/// 在 then 回调里 new Worker('/worker.js') + postMessage + 结果写 DOM。
const PREFETCH_PAGE: &str = r#"<!doctype html>
<html><body><div id="out">pending</div><script>
fetch('/worker.js').then(function(resp) { return resp.text(); }).then(function(txt) {
    var w = new Worker('/worker.js');
    w.onmessage = function(e) {
        document.getElementById('out').textContent = 'got:' + e.data.got;
    };
    w.postMessage({n: 41});
});
</script></body></html>"#;

/// 核心断言：页面 fetch 预取 + Worker 加载同一 URL，/worker.js 只发 1 次请求，
/// 且 Worker 正常执行（got:42 = 41+1）。
#[tokio::test]
async fn worker_source_prefetched_by_page_fetch_hits_cache() {
    let server = MockServer::start().await;
    // expect(1)：/worker.js 必须恰好被请求一次（预取写缓存 → worker_run 命中）。
    // MockServer 关闭时校验，次数不符则 panic → 测试红。
    Mock::given(method("GET"))
        .and(path("/worker.js"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(WORKER_JS)
                .insert_header("content-type", "text/javascript"),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/prefetch-page"))
        .respond_with(ResponseTemplate::new(200).set_body_string(PREFETCH_PAGE))
        .mount(&server)
        .await;
    let url = format!("{}/prefetch-page", server.uri());

    bin()
        .args(["fetch", &url, "--format", "text"])
        .assert()
        .success()
        .stdout(predicate::str::contains("got:42"));
}

/// 反向形状验证：API JSON 响应（fetch 预取）不得写缓存——同 URL 被 Worker
/// 之外的地方再取时必须走网络（这里用第二次页面 fetch 验证新鲜语义：
/// /api/data.json 允许被请求 2 次，用 expect(2) 固化"动态响应不进缓存"）。
const API_PREFETCH_PAGE: &str = r#"<!doctype html>
<html><body><div id="out">pending</div><script>
fetch('/api/data.json').then(function(resp) { return resp.text(); }).then(function() {
    fetch('/api/data.json').then(function(resp2) { return resp2.text(); }).then(function() {
        document.getElementById('out').textContent = 'api-fetched-twice';
    });
});
</script></body></html>"#;

#[tokio::test]
async fn api_json_responses_are_never_cached() {
    let server = MockServer::start().await;
    // expect(2)：动态 API 响应两次都必须真实走网络（M80 纪律——若被误缓存
    // 则第二次命中 SCRIPT_CACHE，请求次数变 1，expect(2) 不满足 → 红）。
    Mock::given(method("GET"))
        .and(path("/api/data.json"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(r#"{"fresh":true}"#)
                .insert_header("content-type", "application/json"),
        )
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api-page"))
        .respond_with(ResponseTemplate::new(200).set_body_string(API_PREFETCH_PAGE))
        .mount(&server)
        .await;
    let url = format!("{}/api-page", server.uri());

    bin()
        .args(["fetch", &url, "--format", "text"])
        .assert()
        .success()
        .stdout(predicate::str::contains("api-fetched-twice"));
}

/// 版本参数形状：`/chunk?v=42`（路径无扩展名但带版本参数）按静态资产缓存，
/// 页面 fetch 预取 + Worker 加载同一 URL 只发 1 次请求。
const VERSIONED_PAGE: &str = r#"<!doctype html>
<html><body><div id="out">pending</div><script>
fetch('/worker?v=42').then(function(resp) { return resp.text(); }).then(function() {
    var w = new Worker('/worker?v=42');
    w.onmessage = function(e) {
        document.getElementById('out').textContent = 'got:' + e.data.got;
    };
    w.postMessage({n: 7});
});
</script></body></html>"#;

#[tokio::test]
async fn versioned_query_worker_source_hits_cache() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/worker"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(WORKER_JS)
                .insert_header("content-type", "text/javascript"),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/versioned-page"))
        .respond_with(ResponseTemplate::new(200).set_body_string(VERSIONED_PAGE))
        .mount(&server)
        .await;
    let url = format!("{}/versioned-page", server.uri());

    bin()
        .args(["fetch", &url, "--format", "text"])
        .assert()
        .success()
        .stdout(predicate::str::contains("got:8"));
}
