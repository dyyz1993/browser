//! E2E tests for the `render-script` subcommand (M3.4).
//!
//! Verifies that <script> tags are executed before layout, and that
//! JS-side DOM mutations become visible in the rendered ASCII output.
//! The SPA fixture under `tests/fixtures/spa-blog.html` exercises
//! __setBody, __appendBody, and __setTitle across three scripts.

use assert_cmd::Command;
use predicates::prelude::*;

const SPA_FIXTURE: &str = "tests/fixtures/spa-blog.html";

fn bin() -> Command {
    Command::cargo_bin("browser").expect("browser binary not found")
}

#[test]
fn render_script_replaces_static_placeholder_with_dynamic_body() {
    // __setBody wipes the placeholder; we should see "Welcome to my
    // blog" but NOT "Static placeholder".
    bin()
        .args(["render-script", SPA_FIXTURE, "--width", "120"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Welcome to my blog"))
        .stdout(predicate::str::contains("Recent posts:"))
        .stdout(predicate::str::contains("Post A"))
        .stdout(predicate::str::contains("Post B"))
        .stdout(predicate::str::contains("Static placeholder").not());
}

#[test]
fn render_script_appends_across_multiple_script_tags() {
    // The second script calls __appendBody; "Author: Jane Doe" should
    // appear after the post list (i.e. somewhere later in the output).
    bin()
        .args(["render-script", SPA_FIXTURE, "--width", "120"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Author: Jane Doe"));
}

#[test]
fn render_script_logs_execution_count_to_stderr() {
    // All three scripts should execute successfully.
    bin()
        .args(["render-script", SPA_FIXTURE, "--width", "120"])
        .assert()
        .success()
        .stderr(predicate::str::contains("3 script(s) executed"));
}

#[test]
fn render_script_does_not_run_js_when_using_render_file() {
    // Sanity: the plain `render-file` subcommand must NOT execute
    // scripts — we should still see "Static placeholder" and NOT
    // "Welcome to my blog".
    bin()
        .args(["render-file", SPA_FIXTURE, "--width", "120"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Static placeholder"))
        .stdout(predicate::str::contains("Welcome to my blog").not());
}

#[test]
fn render_script_missing_file_exits_nonzero() {
    bin()
        .args(["render-script", "/tmp/does-not-exist-spa.html"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("failed to read"));
}

#[test]
fn render_script_html_with_no_scripts_renders_normally() {
    // No <script> in the source — should still succeed, just with
    // "0 script(s) executed".
    let dir = std::env::temp_dir();
    let path = dir.join("browser_no_scripts_test.html");
    std::fs::write(&path, "<html><body><p>plain content</p></body></html>").expect("write temp");
    bin()
        .args(["render-script", path.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("plain content"))
        .stderr(predicate::str::contains("0 script(s) executed"));
    let _ = std::fs::remove_file(&path);
}
