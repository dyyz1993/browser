//! Integration tests for `browser-net`.
//!
//! Uses `wiremock` to spin up a mock HTTP server in-process; no real
//! network is touched here. Real-network smoke testing happens in
//! `examples/fetch_url.rs` (run manually).

use browser_net::{get, HttpClient, NetError};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn test_get_returns_body() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/hello"))
        .respond_with(ResponseTemplate::new(200).set_body_string("hello"))
        .mount(&server)
        .await;

    let url = format!("{}/hello", server.uri());
    let body = get(&url).await.expect("GET should succeed");
    assert_eq!(body, b"hello");
}

#[tokio::test]
async fn test_get_404_returns_err() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/missing"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;

    let url = format!("{}/missing", server.uri());
    let result = get(&url).await;
    assert!(matches!(result, Err(NetError::BadStatus { code: 404 })));
}

#[tokio::test]
async fn test_get_preserves_binary_body() {
    let server = MockServer::start().await;
    let body = vec![0u8, 1, 2, 255, 254, 0, 42];
    Mock::given(method("GET"))
        .and(path("/bin"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(body.clone()))
        .mount(&server)
        .await;

    let url = format!("{}/bin", server.uri());
    let got = get(&url).await.expect("GET should succeed");
    assert_eq!(got, body);
}

// --- M15.2: Cookie 集成测试 ---

#[tokio::test]
async fn test_get_with_cookie_header_sends_it() {
    let server = MockServer::start().await;
    // 只匹配带 Cookie: session=abc 的请求。
    Mock::given(method("GET"))
        .and(path("/secure"))
        .and(header("cookie", "session=abc"))
        .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
        .mount(&server)
        .await;

    let url = format!("{}/secure", server.uri());
    let client = HttpClient::new();
    let (body, _headers) = client
        .get_with_headers(&url, Some("session=abc"))
        .await
        .expect("GET with cookie should match");
    assert_eq!(body, b"ok");
}

#[tokio::test]
async fn test_get_returns_set_cookie_header() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/login"))
        .respond_with(
            ResponseTemplate::new(200).append_header("set-cookie", "SID=xyz123; Path=/; HttpOnly"),
        )
        .mount(&server)
        .await;

    let url = format!("{}/login", server.uri());
    let client = HttpClient::new();
    let (_body, headers) = client
        .get_with_headers(&url, None)
        .await
        .expect("GET should succeed");
    // 验证响应头里的 Set-Cookie 被返回给上层解析。
    let set_cookie = headers
        .get("set-cookie")
        .expect("Set-Cookie header should be present")
        .to_str()
        .unwrap();
    assert!(set_cookie.contains("SID=xyz123"));
}

#[tokio::test]
async fn test_get_without_cookie_header_still_works() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/plain"))
        .respond_with(ResponseTemplate::new(200).set_body_string("hi"))
        .mount(&server)
        .await;

    let url = format!("{}/plain", server.uri());
    let client = HttpClient::new();
    let (body, _headers) = client
        .get_with_headers(&url, None)
        .await
        .expect("GET should succeed");
    assert_eq!(body, b"hi");
}
