//! M16.4 e2e: Promise.then 回调能在 run_scripts 后真正触发。
//!
//! 验证 pump_event_loop 里的 `ctx.run_jobs()` 调用：执行完所有
//! `<script>` 后，boa 的 SimpleJobQueue 里 enqueue 的 Promise then
//! 回调被 drain 执行，DOM 副作用对后续渲染可见。
//!
//! 测试模式同 integration_settimeout：用"累积变量 + 组合标记"避免
//! 源码字面量假阳性。

use browser_html_parser::parse as parse_html;
use browser_js_runtime::{bridge::body_text_content, run_scripts_with_base};

fn run_body(html: &str) -> String {
    let tree = parse_html(html);
    let (shared, _) = run_scripts_with_base(tree, None);
    let borrowed = shared.borrow();
    body_text_content(&borrowed)
}

#[test]
fn promise_resolve_then_fires() {
    // Promise.resolve(42).then(v => result = v) — then 回调应执行。
    let html = r#"<html><body><p>init</p>
<script>
var result = 'before';
Promise.resolve('after').then(function(v) { result = v; });
setTimeout(function() { __setBody('OUT:' + result); }, 0);
</script>
</body></html>"#;
    let body = run_body(html);
    assert!(
        body.contains("OUT:after"),
        "Promise.then should fire and change result. body={body:?}"
    );
}

#[test]
fn promise_chain_fires_in_order() {
    // Promise.then 链：每层 then 依赖上一层的结果。
    let html = r#"<html><body><p>init</p>
<script>
var log = '';
Promise.resolve(1)
    .then(function(v) { log += 'A' + v; return v + 1; })
    .then(function(v) { log += 'B' + v; return v + 1; })
    .then(function(v) { log += 'C' + v; });
setTimeout(function() { __setBody('LOG:' + log); }, 0);
</script>
</body></html>"#;
    let body = run_body(html);
    assert!(
        body.contains("LOG:A1B2C3"),
        "Promise chain should fire in order with propagated values. body={body:?}"
    );
}

#[test]
fn promise_new_with_executor() {
    // new Promise((resolve) => resolve('x')) — executor 同步 resolve。
    let html = r#"<html><body><p>init</p>
<script>
var result = 'none';
new Promise(function(resolve) { resolve('done'); })
    .then(function(v) { result = v; });
setTimeout(function() { __setBody('R:' + result); }, 0);
</script>
</body></html>"#;
    let body = run_body(html);
    assert!(
        body.contains("R:done"),
        "new Promise executor + then should work. body={body:?}"
    );
}

#[test]
fn promise_and_settimeout_interleave() {
    // Promise microtask 与 setTimeout macrotask 交错：
    // script 同步代码先跑 → Promise.then 入 microtask 队列 →
    // setTimeout(0) 入 timer 队列。event loop 先 run_jobs（microtask）
    // 再 drain timer。最终顺序应是 P1（promise）在 T1（timer）前。
    let html = r#"<html><body><p>init</p>
<script>
var order = '';
Promise.resolve().then(function() { order += 'P'; });
setTimeout(function() { order += 'T'; }, 0);
setTimeout(function() { __setBody('ORD:' + order); }, 0);
</script>
</body></html>"#;
    let body = run_body(html);
    assert!(
        body.contains("ORD:PT"),
        "Promise microtask should fire before setTimeout macrotask. body={body:?}"
    );
}

#[test]
fn promise_then_can_schedule_settimeout() {
    // Promise.then 回调内 schedule 的 setTimeout 不会被丢弃——
    // 它会在后续 tick 执行。注意 HTML timer FIFO 语义：同步注册的
    // timer 排在 microtask 里注册的 timer 前面，所以要用嵌套 timer
    // 给 after timer 留出执行窗口。
    let html = r#"<html><body><p>init</p>
<script>
var result = 'before';
Promise.resolve().then(function() {
    setTimeout(function() { result = 'after'; }, 0);
});
// 两层 setTimeout：outer 同步注册，inner 在 outer 回调里注册，
// after timer（microtask 里 schedule）会在 outer 和 inner 之间执行。
setTimeout(function() {
    setTimeout(function() {
        __setBody('OUT:' + result);
    }, 0);
}, 0);
</script>
</body></html>"#;
    let body = run_body(html);
    assert!(
        body.contains("OUT:after"),
        "setTimeout scheduled inside Promise.then should fire (not be dropped). body={body:?}"
    );
}
