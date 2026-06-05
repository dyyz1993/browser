//! Integration tests for `browser-net`.
//!
//! Uses `wiremock` to spin up a mock HTTP server in-process; no real
//! network is touched here. Real-network smoke testing happens in
//! `examples/fetch_url.rs` (run manually).

use browser_net::{get, NetError};
use wiremock::matchers::{method, path};
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
