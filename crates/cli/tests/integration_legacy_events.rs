//! M80.18: `document.createEvent("MouseEvent")` + `initMouseEvent` 回归锚点。
//!
//! WPT uievents/legacy-domevents-tests（dispatchEvent.click.checkbox 等）
//! 依赖老式 createEvent+init* API。修复前两个缺口：
//! 1. createEvent 只认复数 'MouseEvents'，单数 'MouseEvent' 落 `new Event('')`；
//! 2. upgradeEventFamily 只升级了 window.MouseEvent，内部局部绑定仍是
//!    原始构造器（无 initMouseEvent）。
//!
//! 两者叠加 → 合成点击流程在第二段监听器前断裂 → webapi no-results。

use assert_cmd::Command;

fn bin() -> Command {
    Command::cargo_bin("browser").expect("browser binary not found")
}

/// createEvent("MouseEvent")+initMouseEvent 合成点击走完全程，且 checkbox
/// 默认动作（checked 翻转）在 dispatchEvent 中执行、bubbles 旗标保持 false。
#[test]
fn legacy_create_event_click_reaches_target_listener() {
    bin()
        .args([
            "render-file",
            "tests/fixtures/legacy_create_event_click.html",
            "--width",
            "400",
            "--render-mode",
            "pixel",
        ])
        .assert()
        .success()
        .stdout(predicates::str::contains(
            "LEGACY-CLICK-OK checked=true bubbles=false",
        ));
}
