//! M13.4 e2e: render-script 跑 SPA fixture 验证 localStorage 持久化。
//!
//! 验证：
//! 1. SPA 首次运行时 setItem + getItem 都能工作
//! 2. 输出文本包含存储的值

use assert_cmd::Command;

#[test]
fn storage_spa_first_visit_renders_stored_token() {
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/storage-spa.html");

    let output = Command::cargo_bin("browser")
        .expect("browser binary")
        .args([
            "render-script",
            fixture.to_str().expect("path str"),
            "--width",
            "80",
        ])
        .output()
        .expect("spawn browser");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    // 验证 stdout 包含存储后的渲染
    assert!(
        stdout.contains("first visit, stored token=abc-123"),
        "stdout missing stored token. stdout={stdout}\nstderr={stderr}"
    );
    assert!(
        stdout.contains("theme=dark"),
        "stdout missing theme. stdout={stdout}"
    );
}

#[test]
fn storage_spa_via_render_file_does_not_execute() {
    // render-file 不执行 JS，所以应该看到 'before' 而不是 'first visit'
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/storage-spa.html");

    let output = Command::cargo_bin("browser")
        .expect("browser binary")
        .args([
            "render-file",
            fixture.to_str().expect("path str"),
            "--width",
            "80",
        ])
        .output()
        .expect("spawn browser");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("before"), "render-file should show raw body. stdout={stdout}");
    assert!(!stdout.contains("stored token"), "render-file must not execute JS");
}
