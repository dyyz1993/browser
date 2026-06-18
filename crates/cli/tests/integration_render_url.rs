//! E2E tests for the `render-url` subcommand (M4.1).
//!
//! Uses wiremock to serve a local HTML fixture, then asserts that
//! `browser render-url <mock-url>` runs the full SPA pipeline:
//! fetch → parse → execute scripts → render.

use assert_cmd::Command;
use predicates::prelude::*;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

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

const SPA_HTML: &str = r#"<!doctype html>
<html>
<head><title>Static</title></head>
<body>
  <p>placeholder</p>
  <script>__setBody("rendered from url")</script>
</body>
</html>"#;

const NO_JS_HTML: &str = r#"<!doctype html>
<html><body><p>plain page</p></body></html>"#;

#[tokio::test]
async fn render_url_executes_scripts_and_outputs_dynamic_content() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/spa"))
        .respond_with(ResponseTemplate::new(200).set_body_string(SPA_HTML))
        .mount(&server)
        .await;
    let url = format!("{}/spa", server.uri());

    bin()
        .args(["render-url", &url, "--width", "120"])
        .assert()
        .success()
        .stdout(predicate::str::contains("rendered from url"))
        .stdout(predicate::str::contains("placeholder").not())
        .stderr(at_least_n_scripts(1));
}

#[tokio::test]
async fn render_url_no_js_flag_skips_script_execution() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/spa"))
        .respond_with(ResponseTemplate::new(200).set_body_string(SPA_HTML))
        .mount(&server)
        .await;
    let url = format!("{}/spa", server.uri());

    // With --no-js, the placeholder stays and we don't see the dynamic
    // text. Also no script count message on stderr.
    bin()
        .args(["render-url", &url, "--width", "120", "--no-js"])
        .assert()
        .success()
        .stdout(predicate::str::contains("placeholder"))
        .stdout(predicate::str::contains("rendered from url").not())
        .stderr(predicate::str::contains("script(s) executed").not());
}

#[tokio::test]
async fn render_url_works_without_scripts() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/plain"))
        .respond_with(ResponseTemplate::new(200).set_body_string(NO_JS_HTML))
        .mount(&server)
        .await;
    let url = format!("{}/plain", server.uri());

    bin()
        .args(["render-url", &url])
        .assert()
        .success()
        .stdout(predicate::str::contains("plain page"));
    // No HTML <script> in the page — we don't assert an exact "0 scripts"
    // count because the reported number includes built-in install scripts
    // (compat_shim etc.). The content rendering is the real assertion.
}

#[tokio::test]
async fn render_url_404_exits_nonzero() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/missing"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    let url = format!("{}/missing", server.uri());

    bin()
        .args(["render-url", &url])
        .assert()
        .failure()
        .stderr(predicate::str::contains("failed to fetch").or(predicate::str::contains("404")));
}

#[tokio::test]
async fn render_url_malformed_url_exits_nonzero() {
    bin()
        .args(["render-url", "not-a-valid-url://??"])
        .assert()
        .failure();
}
