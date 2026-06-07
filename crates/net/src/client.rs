//! HTTP client based on `hyper` + `hyper-rustls`.
//!
//! M1.1: HTTPS GET only.
//! M15.2: GET + Cookie header / Set-Cookie 返回。
//! M20.1: 通用 `request`（任意 method + body），POST/PUT 支持。

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
        let (_status, body, headers) = self
            .request_full(url, Method::GET, None, None, cookie_header)
            .await?;
        Ok((body, headers))
    }

    /// Issue a POST request with a request body and optional Content-Type.
    ///
    /// M20.1: 表单提交 / API 调用场景。`body` 为空时不发 body（某些 API
    /// 用 POST 不带 body）。
    ///
    /// # Errors
    /// See [`HttpClient::get`].
    pub async fn post(
        &self,
        url: &str,
        body: Option<&str>,
        content_type: Option<&str>,
        cookie_header: Option<&str>,
    ) -> Result<(Vec<u8>, HeaderMap), NetError> {
        self.request(url, Method::POST, body, content_type, cookie_header)
            .await
    }

    /// Issue a PUT request with a request body and optional Content-Type.
    ///
    /// M20.1: REST API 更新场景。
    ///
    /// # Errors
    /// See [`HttpClient::get`].
    pub async fn put(
        &self,
        url: &str,
        body: Option<&str>,
        content_type: Option<&str>,
        cookie_header: Option<&str>,
    ) -> Result<(Vec<u8>, HeaderMap), NetError> {
        self.request(url, Method::PUT, body, content_type, cookie_header)
            .await
    }

    /// Issue a DELETE request. (M20.1)
    ///
    /// # Errors
    /// See [`HttpClient::get`].
    pub async fn delete(
        &self,
        url: &str,
        cookie_header: Option<&str>,
    ) -> Result<(Vec<u8>, HeaderMap), NetError> {
        self.request(url, Method::DELETE, None, None, cookie_header)
            .await
    }

    /// Generic request: any method + optional body + optional Content-Type +
    /// optional Cookie header. Returns body + all response headers.
    ///
    /// M20.1: `get_with_headers` / `post` / `put` / `delete` 的统一后端。
    ///
    /// # Errors
    /// Returns [`NetError`] for invalid URL, unsupported scheme,
    /// transport failure, or non-2xx HTTP status.
    /// Generic request: any method + optional body + optional Content-Type +
    /// optional Cookie header. Returns body + headers (status discarded).
    ///
    /// M20.1: `get_with_headers` / `post` / `put` / `delete` 的统一后端。
    ///
    /// # Errors
    /// Returns [`NetError`] for invalid URL, unsupported scheme,
    /// transport failure, or non-2xx HTTP status.
    pub async fn request(
        &self,
        url: &str,
        method: Method,
        body: Option<&str>,
        content_type: Option<&str>,
        cookie_header: Option<&str>,
    ) -> Result<(Vec<u8>, HeaderMap), NetError> {
        let (_status, body, headers) = self
            .request_full(url, method, body, content_type, cookie_header)
            .await?;
        Ok((body, headers))
    }

    /// M20.3: Like [`request`] but also returns the real HTTP status code.
    /// fetch API 需要真实 status code（201/204 等），不能丢给 is_success()。
    ///
    /// # Errors
    /// See [`HttpClient::get`].
    pub async fn request_full(
        &self,
        url: &str,
        method: Method,
        body: Option<&str>,
        content_type: Option<&str>,
        cookie_header: Option<&str>,
    ) -> Result<(u16, Vec<u8>, HeaderMap), NetError> {
        let parsed = Url::parse(url).map_err(|_| NetError::InvalidUrl {
            url: url.to_string(),
        })?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(NetError::UnsupportedScheme {
                scheme: parsed.scheme().to_string(),
            });
        }
        let mut builder = Request::builder()
            .method(method)
            .uri(url)
            .header(
                "user-agent",
                "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36",
            );
        if let Some(cookie) = cookie_header {
            builder = builder.header("cookie", cookie);
        }
        if let Some(ct) = content_type {
            builder = builder.header("content-type", ct);
        }
        // body：有则包进 Full<Bytes>，无则空 body（GET/DELETE 默认无 body）。
        let req_body = match body {
            Some(b) => Full::from(Bytes::copy_from_slice(b.as_bytes())),
            None => Full::default(),
        };
        let req = builder
            .body(req_body)
            .map_err(|e| NetError::RequestFailed(e.to_string()))?;
        let resp: Response<_> = self
            .inner
            .request(req)
            .await
            .map_err(|e| NetError::RequestFailed(e.to_string()))?;
        let status = resp.status().as_u16();
        if !resp.status().is_success() {
            return Err(NetError::BadStatus { code: status });
        }
        let headers = resp.headers().clone();
        let resp_body = resp
            .into_body()
            .collect()
            .await
            .map_err(|e| NetError::ReadFailed(e.to_string()))?
            .to_bytes();
        Ok((status, resp_body.to_vec(), headers))
    }

    /// M20.3: Like [`request_full`] but takes method as `&str`（避免上层
    /// 依赖 hyper::Method）。method 不区分大小写：GET/POST/PUT/DELETE 等。
    ///
    /// # Errors
    /// See [`HttpClient::get`]. Unknown method → treated as GET.
    pub async fn request_full_str(
        &self,
        url: &str,
        method: &str,
        body: Option<&str>,
        content_type: Option<&str>,
        cookie_header: Option<&str>,
    ) -> Result<(u16, Vec<u8>, HeaderMap), NetError> {
        let m = match method.to_uppercase().as_str() {
            "POST" => Method::POST,
            "PUT" => Method::PUT,
            "DELETE" => Method::DELETE,
            "HEAD" => Method::HEAD,
            "PATCH" => Method::PATCH,
            _ => Method::GET,
        };
        self.request_full(url, m, body, content_type, cookie_header)
            .await
    }
}

/// Convenience helper: build a one-shot client and GET a URL.
///
/// # Errors
/// See [`HttpClient::get`].
pub async fn get(url: &str) -> Result<Vec<u8>, NetError> {
    HttpClient::new().get(url).await
}
