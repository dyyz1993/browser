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

use crate::interceptor::{Interceptor, NoopInterceptor};
#[allow(unused_imports)]
use crate::interceptor::{RequestContext, ResponseContext};

/// 真实 Chrome UA（解决反爬 + 模拟浏览器行为）。
const UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/153.0.0.0 Safari/537.36";

/// M93.2: 浏览器级默认请求头——与 UA（Chrome/126, macOS）**身份一致**的
/// 恒定头组。真实浏览器每个请求（导航/script/XHR）都带这四个头；我们此前
/// 只发裸 UA，WAF 请求评分一眼"非浏览器"。
///
/// 纪律边界（宪法原则 4）：这是**补全基础请求能力**——头组与我们声明的
/// UA 完全一致（不伪装成别的浏览器/平台），全部是 Chrome 对任何资源类型
/// 都恒定发送的真值。不做 per-站点指纹定制、不伪造 sec-fetch 场景头。
fn browser_default_headers() -> reqwest::header::HeaderMap {
    let mut h = reqwest::header::HeaderMap::new();
    let ins = |h: &mut reqwest::header::HeaderMap, k: &'static str, v: &'static str| {
        if let (Ok(name), Ok(val)) = (
            reqwest::header::HeaderName::try_from(k),
            reqwest::header::HeaderValue::from_str(v),
        ) {
            h.insert(name, val);
        }
    };
    ins(&mut h, "accept-language", "en-US,en;q=0.9");
    ins(
        &mut h,
        "sec-ch-ua",
        "\"Not/A)Brand\";v=\"8\", \"Chromium\";v=\"126\", \"Google Chrome\";v=\"126\"",
    );
    ins(&mut h, "sec-ch-ua-mobile", "?0");
    ins(&mut h, "sec-ch-ua-platform", "\"macOS\"");
    h
}

/// HTTP client. Cheap to clone (reqwest internally `Arc`-wrapped).
#[derive(Clone)]
#[allow(dead_code)]
pub struct HttpClient {
    /// M93.11: 主客户端（rustls——ALPN 可靠协商 HTTP/2，对齐 Chrome 协议层；
    /// xcancel 等站 WAF 拒 HTTP/1.1）。
    inner: reqwest::Client,
    /// M93.11: native-tls 兜底（ADR-0003：百度等中国大站 CDN 与 rustls/ring
    /// 不兼容，TLS 握手失败时降级重试）。
    alt: Option<reqwest::Client>,
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
    #[allow(clippy::new_without_default)]
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
            .redirect(reqwest::redirect::Policy::limited(self.redirect_limit))
            // M93.2: 浏览器级默认头 + 压缩协商（Accept-Encoding 由 reqwest
            // 依 feature 自动附加并透明解压）。
            .default_headers(browser_default_headers())
            .gzip(true)
            .brotli(true);

        if let Some(ua) = self.user_agent {
            builder = builder.user_agent(&ua);
        } else {
            builder = builder.user_agent(UA);
        }

        let interceptor = self
            .interceptor
            .unwrap_or_else(|| Arc::new(NoopInterceptor));
        let inner = builder.build()?;
        Ok(HttpClient {
            inner,
            alt: None,
            interceptor,
        })
    }
}

impl HttpClient {
    /// Create a new client with default settings.
    ///
    /// M40: 强制 timeout（connect 10s + overall 30s）。之前无 timeout 导致
    /// 慢响应/挂起服务器（如某些 CDN）让整个爬虫永久 hang——对 G1 爬虫
    /// 场景是致命可靠性缺陷。
    #[must_use]
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        // M93.11: rustls 主（h2）+ native-tls 兜底。跟随 redirect（浏览器标准
        // 行为，最多 10 次防死循环）。
        let mk = |rustls: bool| {
            let mut b = reqwest::Client::builder()
                .user_agent(UA)
                .redirect(reqwest::redirect::Policy::limited(10))
                .connect_timeout(std::time::Duration::from_secs(10))
                .timeout(std::time::Duration::from_secs(60))
                .default_headers(browser_default_headers())
                .gzip(true)
                .brotli(true);
            b = if rustls {
                b.use_rustls_tls()
            } else {
                b.use_native_tls()
            };
            b.build().unwrap_or_else(|_| reqwest::Client::new())
        };
        Self {
            inner: mk(true),
            alt: Some(mk(false)),
            interceptor: Arc::new(NoopInterceptor),
        }
    }

    /// M93: 不跟随 redirect 的客户端——JS 导航闭环（`location.replace` 后
    /// 重新 fetch 文档）需要逐跳手动跟 3xx：每跳的 `Set-Cookie` 必须写回
    /// cookie jar 后才能用于下一跳（Anubis pass-challenge 就是
    /// Set-Cookie + 302 回原页）。reqwest 内部自动跟跳不经过我们的 jar，
    /// 中间跳的 cookie 会丢。
    #[must_use]
    #[allow(clippy::new_without_default)]
    pub fn new_no_redirect() -> Self {
        let mk = |rustls: bool| {
            let mut b = reqwest::Client::builder()
                .user_agent(UA)
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(std::time::Duration::from_secs(10))
                .timeout(std::time::Duration::from_secs(60))
                .default_headers(browser_default_headers())
                .gzip(true)
                .brotli(true);
            b = if rustls {
                b.use_rustls_tls()
            } else {
                b.use_native_tls()
            };
            b.build().unwrap_or_else(|_| reqwest::Client::new())
        };
        Self {
            inner: mk(true),
            alt: Some(mk(false)),
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
        let mut builder = self.inner.request(method.clone(), url);
        if let Some(cookie) = cookie_header {
            builder = builder.header("cookie", cookie);
        }
        if let Some(ct) = content_type {
            builder = builder.header("content-type", ct);
        }
        if let Some(b) = body {
            builder = builder.body(b.to_string());
        }
        let resp = match builder.send().await {
            Ok(r) => r,
            Err(primary_err) => {
                if let Some(alt) = &self.alt {
                    if std::env::var("BROWSER_TRACE_FETCH").is_ok() {
                        eprintln!(
                            "[net-diag] rustls primary failed ({primary_err}), retrying native-tls"
                        );
                    }
                    let mut b2 = alt.request(method.clone(), url);
                    if let Some(cookie) = cookie_header {
                        b2 = b2.header("cookie", cookie);
                    }
                    if let Some(ct) = content_type {
                        b2 = b2.header("content-type", ct);
                    }
                    if let Some(b) = body {
                        b2 = b2.body(b.to_string());
                    }
                    b2.send()
                        .await
                        .map_err(|e| NetError::RequestFailed(e.to_string()))?
                } else {
                    return Err(NetError::RequestFailed(primary_err.to_string()));
                }
            }
        };
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

    /// M70.4: Like [`request_full`] but **never errors on non-2xx status**.
    ///
    /// Returns `(status, body, headers)` for ALL HTTP responses (200/301/404/500...).
    /// Only errors on network failures (DNS/TLS/connection). This is what CDP
    /// `Network.responseReceived` needs — Chrome reports 404/500 pages with
    /// their bodies; the old `request_full` dropped body+headers on `BadStatus`.
    ///
    /// # Errors
    /// Only `NetError::InvalidUrl` / `UnsupportedScheme` / `RequestFailed` /
    /// `ReadFailed`. Never `BadStatus`.
    pub async fn request_full_raw(
        &self,
        url: &str,
        method: &str,
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
        let m = match method.to_uppercase().as_str() {
            "POST" => Method::POST,
            "PUT" => Method::PUT,
            "DELETE" => Method::DELETE,
            "HEAD" => Method::HEAD,
            "PATCH" => Method::PATCH,
            _ => Method::GET,
        };
        let mut builder = self.inner.request(m.clone(), url);
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
        let headers = resp.headers().clone();
        let body = resp
            .bytes()
            .await
            .map_err(|e| NetError::ReadFailed(e.to_string()))?
            .to_vec();
        Ok((status, body, headers))
    }

    /// M93.11: request_full_raw + 任意额外请求头（fetch spec 的 headers
    /// 透传——此前 bridge 只抠 Content-Type，VM/框架发的 Authorization 等
    /// 全部被丢弃，xcancel 挑战 POST 因此 "unauthorized" 403）。
    ///
    /// # Errors
    /// 同 [`HttpClient::request_full_raw`]。
    pub async fn request_full_raw_hdr(
        &self,
        url: &str,
        method: &str,
        body: Option<&str>,
        content_type: Option<&str>,
        cookie_header: Option<&str>,
        extra_headers: &[(String, String)],
    ) -> Result<(u16, Vec<u8>, HeaderMap), NetError> {
        // 先走标准路径拿 builder 不行（私有），这里直接重建请求逻辑：
        // 复用 request_full_raw 的语义 + 追加 extra headers。
        // 简洁实现：通过内部调用 + reqwest 的 header 机制不可行，改为
        // 直接构造（与 request_full_raw 相同的校验/解析）。
        let parsed = Url::parse(url).map_err(|_| NetError::InvalidUrl {
            url: url.to_string(),
        })?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(NetError::UnsupportedScheme {
                scheme: parsed.scheme().to_string(),
            });
        }
        let m = match method.to_uppercase().as_str() {
            "POST" => Method::POST,
            "PUT" => Method::PUT,
            "DELETE" => Method::DELETE,
            "HEAD" => Method::HEAD,
            "PATCH" => Method::PATCH,
            _ => Method::GET,
        };
        let mut builder = self.inner.request(m.clone(), url);
        if let Some(cookie) = cookie_header {
            builder = builder.header("cookie", cookie);
        }
        if let Some(ct) = content_type {
            builder = builder.header("content-type", ct);
        }
        for (k, v) in extra_headers {
            if k.eq_ignore_ascii_case("content-type") || k.eq_ignore_ascii_case("cookie") {
                continue; // 已由专用参数处理，避免重复
            }
            if let (Ok(name), Ok(val)) = (
                reqwest::header::HeaderName::try_from(k.as_str()),
                reqwest::header::HeaderValue::from_str(v),
            ) {
                builder = builder.header(name, val);
            }
        }
        if let Some(b) = body {
            builder = builder.body(b.to_string());
        }
        let resp = match builder.send().await {
            Ok(r) => r,
            Err(primary_err) => {
                // M93.11: 传输层失败（rustls 与部分 CDN 不兼容——ADR-0003）
                // → native-tls 兜底重试一次。
                if let Some(alt) = &self.alt {
                    if std::env::var("BROWSER_TRACE_FETCH").is_ok() {
                        eprintln!(
                            "[net-diag] rustls primary failed ({primary_err}), retrying native-tls"
                        );
                    }
                    let mut b2 = alt.request(m.clone(), url);
                    if let Some(cookie) = cookie_header {
                        b2 = b2.header("cookie", cookie);
                    }
                    if let Some(ct) = content_type {
                        b2 = b2.header("content-type", ct);
                    }
                    for (k, v) in extra_headers {
                        if k.eq_ignore_ascii_case("content-type")
                            || k.eq_ignore_ascii_case("cookie")
                        {
                            continue;
                        }
                        if let (Ok(name), Ok(val)) = (
                            reqwest::header::HeaderName::try_from(k.as_str()),
                            reqwest::header::HeaderValue::from_str(v),
                        ) {
                            b2 = b2.header(name, val);
                        }
                    }
                    if let Some(b) = body {
                        b2 = b2.body(b.to_string());
                    }
                    b2.send()
                        .await
                        .map_err(|e| NetError::RequestFailed(e.to_string()))?
                } else {
                    return Err(NetError::RequestFailed(primary_err.to_string()));
                }
            }
        };
        let status = resp.status().as_u16();
        // M93.11-diag: 协议版本诊断（xcancel WAF 拒 HTTP/1.1）
        if std::env::var("BROWSER_TRACE_FETCH").is_ok() {
            eprintln!("[net-diag] {method} {url} -> proto={:?}", resp.version());
        }
        let headers = resp.headers().clone();
        let body = resp
            .bytes()
            .await
            .map_err(|e| NetError::ReadFailed(e.to_string()))?
            .to_vec();
        Ok((status, body, headers))
    }
}

/// Convenience helper: build a one-shot client and GET a URL.
///
/// # Errors
/// See [`HttpClient::get`].
pub async fn get(url: &str) -> Result<Vec<u8>, NetError> {
    HttpClient::new().get(url).await
}
