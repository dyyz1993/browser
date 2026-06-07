//! M27.2 e2e: `<a href>` 链接目标应在渲染输出中可见（G1 爬虫核心需求）。
//!
//! 背景：真浏览器给 `<a>` 加颜色+下划线；ASCII 模式无颜色。为了让爬虫
//! 看到"链接指向哪里"，我们把 href 内联到文本（`text (url)`）。这对
//! G1（爬虫提取链接图）价值最大——爬虫无需解析 DOM，从渲染文本就能
//! 拿到所有链接。

use assert_cmd::Command;
use predicates::prelude::*;
use std::io::Write;

fn bin() -> Command {
    Command::cargo_bin("browser").expect("browser binary not found")
}

#[test]
fn a_href_appears_in_rendered_text() {
    let tmp = std::env::temp_dir();
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = tmp.join(format!("m27-anchor-{ts}.html"));
    let html = r#"<!doctype html><html><body>
<p>Visit <a href="https://example.org/home">our site</a> now.</p>
</body></html>"#;
    std::fs::File::create(&path)
        .unwrap()
        .write_all(html.as_bytes())
        .unwrap();

    let output = bin()
        .args(["render-file", path.to_str().unwrap(), "--width", "60"])
        .output()
        .expect("run render-file");
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    // 链接文本 + href 都应出现
    assert!(
        predicate::str::contains("our site").eval(&stdout),
        "link text missing. stdout={stdout:?}"
    );
    assert!(
        predicate::str::contains("https://example.org/home").eval(&stdout),
        "href must be visible for crawlers. stdout={stdout:?}"
    );
    assert!(
        predicate::str::contains("Visit").eval(&stdout),
        "surrounding text missing. stdout={stdout:?}"
    );

    let _ = std::fs::remove_file(&path);
}

#[test]
fn empty_anchor_still_shows_href() {
    // `<a href="u"></a>` 没有文本子节点。浏览器仍把它当作可发现链接。
    // 我们应 seed 一个文本叶子显示 href（爬虫不丢链接）。
    let tmp = std::env::temp_dir();
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = tmp.join(format!("m27-empty-anchor-{ts}.html"));
    let html = r#"<!doctype html><html><body>
<a href="https://secret.example/path"></a>
</body></html>"#;
    std::fs::File::create(&path)
        .unwrap()
        .write_all(html.as_bytes())
        .unwrap();

    let output = bin()
        .args(["render-file", path.to_str().unwrap(), "--width", "60"])
        .output()
        .expect("run");
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        predicate::str::contains("https://secret.example/path").eval(&stdout),
        "empty anchor href must still appear. stdout={stdout:?}"
    );

    let _ = std::fs::remove_file(&path);
}

#[test]
fn anchor_without_href_unchanged() {
    // 没有 href 的 <a> 不是链接，不应注入任何东西。
    let tmp = std::env::temp_dir();
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = tmp.join(format!("m27-nohref-anchor-{ts}.html"));
    let html = r#"<!doctype html><html><body>
<a name="bookmark">plain text</a>
</body></html>"#;
    std::fs::File::create(&path)
        .unwrap()
        .write_all(html.as_bytes())
        .unwrap();

    let output = bin()
        .args(["render-file", path.to_str().unwrap(), "--width", "60"])
        .output()
        .expect("run");
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        predicate::str::contains("plain text").eval(&stdout),
        "text should still render. stdout={stdout:?}"
    );
    // 不应出现 "(" 注入（无 href）
    assert!(
        !stdout.contains("("),
        "no href → no injection expected. stdout={stdout:?}"
    );

    let _ = std::fs::remove_file(&path);
}
