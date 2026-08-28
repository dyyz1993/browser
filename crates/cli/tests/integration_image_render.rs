//! M22.2 e2e: `<img src>` 本地图像渲染成 ASCII art。
//!
//! 验证 render 时 `[IMG: src]` 占位符被替换为解码的 ASCII art。
//! M72 噪声治理：http(s)/data-URI/不存在的文件 → 占位符被丢弃或输出
//! 紧凑 `[IMG w×h]`，URL 不再进输出（旧行为是保留 `[IMG: src]` 噪声）。

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
fn nonexistent_img_drops_placeholder() {
    // M72 噪声治理：src 指向不存在的文件 → 占位符被丢弃（不再回显路径）。
    let tmp = std::env::temp_dir();
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let html = tmp.join(format!("m22-missing-{ts}.html"));
    write_html_fixture(&html, "/nope/x.png");

    let mut cmd = bin();
    cmd.args(["render-file", html.to_str().unwrap(), "--width", "60"]);
    let output = cmd.output().expect("run");
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !predicate::str::contains("[IMG").eval(&stdout),
        "missing img should NOT leak placeholder. stdout={stdout:?}"
    );
    assert!(
        !predicate::str::contains("/nope/x.png").eval(&stdout),
        "missing img path should NOT appear in output. stdout={stdout:?}"
    );
    assert!(
        !predicate::str::contains("image:").eval(&stdout),
        "missing img should NOT render ASCII. stdout={stdout:?}"
    );
    // 正文不受影响。
    assert!(
        predicate::str::contains("before").eval(&stdout) && stdout.contains("after"),
        "surrounding text should survive. stdout={stdout:?}"
    );

    let _ = std::fs::remove_file(&html);
}

#[test]
fn https_img_url_is_dropped() {
    // M72 噪声治理：http(s) URL 不下载 → 不输出 URL 噪声（旧行为是
    // 保留 `[IMG: https://...]`，截图里淹没正文）。
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
        !predicate::str::contains("[IMG").eval(&stdout),
        "http URL img should NOT leak placeholder. stdout={stdout:?}"
    );
    assert!(
        !predicate::str::contains("example.com/logo.png").eval(&stdout),
        "http URL should NOT appear in output. stdout={stdout:?}"
    );

    let _ = std::fs::remove_file(&html);
}

#[test]
fn data_uri_img_is_skipped_entirely() {
    // M72 噪声治理：data-URI img 一律跳过（超长、零信息量）。
    let tmp = std::env::temp_dir();
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let html = tmp.join(format!("m22-data-{ts}.html"));
    let mut f = std::fs::File::create(&html).expect("create html");
    f.write_all(
        b"<html><body><p>keep</p><img src=\"data:image/svg+xml,%3Csvg%3E%3C/svg%3E\"><p>me</p></body></html>",
    )
    .expect("write html");
    drop(f);

    let mut cmd = bin();
    cmd.args(["render-file", html.to_str().unwrap(), "--width", "60"]);
    let output = cmd.output().expect("run");
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !predicate::str::contains("[IMG").eval(&stdout) && !stdout.contains("data:image"),
        "data-URI should be skipped. stdout={stdout:?}"
    );
    assert!(
        predicate::str::contains("keep").eval(&stdout) && stdout.contains("me"),
        "surrounding text should survive. stdout={stdout:?}"
    );

    let _ = std::fs::remove_file(&html);
}

#[test]
fn sized_remote_img_shows_compact_placeholder() {
    // M72：带 width/height 的远程 img → 紧凑占位 [IMG w×h]，无 URL。
    let tmp = std::env::temp_dir();
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let html = tmp.join(format!("m22-sized-{ts}.html"));
    let mut f = std::fs::File::create(&html).expect("create html");
    f.write_all(
        b"<html><body><img src=\"https://cdn.example.com/hero.png\" width=\"320\" height=\"240\"></body></html>",
    )
    .expect("write html");
    drop(f);

    let mut cmd = bin();
    cmd.args(["render-file", html.to_str().unwrap(), "--width", "60"]);
    let output = cmd.output().expect("run");
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        predicate::str::contains("[IMG 320x240]").eval(&stdout),
        "sized remote img should show compact placeholder. stdout={stdout:?}"
    );
    assert!(
        !predicate::str::contains("cdn.example.com").eval(&stdout),
        "URL must not leak. stdout={stdout:?}"
    );

    let _ = std::fs::remove_file(&html);
}
