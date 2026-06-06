//! HTTP client based on `hyper` + `hyper-rustls`.
//!
//! M1.1 scope: HTTPS GET only. POST / WebSocket / cookies land in later steps.

use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{header::HeaderMap, Method, Request, Response};
use hyper_rustls::HttpsConnector;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;
use url::Url;

use crate::error::NetError;

/// Build the HTTPS connector. Uses webpki-roots so we ship with the
/// bundle and do not depend on the system trust store — keeps the
/// binary portable across Windows / macOS / Linux.
fn https_connector() -> HttpsConnector<HttpConnector> {
    hyper_rustls::HttpsConnectorBuilder::new()
        .with_webpki_roots()
        .https_or_http()
        .enable_http1()
        .enable_http2()
        .build()
}

/// HTTP client. Cheap to clone (internally `Arc`-wrapped).
#[derive(Clone)]
pub struct HttpClient {
    inner: Client<HttpsConnector<HttpConnector>, Full<Bytes>>,
}

impl Default for HttpClient {
    fn default() -> Self {
        Self::new()
    }
}

impl HttpClient {
    /// Create a new client with default settings.
    #[must_use]
    pub fn new() -> Self {
        let connector = https_connector();
        let inner = Client::builder(TokioExecutor::new())
            .pool_idle_timeout(Some(Duration::from_secs(30)))
            .build(connector);
        Self { inner }
    }

    /// Issue a GET request and return the response body.
    ///
    /// # Errors
    /// Returns [`NetError`] for invalid URL, transport failure,
    /// or non-2xx HTTP status.
    pub async fn get(&self, url: &str) -> Result<Vec<u8>, NetError> {
        let body = self.get_with_headers(url, None).await?;
        Ok(body.0)
    }

    /// Issue a GET request with an optional `Cookie` header, returning
    /// both the response body and all response headers (for `Set-Cookie`).
    ///
    /// M15.2: Cookie jar 集成点。`cookie_header` 为 None 时不发 Cookie 头；
    /// 上层（cli/js-runtime）从返回的 headers 里解析 Set-Cookie 存入 jar。
    ///
    /// # Errors
    /// See [`HttpClient::get`].
    pub async fn get_with_headers(
        &self,
        url: &str,
        cookie_header: Option<&str>,
    ) -> Result<(Vec<u8>, HeaderMap), NetError> {
        let parsed = Url::parse(url).map_err(|_| NetError::InvalidUrl {
            url: url.to_string(),
        })?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(NetError::UnsupportedScheme {
                scheme: parsed.scheme().to_string(),
            });
        }
        // Build the request. `.uri(url)` accepts an &str here because
        // hyper's Uri type can parse a full absolute URL.
        let mut builder = Request::builder()
            .method(Method::GET)
            .uri(url)
            .header("user-agent", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36");
        if let Some(cookie) = cookie_header {
            builder = builder.header("cookie", cookie);
        }
        let req = builder
            .body(Full::default())
            .map_err(|e| NetError::RequestFailed(e.to_string()))?;
        let resp: Response<_> = self
            .inner
            .request(req)
            .await
            .map_err(|e| NetError::RequestFailed(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(NetError::BadStatus {
                code: resp.status().as_u16(),
            });
        }
        let headers = resp.headers().clone();
        let body = resp
            .into_body()
            .collect()
            .await
            .map_err(|e| NetError::ReadFailed(e.to_string()))?
            .to_bytes();
        Ok((body.to_vec(), headers))
    }
}

/// Convenience helper: build a one-shot client and GET a URL.
///
/// # Errors
/// See [`HttpClient::get`].
pub async fn get(url: &str) -> Result<Vec<u8>, NetError> {
    HttpClient::new().get(url).await
}
