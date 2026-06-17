//! `browser-extractor` — 渲染后内容提取（后置过滤器）。
//!
//! M59: 给 `browser fetch` 命令提供「当 curl 用」的输出加工能力。
//! 吃渲染后的 `&Tree`，产出 HTML / 纯文本 / Markdown / 超链接地图。
//!
//! ## 架构边界
//! 只依赖 `dom` / `css-engine` / `url`，**不碰 net/js-runtime/eventloop**。
//! 这是「后置插件」：浏览器核心无感，extractor 单独可测、可换、可删。
//! 设计文档：`docs/plans/M59-fetch-command-design.md`。

#![forbid(unsafe_code)]

pub mod format_html;
pub mod format_links;
pub mod format_text;
pub mod selector;

// clean.rs / format_md.rs 在 Step 2 / Step 3 加入。

use browser_dom::Tree;

/// 输出格式（`--format`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    /// 渲染后完整 HTML（序列化 DOM）。
    Html,
    /// 纯文本（去标签，保留段落结构）。
    Text,
    /// Markdown（HTML→md，turndown 子集）。【Step 3 加入】
    Markdown,
    /// 超链接地图（文本 → URL，绝对化 + 去重）。
    Links,
}

impl OutputFormat {
    /// 从字符串解析（CLI `--format` 参数）。
    ///
    /// # Errors
    /// 未知格式返回 `Err(String)`。
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.to_ascii_lowercase().as_str() {
            "html" | "raw" => Ok(Self::Html),
            "text" | "txt" | "plain" => Ok(Self::Text),
            "markdown" | "md" => Ok(Self::Markdown),
            "links" | "link" => Ok(Self::Links),
            other => Err(format!(
                "unknown format '{other}' (expected: html|text|markdown|links)"
            )),
        }
    }
}

/// 提取选项（`browser fetch` 的过滤参数）。
#[derive(Debug, Clone)]
pub struct FetchOptions {
    /// 输出格式。
    pub format: OutputFormat,
    /// CSS 选择器过滤（`--selector`）。`None` = 整个文档。
    pub selector: Option<String>,
    /// 是否做主内容噪声过滤（`--only-main-content`，默认 true）。
    /// 借鉴 Firecrawl EXCLUDE_NON_MAIN_TAGS（42 选择器）。【Step 2 加入】
    pub only_main_content: bool,
}

impl Default for FetchOptions {
    fn default() -> Self {
        Self {
            format: OutputFormat::Markdown,
            selector: None,
            only_main_content: true,
        }
    }
}

/// 提取结果。
#[derive(Debug, Clone)]
pub struct ExtractResult {
    /// 加工后的内容字符串。
    pub content: String,
    /// 页面 `<title>`（若有），用于 `--json` 输出。
    pub title: Option<String>,
}

/// 主入口：在渲染后的 `tree` 上按 `opts` 提取内容。
///
/// `base_url` 用于 links/markdown 格式的相对 URL 绝对化。
///
/// # Errors
/// 选择器解析失败时返回 `Err(String)`。
pub fn run_extract(
    tree: &Tree,
    base_url: Option<&str>,
    opts: &FetchOptions,
) -> Result<ExtractResult, String> {
    let title = extract_title(tree);
    let selector = opts.selector.as_deref();
    // Step 2 加入噪声过滤后，这里会先 clean_tree（仅 text/markdown/html 且
    // only_main_content=true 时）。当前 Step 1 先直通各 format。
    let content = match opts.format {
        OutputFormat::Html => format_html::to_html(tree, selector)?,
        OutputFormat::Text => format_text::to_text(tree, selector)?,
        OutputFormat::Links => format_links::to_links(tree, base_url, selector)?,
        OutputFormat::Markdown => {
            // Step 3 实装前，markdown 暂时降级为纯文本（保证命令可用）。
            eprintln!("[extractor] markdown format not yet implemented, falling back to text");
            format_text::to_text(tree, selector)?
        }
    };
    Ok(ExtractResult { content, title })
}

/// 从 `<title>` 元素提取页面标题。
fn extract_title(tree: &Tree) -> Option<String> {
    let mut found: Option<String> = None;
    tree.traverse(tree.root(), |id, _| {
        if let browser_dom::NodeData::Element { tag, .. } = &tree.get(id).data {
            if tag.eq_ignore_ascii_case("title") {
                // title 的第一个文本子节点即标题。
                for &child in tree.children_of(id) {
                    if let browser_dom::NodeData::Text(s) = &tree.get(child).data {
                        found = Some(s.trim().to_string());
                        return false; // 找到即停
                    }
                }
            }
        }
        true
    });
    found.filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(html: &str) -> Tree {
        browser_html_parser::parse(html)
    }

    #[test]
    fn output_format_parse_known() {
        assert_eq!(OutputFormat::parse("html").unwrap(), OutputFormat::Html);
        assert_eq!(
            OutputFormat::parse("markdown").unwrap(),
            OutputFormat::Markdown
        );
        assert_eq!(OutputFormat::parse("md").unwrap(), OutputFormat::Markdown);
        assert_eq!(OutputFormat::parse("text").unwrap(), OutputFormat::Text);
        assert_eq!(OutputFormat::parse("links").unwrap(), OutputFormat::Links);
    }

    #[test]
    fn output_format_parse_unknown_errors() {
        assert!(OutputFormat::parse("pdf").is_err());
    }

    #[test]
    fn extract_title_finds_title_element() {
        let tree = parse("<html><head><title>My Page</title></head><body>x</body></html>");
        assert_eq!(extract_title(&tree), Some("My Page".to_string()));
    }

    #[test]
    fn extract_title_none_when_absent() {
        let tree = parse("<p>no title</p>");
        assert_eq!(extract_title(&tree), None);
    }

    #[test]
    fn run_extract_html_returns_full_doc() {
        let tree = parse("<p>hi</p>");
        let opts = FetchOptions {
            format: OutputFormat::Html,
            only_main_content: false,
            ..Default::default()
        };
        let result = run_extract(&tree, None, &opts).expect("html");
        assert!(result.content.contains("<p>"));
        assert!(result.content.contains("hi"));
    }

    #[test]
    fn run_extract_links_with_base() {
        let tree = parse("<a href='/p'>link</a>");
        let opts = FetchOptions {
            format: OutputFormat::Links,
            only_main_content: false,
            ..Default::default()
        };
        let result = run_extract(&tree, Some("https://x.com/"), &opts).expect("links");
        assert!(result.content.contains("https://x.com/p"));
    }
}
