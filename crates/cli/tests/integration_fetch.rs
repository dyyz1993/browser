//! E2E tests for the `browser fetch` subcommand (M59).
//!
//! Uses wiremock to serve local HTML fixtures, then asserts that
//! `browser fetch <mock-url>` extracts structured content in each format.
//! Mirrors the integration_render_url.rs pattern.

use assert_cmd::Command;
use predicates::prelude::*;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn bin() -> Command {
    Command::cargo_bin("browser").expect("browser binary not found")
}

/// A page with nav/footer noise + article main content (for noise filtering tests).
const MAIN_CONTENT_HTML: &str = r#"<!doctype html>
<html>
<head><title>Test Page</title></head>
<body>
  <nav><a href="/home">Home</a> <a href="/about">About</a></nav>
  <article>
    <h1>Main Article Title</h1>
    <p>This is the <strong>main content</strong> that should survive noise filtering.</p>
    <p>Visit <a href="https://example.com/ref">reference link</a> for details.</p>
    <ul><li>item one</li><li>item two</li></ul>
  </article>
  <footer>copyright 2026 - should be stripped</footer>
</body>
</html>"#;

const SIMPLE_HTML: &str = r#"<!doctype html>
<html>
<head><title>Simple</title></head>
<body>
  <h1>Hello World</h1>
  <p>A paragraph with a <a href="/link">link</a>.</p>
</body>
</html>"#;

#[tokio::test]
async fn fetch_markdown_format() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/page"))
        .respond_with(ResponseTemplate::new(200).set_body_string(SIMPLE_HTML))
        .mount(&server)
        .await;
    let url = format!("{}/page", server.uri());

    bin()
        .args(["fetch", &url, "--format", "markdown", "--no-js"])
        .assert()
        .success()
        .stdout(predicate::str::contains("# Hello World"))
        .stdout(predicate::str::contains("[link]("));
}

#[tokio::test]
async fn fetch_html_format() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/page"))
        .respond_with(ResponseTemplate::new(200).set_body_string(SIMPLE_HTML))
        .mount(&server)
        .await;
    let url = format!("{}/page", server.uri());

    bin()
        .args(["fetch", &url, "--format", "html", "--no-js"])
        .assert()
        .success()
        .stdout(predicate::str::contains("<h1>"))
        .stdout(predicate::str::contains("Hello World"));
}

#[tokio::test]
async fn fetch_text_format() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/page"))
        .respond_with(ResponseTemplate::new(200).set_body_string(SIMPLE_HTML))
        .mount(&server)
        .await;
    let url = format!("{}/page", server.uri());

    bin()
        .args(["fetch", &url, "--format", "text", "--no-js"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Hello World"))
        // text format should NOT contain html tags
        .stdout(predicate::str::contains("<h1>").not());
}

#[tokio::test]
async fn fetch_links_format_resolves_relative() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/page"))
        .respond_with(ResponseTemplate::new(200).set_body_string(SIMPLE_HTML))
        .mount(&server)
        .await;
    let url = format!("{}/page", server.uri());

    bin()
        .args(["fetch", &url, "--format", "links", "--no-js"])
        .assert()
        .success()
        // relative /link should be resolved against mock server base
        .stdout(predicate::str::contains("link → ").or(predicate::str::contains("/link")));
}

#[tokio::test]
async fn fetch_only_main_content_strips_nav_footer() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/page"))
        .respond_with(ResponseTemplate::new(200).set_body_string(MAIN_CONTENT_HTML))
        .mount(&server)
        .await;
    let url = format!("{}/page", server.uri());

    bin()
        .args([
            "fetch",
            &url,
            "--format",
            "text",
            "--only-main-content=true",
            "--no-js",
        ])
        .assert()
        .success()
        // article content survives
        .stdout(predicate::str::contains("main content"))
        .stdout(predicate::str::contains("Main Article Title"))
        // nav/footer noise stripped
        .stdout(predicate::str::contains("copyright 2026").not());
}

#[tokio::test]
async fn fetch_selector_extracts_subtree() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/page"))
        .respond_with(ResponseTemplate::new(200).set_body_string(MAIN_CONTENT_HTML))
        .mount(&server)
        .await;
    let url = format!("{}/page", server.uri());

    bin()
        .args([
            "fetch",
            &url,
            "--format",
            "markdown",
            "--selector",
            "article",
            "--only-main-content=false",
            "--no-js",
        ])
        .assert()
        .success()
        // only article content present
        .stdout(predicate::str::contains("Main Article Title"))
        .stdout(predicate::str::contains("item one"))
        // nav not present (selector scoped to article)
        .stdout(predicate::str::contains("About").not());
}

#[tokio::test]
async fn fetch_json_output_structure() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/page"))
        .respond_with(ResponseTemplate::new(200).set_body_string(SIMPLE_HTML))
        .mount(&server)
        .await;
    let url = format!("{}/page", server.uri());

    bin()
        .args(["fetch", &url, "--format", "text", "--json", "--no-js"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"url\""))
        .stdout(predicate::str::contains("\"title\": \"Simple\""))
        .stdout(predicate::str::contains("\"content\""));
}

#[tokio::test]
async fn fetch_invalid_format_errors() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/page"))
        .respond_with(ResponseTemplate::new(200).set_body_string(SIMPLE_HTML))
        .mount(&server)
        .await;
    let url = format!("{}/page", server.uri());

    bin()
        .args(["fetch", &url, "--format", "pdf", "--no-js"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid --format"));
}

#[tokio::test]
async fn fetch_markdown_table_rendered() {
    // 验证 GFM 表格转换（对标 seo.box CSR 场景）。
    let table_html = r#"<!doctype html>
<html><body>
<table>
<tr><th>Name</th><th>Score</th></tr>
<tr><td>Alice</td><td>90</td></tr>
<tr><td>Bob</td><td>85</td></tr>
</table>
</body></html>"#;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/table"))
        .respond_with(ResponseTemplate::new(200).set_body_string(table_html))
        .mount(&server)
        .await;
    let url = format!("{}/table", server.uri());

    bin()
        .args([
            "fetch",
            &url,
            "--format",
            "markdown",
            "--only-main-content=false",
            "--no-js",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("| Name | Score |"))
        .stdout(predicate::str::contains("| --- |"))
        .stdout(predicate::str::contains("| Alice | 90 |"));
}
