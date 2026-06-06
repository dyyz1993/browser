//! E2E tests for `browser open` (M5.3).
//!
//! GUI window creation can't run in CI (no display server). We use
//! the `--check` flag, which runs the full pipeline (fetch + parse +
//! render) but skips the actual winit window and prints the rendered
//! text to stdout instead. This validates everything except the
//! platform-specific GUI surface.

use assert_cmd::Command;
use predicates::prelude::*;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn bin() -> Command {
    Command::cargo_bin("browser").expect("browser binary not found")
}

const STATIC_HTML: &str = r#"<!doctype html>
<html>
<head><title>Static Page</title></head>
<body><p>Hello from a static page.</p></body>
</html>"#;

const SPA_HTML: &str = r#"<!doctype html>
<html>
<head><title>SPA Shell</title></head>
<body>
  <p>Loading...</p>
  <script>
    __setBody("rendered by JS");
  </script>
</body>
</html>"#;

#[tokio::test]
async fn open_check_static_page_outputs_rendered_text() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/static"))
        .respond_with(ResponseTemplate::new(200).set_body_string(STATIC_HTML))
        .mount(&server)
        .await;

    bin()
        .args(["open", &format!("{}/static", server.uri()), "--check"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Hello from a static page"));
}

#[tokio::test]
async fn open_check_with_js_runs_scripts() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/spa"))
        .respond_with(ResponseTemplate::new(200).set_body_string(SPA_HTML))
        .mount(&server)
        .await;

    bin()
        .args(["open", &format!("{}/spa", server.uri()), "--check"])
        .assert()
        .success()
        // JS replaced the placeholder.
        .stdout(predicate::str::contains("rendered by JS"))
        .stdout(predicate::str::contains("Loading...").not())
        .stderr(predicate::str::contains("1 script(s) executed"));
}

#[tokio::test]
async fn open_check_no_js_keeps_placeholder() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/spa"))
        .respond_with(ResponseTemplate::new(200).set_body_string(SPA_HTML))
        .mount(&server)
        .await;

    bin()
        .args([
            "open",
            &format!("{}/spa", server.uri()),
            "--check",
            "--no-js",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Loading..."))
        .stdout(predicate::str::contains("rendered by JS").not())
        // No script count when --no-js is set.
        .stderr(predicate::str::contains("script(s) executed").not());
}

#[tokio::test]
async fn open_check_404_exits_nonzero() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/missing"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;

    bin()
        .args(["open", &format!("{}/missing", server.uri()), "--check"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("failed to fetch"));
}

#[tokio::test]
async fn open_check_malformed_url_exits_nonzero() {
    bin()
        .args(["open", "not-a-url://??", "--check"])
        .assert()
        .failure();
}

#[tokio::test]
async fn open_check_supports_custom_width() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/w"))
        .respond_with(ResponseTemplate::new(200).set_body_string(STATIC_HTML))
        .mount(&server)
        .await;

    bin()
        .args([
            "open",
            &format!("{}/w", server.uri()),
            "--check",
            "--width",
            "40",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Hello"));
}

#[test]
fn open_help_lists_all_flags() {
    bin()
        .args(["open", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--width"))
        .stdout(predicate::str::contains("--scale"))
        .stdout(predicate::str::contains("--win-width"))
        .stdout(predicate::str::contains("--win-height"))
        .stdout(predicate::str::contains("--no-js"))
        .stdout(predicate::str::contains("--check"));
}
