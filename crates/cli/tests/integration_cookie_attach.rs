//! M93.6 e2e: CLI 首跳 fetch 携带 cookie jar 的铁证测试。
//!
//! 悬案：带有效 auth cookie（挑战站签发的 7 天 JWT）的 `--cookie-file` 运行
//! `browser fetch`，首页请求仍收到挑战页——无法区分"服务端不认这个 JWT"
//! 还是"我们的首跳请求根本没带这个 cookie"。本文件用 wiremock 钉死机制：
//!
//! 流程 A（跨进程 round-trip，M21.2 全链路）：
//!   1. `/login` mock 返回 `Set-Cookie: auth=token123; Path=/` + 正文 LOGGED-IN。
//!      进程 1 `fetch <base>/login --cookie-file F` → jar 收 cookie → 退出保存。
//!   2. `/protected` mock **只对携带 `Cookie: auth=token123` 的请求**返回
//!      200 + SECRET-BODY（wiremock header matcher 精确匹配，丢 cookie = 404）。
//!      进程 2 `fetch <base>/protected --cookie-file F`。
//!   3. 两个 `browser` 进程之间唯一通道是 cookie 文件——进程 2 拿到
//!      SECRET-BODY 即为铁证：加载 → 首跳 attach → 保存 全链路闭环。
//!
//! 负向对照：同一 /protected mock，不带 `--cookie-file` → 无 cookie → 404
//! → fetch 必须失败且不泄漏受保护正文。

use assert_cmd::Command;
use predicates::prelude::*;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn bin() -> Command {
    Command::cargo_bin("browser").expect("browser binary not found")
}

fn unique_cookie_file(label: &str) -> std::path::PathBuf {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("browser-cookie-attach-{nanos}-{label}.txt"))
}

const LOGIN_PAGE: &str = "<html><body><div>LOGGED-IN</div></body></html>";
const PROTECTED_PAGE: &str = "<html><body><div>SECRET-BODY</div></body></html>";

/// 流程 A：两个独立进程经 cookie 文件 round-trip，第二跳首请求必须带上
/// 第一跳签发的 cookie（首跳 attach 铁证）。
#[tokio::test]
async fn cookie_file_round_trip_attaches_on_first_hop() {
    let server = MockServer::start().await;
    let base = server.uri();

    // /login：签发 cookie（Set-Cookie + 正文 LOGGED-IN）。
    Mock::given(method("GET"))
        .and(path("/login"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(LOGIN_PAGE)
                .insert_header("set-cookie", "auth=token123; Path=/"),
        )
        .mount(&server)
        .await;
    // /protected：只对精确携带 `Cookie: auth=token123` 的请求 200——
    // 首跳丢 cookie = 404 = 测试失败（jar 里只有一个 cookie，无歧义）。
    Mock::given(method("GET"))
        .and(path("/protected"))
        .and(header("cookie", "auth=token123"))
        .respond_with(ResponseTemplate::new(200).set_body_string(PROTECTED_PAGE))
        .mount(&server)
        .await;

    let cookie_file = unique_cookie_file("roundtrip");
    let _ = std::fs::remove_file(&cookie_file);

    // 第 1 跳（进程 1）：/login → Set-Cookie 进 jar → 退出时保存到文件。
    bin()
        .args(["fetch", &format!("{base}/login"), "--cookie-file"])
        .arg(&cookie_file)
        .args(["--no-js", "--format", "text"])
        .assert()
        .success()
        .stdout(predicate::str::contains("LOGGED-IN"));

    // 铁证前置：文件确实落盘（进程 1 → 进程 2 的唯一通道）。
    // wiremock 监听 127.0.0.1，cookie domain 应为 IP host（无端口）。
    let content = std::fs::read_to_string(&cookie_file).expect("cookie file should be saved");
    assert!(
        content.contains("auth\ttoken123\t127.0.0.1\t/"),
        "cookie file should contain auth cookie scoped to 127.0.0.1. content={content:?}"
    );

    // 第 2 跳（进程 2）：/protected 必须带 `Cookie: auth=token123`。
    bin()
        .args(["fetch", &format!("{base}/protected"), "--cookie-file"])
        .arg(&cookie_file)
        .args(["--no-js", "--format", "text"])
        .assert()
        .success()
        .stderr(predicate::str::contains("[cookie] loaded"))
        .stdout(predicate::str::contains("SECRET-BODY"));

    let _ = std::fs::remove_file(&cookie_file);
}

/// 负向对照：不带 `--cookie-file` → 无 cookie → mock 不匹配 → 404
/// → fetch 失败，且 stdout 不含受保护正文（证明 mock 的 cookie 门禁真的生效，
/// 排除"mock 太宽松导致假阳性"）。
#[tokio::test]
async fn without_cookie_file_protected_page_rejected() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/protected"))
        .and(header("cookie", "auth=token123"))
        .respond_with(ResponseTemplate::new(200).set_body_string(PROTECTED_PAGE))
        .mount(&server)
        .await;
    let url = format!("{}/protected", server.uri());

    let output = bin()
        .args(["fetch", &url, "--no-js", "--format", "text"])
        .output()
        .expect("run fetch");
    assert!(
        !output.status.success(),
        "no cookie → 404 → fetch must fail. stdout={}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        !String::from_utf8_lossy(&output.stdout).contains("SECRET-BODY"),
        "must not leak protected body without cookie"
    );
}
