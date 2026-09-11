//! M19.2 e2e: 标准 `fetch()` API 端到端验证。
//!
//! 验证现代 SPA 最常用的 fetch 模式：
//!   fetch(url) → Promise<Response> → res.text() → Promise<string> → 渲染
//!
//! 这是 React/Vue/Next.js 等 SPA 的标准数据获取方式。
//! 本测试证明自研浏览器能渲染 fetch-based SPA（无需切 deno_core）。

use browser_html_parser::parse as parse_html;
use browser_js_runtime::{bridge::body_text_content, run_scripts_with_base};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn fetch_then_text_renders_response() {
    let server = MockServer::start().await;
    let base = server.uri();
    Mock::given(method("GET"))
        .and(path("/api/msg"))
        .respond_with(ResponseTemplate::new(200).set_body_string("FETCHED_OK"))
        .mount(&server)
        .await;

    let html = format!(
        r#"<html><body><p>init</p>
<script>
// 标准 fetch 模式：fetch().then(res => res.text()).then(render)
fetch('{base}/api/msg').then(function(res) {{
    return res.text();
}}).then(function(text) {{
    __setBody('GOT:' + text);
}});
</script>
</body></html>"#,
        base = base
    );

    let tree = parse_html(&html);
    let (shared, _) = run_scripts_with_base(tree, Some(base));
    let borrowed = shared.borrow();
    let body = body_text_content(&borrowed);
    assert!(
        body.contains("GOT:FETCHED_OK"),
        "fetch + text + then chain should render. body={body:?}"
    );
}

#[tokio::test]
async fn fetch_res_object_has_status_and_ok() {
    let server = MockServer::start().await;
    let base = server.uri();
    Mock::given(method("GET"))
        .and(path("/api/x"))
        .respond_with(ResponseTemplate::new(200).set_body_string("body"))
        .mount(&server)
        .await;

    let html = format!(
        r#"<html><body><p>init</p>
<script>
fetch('{base}/api/x').then(function(res) {{
    __setBody('status=' + res.status + ' ok=' + res.ok);
}});
</script>
</body></html>"#,
        base = base
    );

    let tree = parse_html(&html);
    let (shared, _) = run_scripts_with_base(tree, Some(base));
    let body = body_text_content(&shared.borrow());
    assert!(
        body.contains("status=200 ok=true"),
        "Response object should expose status + ok. body={body:?}"
    );
}

#[tokio::test]
async fn fetch_json_parses_response() {
    let server = MockServer::start().await;
    let base = server.uri();
    Mock::given(method("GET"))
        .and(path("/api/json"))
        .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"name":"Alice","age":30}"#))
        .mount(&server)
        .await;

    // res.json() 标准方法
    let html = format!(
        r#"<html><body><p>init</p>
<script>
fetch('{base}/api/json').then(function(res) {{
    return res.json();
}}).then(function(obj) {{
    __setBody('name=' + obj.name + ' age=' + obj.age);
}});
</script>
</body></html>"#,
        base = base
    );

    let tree = parse_html(&html);
    let (shared, _) = run_scripts_with_base(tree, Some(base));
    let body = body_text_content(&shared.borrow());
    assert!(
        body.contains("name=Alice age=30"),
        "res.json() should parse JSON response. body={body:?}"
    );
}

#[tokio::test]
async fn fetch_error_url_rejects_promise() {
    let server = MockServer::start().await;
    let base = server.uri();
    // 连接失败（保留端口未监听）→ fetch 规范：网络错误 reject TypeError。
    // 注意不能用 wiremock 未匹配路径——那是 404 响应，M83 起按浏览器语义
    // 正常 resolve（res.ok=false），reject 的只有网络层错误。
    let dead = "http://127.0.0.1:1/nonexistent";

    let html = format!(
        r#"<html><body><p>init</p>
<script>
fetch('{dead}').then(function(res) {{
    __setBody('SHOULD_NOT_RESOLVE');
}}).catch(function(err) {{
    __setBody('CAUGHT:' + (err instanceof TypeError ? 'TypeError' : 'Other'));
}});
</script>
</body></html>"#,
        dead = dead
    );

    let tree = parse_html(&html);
    let _ = base;
    let (shared, _) = run_scripts_with_base(tree, Some(base));
    let body = body_text_content(&shared.borrow());
    assert!(
        body.contains("CAUGHT:TypeError"),
        "fetch on error URL should reject with TypeError. body={body:?}"
    );
}

#[tokio::test]
async fn multiple_fetches_render_in_order() {
    let server = MockServer::start().await;
    let base = server.uri();
    Mock::given(method("GET"))
        .and(path("/api/first"))
        .respond_with(ResponseTemplate::new(200).set_body_string("first-data"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/second"))
        .respond_with(ResponseTemplate::new(200).set_body_string("second-data"))
        .mount(&server)
        .await;

    // 两个 fetch 按注册顺序 resolve（FIFO macrotask）
    let html = format!(
        r#"<html><body><p>init</p>
<script>
function fetchAppend(url) {{
    fetch(url).then(function(res) {{ return res.text(); }}).then(function(t) {{
        __appendBody(t);
    }});
}}
fetchAppend('{base}/api/first');
fetchAppend('{base}/api/second');
</script>
</body></html>"#,
        base = base
    );

    let tree = parse_html(&html);
    let (shared, _) = run_scripts_with_base(tree, Some(base));
    let body = body_text_content(&shared.borrow());
    let pos_first = body.find("first-data").expect("first should render");
    let pos_second = body.find("second-data").expect("second should render");
    assert!(
        pos_first < pos_second,
        "fetches should resolve in order. body={body:?}"
    );
}

// ===== M20.3: fetch POST / PUT / DELETE =====

#[tokio::test]
async fn fetch_post_with_form_body() {
    let server = MockServer::start().await;
    let base = server.uri();
    Mock::given(method("POST"))
        .and(path("/login"))
        .and(header("content-type", "application/x-www-form-urlencoded"))
        .respond_with(ResponseTemplate::new(200).set_body_string("logged-in"))
        .mount(&server)
        .await;

    let html = format!(
        r#"<html><body><p>init</p>
<script>
// 标准 fetch POST：method + body + headers（React/Vue 表单提交同款）
fetch('{base}/login', {{
    method: 'POST',
    body: 'user=alice&pass=x',
    headers: {{ 'Content-Type': 'application/x-www-form-urlencoded' }}
}}).then(function(res) {{
    return res.text();
}}).then(function(text) {{
    __setBody('GOT:' + text);
}});
</script>
</body></html>"#,
        base = base
    );

    let tree = parse_html(&html);
    let (shared, _) = run_scripts_with_base(tree, Some(base));
    let body = body_text_content(&shared.borrow());
    assert!(
        body.contains("GOT:logged-in"),
        "fetch POST form should render response. body={body:?}"
    );
}

#[tokio::test]
async fn fetch_post_json_body() {
    let server = MockServer::start().await;
    let base = server.uri();
    Mock::given(method("POST"))
        .and(path("/api/users"))
        .and(header("content-type", "application/json"))
        .respond_with(ResponseTemplate::new(201).set_body_string("created"))
        .mount(&server)
        .await;

    let html = format!(
        r#"<html><body><p>init</p>
<script>
fetch('{base}/api/users', {{
    method: 'POST',
    body: JSON.stringify({{name: 'Alice'}}),
    headers: {{ 'Content-Type': 'application/json' }}
}}).then(function(res) {{
    __setBody('status=' + res.status);
}});
</script>
</body></html>"#,
        base = base
    );

    let tree = parse_html(&html);
    let (shared, _) = run_scripts_with_base(tree, Some(base));
    let body = body_text_content(&shared.borrow());
    assert!(
        body.contains("status=201"),
        "fetch POST JSON should get 201. body={body:?}"
    );
}

#[tokio::test]
async fn fetch_put_updates_resource() {
    let server = MockServer::start().await;
    let base = server.uri();
    Mock::given(method("PUT"))
        .and(path("/items/5"))
        .respond_with(ResponseTemplate::new(200).set_body_string("updated"))
        .mount(&server)
        .await;

    let html = format!(
        r#"<html><body><p>init</p>
<script>
fetch('{base}/items/5', {{
    method: 'PUT',
    body: 'new-data'
}}).then(function(res) {{ return res.text(); }}).then(function(t) {{
    __setBody('PUT:' + t);
}});
</script>
</body></html>"#,
        base = base
    );

    let tree = parse_html(&html);
    let (shared, _) = run_scripts_with_base(tree, Some(base));
    let body = body_text_content(&shared.borrow());
    assert!(
        body.contains("PUT:updated"),
        "fetch PUT should render. body={body:?}"
    );
}

#[tokio::test]
async fn fetch_delete_removes_resource() {
    let server = MockServer::start().await;
    let base = server.uri();
    Mock::given(method("DELETE"))
        .and(path("/items/9"))
        .respond_with(ResponseTemplate::new(200).set_body_string("deleted"))
        .mount(&server)
        .await;

    let html = format!(
        r#"<html><body><p>init</p>
<script>
fetch('{base}/items/9', {{ method: 'DELETE' }})
    .then(function(res) {{ return res.text(); }})
    .then(function(t) {{ __setBody('DEL:' + t); }});
</script>
</body></html>"#,
        base = base
    );

    let tree = parse_html(&html);
    let (shared, _) = run_scripts_with_base(tree, Some(base));
    let body = body_text_content(&shared.borrow());
    assert!(
        body.contains("DEL:deleted"),
        "fetch DELETE should render. body={body:?}"
    );
}

#[tokio::test]
async fn fetch_post_carries_cookie_jar() {
    // POST 也应自动带 cookie jar（登录后调用受保护 API 场景）
    let server = MockServer::start().await;
    let base = server.uri();
    // 先 GET 设 cookie
    Mock::given(method("GET"))
        .and(path("/set"))
        .respond_with(
            ResponseTemplate::new(200)
                .append_header("set-cookie", "AUTH=secret; Path=/")
                .set_body_string("set"),
        )
        .mount(&server)
        .await;
    // POST 期望带 cookie
    Mock::given(method("POST"))
        .and(path("/secure"))
        .and(header("cookie", "AUTH=secret"))
        .respond_with(ResponseTemplate::new(200).set_body_string("authorized"))
        .mount(&server)
        .await;

    let html = format!(
        r#"<html><body><p>init</p>
<script>
// 1. GET 设 cookie（进 jar）
fetch('{base}/set').then(function() {{
    // 2. POST 应自动带 cookie jar
    return fetch('{base}/secure', {{ method: 'POST', body: 'x' }});
}}).then(function(res) {{ return res.text(); }}).then(function(t) {{
    __setBody('FINAL:' + t);
}});
</script>
</body></html>"#,
        base = base
    );

    let tree = parse_html(&html);
    let (shared, _) = run_scripts_with_base(tree, Some(base));
    let body = body_text_content(&shared.borrow());
    assert!(
        body.contains("FINAL:authorized"),
        "fetch POST should carry cookie jar. body={body:?}"
    );
}
