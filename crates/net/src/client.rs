//! HTTP client based on `reqwest` (default-tls = native-tls = 系统 TLS 库).
//!
//! M1.1: HTTPS GET only (hyper-rustls).
//! M15.2: GET + Cookie header / Set-Cookie 返回。
//! M20.1: 通用 `request`（任意 method + body），POST/PUT 支持。
//! M24.2: **TLS 后端切换** hyper-rustls(ring) → reqwest(native-tls)。
//!
//! ## M24 切换根因
//! hyper-rustls 的 ring provider 与百度/腾讯等中国大站 CDN 的 TLS 实现不兼容
//! （握手时收到 `AlertReceived(ProtocolVersion)`）。而系统 TLS 库（native-tls，
//! curl/wget 同款）能正常连接。连真实站点是 GOALS.md 最高优先级（G1 爬虫），
//! 高于"纯 Rust 依赖 + webpki-roots 可移植"原则。
//! 详见 `docs/decisions/0003-tls-backend-native-tls.md`。

use reqwest::header::HeaderMap;
use reqwest::Method;
use url::Url;

use crate::error::NetError;
use std::sync::Arc;
use std::time::Duration;

use crate::interceptor::{Interceptor, NoopInterceptor, RequestContext, ResponseContext};

/// 真实 Chrome UA（解决反爬 + 模拟浏览器行为）。
const UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36";

/// HTTP client. Cheap to clone (reqwest internally `Arc`-wrapped).
#[derive(Clone)]
pub struct HttpClient {
    inner: reqwest::Client,
    interceptor: Arc<dyn Interceptor + Send + Sync>,
}

/// Builder pattern for configuring HttpClient.
#[derive(Default)]
pub struct ClientBuilder {
    user_agent: Option<String>,
    connect_timeout: Duration,
    timeout: Duration,
    redirect_limit: usize,
    interceptor: Option<Arc<dyn Interceptor + Send + Sync>>,
}

impl ClientBuilder {
    /// Create a new builder with default settings.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set custom user agent.
    pub fn user_agent<S: Into<String>>(mut self, ua: S) -> Self {
        self.user_agent = Some(ua.into());
        self
    }

    /// Set connect timeout (default 10s).
    pub fn connect_timeout(mut self, dur: Duration) -> Self {
        self.connect_timeout = dur;
        self
    }

    /// Set overall request timeout (default 30s).
    pub fn timeout(mut self, dur: Duration) -> Self {
        self.timeout = dur;
        self
    }

    /// Set redirect limit (default 10).
    pub fn redirect_limit(mut self, limit: usize) -> Self {
        self.redirect_limit = limit;
        self
    }

    /// Set network interceptor (for request/response modification).
    pub fn interceptor(mut self, interceptor: Arc<dyn Interceptor + Send + Sync>) -> Self {
        self.interceptor = Some(interceptor);
        self
    }

    /// Build the HttpClient.
    ///
    /// # Errors
    /// Returns error if reqwest client building fails.
    pub fn build(self) -> Result<HttpClient, reqwest::Error> {
        let mut builder = reqwest::Client::builder()
            .connect_timeout(self.connect_timeout)
            .timeout(self.timeout)
            .redirect(reqwest::redirect::Policy::limited(self.redirect_limit));

        if let Some(ua) = self.user_agent {
            builder = builder.user_agent(&ua);
        } else {
            builder = builder.user_agent(UA);
        }

        let interceptor = self.interceptor.unwrap_or_else(|| Arc::new(NoopInterceptor));
        let inner = builder.build()?;
        Ok(HttpClient { inner, interceptor })
    }
}



impl HttpClient {
    /// Create a new client with default settings.
    ///
    /// M40: 强制 timeout（connect 10s + overall 30s）。之前无 timeout 导致
    /// 慢响应/挂起服务器（如某些 CDN）让整个爬虫永久 hang——对 G1 爬虫
    /// 场景是致命可靠性缺陷。
    #[must_use]
    pub fn new() -> Self {
        // 跟随 redirect（浏览器标准行为，最多 10 次防死循环）。
        let inner = reqwest::Client::builder()
            .user_agent(UA)
            .redirect(reqwest::redirect::Policy::limited(10))
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            inner,
            interceptor: Arc::new(NoopInterceptor),
        }
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
    /// M15.2: Cookie jar 集成点。
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

    /// Issue a POST request with a request body and optional Content-Type. (M20.1)
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

    /// Issue a PUT request with a request body and optional Content-Type. (M20.1)
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
    /// (status discarded — use [`request_full`] for status)
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

    /// Like [`request`] but also returns the real HTTP status code. (M20.3)
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
        let mut builder = self.inner.request(method, url);
        if let Some(cookie) = cookie_header {
            builder = builder.header("cookie", cookie);
        }
        if let Some(ct) = content_type {
            builder = builder.header("content-type", ct);
        }
        if let Some(b) = body {
            builder = builder.body(b.to_string());
        }
        let resp = builder
            .send()
            .await
            .map_err(|e| NetError::RequestFailed(e.to_string()))?;
        let status = resp.status().as_u16();
        if !resp.status().is_success() {
            return Err(NetError::BadStatus { code: status });
        }
        let headers = resp.headers().clone();
        let body = resp
            .bytes()
            .await
            .map_err(|e| NetError::ReadFailed(e.to_string()))?
            .to_vec();
        Ok((status, body, headers))
    }

    /// Like [`request_full`] but takes method as `&str`（避免上层依赖
    /// `reqwest::Method`）。method 不区分大小写：GET/POST/PUT/DELETE 等。
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
