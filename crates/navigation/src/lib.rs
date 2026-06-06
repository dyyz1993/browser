//! M14: Navigation API（History + Location）。
//!
//! 为 SPA 路由提供基础——React Router / Vue Router 等都依赖
//! `history.pushState` / `popstate` 事件。
//!
//! 实现：
//! - `HistoryStack`：导航栈（entries + current 指针）
//! - `LocationData`：当前 URL 的各部分（href/protocol/host/pathname/...）
//! - MVP：`pushState`/`replaceState` 更新栈，不触发真实 fetch
//!   （SPA 爬虫场景，JS 中 pushState 通常只改 URL 不重新加载）
//!
//! 同时提供 `location.replace()` 支持——百度等强反爬站点会返回
//! `<script>location.replace(...)</script>` 来强制 http 重定向，
//! 我们记录重定向目标，由 cli 层面可选跟随。

use std::cell::RefCell;
use std::rc::Rc;
use url::Url;

/// 历史栈的一条记录。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryEntry {
    pub url: String,
    /// JSON 字符串（boa JsValue 序列化后的）。
    pub state: Option<String>,
}

/// 导航状态共享句柄。同 boa bridge 的 Tree / StorageHandle 模式。
pub type NavigationHandle = Rc<RefCell<NavigationState>>;

/// 导航状态：history 栈 + current 指针。
pub struct NavigationState {
    pub entries: Vec<HistoryEntry>,
    pub current: usize,
}

/// 创建新的导航状态，初始 URL 为 `initial_url`。
#[must_use]
pub fn new_navigation(initial_url: &str) -> NavigationHandle {
    Rc::new(RefCell::new(NavigationState {
        entries: vec![HistoryEntry {
            url: initial_url.to_string(),
            state: None,
        }],
        current: 0,
    }))
}

/// `history.length`
pub fn history_len(handle: &NavigationHandle) -> usize {
    handle.borrow().entries.len()
}

/// `history.state`（当前条目的 state）。
pub fn history_state(handle: &NavigationHandle) -> Option<String> {
    let s = handle.borrow();
    s.entries.get(s.current).and_then(|e| e.state.clone())
}

/// `history.pushState(state, title, url)`。
///
/// 标准行为：截断 current 之后的所有条目，推入新条目。
pub fn history_push(handle: &NavigationHandle, state: Option<String>, url: String) {
    let mut s = handle.borrow_mut();
    let cur = s.current;
    s.entries.truncate(cur + 1);
    s.entries.push(HistoryEntry { url, state });
    s.current = s.entries.len() - 1;
}

/// `history.replaceState(state, title, url)`。
pub fn history_replace(handle: &NavigationHandle, state: Option<String>, url: String) {
    let mut s = handle.borrow_mut();
    let cur = s.current;
    if let Some(entry) = s.entries.get_mut(cur) {
        entry.url = url;
        entry.state = state;
    }
}

/// `history.back()` — 返回是否有上一条。
pub fn history_back(handle: &NavigationHandle) -> bool {
    let mut s = handle.borrow_mut();
    if s.current > 0 {
        s.current -= 1;
        true
    } else {
        false
    }
}

/// `history.forward()`
pub fn history_forward(handle: &NavigationHandle) -> bool {
    let mut s = handle.borrow_mut();
    if s.current + 1 < s.entries.len() {
        s.current += 1;
        true
    } else {
        false
    }
}

/// `history.go(n)` — n 可正可负。
pub fn history_go(handle: &NavigationHandle, n: i64) -> bool {
    let mut s = handle.borrow_mut();
    let target = s.current as i64 + n;
    if target >= 0 && (target as usize) < s.entries.len() {
        s.current = target as usize;
        true
    } else {
        false
    }
}

/// 当前 location 的 URL 字符串。
pub fn current_url(handle: &NavigationHandle) -> String {
    let s = handle.borrow();
    s.entries
        .get(s.current)
        .map(|e| e.url.clone())
        .unwrap_or_default()
}

/// `location.replace(url)` — 替换当前条目（不留历史）。
pub fn location_replace(handle: &NavigationHandle, url: String) {
    history_replace(handle, None, url);
}

/// `location.assign(url)` — 推入新条目。
pub fn location_assign(handle: &NavigationHandle, url: String) {
    history_push(handle, None, url);
}

/// URL 各部分（对应 location.protocol / host / pathname 等）。
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct UrlParts {
    pub href: String,
    pub protocol: String,
    pub host: String,
    pub hostname: String,
    pub port: String,
    pub pathname: String,
    pub search: String,
    pub hash: String,
}

/// 解析 URL 的各部分（无法解析时回退为 pathname = 原始字符串）。
#[must_use]
pub fn parse_url_parts(href: &str) -> UrlParts {
    match Url::parse(href) {
        Ok(u) => {
            let host = u.host_str().unwrap_or("").to_string();
            let port = u.port().map(|p| p.to_string()).unwrap_or_default();
            // location.host 包含端口，location.hostname 不含。
            let host_with_port = if port.is_empty() {
                host.clone()
            } else {
                format!("{host}:{port}")
            };
            UrlParts {
                href: href.to_string(),
                protocol: format!("{}:", u.scheme()),
                host: host_with_port,
                hostname: host,
                port,
                pathname: u.path().to_string(),
                search: u.query().map(|q| format!("?{q}")).unwrap_or_default(),
                hash: u.fragment().map(|h| format!("#{h}")).unwrap_or_default(),
            }
        }
        Err(_) => UrlParts {
            href: href.to_string(),
            pathname: href.to_string(),
            ..Default::default()
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_navigation_has_single_entry() {
        let nav = new_navigation("https://example.com/");
        assert_eq!(history_len(&nav), 1);
        assert_eq!(current_url(&nav), "https://example.com/");
    }

    #[test]
    fn push_increases_length_and_advances_current() {
        let nav = new_navigation("https://example.com/");
        history_push(&nav, None, "https://example.com/page2".to_string());
        assert_eq!(history_len(&nav), 2);
        assert_eq!(current_url(&nav), "https://example.com/page2");
    }

    #[test]
    fn push_truncates_forward_history() {
        let nav = new_navigation("https://example.com/");
        history_push(&nav, None, "https://example.com/a".to_string());
        history_push(&nav, None, "https://example.com/b".to_string());
        assert_eq!(history_len(&nav), 3);
        // back 一次
        assert!(history_back(&nav));
        // 现在 push 应该截断 b
        history_push(&nav, None, "https://example.com/c".to_string());
        assert_eq!(history_len(&nav), 3); // 1 + a + c
        assert_eq!(current_url(&nav), "https://example.com/c");
    }

    #[test]
    fn replace_does_not_increase_length() {
        let nav = new_navigation("https://example.com/");
        history_replace(&nav, None, "https://example.com/replaced".to_string());
        assert_eq!(history_len(&nav), 1);
        assert_eq!(current_url(&nav), "https://example.com/replaced");
    }

    #[test]
    fn back_forward_navigate() {
        let nav = new_navigation("https://example.com/");
        history_push(&nav, None, "https://example.com/a".to_string());
        history_push(&nav, None, "https://example.com/b".to_string());
        assert!(history_back(&nav));
        assert_eq!(current_url(&nav), "https://example.com/a");
        assert!(history_back(&nav));
        assert_eq!(current_url(&nav), "https://example.com/");
        assert!(!history_back(&nav)); // 已经到顶
        assert!(history_forward(&nav));
        assert_eq!(current_url(&nav), "https://example.com/a");
    }

    #[test]
    fn go_positive_and_negative() {
        let nav = new_navigation("https://example.com/");
        history_push(&nav, None, "https://example.com/a".to_string());
        history_push(&nav, None, "https://example.com/b".to_string());
        assert!(history_go(&nav, -2));
        assert_eq!(current_url(&nav), "https://example.com/");
        assert!(history_go(&nav, 1));
        assert_eq!(current_url(&nav), "https://example.com/a");
        assert!(!history_go(&nav, 100)); // 越界
    }

    #[test]
    fn state_persists_across_navigation() {
        let nav = new_navigation("https://example.com/");
        history_push(
            &nav,
            Some(r#"{"page":2}"#.to_string()),
            "https://example.com/a".to_string(),
        );
        assert_eq!(history_state(&nav), Some(r#"{"page":2}"#.to_string()));
        // replace 更新 state
        history_replace(
            &nav,
            Some(r#"{"page":3}"#.to_string()),
            "https://example.com/a".to_string(),
        );
        assert_eq!(history_state(&nav), Some(r#"{"page":3}"#.to_string()));
    }

    #[test]
    fn handle_clones_share_state() {
        let n1 = new_navigation("https://example.com/");
        let n2 = Rc::clone(&n1);
        history_push(&n1, None, "https://example.com/a".to_string());
        assert_eq!(history_len(&n2), 2);
    }

    #[test]
    fn parse_url_parts_full_url() {
        let p = parse_url_parts("https://example.com:8080/path?q=1#hash");
        assert_eq!(p.protocol, "https:");
        assert_eq!(p.host, "example.com:8080");
        assert_eq!(p.hostname, "example.com");
        assert_eq!(p.port, "8080");
        assert_eq!(p.pathname, "/path");
        assert_eq!(p.search, "?q=1");
        assert_eq!(p.hash, "#hash");
    }

    #[test]
    fn parse_url_parts_no_port() {
        let p = parse_url_parts("https://example.com/path");
        assert_eq!(p.host, "example.com");
        assert_eq!(p.port, "");
    }

    #[test]
    fn parse_url_parts_relative_fallback() {
        let p = parse_url_parts("/just/path");
        // 相对 URL 无法解析，pathname 回退为原字符串
        assert_eq!(p.pathname, "/just/path");
    }
}
