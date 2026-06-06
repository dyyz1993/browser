//! Snapshot tests for `render-file` — M6.1.
//!
//! These tests pin down the exact rendered ASCII output for our
//! fixture HTML files. They are the regression net for the M6.0a/b/c
//! layout fixes:
//!
//! - M6.0a (word-wrap): the example.com paragraph wraps at the
//!   viewport width, doesn't truncate.
//! - M6.0b (<head> skip): `<title>` text does not appear in the
//!   body output.
//! - M6.0c (<li> bullets + paragraph spacing): list items are
//!   prefixed with `• ` and separated by blank lines.
//!
//! ## Updating snapshots
//!
//! When a layout change intentionally modifies the rendered output,
//! regenerate the golden files:
//!
//! ```sh
//! UPDATE_SNAPSHOTS=1 cargo test -p browser-cli --test integration_snapshot
//! ```
//!
//! Then inspect `git diff tests/snapshots/*-ours.txt` and commit if
//! the change is intentional.

use assert_cmd::Command;
use std::env;
use std::path::{Path, PathBuf};

fn project_root() -> PathBuf {
    // CARGO_MANIFEST_DIR points at crates/cli. Snapshots live three
    // levels up under tests/snapshots/.
    PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"))
}

fn workspace_root() -> PathBuf {
    let manifest = project_root();
    // crates/cli → workspace root.
    manifest
        .ancestors()
        .nth(2)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| manifest.clone())
}

fn bin() -> Command {
    Command::cargo_bin("browser").expect("browser binary not found")
}

fn snapshot_pair(name: &str) -> (PathBuf, PathBuf) {
    let root = workspace_root();
    let fixture = root
        .join("tests")
        .join("fixtures")
        .join(format!("{name}.html"));
    let golden = root
        .join("tests")
        .join("snapshots")
        .join(format!("{name}-ours.txt"));
    (fixture, golden)
}

fn render_to_string(fixture: &Path, width: u32) -> String {
    let output = bin()
        .args([
            "render-file",
            fixture.to_str().expect("utf8 path"),
            "--width",
            &width.to_string(),
        ])
        .output()
        .expect("spawn browser");
    assert!(
        output.status.success(),
        "render-file failed: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("utf8 stdout")
}

fn check_or_update_snapshot(name: &str, width: u32) {
    let (fixture, golden) = snapshot_pair(name);
    assert!(fixture.exists(), "missing fixture: {}", fixture.display());
    assert!(
        golden.exists(),
        "missing golden snapshot: {}",
        golden.display()
    );

    let actual = render_to_string(&fixture, width);

    if env::var("UPDATE_SNAPSHOTS").as_deref() == Ok("1") {
        std::fs::write(&golden, &actual).expect("write golden");
        eprintln!("updated snapshot: {}", golden.display());
        return;
    }

    let expected = std::fs::read_to_string(&golden).expect("read golden");
    if actual != expected {
        // Write the actual to a sibling .actual file so the dev can
        // `diff` it locally.
        let actual_path = golden.with_extension("txt.actual");
        std::fs::write(&actual_path, &actual).expect("write actual");
        panic!(
            "snapshot mismatch for {name} (width={width}).\n\
             Expected (golden): {}\n\
             Actual  (current): {}\n\
             Diff written to: {}\n\
             To regenerate: UPDATE_SNAPSHOTS=1 cargo test -p browser-cli --test integration_snapshot",
            golden.display(),
            actual_path.display(),
            actual_path.display(),
        );
    }
}

#[test]
fn snapshot_demo_html_width_80() {
    // Pins M6.0b (<title> suppressed) + M6.0c (bullets, blank lines).
    check_or_update_snapshot("demo", 80);
}

#[test]
fn snapshot_example_width_100() {
    // Pins M6.0a (word-wrap). example.com's paragraph wraps inside
    // 100 chars at "without needing permission."
    check_or_update_snapshot("example.com", 100);
}

/// Sanity: the snapshot files we're comparing against actually
/// exist on disk (catches "ran the tests without committing
/// snapshots" mistake).
#[test]
fn snapshot_golden_files_exist() {
    for name in ["demo", "example.com"] {
        let (_, golden) = snapshot_pair(name);
        assert!(golden.exists(), "missing {}", golden.display());
    }
}
