//! M96.19: 资源下载器（fetch 命令配套）——按 glob/域名/后缀匹配并下载
//! 页面引用的资源（图片/字体/媒体等引擎不主动加载的静态资产）。
//!
//! 匹配语义（对完整 URL）：
//! - 含 `/` 的 pattern 走 **glob**：`*` 单段（不跨 `/`）、`**` 跨段、`?` 单字符。
//!   例：`https://pbs.twimg.com/**/*.jpg`、`**/media/**`
//! - 不含 `/` 且含 `.` 开头的（如 `.png`）→ **后缀**匹配（URL path 以其结尾）
//! - 其余不含 `/` 的（如 `pbs.twimg.com`）→ **域名**匹配（host 等于或为其子域）

use std::collections::HashSet;

/// glob 段匹配（`*` 任意非 `/` 序列、`?` 单字符）。
fn glob_seg_match(pat: &[u8], s: &[u8]) -> bool {
    // 简易 NFA：双指针 + 回溯
    let (mut pi, mut si) = (0usize, 0usize);
    let (mut star_p, mut star_s) = (usize::MAX, 0usize);
    while si < s.len() {
        if pi < pat.len() && (pat[pi] == b'?' || pat[pi] == s[si]) {
            pi += 1;
            si += 1;
        } else if pi < pat.len() && pat[pi] == b'*' {
            star_p = pi;
            star_s = si;
            pi += 1;
        } else if star_p != usize::MAX {
            pi = star_p + 1;
            star_s += 1;
            si = star_s;
        } else {
            return false;
        }
    }
    while pi < pat.len() && pat[pi] == b'*' {
        pi += 1;
    }
    pi == pat.len()
}

/// 完整 glob 匹配：按 `/` 分段，`**` 段可吞任意数量段。
pub fn glob_match(pattern: &str, url: &str) -> bool {
    let p_segs: Vec<&str> = pattern.split('/').collect();
    let u_segs: Vec<&str> = url.split('/').collect();
    fn rec(p: &[&str], u: &[&str]) -> bool {
        if p.is_empty() {
            return u.is_empty();
        }
        // 含 ** 的段（含整段 "**"）可吞 0..=n 段（段内 ** 跨 `/`——
        // `**_400x400.jpg` 能匹配 `id/name_400x400.jpg` 两段拼接）
        if p[0].contains("**") {
            for skip in 0..=u.len() {
                let joined = u[..skip].join("/");
                if glob_seg_match(p[0].as_bytes(), joined.as_bytes()) && rec(&p[1..], &u[skip..]) {
                    return true;
                }
            }
            return false;
        }
        if u.is_empty() {
            return false;
        }
        glob_seg_match(p[0].as_bytes(), u[0].as_bytes()) && rec(&p[1..], &u[1..])
    }
    rec(&p_segs, &u_segs)
}

/// 模式匹配总入口（域名/后缀/glob 三态）。
pub fn pattern_match(pattern: &str, url: &str) -> bool {
    let parsed = url::Url::parse(url).ok();
    let host = parsed.as_ref().and_then(|u| u.host_str()).unwrap_or("");
    let path = parsed.as_ref().map(|u| u.path()).unwrap_or("");
    if !pattern.contains('/') {
        if pattern.starts_with('.') {
            // 后缀（可带 query：path 部分匹配）
            let bare = path.split('?').next().unwrap_or(path);
            return bare.ends_with(pattern);
        }
        // 域名（等于或子域）
        return host == pattern || host.ends_with(&format!(".{pattern}"));
    }
    // glob：对去 query/fragment 的 URL 匹配（? 在 pattern 里是通配符，
    // URL 的 ?query 是参数——两者不冲突的唯一解是匹配面向纯路径形态）
    let bare_url = url.split(['?', '#']).next().unwrap_or(url);
    glob_match(pattern, bare_url)
}

/// 资源属性表：标签 → 抓取的属性。
const ASSET_TAGS: &[(&str, &[&str])] = &[
    (
        "img",
        &[
            "src",
            "data-src",
            "data-original",
            "data-lazy-src",
            "srcset",
        ],
    ),
    ("source", &["src", "srcset"]),
    ("video", &["src", "poster"]),
    ("audio", &["src"]),
    ("embed", &["src"]),
    ("link", &["href"]),
    ("script", &["src"]),
    ("meta", &["content"]), // og:image 等
];

/// 从最终 DOM 收集全部资源 URL（绝对化 + srcset 展开 + 去重）。
/// 直接遍历 arena tree（qjs_bridge::all_ids 返回的是 id 属性值非节点表——
/// 不可用于遍历）。
pub fn collect_asset_urls(
    shared: &browser_js_runtime::bridge::SharedTree,
    base_url: &str,
) -> Vec<String> {
    let mut urls: Vec<String> = Vec::new();
    let tree = shared.borrow();
    tree.traverse(tree.root(), |_, node| {
        if let browser_dom::node::NodeData::Element { tag, attrs } = &node.data {
            let tag = tag.to_ascii_lowercase();
            let Some((_, wanted)) = ASSET_TAGS.iter().find(|(t, _)| *t == tag) else {
                return true;
            };
            for (k, v) in attrs {
                let k = k.to_ascii_lowercase();
                let wants = wanted.contains(&k.as_str())
                    || (tag == "meta" && k == "content" && v.contains("twimg.com"));
                if !wants || v.is_empty() || v.starts_with("data:") || v.starts_with("javascript:")
                {
                    continue;
                }
                let candidates: Vec<String> = if k == "srcset" {
                    v.split(',')
                        .filter_map(|p| p.split_whitespace().next().map(str::to_string))
                        .collect()
                } else {
                    vec![v.clone()]
                };
                for c in candidates {
                    if let Some(abs) = resolve(&c, base_url) {
                        urls.push(abs);
                    }
                }
            }
        }
        true
    });
    let mut seen = HashSet::new();
    urls.retain(|u| seen.insert(u.clone()));
    urls
}

fn resolve(url: &str, base: &str) -> Option<String> {
    if url.contains("://") {
        return Some(url.to_string());
    }
    let b = url::Url::parse(base).ok()?;
    b.join(url).ok().map(|u| u.to_string())
}

/// 下载命中的 URL 到目录。返回 (成功列表, 失败列表)。
pub async fn save_assets(
    urls: &[String],
    patterns: &[String],
    dir: &str,
    proxy_env: bool,
) -> (Vec<String>, Vec<String>) {
    let _ = proxy_env;
    std::fs::create_dir_all(dir).ok();
    let client = browser_net::HttpClient::new();
    let mut ok = Vec::new();
    let mut fail = Vec::new();
    for u in urls {
        if !patterns.iter().any(|p| pattern_match(p, u)) {
            continue;
        }
        // 文件名：url hash + 原扩展（防穿越/防重名）
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        std::hash::Hash::hash(&u, &mut hasher);
        let h = std::hash::Hasher::finish(&hasher);
        let ext = url::Url::parse(u)
            .ok()
            .and_then(|pu| pu.path().rsplit('.').next().map(str::to_string))
            .filter(|e| e.len() <= 5 && e.chars().all(|c| c.is_ascii_alphanumeric()))
            .unwrap_or_else(|| "bin".to_string());
        let fname = format!("{dir}/{h:016x}.{ext}");
        match client.get(u).await {
            Ok(bytes) if !bytes.is_empty() => {
                if std::fs::write(&fname, &bytes).is_ok() {
                    eprintln!("[assets] saved {} ({}B) -> {fname}", short(u), bytes.len());
                    ok.push(u.clone());
                } else {
                    fail.push(u.clone());
                }
            }
            _ => {
                eprintln!("[assets] FAILED {}", short(u));
                fail.push(u.clone());
            }
        }
    }
    (ok, fail)
}

fn short(u: &str) -> String {
    if u.len() > 80 {
        format!("{}…", &u[..77])
    } else {
        u.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_basic() {
        assert!(glob_match(
            "https://pbs.twimg.com/**/*.jpg",
            "https://pbs.twimg.com/media/abc/xyz.jpg"
        ));
        assert!(!glob_match(
            "https://pbs.twimg.com/*.jpg",
            "https://pbs.twimg.com/media/xyz.jpg" // * 不跨段
        ));
        assert!(glob_match("**/media/**", "https://x.com/a/media/b/c.png"));
        assert!(glob_match(
            "https://*.twimg.com/profile_images/*/*.jpg",
            "https://pbs.twimg.com/profile_images/123/456.jpg"
        ));
        assert!(!glob_match(
            "https://*.twimg.com/**/*.gif",
            "https://pbs.twimg.com/a/b.jpg"
        ));
    }

    #[test]
    fn pattern_three_modes() {
        // 域名
        assert!(pattern_match(
            "pbs.twimg.com",
            "https://pbs.twimg.com/media/a.jpg"
        ));
        assert!(pattern_match(
            "twimg.com",
            "https://pbs.twimg.com/media/a.jpg"
        ));
        assert!(!pattern_match("twimg.com", "https://twitter.com/a.jpg"));
        // 后缀
        assert!(pattern_match(".jpg", "https://x.com/a/b.jpg?name=small"));
        assert!(!pattern_match(".jpg", "https://x.com/a/b.png"));
        // glob
        assert!(pattern_match(
            "**/*.woff2",
            "https://fonts.x.com/f/fontello.woff2?596"
        ));
    }
}
