//! M16.3 e2e: event loop 接入后，setTimeout 回调真的能触发。
//!
//! 验证 pump_event_loop 的核心价值：执行完所有 `<script>` 后，
//! drain 到期 timer 回调，回调的 DOM 副作用对后续渲染可见。
//!
//! **测试模式**：body_text_content 返回 script 源码 + body 文本，
//! 所以直接字面量（如 `'fired'`）无法区分 timer 触发与否（源码里就有）。
//! 本文件统一用“累积变量 + 最终 setBody 输出组合标记”模式：
//! 组合标记（如 `"OUT:after"`）只在 timer 真实触发时才拼接出来，
//! script 源码里是 `'OUT:' + result`（变量），不含组合串，能可靠区分。

use browser_html_parser::parse as parse_html;
use browser_js_runtime::{bridge::body_text_content, run_scripts_with_base};

/// 辅助：跑 HTML 后返回 body 文本。注意 borrow 要在 shared 存活期间提取。
fn run_body(html: &str) -> String {
    let tree = parse_html(html);
    let (shared, _) = run_scripts_with_base(tree, None);
    let borrowed = shared.borrow();
    body_text_content(&borrowed)
}

#[test]
fn settimeout_zero_delay_fires_callback() {
    // timer 改 result，终结 timer 输出组合标记。触发→"OUT:after"。
    let html = r#"<html><body><p>init</p>
<script>
var result = 'before';
setTimeout(function() { result = 'after'; }, 0);
setTimeout(function() { __setBody('OUT:' + result); }, 0);
</script>
</body></html>"#;
    let body = run_body(html);
    assert!(
        body.contains("OUT:after"),
        "setTimeout callback should fire, changing result. body={body:?}"
    );
    assert!(
        !body.contains("OUT:before"),
        "unfired state should not be output. body={body:?}"
    );
}

#[test]
fn settimeout_via_global_name_works() {
    // 验证 Web 标准全局名 `setTimeout`（不带 __ 前缀）也能工作。
    let html = r#"<html><body><p>init</p>
<script>
var r = 'no';
setTimeout(function() { r = 'yes'; }, 0);
setTimeout(function() { __setBody('G:' + r); }, 0);
</script>
</body></html>"#;
    let body = run_body(html);
    assert!(
        body.contains("G:yes"),
        "global setTimeout name should fire callback. body={body:?}"
    );
}

#[test]
fn cleartimeout_prevents_callback() {
    // cancel 的 timer 不应执行 → result 不含 BAD。组合 "OUT:BAD" 只在
    // timer 误触发时出现（源码是 result='BAD' 分开，不含 OUT:BAD 组合）。
    let html = r#"<html><body><p>init</p>
<script>
var result = '';
var id = setTimeout(function() { result = 'BAD'; }, 0);
clearTimeout(id);
setTimeout(function() { __setBody('OUT:' + result); }, 0);
</script>
</body></html>"#;
    let body = run_body(html);
    assert!(
        !body.contains("OUT:BAD"),
        "cancelled timer must not fire. body={body:?}"
    );
    assert!(
        body.contains("OUT:"),
        "non-cancelled output timer should still fire. body={body:?}"
    );
}

#[test]
fn multiple_timers_fire_in_fifo_order() {
    // 同 delay 的多个 timer → 按 FIFO 触发（HTML spec 要求）。
    // 组合 "ORDER:ABC" 只在 A→B→C 顺序触发时出现。
    let html = r#"<html><body><p>init</p>
<script>
var order = '';
setTimeout(function() { order += 'A'; }, 0);
setTimeout(function() { order += 'B'; }, 0);
setTimeout(function() { order += 'C'; }, 0);
setTimeout(function() { __setBody('ORDER:' + order); }, 0);
</script>
</body></html>"#;
    let body = run_body(html);
    assert!(
        body.contains("ORDER:ABC"),
        "timers should fire in FIFO order. body={body:?}"
    );
}

#[test]
fn recursive_settimeout_chain_fires() {
    // 回调内部 schedule 新 timer → event loop 下一轮 tick 执行。
    // 最后一次 tick 输出累积 log。组合 "LOG:t1t2t3" 只在 3 次递归后出现。
    let html = r#"<html><body><p>init</p>
<script>
var n = 0;
var log = '';
function tick() {
    n++;
    log += 't' + n;
    if (n < 3) {
        setTimeout(tick, 0);
    } else {
        __setBody('LOG:' + log);
    }
}
setTimeout(tick, 0);
</script>
</body></html>"#;
    let body = run_body(html);
    assert!(
        body.contains("LOG:t1t2t3"),
        "recursive setTimeout should fire 3 times in order. body={body:?}"
    );
    assert!(
        !body.contains("LOG:t1t2t3t4"),
        "chain should stop at n=3. body={body:?}"
    );
}

#[test]
fn settimeout_returned_id_is_number() {
    // setTimeout 必须返回 number（给 clearTimeout 用）。
    // 组合 "type:number" 只在 typeof id === 'number' 时拼接出来。
    let html = r#"<html><body><p>init</p>
<script>
var id = setTimeout(function(){}, 0);
__setBody('type:' + typeof id);
</script>
</body></html>"#;
    let body = run_body(html);
    assert!(
        body.contains("type:number"),
        "setTimeout should return a number. body={body:?}"
    );
}
