//! E2E regression tests for flex gap / block-level-in-inline-run (M78.142).
//!
//! Real-site symptom (nextjs.org feature cards): `<a style="display:flex">`
//! cards grouped into anonymous inline wrappers never ran flex layout, so
//! every card's text piled at overlapping positions and the painted words
//! interleaved into glued garbage like
//! `Addccomponentsowithoutssendingnadditionals...`.
//!
//! Fixture: `tests/fixtures/flex_anchor_cards.html` — anchor cards laid out
//! as flex columns, each title a flex row with `gap: 8px` between `<span>`
//! words (the modern hero-copy pattern).

use assert_cmd::Command;
use predicates::prelude::*;

const FIXTURE: &str = "tests/fixtures/flex_anchor_cards.html";

fn bin() -> Command {
    Command::cargo_bin("browser").expect("browser binary not found")
}

#[test]
fn flex_card_words_not_glued_together() {
    // The subtitle is plain text: words must stay separated by spaces.
    bin()
        .args(["render-file", FIXTURE, "--width", "100"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Add components without sending additional client-side JavaScript.",
        ))
        .stdout(predicate::str::contains(
            "Flexible rendering and caching options, including ISR.",
        ));
}

#[test]
fn flex_row_gap_separates_span_words() {
    // Title spans in a flex row with gap:8px must not glue into
    // "ReactServerComponents". ASCII convention is 1px = 1 char for gap
    // (boxes.rs "character units"), so each gap renders as >=1 space.
    let out = bin()
        .args(["render-file", FIXTURE, "--width", "100"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8_lossy(&out);
    // All words on ONE line, separated only by whitespace (the gap).
    let spaced = |words: &[&str]| {
        text.lines().any(|line| {
            let mut rest = line;
            for w in words {
                let Some(pos) = rest.find(w) else {
                    return false;
                };
                // Gap before each word must be whitespace-only.
                if !rest[..pos].chars().all(char::is_whitespace) {
                    return false;
                }
                rest = &rest[pos + w.len()..];
            }
            true
        })
    };
    assert!(
        spaced(&["React", "Server", "Components"]),
        "expected whitespace-separated title spans, got:\n{text}"
    );
    assert!(
        spaced(&["Client", "and", "Server", "Rendering"]),
        "expected whitespace-separated title spans, got:\n{text}"
    );
    // No glued variant anywhere.
    assert!(
        !text.contains("ReactServer"),
        "flex gap lost: found glued 'ReactServer':\n{text}"
    );
    assert!(
        !text.contains("Addcomponents"),
        "inline words glued:\n{text}"
    );
}

#[test]
fn anchor_flex_cards_stack_as_column() {
    // display:flex on an inline tag (<a>) must produce a block-level flex
    // column: the card title and subtitle stack on separate lines instead
    // of sharing one inline line.
    let out = bin()
        .args(["render-file", FIXTURE, "--width", "100"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8_lossy(&out);
    let title_line = text
        .lines()
        .find(|l| l.contains("React") && l.contains("Components"))
        .expect("title line present");
    // The title line must NOT also contain the card subtitle (they used to
    // overlap on one line when flex layout never ran).
    assert!(
        !title_line.contains("Add components"),
        "card title and subtitle overlap on one line:\n{title_line}"
    );
}
