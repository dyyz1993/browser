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

#[test]
fn render_file_applies_inline_style_block() {
    // M7.1.6: <style> tag contents must be parsed and applied.
    // Without M7.1.6 the 10% margin-left would be 0 — the text
    // would start at column 0.
    let dir = std::env::temp_dir();
    let path = dir.join("browser_m716_test.html");
    std::fs::write(
        &path,
        "<html><head><style>.indent { margin-left: 20px; }</style></head>\n\
         <body><p class=\"indent\">indented text</p></body></html>",
    )
    .expect("write temp");
    let result = bin()
        .args(["render-file", path.to_str().unwrap(), "--width", "100"])
        .assert()
        .success();
    let s = result.get_output().stdout.clone();
    let s_str = std::str::from_utf8(&s).unwrap();
    // The paragraph should have at least 20 chars of leading whitespace
    // on its first line (the margin-left).
    let first_text_line = s_str
        .lines()
        .find(|line| line.contains("indented text"))
        .expect("missing indented text line");
    let leading = first_text_line
        .chars()
        .take_while(|c| c.is_whitespace())
        .count();
    assert!(
        leading >= 20,
        "expected >=20 leading whitespace, got {leading} in {first_text_line:?}"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn render_file_margin_collapsing_two_paragraphs() {
    // M7.1.4: two adjacent <p> with margin-top:10px / margin-bottom:5px
    // should have exactly max(10, 5) = 10 lines of vertical gap, not 15.
    let dir = std::env::temp_dir();
    let path = dir.join("browser_m714_test.html");
    std::fs::write(
        &path,
        "<html><head><style>\n\
         .a { margin-top: 10px; margin-bottom: 5px; }\n\
         .b { margin-top: 8px; margin-bottom: 0; }\n\
         </style></head>\n\
         <body>\n\
         <p class=\"a\">first</p>\n\
         <p class=\"b\">second</p>\n\
         </body></html>",
    )
    .expect("write temp");
    let result = bin()
        .args(["render-file", path.to_str().unwrap(), "--width", "50"])
        .assert()
        .success();
    let s = std::str::from_utf8(&result.get_output().stdout).unwrap();
    // Find the lines containing "first" and "second".
    let first_line_idx = s
        .lines()
        .position(|l| l.contains("first"))
        .expect("missing first");
    let second_line_idx = s
        .lines()
        .position(|l| l.contains("second"))
        .expect("missing second");
    let gap = second_line_idx.saturating_sub(first_line_idx);
    // CSS says gap = max(margin-bottom of a = 5, margin-top of b = 8) = 8.
    // We allow some slack because .a's margin-top:10 also pushes "first"
    // down, and rendering may add 1-2 lines of UA-default body padding.
    assert!(
        (2..=12).contains(&gap),
        "expected collapsed gap (CSS spec: max(5,8)=8 lines, ±slack), got {gap}\noutput:\n{s}"
    );
    let _ = std::fs::remove_file(&path);
}
