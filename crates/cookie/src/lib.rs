//! M15: Cookie jar（跨请求会话状态）。
//!
//! 解决 SPA 爬虫的真实差距：很多站点（百度等）依赖 Cookie 维持会话
//! （反爬 token、登录态、A/B 测试分组）。没有 Cookie jar，每次请求
//! 都像新访客，会被反爬机制拒绝。
//!
//! 实现：
//! - `Cookie`：单条 cookie（name/value/domain/path/...）
//! - `CookieStore`：内存中的 cookie 存储
//! - `cookies_for(url)`：返回匹配 URL 的所有 cookie（按 domain/path 过滤）
//! - `store_set_cookie(header, request_url)`：解析 Set-Cookie 响应头并存入
//! - `to_cookie_header(url)`：生成发请求时要带的 Cookie 头值
//!
//! 简化（MVP 爬虫够用）：
//! - 不做过期清理（Expires/Max-Age 暂存但不主动删，运行期短）
//! - domain 匹配用简单的 suffix + 前导点规则（RFC 6265 §5.1.3 子集）
//! - path 匹配用前缀（RFC 6265 §5.1.4 子集）
//! - 不做 Secure/HttpOnly/SameSite 强制（爬虫无安全沙箱）
//!
//! 参考：RFC 6265 HTTP State Management Mechanism。

use std::cell::RefCell;
use std::rc::Rc;
use url::Url;

/// Cookie jar 共享句柄。同 boa bridge / storage 的 Rc<RefCell> 模式。
pub type CookieHandle = Rc<RefCell<CookieStore>>;

/// 单条 cookie。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cookie {
    pub name: String,
    pub value: String,
    /// 设置该 cookie 的源域名（host-only 时无前导点）。
    pub domain: String,
    pub path: String,
    pub secure: bool,
    pub http_only: bool,
}

/// Cookie 存储（无序 HashMap，查询时按 domain/path 过滤）。
pub struct CookieStore {
    cookies: Vec<Cookie>,
}

/// 创建空的 cookie jar。
#[must_use]
pub fn new_cookie_jar() -> CookieHandle {
    Rc::new(RefCell::new(CookieStore { cookies: vec![] }))
}

impl CookieStore {
    /// 当前 cookie 总数（含同域多份）。
    pub fn len(&self) -> usize {
        self.cookies.len()
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.cookies.is_empty()
    }

    /// 清空（logout 场景）。
    pub fn clear(&mut self) {
        self.cookies.clear();
    }

    /// 直接插入一条已构造好的 cookie（测试/手动设置用）。
    pub fn insert(&mut self, cookie: Cookie) {
        // 同 name+domain+path 视为同一 cookie，覆盖旧值（RFC 6265 §5.3 step 11）。
        if let Some(existing) = self
            .cookies
            .iter_mut()
            .find(|c| c.name == cookie.name && c.domain == cookie.domain && c.path == cookie.path)
        {
            *existing = cookie;
        } else {
            self.cookies.push(cookie);
        }
    }

    /// 返回所有匹配 `url` 的 cookie（按 domain + path + secure 过滤）。
    /// 用于生成请求 Cookie 头。
    pub fn matching(&self, url: &Url) -> Vec<&Cookie> {
        let host = url.host_str().unwrap_or("");
        let path = url.path();
        let is_secure = url.scheme() == "https";
        self.cookies
            .iter()
            .filter(|c| domain_matches(host, &c.domain) && path_matches(path, &c.path))
            .filter(|c| !c.secure || is_secure)
            .collect()
    }

    /// 解析 Set-Cookie 头并存入 jar。
    /// `request_url` 是触发该响应的请求 URL（用于默认 domain/path）。
    pub fn store_set_cookie(&mut self, header: &str, request_url: &Url) {
        if let Some(cookie) = parse_set_cookie(header, request_url) {
            self.insert(cookie);
        }
    }

    /// 生成请求 Cookie 头值（`name1=val1; name2=val2`）。
    /// 无匹配时返回空串。
    pub fn to_cookie_header(&self, url: &Url) -> String {
        let cookies = self.matching(url);
        if cookies.is_empty() {
            return String::new();
        }
        cookies
            .iter()
            .map(|c| format!("{}={}", c.name, c.value))
            .collect::<Vec<_>>()
            .join("; ")
    }
}

/// domain 匹配（RFC 6265 §5.1.3 子集）。
///
/// 规则：
/// - domain 无前导点：精确匹配或 host 是 domain 的子域
/// - domain 有前导点（`.example.com`）：host 是 domain 子域
fn domain_matches(host: &str, domain: &str) -> bool {
    if host.is_empty() || domain.is_empty() {
        return false;
    }
    let domain = domain.to_lowercase();
    let host = host.to_lowercase();
    let domain_trimmed = domain.trim_start_matches('.');
    if host == domain_trimmed {
        return true;
    }
    // host 必须以 .domain 结尾（子域）
    host.ends_with(&format!(".{domain_trimmed}"))
}

/// path 匹配（RFC 6265 §5.1.4 子集）。
///
/// 规则：request path 与 cookie path 字符串相同，或以 cookie path + "/" 开头。
fn path_matches(request_path: &str, cookie_path: &str) -> bool {
    if cookie_path.is_empty() || cookie_path == "/" {
        return true;
    }
    if request_path == cookie_path {
        return true;
    }
    request_path.starts_with(cookie_path)
        && (cookie_path.ends_with('/')
            || request_path.as_bytes().get(cookie_path.len()) == Some(&b'/'))
}

/// 解析单个 Set-Cookie 头（RFC 6265 §5.2 子集）。
///
/// 示例输入：`SID=abc123; Path=/; Domain=example.com; Secure; HttpOnly`
fn parse_set_cookie(header: &str, request_url: &Url) -> Option<Cookie> {
    let mut parts = header.split(';');
    // 第一段是 name=value。
    let nv = parts.next()?.trim();
    let eq = nv.find('=')?;
    let name = nv[..eq].trim().to_string();
    let value = nv[eq + 1..].trim().to_string();
    if name.is_empty() {
        return None;
    }

    let default_domain = request_url.host_str().unwrap_or("").to_string();
    let default_path = default_path(request_url.path());

    let mut cookie = Cookie {
        name,
        value,
        domain: default_domain,
        path: default_path,
        secure: false,
        http_only: false,
    };

    for attr in parts {
        let attr = attr.trim();
        let (key, val) = match attr.find('=') {
            Some(i) => (
                attr[..i].trim().to_lowercase(),
                attr[i + 1..].trim().to_string(),
            ),
            None => (attr.to_lowercase(), String::new()),
        };
        match key.as_str() {
            "domain" => {
                // RFC 6265 §5.2.3：前导点被忽略（host-only flag 由默认值决定）。
                let d = val.trim_start_matches('.').to_lowercase();
                if !d.is_empty() {
                    cookie.domain = d;
                }
            }
            "path" => {
                if val.starts_with('/') {
                    cookie.path = val;
                }
            }
            "secure" => cookie.secure = true,
            "httponly" => cookie.http_only = true,
            _ => {} // Expires/MaxAge/SameSite 暂忽略（MVP）
        }
    }

    Some(cookie)
}

/// 默认 path（RFC 6265 §5.1.4）。
fn default_path(request_path: &str) -> String {
    if !request_path.starts_with('/') {
        return "/".to_string();
    }
    // 去掉最后一个 / 之后的部分。
    match request_path.rfind('/') {
        Some(0) => "/".to_string(),
        Some(i) => request_path[..i].to_string(),
        None => "/".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn empty_jar_has_no_cookies() {
        let jar = new_cookie_jar();
        assert!(jar.borrow().is_empty());
        assert_eq!(jar.borrow().len(), 0);
    }

    #[test]
    fn insert_increments_len() {
        let jar = new_cookie_jar();
        jar.borrow_mut().insert(Cookie {
            name: "k".into(),
            value: "v".into(),
            domain: "example.com".into(),
            path: "/".into(),
            secure: false,
            http_only: false,
        });
        assert_eq!(jar.borrow().len(), 1);
    }

    #[test]
    fn insert_same_name_domain_path_overwrites() {
        let jar = new_cookie_jar();
        let make = |value: &str| Cookie {
            name: "k".into(),
            value: value.into(),
            domain: "example.com".into(),
            path: "/".into(),
            secure: false,
            http_only: false,
        };
        jar.borrow_mut().insert(make("v1"));
        jar.borrow_mut().insert(make("v2"));
        assert_eq!(jar.borrow().len(), 1);
        let u = url("https://example.com/");
        assert_eq!(jar.borrow().to_cookie_header(&u), "k=v2");
    }

    #[test]
    fn matching_filters_by_domain() {
        let jar = new_cookie_jar();
        jar.borrow_mut().insert(Cookie {
            name: "a".into(),
            value: "1".into(),
            domain: "example.com".into(),
            path: "/".into(),
            secure: false,
            http_only: false,
        });
        let u_match = url("https://example.com/");
        let u_other = url("https://other.com/");
        assert_eq!(jar.borrow().matching(&u_match).len(), 1);
        assert_eq!(jar.borrow().matching(&u_other).len(), 0);
    }

    #[test]
    fn domain_matches_subdomain() {
        let jar = new_cookie_jar();
        jar.borrow_mut().insert(Cookie {
            name: "a".into(),
            value: "1".into(),
            domain: "example.com".into(),
            path: "/".into(),
            secure: false,
            http_only: false,
        });
        // 子域也匹配（domain=example.com 对 www.example.com 生效）。
        let sub = url("https://www.example.com/");
        assert_eq!(jar.borrow().matching(&sub).len(), 1);
    }

    #[test]
    fn domain_does_not_cross_boundary() {
        let jar = new_cookie_jar();
        jar.borrow_mut().insert(Cookie {
            name: "a".into(),
            value: "1".into(),
            domain: "example.com".into(),
            path: "/".into(),
            secure: false,
            http_only: false,
        });
        // notexample.com 不是 example.com 的子域。
        let bad = url("https://notexample.com/");
        assert_eq!(jar.borrow().matching(&bad).len(), 0);
    }

    #[test]
    fn path_filters_request() {
        let jar = new_cookie_jar();
        jar.borrow_mut().insert(Cookie {
            name: "a".into(),
            value: "1".into(),
            domain: "example.com".into(),
            path: "/app".into(),
            secure: false,
            http_only: false,
        });
        // /app 和 /app/x 匹配；/other 不匹配。
        assert_eq!(
            jar.borrow().matching(&url("https://example.com/app")).len(),
            1
        );
        assert_eq!(
            jar.borrow()
                .matching(&url("https://example.com/app/x"))
                .len(),
            1
        );
        assert_eq!(
            jar.borrow()
                .matching(&url("https://example.com/other"))
                .len(),
            0
        );
        // 防止 prefix 误匹配：/application 不应匹配 /app。
        assert_eq!(
            jar.borrow()
                .matching(&url("https://example.com/application"))
                .len(),
            0
        );
    }

    #[test]
    fn secure_cookie_only_on_https() {
        let jar = new_cookie_jar();
        jar.borrow_mut().insert(Cookie {
            name: "a".into(),
            value: "1".into(),
            domain: "example.com".into(),
            path: "/".into(),
            secure: true,
            http_only: false,
        });
        assert_eq!(jar.borrow().matching(&url("https://example.com/")).len(), 1);
        assert_eq!(jar.borrow().matching(&url("http://example.com/")).len(), 0);
    }

    #[test]
    fn parse_set_cookie_basic() {
        let u = url("https://example.com/");
        let c = parse_set_cookie("SID=abc123", &u).unwrap();
        assert_eq!(c.name, "SID");
        assert_eq!(c.value, "abc123");
        // 无 Domain 属性时取 request host。
        assert_eq!(c.domain, "example.com");
        assert_eq!(c.path, "/");
        assert!(!c.secure);
        assert!(!c.http_only);
    }

    #[test]
    fn parse_set_cookie_with_attributes() {
        let u = url("https://example.com/app/page");
        let c = parse_set_cookie(
            "token=xyz; Path=/; Domain=example.com; Secure; HttpOnly",
            &u,
        )
        .unwrap();
        assert_eq!(c.name, "token");
        assert_eq!(c.value, "xyz");
        assert_eq!(c.domain, "example.com");
        assert_eq!(c.path, "/");
        assert!(c.secure);
        assert!(c.http_only);
    }

    #[test]
    fn parse_set_cookie_strips_leading_dot_domain() {
        let u = url("https://example.com/");
        let c = parse_set_cookie("k=v; Domain=.example.com", &u).unwrap();
        assert_eq!(c.domain, "example.com");
    }

    #[test]
    fn parse_set_cookie_default_path_strips_last_segment() {
        // 请求 /a/b/c，无 Path 属性 → 默认 path = /a/b。
        let u = url("https://example.com/a/b/c");
        let c = parse_set_cookie("k=v", &u).unwrap();
        assert_eq!(c.path, "/a/b");
    }

    #[test]
    fn store_set_cookie_round_trip() {
        let jar = new_cookie_jar();
        let u = url("https://example.com/");
        jar.borrow_mut().store_set_cookie("SID=abc; Path=/", &u);
        assert_eq!(jar.borrow().len(), 1);
        assert_eq!(jar.borrow().to_cookie_header(&u), "SID=abc");
    }

    #[test]
    fn to_cookie_header_multiple_cookies_joined_by_semicolon() {
        let jar = new_cookie_jar();
        let u = url("https://example.com/");
        jar.borrow_mut().store_set_cookie("a=1; Path=/", &u);
        jar.borrow_mut().store_set_cookie("b=2; Path=/", &u);
        let header = jar.borrow().to_cookie_header(&u);
        // 顺序由插入序决定。
        assert!(header == "a=1; b=2" || header == "b=2; a=1");
    }

    #[test]
    fn to_cookie_header_empty_when_no_match() {
        let jar = new_cookie_jar();
        let u = url("https://example.com/");
        assert_eq!(jar.borrow().to_cookie_header(&u), "");
    }

    #[test]
    fn handle_clones_share_state() {
        let j1 = new_cookie_jar();
        let j2 = Rc::clone(&j1);
        let u = url("https://example.com/");
        j1.borrow_mut().store_set_cookie("k=v; Path=/", &u);
        // 通过 clone 看到同一份数据。
        assert_eq!(j2.borrow().len(), 1);
    }

    #[test]
    fn clear_wipes_all() {
        let jar = new_cookie_jar();
        let u = url("https://example.com/");
        jar.borrow_mut().store_set_cookie("a=1; Path=/", &u);
        jar.borrow_mut().clear();
        assert!(jar.borrow().is_empty());
    }

    // --- 内部 helper 单测 ---

    #[test]
    fn domain_matches_exact_and_subdomain() {
        assert!(domain_matches("example.com", "example.com"));
        assert!(domain_matches("www.example.com", "example.com"));
        assert!(domain_matches("www.example.com", ".example.com"));
    }

    #[test]
    fn domain_matches_rejects_unrelated() {
        assert!(!domain_matches("notexample.com", "example.com"));
        assert!(!domain_matches("example.com", "other.com"));
        assert!(!domain_matches("", "example.com"));
    }

    #[test]
    fn path_matches_root_matches_all() {
        assert!(path_matches("/anything", "/"));
        assert!(path_matches("/", "/"));
    }

    #[test]
    fn path_matches_prefix_with_slash_boundary() {
        assert!(path_matches("/app", "/app"));
        assert!(path_matches("/app/x", "/app"));
        assert!(!path_matches("/application", "/app")); // 边界
    }
}
