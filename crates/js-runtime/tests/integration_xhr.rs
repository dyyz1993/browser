//! M17.3 e2e: XMLHttpRequest 真实 fetch + onload 渲染。
//!
//! 验证 M17.1+M17.2 的端到端链路：
//!   JS new XMLHttpRequest() → open → send（同步 fetch）→
//!   setTimeout(0) 触发 onload → getResponseText() 读响应 → 渲染 DOM
//!
//! 这是 ADR-0002 路径 1（同步 fetch + event loop 异步 onload）的验收。
//! 老 SPA（jQuery 时代）常用 XHR 模式，本测试证明可渲染。

use browser_html_parser::parse as parse_html;
use browser_js_runtime::{bridge::body_text_content, run_scripts_with_base};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn xhr_onload_renders_response_to_dom() {
    let server = MockServer::start().await;
    let base = server.uri();

    Mock::given(method("GET"))
        .and(path("/api/data"))
        .respond_with(ResponseTemplate::new(200).set_body_string("XHR_DATA_OK"))
        .mount(&server)
        .await;

    // SPA: new XHR → open → send → onload 里 getResponseText + setBody
    let html = format!(
        r#"<html><body><p>init</p>
<script>
var xhr = new XMLHttpRequest();
xhr.onload = function() {{
    var text = this.getResponseText();
    __setBody('GOT:' + text);
}};
xhr.open('GET', '{base}/api/data');
xhr.send();
</script>
</body></html>"#,
        base = base
    );

    let tree = parse_html(&html);
    let (shared, _) = run_scripts_with_base(tree, Some(base));
    let borrowed = shared.borrow();
    let body = body_text_content(&borrowed);
    assert!(
        body.contains("GOT:XHR_DATA_OK"),
        "XHR onload should render response. body={body:?}"
    );
}

#[tokio::test]
async fn xhr_multiple_requests_render_in_order() {
    let server = MockServer::start().await;
    let base = server.uri();

    Mock::given(method("GET"))
        .and(path("/api/a"))
        .respond_with(ResponseTemplate::new(200).set_body_string("alpha"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/b"))
        .respond_with(ResponseTemplate::new(200).set_body_string("beta"))
        .mount(&server)
        .await;

    // 两个 XHR 请求，按注册顺序渲染（FIFO）
    let html = format!(
        r#"<html><body><p>init</p>
<script>
function fetchInto(url) {{
    var x = new XMLHttpRequest();
    x.onload = function() {{ __appendBody(this.getResponseText()); }};
    x.open('GET', url);
    x.send();
}}
fetchInto('{base}/api/a');
fetchInto('{base}/api/b');
</script>
</body></html>"#,
        base = base
    );

    let tree = parse_html(&html);
    let (shared, _) = run_scripts_with_base(tree, Some(base));
    let borrowed = shared.borrow();
    let body = body_text_content(&borrowed);
    let pos_a = body.find("alpha").expect("alpha should render");
    let pos_b = body.find("beta").expect("beta should render");
    assert!(
        pos_a < pos_b,
        "XHR requests should render in order. body={body:?}"
    );
}

#[test]
fn xhr_relative_url_resolves_against_base() {
    // 相对 URL 应通过 resolve_url 解析（复用 M4 base_url 机制）
    let html = r#"<html><body><p>init</p>
<script>
var xhr = new XMLHttpRequest();
xhr.onload = function() {
    __setBody('REL_OK');
};
xhr.open('GET', '/api/relative');
xhr.send();
</script>
</body></html>"#;
    // 没有真实 server，fetch 会失败，但 onload 仍触发（response 为空）
    // 验证不 panic + onload 能触发
    let tree = parse_html(html);
    let (shared, _) = run_scripts_with_base(tree, Some("http://localhost:9999".to_string()));
    let borrowed = shared.borrow();
    let body = body_text_content(&borrowed);
    assert!(
        body.contains("REL_OK"),
        "onload should fire even on fetch failure. body={body:?}"
    );
}
