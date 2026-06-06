//! E2E test for a true SPA-style page (M4.3).
//!
//! The fixture: an HTML page with almost-empty body + a <script> that
//! fetches two API endpoints via __fetchAppendBody and concatenates
//! them into the rendered output. The `{base}` placeholder is
//! string-replaced with the wiremock server URI before serving, so
//! the JS sees absolute URLs (relative-URL resolution lands in M4.4).
//!
//! Wiremock serves the HTML page and both API endpoints on the same
//! MockServer.

use assert_cmd::Command;
use predicates::prelude::*;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn bin() -> Command {
    Command::cargo_bin("browser").expect("browser binary not found")
}

fn spa_shell_html(base: &str) -> String {
    format!(
        r#"<!doctype html>
<html>
<head><title>SPA Shell</title></head>
<body>
  <p>Loading...</p>
  <script>
    // Real SPAs fetch data and render. We simulate this with our
    // synchronous fetch bridge.
    __setBody("Posts:" + "\n");
    __fetchAppendBody("{base}/api/posts-1");
    __appendBody("\n");
    __fetchAppendBody("{base}/api/posts-2");
  </script>
</body>
</html>"#
    )
}

#[tokio::test]
async fn spa_shell_renders_combined_api_output() {
    let server = MockServer::start().await;
    let base = server.uri();

    Mock::given(method("GET"))
        .and(path("/spa"))
        .respond_with(ResponseTemplate::new(200).set_body_string(spa_shell_html(&base)))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/posts-1"))
        .respond_with(ResponseTemplate::new(200).set_body_string("Post A | Post B"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/posts-2"))
        .respond_with(ResponseTemplate::new(200).set_body_string("Post C | Post D"))
        .mount(&server)
        .await;

    let url = format!("{base}/spa");

    // Full pipeline:
    //   fetch /spa → parse → run <script>:
    //     __setBody wipes "Loading..." → "Posts:\n"
    //     __fetchAppendBody(/api/posts-1) → "Post A | Post B"
    //     __appendBody("\n")
    //     __fetchAppendBody(/api/posts-2) → "Post C | Post D"
    //   → render all of that as ASCII
    bin()
        .args(["render-url", &url, "--width", "200"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Posts:"))
        .stdout(predicate::str::contains("Post A | Post B"))
        .stdout(predicate::str::contains("Post C | Post D"))
        // "Loading..." placeholder must be gone — JS replaced it.
        .stdout(predicate::str::contains("Loading...").not())
        .stderr(predicate::str::contains("1 script(s) executed"));
}

#[tokio::test]
async fn spa_shell_no_js_shows_static_placeholder() {
    let server = MockServer::start().await;
    let base = server.uri();
    Mock::given(method("GET"))
        .and(path("/spa"))
        .respond_with(ResponseTemplate::new(200).set_body_string(spa_shell_html(&base)))
        .mount(&server)
        .await;

    bin()
        .args(["render-url", &format!("{base}/spa"), "--no-js"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Loading..."))
        .stdout(predicate::str::contains("Post A").not())
        .stderr(predicate::str::contains("script(s) executed").not());
}

#[tokio::test]
async fn spa_shell_partial_api_failure_renders_whatever_succeeded() {
    // API-1 returns 200, API-2 returns 500. The 500 should be logged
    // but the rest of the page (Posts: + Post A + Post B) should
    // still render — partial-success is critical for scraping.
    let server = MockServer::start().await;
    let base = server.uri();

    Mock::given(method("GET"))
        .and(path("/spa"))
        .respond_with(ResponseTemplate::new(200).set_body_string(spa_shell_html(&base)))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/posts-1"))
        .respond_with(ResponseTemplate::new(200).set_body_string("Post A | Post B"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/posts-2"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;

    bin()
        .args(["render-url", &format!("{base}/spa"), "--width", "200"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Posts:"))
        .stdout(predicate::str::contains("Post A | Post B"))
        // Post C must NOT appear (its fetch failed).
        .stdout(predicate::str::contains("Post C").not())
        .stderr(predicate::str::contains("[js-fetch]"));
}

#[tokio::test]
async fn spa_shell_then_set_title_via_script() {
    // After fetching data, the script also updates the title — proves
    // multiple bridge APIs compose cleanly.
    let server = MockServer::start().await;
    let base = server.uri();

    let html = r#"<!doctype html>
<html>
<head><title>Old</title></head>
<body>
  <script>
    __setTitle("Dynamic Title");
    __setBody("rendered");
  </script>
</body>
</html>"#;
    Mock::given(method("GET"))
        .and(path("/page"))
        .respond_with(ResponseTemplate::new(200).set_body_string(html))
        .mount(&server)
        .await;

    bin()
        .args(["render-url", &format!("{base}/page"), "--width", "80"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Dynamic Title"))
        .stdout(predicate::str::contains("rendered"))
        .stdout(predicate::str::contains("Old").not());
}
