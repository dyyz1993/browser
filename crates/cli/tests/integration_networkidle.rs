//! M18.2 e2e: `--assert-network-idle` CLI flag。
//!
//! 验证爬虫调试能力：渲染完 SPA 后断言 network idle，
//! idle 时退出码 0 + 打印 OK，否则非零退出 + 报告 pending 数。

use assert_cmd::Command;
use predicates::prelude::*;

fn bin() -> Command {
    Command::cargo_bin("browser").expect("browser binary not found")
}

#[test]
fn assert_network_idle_ok_for_timer_spa() {
    // timer-spa.html 用 setTimeout + Promise，pump_event_loop drain 完后应 idle。
    let mut cmd = bin();
    cmd.args([
        "render-script",
        "tests/fixtures/timer-spa.html",
        "--width",
        "60",
        "--assert-network-idle",
    ]);
    let output = cmd.output().expect("run render-script");
    assert!(
        output.status.success(),
        "should exit 0 (idle). stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        predicate::str::contains("[networkidle] OK").eval(&stderr),
        "should print OK. stderr={stderr:?}"
    );
}

#[test]
fn assert_network_idle_noop_without_flag() {
    // 不带 flag 时不应打印 networkidle 信息
    let mut cmd = bin();
    cmd.args([
        "render-script",
        "tests/fixtures/timer-spa.html",
        "--width",
        "60",
    ]);
    let output = cmd.output().expect("run render-script");
    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("networkidle"),
        "no flag → no networkidle output. stderr={stderr:?}"
    );
}
