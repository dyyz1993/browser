//! E2E tests for the `--hover` flag (M81: hover 事件族合成).
//!
//! 语义：JS 跑完后、渲染/截图前，按序对每个 selector 在**同一 JS 会话**
//! 内合成浏览器标准 hover 序列：`mouseover`（bubbles）→ `mouseenter`
//! （不冒泡）→ `mousemove`（bubbles）；若此前 hover 过别的元素，先对旧
//! 元素派 `mouseout`（bubbles）→ `mouseleave`（不冒泡）——out/over、
//! enter/leave 成对。每次悬停后泵一轮事件循环（hover 展开的 setTimeout/
//! fetch 异步内容需要 drain）。
//!
//! 覆盖：
//! 1. onmouseenter 属性 handler（存 DOM 树属性表，dispatch 时编译执行）
//! 2. addEventListener + setTimeout 异步 handler（悬停展开菜单工作流）
//! 3. 事件顺序：mouseover → mouseenter → mousemove（浏览器标准序列）
//! 4. out/leave 配对：hover 第二个元素前，旧元素收到 mouseout/mouseleave
//! 5. 未命中 selector → stderr 报告且不 panic
//! 6. document.elementFromPoint 命中测试配套存在且返回元素

use assert_cmd::Command;

const FIXTURE: &str = "tests/fixtures/hover_menu.html";

fn bin() -> Command {
    Command::cargo_bin("browser").expect("browser binary not found")
}

/// M81: onmouseenter 属性 handler 被 --hover 合成悬停触发。
#[test]
fn hover_attr_handler_updates_dom() {
    bin()
        .args(["render-file", FIXTURE, "--width", "200", "--hover", "#alt"])
        .assert()
        .success()
        .stdout(predicates::str::contains("alt-enter"));
}

/// M81: addEventListener 注册的异步 handler 被触发（hover 后事件循环泵
/// 等到 30ms timer 到期，悬停展开的子菜单内容反映到渲染输出）。
#[test]
fn hover_async_listener_updates_dom() {
    bin()
        .args(["render-file", FIXTURE, "--width", "200", "--hover", "#nav"])
        .assert()
        .success()
        .stdout(predicates::str::contains("SUBMENU-LOADED"));
}

/// M81: 合成序列对齐浏览器标准——mouseover → mouseenter → mousemove。
#[test]
fn hover_event_order_standard_sequence() {
    bin()
        .args(["render-file", FIXTURE, "--width", "200", "--hover", "#nav"])
        .assert()
        .success()
        .stdout(predicates::str::contains("mouseover>mouseenter>mousemove"));
}

/// M81: out/leave 配对——hover 第二个元素前，旧元素先收到 mouseleave
/// （enter/leave 不冒泡，仅旧元素自身的 attr handler 触发）。
#[test]
fn hover_leave_dispatched_on_previous_target() {
    bin()
        .args([
            "render-file",
            FIXTURE,
            "--width",
            "200",
            "--hover",
            "#nav",
            "--hover",
            "#alt",
        ])
        .assert()
        .success()
        .stdout(predicates::str::contains("nav-enter,nav-leave,alt-enter"));
}

/// M81: 未命中 selector → stderr 报告 "no element matched"，渲染不失败。
#[test]
fn hover_miss_reported_on_stderr() {
    bin()
        .args([
            "render-file",
            FIXTURE,
            "--width",
            "200",
            "--hover",
            "#no-such-element",
        ])
        .assert()
        .success()
        .stdout(predicates::str::contains("empty"))
        .stderr(predicates::str::contains("no element matched"));
}

/// M81: document.elementFromPoint 命中测试配套——存在、不抛错、返回元素。
/// 本引擎 JS 会话无布局树（rect 零桩），返回 body 兜底属预期近似。
#[test]
fn element_from_point_returns_element() {
    bin()
        .args(["render-file", FIXTURE, "--width", "200"])
        .assert()
        .success()
        .stdout(predicates::str::contains("EFP=BODY"));
}
