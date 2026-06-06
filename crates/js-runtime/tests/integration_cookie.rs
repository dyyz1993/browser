//! M15.5 e2e: cookie jar 跨请求会话保持。
//!
//! 验证 M15.3 + M15.4 的核心链路：
//!   主请求 fetch HTML → 响应 Set-Cookie → jar 存入
//!   → JS __fetchSetBody → fetch_with_jar 读 jar → 带 Cookie 头
//!
//! 用 wiremock 起两个 mock：
//!   1. /api/session 设 Set-Cookie: SID=abc
//!   2. /api/data 只在带 Cookie: SID=abc 时返回数据
//!
//! 验证 JS fetch 能继承主请求的会话（这是百度反爬场景的核心）。

use browser_html_parser::parse as parse_html;
use browser_js_runtime::{bridge::body_text_content, run_scripts_with_base};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn cookie_jar_persists_from_main_fetch_to_js_fetch() {
    let server = MockServer::start().await;
    let base = server.uri();

    // 1. /api/session 设 Set-Cookie（模拟主请求或 SPA 首个 fetch）
    Mock::given(method("GET"))
        .and(path("/api/session"))
        .respond_with(
            ResponseTemplate::new(200)
                .append_header("set-cookie", "SID=abc123; Path=/")
                .set_body_string("session set"),
        )
        .mount(&server)
        .await;

    // 2. /api/data 只在带 Cookie: SID=abc123 时返回数据（验证 jar 注入）
    Mock::given(method("GET"))
        .and(path("/api/data"))
        .and(header("cookie", "SID=abc123"))
        .respond_with(ResponseTemplate::new(200).set_body_string("PROTECTED_DATA"))
        .mount(&server)
        .await;

    // SPA: 先 __fetchSetBody 设 cookie，再 __fetchAppendBody 带 cookie 拿数据
    let html = format!(
        r#"<html><body><p>init</p>
<script>
__fetchSetBody("{base}/api/session");
__fetchAppendBody("{base}/api/data");
</script>
</body></html>"#,
        base = base
    );

    let tree = parse_html(&html);
    let (shared, n) = run_scripts_with_base(tree, Some(base));
    assert_eq!(n, 1, "one script should execute");

    let body = body_text_content(&shared.borrow());
    // 验证：JS fetch 继承了 session fetch 设的 cookie
    assert!(
        body.contains("PROTECTED_DATA"),
        "JS fetch should inherit cookie from prior fetch. body={body:?}"
    );
}

#[tokio::test]
async fn cookie_jar_no_cookie_when_no_prior_set_cookie() {
    let server = MockServer::start().await;
    let base = server.uri();

    // /api/data 不设任何 cookie 匹配要求，但默认返回（验证没 jar 干扰时不报错）
    Mock::given(method("GET"))
        .and(path("/api/data"))
        .respond_with(ResponseTemplate::new(200).set_body_string("OK"))
        .mount(&server)
        .await;

    let html = format!(
        r#"<html><body><p>init</p>
<script>
__fetchAppendBody("{base}/api/data");
</script>
</body></html>"#,
        base = base
    );

    let tree = parse_html(&html);
    let (shared, n) = run_scripts_with_base(tree, Some(base));
    assert_eq!(n, 1);

    let body = body_text_content(&shared.borrow());
    assert!(
        body.contains("OK"),
        "fetch without cookie should still work. body={body:?}"
    );
}
