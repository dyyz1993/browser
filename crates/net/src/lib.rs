//! `browser-net` — HTTP / HTTPS / WebSocket client.
//!
//! M1.1 scope: HTTPS GET.
//! - [`get`] — one-shot helper
//! - [`HttpClient`] — reusable client (cheap to clone)
//! - [`NetError`] — error enum

#![forbid(unsafe_code)]

pub mod client;
pub mod error;
pub mod interceptor;

pub use client::{get, HttpClient};
pub use error::NetError;
pub use interceptor::{
    Interceptor, NoopInterceptor, LoggingInterceptor, BlockInterceptor,
    RequestContext, ResponseContext, MockResponse
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ping() {
        assert_eq!(env!("CARGO_PKG_NAME"), "browser-net");
    }

    #[tokio::test]
    async fn test_invalid_url_returns_err() {
        let result = get("not a url").await;
        assert!(matches!(result, Err(NetError::InvalidUrl { .. })));
    }

    #[tokio::test]
    async fn test_unsupported_scheme_returns_err() {
        let result = get("file:///etc/passwd").await;
        assert!(matches!(result, Err(NetError::UnsupportedScheme { .. })));
    }
}
