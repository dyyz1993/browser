//! M26.1 e2e: `<textarea>` 内容不应渲染到文档流。
//!
//! 背景：百度等大站把 CSS 文本塞进 `<textarea id="s_is_result_css"
//! style="display:none">` 做延迟加载（JS 运行时注入 `<style>`）。
//! 旧版把 textarea 当普通块级元素，CSS 文本被渲染（69% 输出是噪音，
//! 截图 50MB 且混乱）。真浏览器 textarea 内容是初始值，不参与渲染。

use assert_cmd::Command;
use predicates::prelude::*;
use std::io::Write;

fn bin() -> Command {
    Command::cargo_bin("browser").expect("browser binary not found")
}

#[test]
fn textarea_content_not_rendered() {
    let tmp = std::env::temp_dir();
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let html_path = tmp.join(format!("m26-textarea-{ts}.html"));
    let html = r#"<!doctype html><html><body>
<textarea>SHOULD_NOT_APPEAR</textarea>
<p>visible text</p>
</body></html>"#;
    std::fs::File::create(&html_path)
        .unwrap()
        .write_all(html.as_bytes())
        .unwrap();

    let output = bin()
        .args(["render-file", html_path.to_str().unwrap(), "--width", "40"])
        .output()
        .expect("run render-file");
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        predicate::str::contains("SHOULD_NOT_APPEAR")
            .not()
            .eval(&stdout),
        "textarea content must not be rendered. stdout={stdout:?}"
    );
    assert!(
        predicate::str::contains("visible text").eval(&stdout),
        "normal text should still render. stdout={stdout:?}"
    );

    let _ = std::fs::remove_file(&html_path);
}

#[test]
fn textarea_with_css_payload_not_leaked() {
    // 模拟百度的延迟加载模式：textarea 里塞 CSS 文本
    let tmp = std::env::temp_dir();
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let html_path = tmp.join(format!("m26-cssleak-{ts}.html"));
    let html = r#"<!doctype html><html><body>
<textarea id="s_is_result_css" style="display:none">html{font-size:100px}body{color:#333;background:#fff}</textarea>
<p>real content</p>
</body></html>"#;
    std::fs::File::create(&html_path)
        .unwrap()
        .write_all(html.as_bytes())
        .unwrap();

    let output = bin()
        .args(["render-file", html_path.to_str().unwrap(), "--width", "40"])
        .output()
        .expect("run");
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    // CSS 关键字不应泄漏
    assert!(
        !predicate::str::contains("font-size").eval(&stdout),
        "CSS must not leak. stdout={stdout:?}"
    );
    assert!(
        !predicate::str::contains("background").eval(&stdout),
        "CSS must not leak. stdout={stdout:?}"
    );
    assert!(
        predicate::str::contains("real content").eval(&stdout),
        "real content should render. stdout={stdout:?}"
    );

    let _ = std::fs::remove_file(&html_path);
}
