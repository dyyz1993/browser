//! M14.4 e2e: render-script 跑 SPA fixture 验证 history/location。
//!
//! 验证：
//! 1. history.pushState 推入新条目（length +1）
//! 2. history.replaceState 替换当前条目（length 不变）
//! 3. history.back() 回退
//! 4. location.replace() 强制跳转（百度反爬场景）
//! 5. location.pathname() / href() 读取当前 URL

use assert_cmd::Command;

fn render(fixture_name: &str) -> (String, String) {
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(fixture_name);
    let output = Command::cargo_bin("browser")
        .expect("browser binary")
        .args([
            "render-script",
            fixture.to_str().expect("path str"),
            "--width",
            "80",
        ])
        .output()
        .expect("spawn browser");
    (
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

/// 把 stdout 里连续空白（含折行）压成单空格，避免渲染器折行破坏断言。
fn normalize(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[test]
fn navigation_spa_push_state_advances_length_and_href() {
    let (stdout, stderr) = render("navigation-spa.html");
    // pushState 后 length = 2，href = /about
    assert!(
        stdout.contains("AFTER_PUSH_LENGTH: 2"),
        "push should advance length. stdout={stdout}\nstderr={stderr}"
    );
    assert!(
        stdout.contains("AFTER_PUSH_HREF: /about"),
        "push should update href. stdout={stdout}"
    );
}

#[test]
fn navigation_spa_replace_state_keeps_length() {
    let (stdout, _stderr) = render("navigation-spa.html");
    // replaceState 后 length 仍 = 2（替换不增加），href = /about-v2
    assert!(
        stdout.contains("AFTER_REPLACE_LENGTH: 2"),
        "replace should not increase length. stdout={stdout}"
    );
    assert!(
        stdout.contains("AFTER_REPLACE_HREF: /about-v2"),
        "replace should update href. stdout={stdout}"
    );
}

#[test]
fn navigation_spa_back_navigates_to_previous() {
    let (stdout, _stderr) = render("navigation-spa.html");
    // back 回到首页（about:blank 或初始 URL）
    // 注意：初始 URL 由 run_scripts_with_base 设为 "about:blank"（无 base_url）
    assert!(
        normalize(&stdout).contains("AFTER_BACK_HREF: about:blank"),
        "back should navigate to initial url. stdout={stdout}"
    );
}

#[test]
fn navigation_spa_location_replace_forces_redirect() {
    let (stdout, _stderr) = render("navigation-spa.html");
    // location.replace('/redirected') 模拟百度反爬跳转
    assert!(
        stdout.contains("AFTER_LOCATION_REPLACE: /redirected"),
        "location.replace should update href. stdout={stdout}"
    );
}

#[test]
fn navigation_spa_initial_route_pathname_is_blank() {
    let (stdout, _stderr) = render("navigation-spa.html");
    // 无 base_url 时，初始 URL = "about:blank"
    // url crate 按 WHATWG 解析：scheme=about, pathname="blank"
    // （这是 Web 标准行为，不是 bug）
    assert!(
        stdout.contains("INITIAL_ROUTE: blank"),
        "initial pathname should be 'blank' (about:blank parsed). stdout={stdout}"
    );
}
