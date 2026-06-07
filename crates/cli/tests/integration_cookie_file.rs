//! M21.2 e2e: `--cookie-file` flag 跨进程 cookie 持久化。
//!
//! 验证：首次运行（无文件）→ 保存 jar；第二次运行（有文件）→ 加载 jar。
//! 解决"爬虫重启要重新登录"痛点。

use assert_cmd::Command;
use predicates::prelude::*;

fn bin() -> Command {
    Command::cargo_bin("browser").expect("browser binary not found")
}

fn unique_cookie_file(label: &str) -> std::path::PathBuf {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("browser-cookie-e2e-{nanos}-{label}.txt"))
}

#[test]
fn cookie_file_created_on_first_run() {
    let cookie_file = unique_cookie_file("first");
    // 确保文件不存在
    let _ = std::fs::remove_file(&cookie_file);

    let mut cmd = bin();
    cmd.args([
        "render-script",
        "tests/fixtures/timer-spa.html",
        "--width",
        "60",
        "--cookie-file",
    ]);
    cmd.arg(&cookie_file);
    let output = cmd.output().expect("run render-script");
    assert!(
        output.status.success(),
        "should succeed. stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );

    // 文件应被创建（含 header）
    assert!(
        cookie_file.exists(),
        "cookie file should be created on first run"
    );
    let content = std::fs::read_to_string(&cookie_file).unwrap();
    assert!(
        predicate::str::contains("# browser-cookie v1").eval(&content),
        "file should start with format header. content={content:?}"
    );
    // stderr 应有 saved 提示
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        predicate::str::contains("[cookie] saved").eval(&stderr),
        "should log saved. stderr={stderr:?}"
    );

    let _ = std::fs::remove_file(&cookie_file);
}

#[test]
fn cookie_file_loaded_on_subsequent_run() {
    let cookie_file = unique_cookie_file("second");

    // 第一次运行：创建文件
    let mut first = bin();
    first.args([
        "render-script",
        "tests/fixtures/timer-spa.html",
        "--width",
        "60",
        "--cookie-file",
    ]);
    first.arg(&cookie_file);
    first.output().expect("first run");
    assert!(cookie_file.exists());

    // 第二次运行：应显示 loaded
    let mut second = bin();
    second.args([
        "render-script",
        "tests/fixtures/timer-spa.html",
        "--width",
        "60",
        "--cookie-file",
    ]);
    second.arg(&cookie_file);
    let output = second.output().expect("second run");
    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        predicate::str::contains("[cookie] loaded").eval(&stderr),
        "second run should load existing file. stderr={stderr:?}"
    );

    let _ = std::fs::remove_file(&cookie_file);
}

#[test]
fn cookie_file_default_none_no_stderr() {
    // 不带 --cookie-file 时不应有 [cookie] 输出
    let mut cmd = bin();
    cmd.args([
        "render-script",
        "tests/fixtures/timer-spa.html",
        "--width",
        "60",
    ]);
    let output = cmd.output().expect("run");
    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("[cookie]"),
        "no flag → no cookie logs. stderr={stderr:?}"
    );
}
