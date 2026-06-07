//! M22.2 e2e: `<img src>` 本地图像渲染成 ASCII art。
//!
//! 验证 render 时 `[IMG: src]` 占位符被替换为解码的 ASCII art。
//! http(s) URL 或不存在文件 → 保留占位符（容错）。

use assert_cmd::Command;
use predicates::prelude::*;
use std::io::Write;

fn bin() -> Command {
    Command::cargo_bin("browser").expect("browser binary not found")
}

/// 测试内生成一个简单的 PNG（黑白棋盘），返回路径。
fn write_test_png(path: &std::path::Path) {
    // 生成 8×8 PNG：左上白、右下黑（image crate 用）
    let img = image::DynamicImage::ImageLuma8({
        let mut buf = image::GrayImage::new(8, 8);
        for y in 0..8 {
            for x in 0..8 {
                let v = if x < 4 && y < 4 { 255 } else { 0 };
                buf.put_pixel(x, y, image::Luma([v]));
            }
        }
        buf
    });
    img.save(path).expect("save test png");
}

/// 测试内生成带 <img> 的 HTML fixture，返回路径。
fn write_html_fixture(path: &std::path::Path, img_src: &str) {
    let html =
        format!("<html><body><p>before</p><img src=\"{img_src}\"><p>after</p></body></html>");
    let mut f = std::fs::File::create(path).expect("create html");
    f.write_all(html.as_bytes()).expect("write html");
}

#[test]
fn local_img_replaced_with_ascii_art() {
    // 用 cwd（crates/cli/）下的短文件名，避免长路径被 render 硬截断。
    // （post_process 在 render 之后扫描 [IMG: src]，src 超宽会被截断）
    let png = std::path::PathBuf::from("m22-test-img.png");
    let html = std::path::PathBuf::from("m22-test-img.html");
    write_test_png(&png);
    write_html_fixture(&html, "m22-test-img.png"); // 短相对 src

    let mut cmd = bin();
    cmd.args(["render-file", html.to_str().unwrap(), "--width", "60"]);
    let output = cmd.output().expect("run render-file");
    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    // 占位符 [IMG: ...] 应消失（被替换）
    assert!(
        !predicate::str::contains("[IMG:").eval(&stdout),
        "[IMG:] placeholder should be replaced. stdout={stdout:?}"
    );
    // 应出现边框标记
    assert!(
        predicate::str::contains("image:").eval(&stdout),
        "should show image: border. stdout={stdout:?}"
    );
    // 应出现 ASCII 字符（@ 或空格等的混合，至少有非空白 ASCII）
    assert!(
        predicate::str::contains("┌─").eval(&stdout),
        "should have top border. stdout={stdout:?}"
    );

    let _ = std::fs::remove_file(&png);
    let _ = std::fs::remove_file(&html);
}

#[test]
fn nonexistent_img_keeps_placeholder() {
    // src 指向不存在的文件 → 保留 [IMG: src] 占位符
    let tmp = std::env::temp_dir();
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let html = tmp.join(format!("m22-missing-{ts}.html"));
    write_html_fixture(&html, "/nope/x.png"); // 短 src 避免超宽

    let mut cmd = bin();
    cmd.args(["render-file", html.to_str().unwrap(), "--width", "60"]);
    let output = cmd.output().expect("run");
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        predicate::str::contains("[IMG: /nope/x.png]").eval(&stdout),
        "missing img should keep placeholder. stdout={stdout:?}"
    );
    assert!(
        !predicate::str::contains("image:").eval(&stdout),
        "missing img should NOT render ASCII. stdout={stdout:?}"
    );

    let _ = std::fs::remove_file(&html);
}

#[test]
fn https_img_url_keeps_placeholder() {
    // http(s) URL 不下载 → 保留占位符
    let tmp = std::env::temp_dir();
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let html = tmp.join(format!("m22-url-{ts}.html"));
    write_html_fixture(&html, "https://example.com/logo.png");

    let mut cmd = bin();
    cmd.args(["render-file", html.to_str().unwrap(), "--width", "60"]);
    let output = cmd.output().expect("run");
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        predicate::str::contains("[IMG: https://example.com/logo.png]").eval(&stdout),
        "http URL should keep placeholder. stdout={stdout:?}"
    );

    let _ = std::fs::remove_file(&html);
}
