//! End-to-end tests for the `browser` CLI.
//!
//! These tests invoke the actual `browser` binary as a subprocess
//! using `assert_cmd`, then assert on stdout/stderr. For the `get`
//! subcommand we spin up a `wiremock` mock server so the tests stay
//! offline and deterministic.

use assert_cmd::Command;
use predicates::prelude::*;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn bin() -> Command {
    Command::cargo_bin("browser").expect("browser binary not found")
}

#[tokio::test]
async fn test_get_returns_dom_tree_for_mock_server() {
    let body = include_str!("../../html-parser/tests/fixtures/simple.html");
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/simple.html"))
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .mount(&server)
        .await;

    let url = format!("{}/simple.html", server.uri());

    bin()
        .args(["get", &url])
        .assert()
        .success()
        .stdout(predicate::str::contains("Document"))
        .stdout(predicate::str::contains("Element(html)"))
        .stdout(predicate::str::contains("Element(body)"))
        .stdout(predicate::str::contains("Element(p)"))
        .stdout(predicate::str::contains("Text(\"hello\")"));
}

#[test]
fn test_parse_local_fixture() {
    // Tests run from the crate root, so the fixture lives two dirs up.
    let fixture = "tests/../../html-parser/tests/fixtures/simple.html";
    bin()
        .args(["parse", fixture])
        .assert()
        .success()
        .stdout(predicate::str::contains("Document"))
        .stdout(predicate::str::contains("Element(html)"))
        .stdout(predicate::str::contains("Element(body)"))
        .stdout(predicate::str::contains("Text(\"hello\")"));
}

#[test]
fn test_get_with_invalid_url_exits_nonzero() {
    bin()
        .args(["get", "not a url"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid URL"));
}

#[test]
fn test_parse_missing_file_exits_nonzero() {
    bin()
        .args(["parse", "/tmp/does-not-exist-9q8z7r.html"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("failed to read"));
}

#[test]
fn test_help_lists_subcommands() {
    bin()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("parse"))
        .stdout(predicate::str::contains("get"));
}
