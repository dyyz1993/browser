//! M93.4: 文档导航循环的 storage 跨页持久化 集成测试。
//!
//! 真实浏览器语义：同源导航后 localStorage / sessionStorage 都保留
//! （session 作用域是标签页不是文档）；跨源导航才是全新 storage。
//! 实现场：`run_scripts_quickjs` 导航循环层持有 StorageHandle——下一跳
//! origin 与当前页相同则复用句柄（TreeGuard::drop 只清 thread_local slot，
//! 句柄本身存活），跨源/首跳 `browser_storage::new_storage()` 新建。
//! 页面可见的 localStorage 由 QUICKJS_GLOBAL_SHIM 桥接 `__storage*` 到
//! 该句柄（M93.4 前是纯 JS 对象，随每页引擎销毁而丢）。
//!
//! 覆盖：
//! 1. 同源保留——page1 写 localStorage+sessionStorage → location.replace
//!    → page2 读到写入值。
//! 2. 跨源隔离——server A page1 写 localStorage → location.replace 到
//!    server B（不同 MockServer 实例 = 不同端口 = 不同 origin）→ page2
//!    读同 key 为 null。

use assert_cmd::Command;
use predicates::prelude::*;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn bin() -> Command {
    Command::cargo_bin("browser").expect("browser binary not found")
}

/// page2 通用读取页：读 localStorage['nav-k'] 与 sessionStorage['nav-s']
/// 并渲染（null 渲染为标记，避免"输出里没有值"与"页面压根没执行"混淆）。
fn reader_page_body() -> String {
    r#"<!doctype html>
<html><body><div id="out">pending</div><script>
var v = localStorage.getItem('nav-k');
var s = sessionStorage.getItem('nav-s');
document.getElementById('out').textContent =
    'storage-read:' + (v === null ? 'NULL' : v) + '|sess:' + (s === null ? 'NULL' : s);
</script></body></html>"#
        .to_string()
}

#[tokio::test]
async fn same_origin_navigation_preserves_storage() {
    let server = MockServer::start().await;
    let page1 = r#"<!doctype html>
<html><body><div>NAV-STORAGE-SOURCE</div><script>
localStorage.setItem('nav-k', 'persisted');
sessionStorage.setItem('nav-s', 'sess-persisted');
setTimeout(function() { location.replace('/page2'); }, 10);
</script></body></html>"#;
    Mock::given(method("GET"))
        .and(path("/page1"))
        .respond_with(ResponseTemplate::new(200).set_body_string(page1))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/page2"))
        .respond_with(ResponseTemplate::new(200).set_body_string(reader_page_body()))
        .mount(&server)
        .await;
    let url = format!("{}/page1", server.uri());

    bin()
        .args(["fetch", &url, "--format", "text"])
        .assert()
        .success()
        // localStorage 跨同源导航保留
        .stdout(predicate::str::contains("storage-read:persisted"))
        // sessionStorage 同样保留（session 作用域是标签页不是文档）
        .stdout(predicate::str::contains("sess:sess-persisted"))
        // 中间页内容必须被换掉（导航确实发生了）
        .stdout(predicate::str::contains("NAV-STORAGE-SOURCE").not());
}

#[tokio::test]
async fn cross_origin_navigation_gets_fresh_storage() {
    let server_a = MockServer::start().await;
    let server_b = MockServer::start().await;
    // 不同 MockServer 实例 = 不同端口 = 不同 origin。
    assert_ne!(
        server_a.uri(),
        server_b.uri(),
        "两个 MockServer 必须是不同 origin"
    );
    let page1 = format!(
        r#"<!doctype html>
<html><body><div>NAV-CROSS-SOURCE</div><script>
localStorage.setItem('nav-k', 'persisted');
setTimeout(function() {{ location.replace('{}/page2'); }}, 10);
</script></body></html>"#,
        server_b.uri()
    );
    Mock::given(method("GET"))
        .and(path("/page1"))
        .respond_with(ResponseTemplate::new(200).set_body_string(page1))
        .mount(&server_a)
        .await;
    Mock::given(method("GET"))
        .and(path("/page2"))
        .respond_with(ResponseTemplate::new(200).set_body_string(reader_page_body()))
        .mount(&server_b)
        .await;
    let url = format!("{}/page1", server_a.uri());

    bin()
        .args(["fetch", &url, "--format", "text"])
        .assert()
        .success()
        // 跨源 = 全新 storage：读到 null 标记，且绝不能出现 server A 写入的值
        .stdout(predicate::str::contains("storage-read:NULL"))
        .stdout(predicate::str::contains("persisted").not())
        .stdout(predicate::str::contains("NAV-CROSS-SOURCE").not());
}
