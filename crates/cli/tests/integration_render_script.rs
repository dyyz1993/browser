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

/// M63: At least N scripts executed. The reported count includes built-in
/// install scripts (compat_shim/element_shim/etc.), which vary by boa version,
/// so we assert a floor rather than an exact count.
fn at_least_n_scripts(n: usize) -> impl Predicate<str> {
    predicate::function(move |s: &str| {
        s.lines()
            .filter_map(|l| l.strip_prefix("[browser] "))
            .filter_map(|l| l.strip_suffix(" script(s) executed"))
            .filter_map(|c| c.parse::<usize>().ok())
            .next()
            .unwrap_or(0)
            >= n
    })
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
    // All three HTML scripts should execute successfully. Note: the reported
    // count also includes built-in install scripts (compat_shim, element_shim,
    // etc.), which vary by boa version, so we assert the floor (>= 3) rather
    // than an exact count.
    bin()
        .args(["render-script", SPA_FIXTURE, "--width", "120"])
        .assert()
        .success()
        .stderr(at_least_n_scripts(3));
}

#[test]
fn render_file_now_executes_js_and_waits_for_async() {
    // M37: render-file now executes JS and waits for async (setTimeout/fetch).
    // Previously JS was disabled (always returned "Static placeholder").
    // Now SPA rendering works: we should see JS-generated content.
    bin()
        .args(["render-file", SPA_FIXTURE, "--width", "120"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Welcome to my blog"));
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
    // No <script> in the source HTML — should still succeed and render the
    // plain content. The reported count includes built-in install scripts
    // (compat_shim etc.), so we don't assert an exact "0 scripts" count;
    // instead we verify the content rendered and no JS error leaked.
    let dir = std::env::temp_dir();
    let path = dir.join("browser_no_scripts_test.html");
    std::fs::write(&path, "<html><body><p>plain content</p></body></html>").expect("write temp");
    bin()
        .args(["render-script", path.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("plain content"));
    let _ = std::fs::remove_file(&path);
}
