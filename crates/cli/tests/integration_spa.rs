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
        .stderr(at_least_n_scripts(1));
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
async fn spa_shell_then_set_title_does_not_leak_into_body() {
    // After fetching data, the script also updates the title — proves
    // multiple bridge APIs compose cleanly. Crucially, with M6.0b's
    // `<head>` skip, the title text should NOT appear in the body
    // render output (it would go to the window titlebar in a real
    // browser, not into the page text).
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
        // Body text set by __setBody should appear.
        .stdout(predicate::str::contains("rendered"))
        // The original <title> from static HTML should NOT appear
        // (head subtree is suppressed — M6.0b fix).
        .stdout(predicate::str::contains("Old").not())
        // The dynamically-set title should NOT appear in body either
        // (titles belong to the window chrome, not the page text).
        .stdout(predicate::str::contains("Dynamic Title").not());
}

#[tokio::test]
async fn spa_shell_relative_urls_resolve_against_page_url() {
    // M4.4 e2e: script uses relative URLs and the CLI must resolve
    // them against the page URL passed to render-url.
    let server = MockServer::start().await;
    let base = server.uri();

    let html = r#"<!doctype html>
<html><body>
  <p>Loading...</p>
  <script>
    __setBody("Posts:");
    __fetchAppendBody("/api/posts");
    __appendBody(" | ");
    __fetchAppendBody("api/comments");
  </script>
</body></html>"#;

    Mock::given(method("GET"))
        .and(path("/spa"))
        .respond_with(ResponseTemplate::new(200).set_body_string(html))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/posts"))
        .respond_with(ResponseTemplate::new(200).set_body_string("Post A|Post B"))
        .mount(&server)
        .await;
    // RFC 3986: base=/spa + rel="api/comments" → /api/comments
    // (last segment replaced). This matches browser behavior.
    Mock::given(method("GET"))
        .and(path("/api/comments"))
        .respond_with(ResponseTemplate::new(200).set_body_string("C1|C2"))
        .mount(&server)
        .await;

    let bin_result = bin()
        .args(["render-url", &format!("{base}/spa"), "--width", "200"])
        .assert()
        .success();
    let output = bin_result.get_output();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(stdout.contains("Posts:"), "missing Posts: in {stdout}");
    assert!(
        stdout.contains("Post A|Post B"),
        "missing Post A|Post B in {stdout}"
    );
    assert!(stdout.contains("C1|C2"), "missing C1|C2 in {stdout}");
    assert!(
        at_least_n_scripts(1).eval(&stderr),
        "missing script count in {stderr}"
    );
}

#[tokio::test]
async fn spa_shell_relative_url_with_no_base_is_logged_error() {
    // render-script has no source URL, so no base. A relative URL
    // inside the script must be logged as a fetch failure but NOT
    // crash the pipeline.
    let html = r#"<!doctype html>
<html><body>
  <script>
    __setBody("before");
    __fetchAppendBody("/api/missing");
  </script>
</body></html>"#;

    let dir = std::env::temp_dir();
    let path = dir.join("browser_relative_no_base.html");
    std::fs::write(&path, html).unwrap();

    bin()
        .args(["render-script", path.to_str().unwrap(), "--width", "80"])
        .assert()
        .success()
        // Body before the failed fetch is still there.
        .stdout(predicate::str::contains("before"))
        .stderr(predicate::str::contains("[js-fetch]"));

    let _ = std::fs::remove_file(&path);
}
