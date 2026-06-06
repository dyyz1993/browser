//! E2E tests for the `render-file` subcommand.
//!
//! Uses the HN-style fixture under `tests/fixtures/`. We assert that
//! titles, ranks, and meta text appear in the rendered ASCII output.
//!
//! Width is set to 400 for title assertions so the long titles don't
//! get wrapped mid-word.

use assert_cmd::Command;
use predicates::prelude::*;

const HN_FIXTURE: &str = "tests/fixtures/news.ycombinator.com.html";

fn bin() -> Command {
    Command::cargo_bin("browser").expect("browser binary not found")
}

#[test]
fn render_file_outputs_titles() {
    bin()
        .args(["render-file", HN_FIXTURE, "--width", "400"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Show HN: A browser written in Rust",
        ))
        .stdout(predicate::str::contains(
            "How modern CSS layout really works",
        ))
        .stdout(predicate::str::contains(
            "The death of server-side rendering has been greatly exaggerated",
        ))
        .stdout(predicate::str::contains("Servo 2026 roadmap"));
}

#[test]
fn render_file_outputs_ranks_and_points() {
    bin()
        .args(["render-file", HN_FIXTURE, "--width", "400"])
        .assert()
        .success()
        .stdout(predicate::str::contains("1."))
        .stdout(predicate::str::contains("2."))
        .stdout(predicate::str::contains("3."))
        .stdout(predicate::str::contains("4."))
        .stdout(predicate::str::contains("512 points"))
        .stdout(predicate::str::contains("298 points"))
        .stdout(predicate::str::contains("421 points"))
        .stdout(predicate::str::contains("187 points"));
}

#[test]
fn render_file_outputs_usernames_and_ages() {
    bin()
        .args(["render-file", HN_FIXTURE, "--width", "400"])
        .assert()
        .success()
        .stdout(predicate::str::contains("rustdev"))
        .stdout(predicate::str::contains("cssnerd"))
        .stdout(predicate::str::contains("isomorphic"))
        .stdout(predicate::str::contains("layoutguru"))
        .stdout(predicate::str::contains("hours ago"));
}

#[test]
fn render_file_custom_width() {
    bin()
        .args(["render-file", HN_FIXTURE, "--width", "60"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Show HN"));
}

#[test]
fn render_file_missing_file_exits_nonzero() {
    bin()
        .args(["render-file", "/tmp/does-not-exist-xyz.html"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("failed to read"));
}

#[test]
fn render_file_empty_input_no_panic() {
    let dir = std::env::temp_dir();
    let path = dir.join("browser_empty_test.html");
    std::fs::write(&path, "").expect("write temp");
    let result = bin()
        .args(["render-file", path.to_str().unwrap()])
        .assert()
        .success();
    let _ = result;
    let _ = std::fs::remove_file(&path);
}
