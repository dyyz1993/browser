//! M7.2.2 e2e: JS DOM API (__createEl/setText/setAttr/appendChild/getElById) actually mutates DOM.

use assert_cmd::Command;
use predicates::prelude::*;

fn bin() -> Command {
    Command::cargo_bin("browser").expect("browser binary not found")
}

#[test]
fn js_dom_api_creates_elements_and_appends_to_body() {
    bin()
        .arg("render-script")
        .arg("tests/fixtures/dom-api.html")
        .arg("--width")
        .arg("80")
        .assert()
        .success()
        // M72.1 UA stylesheet: <h1> gets font-size:2em → rendered UPPERCASE.
        .stdout(predicate::str::contains("HELLO FROM JS"))
        .stdout(predicate::str::contains(
            "This paragraph was created via DOM API",
        ))
        .stdout(predicate::str::contains("panic").not());
}

#[test]
fn js_dom_api_h1_before_paragraph() {
    let output = bin()
        .arg("render-script")
        .arg("tests/fixtures/dom-api.html")
        .arg("--width")
        .arg("80")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let output = String::from_utf8_lossy(&output);
    // M72.1 UA stylesheet: <h1> renders UPPERCASE (font-size:2em default).
    let h1_idx = output.find("HELLO FROM JS").expect("Missing h1 text");
    let p_idx = output
        .find("This paragraph was created via DOM API")
        .expect("Missing paragraph text");
    assert!(
        h1_idx < p_idx,
        "h1 should appear before paragraph (h1 idx={h1_idx}, p idx={p_idx})"
    );
}
