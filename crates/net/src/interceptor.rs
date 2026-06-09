// Request/response interception framework for browser-net crate
// M58.1: Define Interceptor trait and contexts


use async_trait::async_trait;
use reqwest::{StatusCode, header::HeaderMap};
use url::Url;

/// Request context passed to interceptor before sending
#[derive(Clone, Debug)]
pub struct RequestContext {
    /// Target URL
    pub url: Url,
    /// HTTP method
    pub method: String,
    /// Request headers (mutable for adding custom headers)
    pub headers: HeaderMap,
    /// Whether to block this request (e.g., ad-blocking)
    pub block: bool,
    /// Custom mock response (Some() = override network request)
    pub mock_response: Option<MockResponse>,
}

/// Response context passed to interceptor after receiving
#[derive(Clone, Debug)]
pub struct ResponseContext {
    /// Request URL
    pub url: Url,
    /// Response status code
    pub status: StatusCode,
    /// Response headers
    pub headers: HeaderMap,
    /// Response body bytes
    pub body: Vec<u8>,
    /// Whether to modify the response (e.g., rewrite HTML)
    pub rewrite: bool,
}

/// Mock response for overriding network requests
#[derive(Clone, Debug)]
pub struct MockResponse {
    /// Status code
    pub status: StatusCode,
    /// Response headers
    pub headers: HeaderMap,
    /// Response body bytes
    pub body: Vec<u8>,
}

impl MockResponse {
    /// Create a JSON mock response
    pub fn json(status: u16, body: &str) -> Self {
        let mut headers = HeaderMap::new();
        headers.insert("content-type", "application/json".parse().unwrap());

        Self {
            status: StatusCode::from_u16(status).unwrap(),
            headers,
            body: body.as_bytes().to_vec(),
        }
    }

    /// Create an HTML mock response
    pub fn html(body: &str) -> Self {
        let mut headers = HeaderMap::new();
        headers.insert("content-type", "text/html; charset=utf-8".parse().unwrap());

        Self {
            status: StatusCode::OK,
            headers,
            body: body.as_bytes().to_vec(),
        }
    }

    /// Create a text mock response
    pub fn text(body: &str) -> Self {
        let mut headers = HeaderMap::new();
        headers.insert("content-type", "text/plain; charset=utf-8".parse().unwrap());

        Self {
            status: StatusCode::OK,
            headers,
            body: body.as_bytes().to_vec(),
        }
    }
}

/// Interceptor trait for customizing requests/responses
#[async_trait]
pub trait Interceptor: Send + Sync {
    /// Intercept request before sending.
    /// Return modified RequestContext.
    async fn intercept_request(&self, ctx: RequestContext) -> RequestContext;

    /// Intercept response after receiving.
    /// Return modified ResponseContext (e.g., rewrite body).
    async fn intercept_response(&self, ctx: ResponseContext) -> ResponseContext;
}

/// No-op interceptor for zero-cost when no interception needed
pub struct NoopInterceptor;

#[async_trait]
impl Interceptor for NoopInterceptor {
    async fn intercept_request(&self, ctx: RequestContext) -> RequestContext {
        ctx
    }

    async fn intercept_response(&self, ctx: ResponseContext) -> ResponseContext {
        ctx
    }
}

/// Logging interceptor for debugging
#[derive(Clone)]
pub struct LoggingInterceptor {
    log_prefix: String,
}

impl LoggingInterceptor {
    /// Create new logging interceptor with prefix
    pub fn new(prefix: &str) -> Self {
        Self {
            log_prefix: prefix.to_string(),
        }
    }
}

#[async_trait]
impl Interceptor for LoggingInterceptor {
    async fn intercept_request(&self, ctx: RequestContext) -> RequestContext {
        eprintln!("[{} REQ] {} {}", self.log_prefix, ctx.method, ctx.url);
        ctx
    }

    async fn intercept_response(&self, ctx: ResponseContext) -> ResponseContext {
        eprintln!("[{} RES] {} {} ({} bytes)", self.log_prefix, ctx.url, ctx.status.as_u16(), ctx.body.len());
        ctx
    }
}

/// Block interceptor for filtering requests (e.g., ad-blocking, telemetry)
#[derive(Clone)]
pub struct BlockInterceptor {
    /// Blocked URL patterns (exact match)
    blocked_exact: Vec<String>,
    /// Blocked URL patterns (substring match)
    blocked_substring: Vec<String>,
    /// Blocked domains
    blocked_domains: Vec<String>,
}

impl BlockInterceptor {
    #[allow(clippy::new_without_default)]
    /// Create new block interceptor
    pub fn new() -> Self {
        Self {
            blocked_exact: Vec::new(),
            blocked_substring: Vec::new(),
            blocked_domains: Vec::new(),
        }
    }

    /// Block exact URL match
    pub fn block_exact(mut self, url: &str) -> Self {
        self.blocked_exact.push(url.to_string());
        self
    }

    /// Block URLs containing substring
    pub fn block_substring(mut self, pattern: &str) -> Self {
        self.blocked_substring.push(pattern.to_string());
        self
    }

    /// Block entire domain
    pub fn block_domain(mut self, domain: &str) -> Self {
        self.blocked_domains.push(domain.to_string());
        self
    }

    /// Check if URL should be blocked
    fn should_block(&self, url: &Url) -> bool {
        let url_str = url.as_str();

        // Exact match
        if self.blocked_exact.iter().any(|b| url_str == b) {
            return true;
        }

        // Substring match
        if self.blocked_substring.iter().any(|b| url_str.contains(b)) {
            return true;
        }

        // Domain match
        if let Some(host) = url.host_str() {
            if self.blocked_domains.iter().any(|d| host == d || host.ends_with(&format!(".{}", d))) {
                return true;
            }
        }

        false
    }
}

#[async_trait]
impl Interceptor for BlockInterceptor {
    async fn intercept_request(&self, mut ctx: RequestContext) -> RequestContext {
        ctx.block = self.should_block(&ctx.url);
        ctx
    }

    async fn intercept_response(&self, ctx: ResponseContext) -> ResponseContext {
        ctx
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mock_response_json() {
        let mock = MockResponse::json(200, r#"{"status":"ok"}"#);
        assert_eq!(mock.status, StatusCode::OK);
        assert_eq!(mock.headers.get("content-type").unwrap(), "application/json");
        assert_eq!(String::from_utf8_lossy(&mock.body), r#"{"status":"ok"}"#);
    }

    #[test]
    fn test_mock_response_html() {
        let mock = MockResponse::html("<html><body>Test</body></html>");
        assert_eq!(mock.status, StatusCode::OK);
        assert_eq!(mock.headers.get("content-type").unwrap(), "text/html; charset=utf-8");
        assert_eq!(String::from_utf8_lossy(&mock.body), "<html><body>Test</body></html>");
    }

    #[test]
    fn test_mock_response_text() {
        let mock = MockResponse::text("Hello, world!");
        assert_eq!(mock.status, StatusCode::OK);
        assert_eq!(mock.headers.get("content-type").unwrap(), "text/plain; charset=utf-8");
        assert_eq!(String::from_utf8_lossy(&mock.body), "Hello, world!");
    }

    #[test]
    fn test_block_interceptor_exact() {
        let interceptor = BlockInterceptor::new()
            .block_exact("https://example.com/tracker.js");

        let ctx = RequestContext {
            url: Url::parse("https://example.com/tracker.js").unwrap(),
            method: "GET".to_string(),
            headers: HeaderMap::new(),
            block: false,
            mock_response: None,
        };

        let ctx = futures::executor::block_on(interceptor.intercept_request(ctx));
        assert!(ctx.block);
    }

    #[test]
    fn test_block_interceptor_substring() {
        let interceptor = BlockInterceptor::new()
            .block_substring("analytics")
            .block_substring("telemetry");

        let ctx = RequestContext {
            url: Url::parse("https://cdn.example.com/analytics/v1.js").unwrap(),
            method: "GET".to_string(),
            headers: HeaderMap::new(),
            block: false,
            mock_response: None,
        };

        let ctx = futures::executor::block_on(interceptor.intercept_request(ctx));
        assert!(ctx.block);
    }

    #[test]
    fn test_block_interceptor_domain() {
        let interceptor = BlockInterceptor::new()
            .block_domain("analytics.example.com")
            .block_domain("ads.example.org");

        let ctx1 = RequestContext {
            url: Url::parse("https://analytics.example.com/collect").unwrap(),
            method: "GET".to_string(),
            headers: HeaderMap::new(),
            block: false,
            mock_response: None,
        };

        let ctx1 = futures::executor::block_on(interceptor.intercept_request(ctx1));
        assert!(ctx1.block);

        let ctx2 = RequestContext {
            url: Url::parse("https://sub.analytics.example.com/track").unwrap(),
            method: "GET".to_string(),
            headers: HeaderMap::new(),
            block: false,
            mock_response: None,
        };

        let ctx2 = futures::executor::block_on(interceptor.intercept_request(ctx2));
        assert!(ctx2.block);
    }

    #[test]
    fn test_block_interceptor_no_match() {
        let interceptor = BlockInterceptor::new()
            .block_exact("https://blocked.com/bad.js");

        let ctx = RequestContext {
            url: Url::parse("https://example.com/good.js").unwrap(),
            method: "GET".to_string(),
            headers: HeaderMap::new(),
            block: false,
            mock_response: None,
        };

        let ctx = futures::executor::block_on(interceptor.intercept_request(ctx));
        assert!(!ctx.block);
    }
}