//! M82: 内容级启发式警告（P0-2 反爬/验证页静默通过问题）。
//!
//! 背景：百度搜索稳定返回「网络不给力，请稍后重试」（60B）、36kr 返回
//! 「正在进行安全检测...」——都是 **HTTP 200 + exit 0**，与真页面走同一
//! 输出通道，调用方（agent/上游工具）无任何信号区分。
//!
//! 方案：对最终提取内容做启发式扫描，产出 `Vec<String>` 警告——
//! CLI 层打印到 stderr（`[warn] ...`），`--json` 模式额外放进 `warnings`
//! 数组。**只发信号，不拦截输出**（误判代价 > 漏判代价，措辞用 possible）。
//!
//! 已知局限：启发式是字符串匹配，防不住与正文等长的深度伪装页；也不做
//! 指纹伪造/重试对抗（G4 反爬不重点处理）。

/// 反爬/验证页标记（内容 **小于** [`MARKER_SCAN_LIMIT`] 时才扫描——
/// 真实正文页通常远大于此，且可能合法提及 captcha 等词）。
///
/// 每项 (ascii?, marker)。ascii 标记用小写匹配；中文直接子串匹配。
const ANTI_BOT_MARKERS: &[(bool, &str)] = &[
    (false, "正在进行安全检测"),
    (false, "安全验证"),
    (false, "人机验证"),
    (false, "滑动验证"),
    (false, "拖动滑块"),
    (false, "网络不给力"),
    (false, "请稍后重试"),
    (false, "访问过于频繁"),
    (true, "just a moment"),
    (true, "checking your browser"),
    (true, "verify you are human"),
    (true, "cf-chl"),       // Cloudflare challenge 路径/类名
    (true, "cf-turnstile"), // Cloudflare Turnstile 组件
    (true, "g-recaptcha"),  // reCAPTCHA 容器类名
];

/// 标记扫描的内容长度上限（字节）。反爬/验证页几乎都是壳页（< 4KB）；
/// 正文章节页即使提及 captcha 也不会误报。
const MARKER_SCAN_LIMIT: usize = 4096;

/// 内容过短的阈值（字节）——低于此值大概率不是真实页面。
const SHORT_CONTENT_LIMIT: usize = 200;

/// 对最终提取内容做启发式扫描，返回警告列表（空 = 无警告）。
///
/// 输出会被 CLI 放进 stderr（`[warn]` 前缀）和 `--json` 的 `warnings` 数组。
#[must_use]
pub fn content_warnings(content: &str) -> Vec<String> {
    let trimmed = content.trim();
    let mut out = Vec::new();

    if trimmed.is_empty() {
        out.push("content is empty — page may have failed to render".to_string());
        return out;
    }

    let len = trimmed.len();
    if len < MARKER_SCAN_LIMIT {
        let lower = trimmed.to_ascii_lowercase();
        for &(is_ascii, marker) in ANTI_BOT_MARKERS {
            let hit = if is_ascii {
                lower.contains(marker)
            } else {
                trimmed.contains(marker)
            };
            if hit {
                out.push(format!(
                    "possible anti-bot/verification page detected (matched: \"{marker}\")"
                ));
                break; // 一个标记就足以发信号，不堆叠
            }
        }
    }

    if len < SHORT_CONTENT_LIMIT {
        out.push(format!(
            "content is very short ({len} bytes) — likely not the real page"
        ));
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_zh_safety_check_page() {
        // 36kr 实测样本：「正在进行安全检测...」
        let w = content_warnings("正在进行安全检测，请稍候…");
        assert!(
            w.iter().any(|s| s.contains("anti-bot")),
            "36kr safety page should warn: {w:?}"
        );
    }

    #[test]
    fn detects_zh_network_retry_page() {
        // 百度实测样本：「网络不给力，请稍后重试」
        let w = content_warnings("网络不给力，请稍后重试。");
        assert!(
            w.iter().any(|s| s.contains("anti-bot")),
            "baidu retry page should warn: {w:?}"
        );
    }

    #[test]
    fn detects_cloudflare_just_a_moment() {
        let w = content_warnings("Just a moment... \n Enable JavaScript and cookies to continue");
        assert!(
            w.iter().any(|s| s.contains("anti-bot")),
            "CF challenge should warn: {w:?}"
        );
    }

    #[test]
    fn empty_content_warns() {
        let w = content_warnings("   ");
        assert!(
            w.iter().any(|s| s.contains("empty")),
            "empty content should warn: {w:?}"
        );
    }

    #[test]
    fn short_content_warns_without_marker() {
        let w = content_warnings("hello world");
        assert!(
            w.iter().any(|s| s.contains("very short")),
            "short content should warn: {w:?}"
        );
        assert!(
            !w.iter().any(|s| s.contains("anti-bot")),
            "no false anti-bot on benign short text: {w:?}"
        );
    }

    #[test]
    fn long_article_mentioning_captcha_does_not_warn() {
        // 真实正文页（> 4KB）合法提及 captcha 不应误报。
        let article = format!(
            "{} captcha research {}",
            "word ".repeat(2000),
            "word ".repeat(2000)
        );
        assert!(
            content_warnings(&article).is_empty(),
            "long article should have no warnings"
        );
    }

    #[test]
    fn normal_long_content_no_warnings() {
        let article = "这是一段正常的正文内容。".repeat(100);
        assert!(content_warnings(&article).is_empty());
    }
}
