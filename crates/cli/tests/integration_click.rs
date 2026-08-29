//! E2E tests for the `--click` flag (M81: 最小可用点击交互).
//!
//! 语义：JS 跑完后、渲染/截图前，按序对每个 selector 在**同一 JS 会话**
//! 内合成 MouseEvent('click') 并 dispatchEvent（addEventListener 监听器
//! 注册在会话内的元素包装上，引擎 drop 即失效），每次点击后泵一轮事件
//! 循环（点击触发的 setTimeout/fetch 需要时间完成）。
//!
//! 覆盖：
//! 1. onclick 属性 handler（存 DOM 树属性表，dispatch 时 new Function 编译）
//! 2. addEventListener + setTimeout 异步 handler（同会话监听器 + timer 泵）
//! 3. 未命中 selector → stderr 报告且不 panic

use assert_cmd::Command;

const FIXTURE: &str = "tests/fixtures/click_button.html";

fn bin() -> Command {
    Command::cargo_bin("browser").expect("browser binary not found")
}

/// M81: onclick 属性 handler 被 --click 合成点击触发。
#[test]
fn click_attr_handler_updates_dom() {
    bin()
        .args([
            "render-file",
            FIXTURE,
            "--width",
            "400",
            "--render-mode",
            "pixel",
            "--click",
            "#btn",
        ])
        .assert()
        .success()
        .stdout(predicates::str::contains("CLICKED-OK"));
}

/// M81: addEventListener 注册的异步 handler 被触发（点击后事件循环泵
/// 等到 50ms timer 到期）。
#[test]
fn click_async_listener_updates_dom() {
    bin()
        .args([
            "render-file",
            FIXTURE,
            "--width",
            "400",
            "--render-mode",
            "pixel",
            "--click",
            "#async-btn",
        ])
        .assert()
        .success()
        .stdout(predicates::str::contains("ASYNC-CLICK-OK"));
}

/// M81: 未命中 selector → stderr 报告 "no element matched"，渲染不失败。
#[test]
fn click_miss_reported_on_stderr() {
    bin()
        .args([
            "render-file",
            FIXTURE,
            "--width",
            "400",
            "--render-mode",
            "pixel",
            "--click",
            "#no-such-element",
        ])
        .assert()
        .success()
        .stdout(predicates::str::contains("before"))
        .stderr(predicates::str::contains("no element matched"));
}
