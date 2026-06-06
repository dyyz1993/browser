//! M18.1 e2e: networkidle 信号正确反映 SPA 状态。
//!
//! 验证 is_network_idle()：pending_timers==0 && pending_requests==0。
//! - SPA 用 setTimeout/Promise + XHR fetch 时，运行期间 idle=false
//! - 渲染完后 idle=true（爬虫判断"渲染完了"的信号）

use browser_html_parser::parse as parse_html;
use browser_js_runtime::{is_network_idle, run_scripts_with_base};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[test]
fn idle_true_after_simple_script_with_no_async_work() {
    // 无 setTimeout/fetch 的脚本：run 后应立即 idle
    let html = r#"<html><body><p>init</p>
<script>__setBody('done');</script>
</body></html>"#;
    let tree = parse_html(html);
    let _ = run_scripts_with_base(tree, None);
    assert!(
        is_network_idle(),
        "should be idle after sync-only script (no timer/fetch)"
    );
}

#[test]
fn idle_true_after_settimeout_chain_drained() {
    // setTimeout 链在 pump_event_loop 里被 drain 完，之后应 idle
    let html = r#"<html><body><p>init</p>
<script>
var n = 0;
function tick() {
    n++;
    if (n < 3) setTimeout(tick, 0);
}
setTimeout(tick, 0);
</script>
</body></html>"#;
    let tree = parse_html(html);
    let _ = run_scripts_with_base(tree, None);
    assert!(
        is_network_idle(),
        "should be idle after setTimeout chain drained"
    );
}

#[tokio::test]
async fn idle_true_after_xhr_fetches_complete() {
    let server = MockServer::start().await;
    let base = server.uri();
    Mock::given(method("GET"))
        .and(path("/api/x"))
        .respond_with(ResponseTemplate::new(200).set_body_string("data"))
        .mount(&server)
        .await;

    let html = format!(
        r#"<html><body><p>init</p>
<script>
var xhr = new XMLHttpRequest();
xhr.onload = function() {{ __setBody('done:' + this.getResponseText()); }};
xhr.open('GET', '{base}/api/x');
xhr.send();
</script>
</body></html>"#,
        base = base
    );
    let tree = parse_html(&html);
    let _ = run_scripts_with_base(tree, Some(base));
    // XHR fetch 已完成 + onload 触发完 → idle
    assert!(
        is_network_idle(),
        "should be idle after XHR fetch + onload completed"
    );
}

#[test]
fn pending_requests_zero_when_no_fetch_in_flight() {
    // 无网络请求时 pending_requests 应为 0
    let html = r#"<html><body><p>init</p><script>__setBody('x');</script></body></html>"#;
    let tree = parse_html(html);
    let _ = run_scripts_with_base(tree, None);
    assert_eq!(
        browser_js_runtime::pending_requests(),
        0,
        "no fetch in flight → pending_requests = 0"
    );
}

#[test]
fn pending_timers_zero_after_all_drained() {
    let html = r#"<html><body><p>init</p>
<script>setTimeout(function(){ __appendBody('a'); }, 0);</script>
</body></html>"#;
    let tree = parse_html(html);
    let _ = run_scripts_with_base(tree, None);
    assert_eq!(
        browser_js_runtime::pending_timers(),
        0,
        "timer drained → pending_timers = 0"
    );
}
