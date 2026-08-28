//! M62: SPA 渲染模式集成测试（5 种核心模式，本地 fixture）。
//!
//! 这是可控、可回归的 SPA 测试集，不受公网站点下线/反爬/DNS 影响。
//! 每种模式精准测一个 SPA 能力点，验收 JS 执行后的 DOM 状态。
//! Fixture 位于 tests/fixtures/spa/。

use assert_cmd::Command;
use predicates::prelude::*;

fn bin() -> Command {
    Command::cargo_bin("browser").expect("browser binary not found")
}

fn fixture_path(name: &str) -> String {
    // 测试从 crate root 运行，fixture 在仓库根的 tests/fixtures/spa/。
    format!("../../tests/fixtures/spa/{name}.html")
}

/// 模式 1: async-data —— 异步数据加载（async/await + setTimeout + DOM 改写）。
/// 验收：渲染后含动态加载的商品数据，不含 "loading" 占位。
#[test]
fn spa_async_data_renders_after_async_load() {
    bin()
        .args([
            "render-script",
            &fixture_path("async-data"),
            "--width",
            "100",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Product: Rust Book"))
        .stdout(predicate::str::contains("Price: $39.99"))
        .stdout(predicate::str::contains("In Stock"))
        // loading 占位应被替换
        .stdout(predicate::str::contains("loading...").not());
}

/// 模式 2: route-switch —— 客户端路由（默认路由渲染）。
/// 验收：默认 home 视图内容出现。
/// 注：我们的渲染不处理 CSS display:none，所以 About 也会出现——
/// 验收点只确认 home 内容存在（JS 路由逻辑执行了）。
/// M72.1：fixture 的 <h2> 命中 UA 样式表 font-size:1.5em → 渲染为大写。
#[test]
fn spa_route_switch_renders_default_route() {
    bin()
        .args([
            "render-script",
            &fixture_path("route-switch"),
            "--width",
            "100",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("HOME PAGE CONTENT"));
}

/// 模式 3: lazy-load —— 动态 DOM 构建（createElement + appendChild 循环）。
/// 验收：4 个任务项全部动态渲染出来。
#[test]
fn spa_lazy_load_renders_dynamic_list() {
    bin()
        .args([
            "render-script",
            &fixture_path("lazy-load"),
            "--width",
            "100",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Task: Write tests"))
        .stdout(predicate::str::contains("Task: Fix bugs"))
        .stdout(predicate::str::contains("Task: Refactor code"))
        .stdout(predicate::str::contains("Task: Deploy"));
}

/// 模式 4: dynamic-form —— 表单状态 + 条件渲染。
/// 验收：JS 执行后的初始状态摘要出现。
#[test]
fn spa_dynamic_form_renders_initial_state() {
    bin()
        .args([
            "render-script",
            &fixture_path("dynamic-form"),
            "--width",
            "100",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Summary: 2 items selected"));
}

/// 模式 5: js-redirect —— JS 重定向 / 鉴权逻辑。
/// 验收：重定向逻辑执行，显示目标页内容而非 "checking" 占位。
#[test]
fn spa_js_redirect_executes_auth_logic() {
    bin()
        .args([
            "render-script",
            &fixture_path("js-redirect"),
            "--width",
            "100",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Redirecting to dashboard"))
        .stdout(predicate::str::contains("Welcome back, alice"))
        // checking 占位应被替换
        .stdout(predicate::str::contains("checking session").not());
}
