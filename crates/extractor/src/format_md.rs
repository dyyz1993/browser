//! `--format markdown`：HTML → Markdown 转换（turndown 子集）。
//!
//! 自研实现，不引入 turndown 依赖（符合项目「自研优先、复杂度门槛」原则）。
//! 首版覆盖 ~80% 常见元素：标题/链接/列表/强调/代码/引用/表格/图片/水平线。
//! 边界 case（复杂嵌套、罕见标签）记为已知局限，不硬刚。
//!
//! ## 设计
//! DFS 遍历 DOM，维护一个「行缓冲」(current line) + 「块缓冲」(blocks)。
//! inline 元素（a/strong/em/code）拼进行缓冲，block 元素（p/h/li/tr）结束时
//! 把行缓冲刷成一个 block。这样能正确处理 inline 嵌套。

use browser_dom::{NodeData, NodeId, Tree};
use std::collections::HashSet;
use url::Url;

use crate::selector::query_all;

/// 转 Markdown。
///
/// - `base_url`：相对 URL 绝对化（a/img）。
/// - `selector`：可选，只转换匹配子树。
/// - `excluded`：噪声过滤排除集（clean.rs 计算）。
///
/// # Errors
/// 选择器解析失败时返回 `Err(String)`。
pub fn to_markdown(
    tree: &Tree,
    base_url: Option<&str>,
    selector: Option<&str>,
    excluded: &HashSet<NodeId>,
) -> Result<String, String> {
    let base = base_url.and_then(|b| Url::parse(b).ok());
    let roots: Vec<NodeId> = match selector {
        Some(sel) => query_all(tree, sel)?,
        None => vec![tree.root()],
    };

    let mut ctx = MdCtx {
        tree,
        base: base.as_ref(),
        excluded,
        blocks: Vec::new(),
        line: String::new(),
        list_stack: Vec::new(),
        in_pre: false,
    };

    for root in roots {
        walk_md(&mut ctx, root);
    }
    ctx.flush_line();

    Ok(finish(ctx.blocks))
}

/// Markdown 转换上下文。
struct MdCtx<'a> {
    tree: &'a Tree,
    base: Option<&'a Url>,
    excluded: &'a HashSet<NodeId>,
    /// 已完成的块（段落/标题/列表项/表格行等），每个块之间空行分隔。
    blocks: Vec<String>,
    /// 当前正在拼接的行（inline 元素往这里 push）。
    line: String,
    /// 列表嵌套栈：每层是 (是否有序, 当前序号)。
    list_stack: Vec<(bool, usize)>,
    /// 是否在 <pre> 内（内部不转义、保留原样）。
    in_pre: bool,
}

impl<'a> MdCtx<'a> {
    /// 把当前行刷成一个 block。
    fn flush_line(&mut self) {
        let trimmed = self.line.trim();
        if !trimmed.is_empty() {
            self.blocks.push(trimmed.to_string());
        }
        self.line.clear();
    }

    /// 追加 inline 文本到当前行。
    fn push_inline(&mut self, s: &str) {
        if self.in_pre {
            self.line.push_str(s);
        } else {
            // 折叠空白（非 pre 模式）。
            let need_space =
                !self.line.is_empty() && !self.line.ends_with(' ') && !self.line.ends_with('\n');
            let s = s.trim();
            if !s.is_empty() {
                if need_space {
                    self.line.push(' ');
                }
                self.line.push_str(s);
            }
        }
    }
}

/// 递归遍历，按 tag 分发。
fn walk_md(ctx: &mut MdCtx, id: NodeId) {
    if ctx.excluded.contains(&id) {
        return;
    }
    match &ctx.tree.get(id).data {
        NodeData::Text(s) => {
            ctx.push_inline(s);
        }
        NodeData::Element { tag, attrs } => {
            handle_element(ctx, id, tag, attrs);
        }
        NodeData::Document | NodeData::Doctype { .. } => {
            for &child in ctx.tree.children_of(id) {
                walk_md(ctx, child);
            }
        }
        NodeData::Comment(_) => {}
    }
}

/// 处理元素节点：按 tag 决定是 block（先 flush 再处理）还是 inline。
fn handle_element(ctx: &mut MdCtx, id: NodeId, tag: &str, attrs: &[(String, String)]) {
    let t = tag.to_ascii_lowercase();
    match t.as_str() {
        // 标题：flush 前置内容，加 # 前缀，处理完 flush。
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
            ctx.flush_line();
            let level = t[1..].parse::<usize>().unwrap_or(1);
            ctx.line.push_str(&"#".repeat(level));
            ctx.line.push(' ');
            walk_children(ctx, id);
            ctx.flush_line();
        }
        // 段落 / div：作为独立 block。
        "p" | "div" | "section" | "article" | "main" | "header" | "footer" | "nav" | "aside" => {
            ctx.flush_line();
            walk_children(ctx, id);
            ctx.flush_line();
        }
        // 换行。
        "br" => {
            ctx.line.push_str("  \n");
        }
        // 水平线。
        "hr" => {
            ctx.flush_line();
            ctx.blocks.push("---".to_string());
        }
        // 强调。
        "strong" | "b" => {
            wrap_inline(ctx, id, "**");
        }
        "em" | "i" => {
            wrap_inline(ctx, id, "*");
        }
        "del" | "s" => {
            wrap_inline(ctx, id, "~~");
        }
        // 行内代码。
        "code" if !ctx.in_pre => {
            wrap_inline(ctx, id, "`");
        }
        // 代码块。
        "pre" => {
            ctx.flush_line();
            let prev = ctx.in_pre;
            ctx.in_pre = true;
            ctx.line.push_str("```\n");
            walk_children(ctx, id);
            ctx.line.push_str("\n```");
            ctx.in_pre = prev;
            ctx.flush_line();
        }
        // 引用。
        "blockquote" => {
            ctx.flush_line();
            // 收集子内容，每行加 > 前缀。
            let mut sub = MdCtx {
                tree: ctx.tree,
                base: ctx.base,
                excluded: ctx.excluded,
                blocks: Vec::new(),
                line: String::new(),
                list_stack: Vec::new(),
                in_pre: false,
            };
            walk_children(&mut sub, id);
            sub.flush_line();
            let quoted: Vec<String> = sub
                .blocks
                .iter()
                .flat_map(|b| b.split('\n'))
                .map(|l| format!("> {l}"))
                .collect();
            ctx.blocks.extend(quoted);
            ctx.blocks.push(String::new());
        }
        // 链接：[text](href)。用独立 buffer 收集链接文本。
        "a" => {
            let href = attrs
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("href"))
                .map(|(_, v)| v.as_str());
            if let Some(href) = href {
                if !href.is_empty() && !href.starts_with("javascript:") {
                    let absolute = resolve_url(href, ctx.base);
                    let saved = std::mem::take(&mut ctx.line);
                    walk_children(ctx, id);
                    let text = ctx.line.trim().to_string();
                    ctx.line = saved;
                    ctx.push_inline(&format!("[{text}]({absolute})"));
                } else {
                    walk_children(ctx, id);
                }
            } else {
                walk_children(ctx, id);
            }
        }
        // 图片：![alt](src)。
        "img" => {
            let src = attrs
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("src"))
                .map(|(_, v)| v.as_str());
            let alt = attrs
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("alt"))
                .map(|(_, v)| v.as_str())
                .unwrap_or("");
            if let Some(src) = src {
                if !src.is_empty() {
                    let absolute = resolve_url(src, ctx.base);
                    ctx.push_inline(&format!("![{alt}]({absolute})"));
                }
            }
        }
        // 列表。
        "ul" => {
            ctx.flush_line();
            ctx.list_stack.push((false, 0));
            walk_children(ctx, id);
            ctx.list_stack.pop();
            ctx.blocks.push(String::new());
        }
        "ol" => {
            ctx.flush_line();
            ctx.list_stack.push((true, 1));
            walk_children(ctx, id);
            ctx.list_stack.pop();
            ctx.blocks.push(String::new());
        }
        "li" => {
            ctx.flush_line();
            let indent = "  ".repeat(ctx.list_stack.len().saturating_sub(1));
            let marker = match ctx.list_stack.last() {
                Some((true, n)) => {
                    let m = format!("{n}. ");
                    if let Some((_, counter)) = ctx.list_stack.last_mut() {
                        *counter += 1;
                    }
                    m
                }
                Some((false, _)) => "- ".to_string(),
                None => "- ".to_string(),
            };
            ctx.line.push_str(&indent);
            ctx.line.push_str(&marker);
            walk_children(ctx, id);
            ctx.flush_line();
        }
        // 表格（GFM）：简化处理——thead/tbody 的 tr 转 markdown 表格。
        "table" => {
            ctx.flush_line();
            render_table(ctx, id);
            ctx.blocks.push(String::new());
        }
        // 其他 inline 容器（span 等）：透明透传。
        "span" | "sup" | "sub" | "abbr" | "small" | "mark" | "u" => {
            walk_children(ctx, id);
        }
        // 其余未识别 tag：透传子节点（保内容，丢语义）。
        // 但 script/style 的内容不应泄漏到 markdown 中。
        "script" | "style" | "noscript" | "template" => {
            // 忽略标签内容，不进入 markdown
        }
        "svg" => {
            // SVG 图形描述标签，不应进入 markdown
        }
        _ => {
            walk_children(ctx, id);
        }
    }
}

/// 把子节点内容包裹进 `wrapper`（用于 strong/em/code）。
/// 用独立 buffer 收集子内容，避免与外层 line 交错。
fn wrap_inline(ctx: &mut MdCtx, id: NodeId, wrapper: &str) {
    let saved = std::mem::take(&mut ctx.line);
    walk_children(ctx, id);
    let inner = ctx.line.trim().to_string();
    ctx.line = saved;
    if inner.is_empty() {
        // 空内容不输出 wrapper。
    } else {
        ctx.push_inline(&format!("{wrapper}{inner}{wrapper}"));
    }
}

fn walk_children(ctx: &mut MdCtx, id: NodeId) {
    for &child in ctx.tree.children_of(id) {
        walk_md(ctx, child);
    }
}

/// 渲染 GFM 表格。
fn render_table(ctx: &mut MdCtx, table_id: NodeId) {
    let mut rows: Vec<Vec<String>> = Vec::new();
    collect_table_rows(ctx, table_id, &mut rows);
    if rows.is_empty() {
        return;
    }
    let mut out = String::new();
    // 第一行作为表头。
    out.push_str("| ");
    out.push_str(&rows[0].join(" | "));
    out.push_str(" |\n");
    // 分隔行。
    out.push('|');
    for _ in &rows[0] {
        out.push_str(" --- |");
    }
    out.push('\n');
    // 数据行。
    for row in &rows[1..] {
        out.push_str("| ");
        out.push_str(&row.join(" | "));
        out.push_str(" |\n");
    }
    ctx.blocks.push(out.trim_end().to_string());
}

/// 递归收集所有 <tr>，每个 <tr> 的 <th>/<td> 转成一个单元格文本。
fn collect_table_rows(ctx: &MdCtx, id: NodeId, rows: &mut Vec<Vec<String>>) {
    for &child in ctx.tree.children_of(id) {
        if ctx.excluded.contains(&child) {
            continue;
        }
        if let NodeData::Element { tag, .. } = &ctx.tree.get(child).data {
            if tag.eq_ignore_ascii_case("tr") {
                let row = collect_cells(ctx, child);
                rows.push(row);
            } else {
                // thead/tbody/tfoot：递归找 tr。
                collect_table_rows(ctx, child, rows);
            }
        }
    }
}

/// 收集一个 <tr> 的所有 <th>/<td> 文本。
fn collect_cells(ctx: &MdCtx, tr_id: NodeId) -> Vec<String> {
    let mut cells = Vec::new();
    for &child in ctx.tree.children_of(tr_id) {
        if ctx.excluded.contains(&child) {
            continue;
        }
        if let NodeData::Element { tag, .. } = &ctx.tree.get(child).data {
            if tag.eq_ignore_ascii_case("th") || tag.eq_ignore_ascii_case("td") {
                cells.push(cell_text(ctx, child));
            }
        }
    }
    cells
}

/// 提取单元格纯文本（不含 markdown 格式，避免破坏表格结构）。
fn cell_text(ctx: &MdCtx, cell_id: NodeId) -> String {
    let mut s = String::new();
    collect_plain_text(ctx, cell_id, &mut s);
    s.trim().replace('|', "\\|").replace('\n', " ")
}

/// 递归收集纯文本（单元格用）。
fn collect_plain_text(ctx: &MdCtx, id: NodeId, out: &mut String) {
    match &ctx.tree.get(id).data {
        NodeData::Text(s) => out.push_str(s),
        NodeData::Element { .. } => {
            for &child in ctx.tree.children_of(id) {
                if !ctx.excluded.contains(&child) {
                    collect_plain_text(ctx, child, out);
                }
            }
        }
        _ => {}
    }
}

/// 解析相对/绝对 URL。
fn resolve_url(href: &str, base: Option<&Url>) -> String {
    if let Ok(abs) = Url::parse(href) {
        return abs.to_string();
    }
    if let Some(b) = base {
        if let Ok(joined) = b.join(href) {
            return joined.to_string();
        }
    }
    href.to_string()
}

/// 最终后处理：blocks 用双换行拼接，折叠多余空行。
fn finish(blocks: Vec<String>) -> String {
    let mut out = String::new();
    let mut prev_blank = false;
    for block in &blocks {
        if block.is_empty() {
            if !prev_blank && !out.is_empty() {
                out.push('\n');
            }
            prev_blank = true;
        } else {
            out.push_str(block);
            out.push('\n');
            prev_blank = false;
        }
    }
    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(html: &str) -> Tree {
        browser_html_parser::parse(html)
    }

    fn md(html: &str) -> String {
        let tree = parse(html);
        to_markdown(&tree, None, None, &HashSet::new()).expect("md")
    }

    fn md_base(html: &str, base: &str) -> String {
        let tree = parse(html);
        to_markdown(&tree, Some(base), None, &HashSet::new()).expect("md")
    }

    #[test]
    fn heading_produces_hash_prefix() {
        let m = md("<h1>Title</h1>");
        assert!(m.contains("# Title"), "h1 -> #: {m:?}");
        let m = md("<h3>Sub</h3>");
        assert!(m.contains("### Sub"), "h3 -> ###: {m:?}");
    }

    #[test]
    fn paragraph_becomes_plain_text() {
        let m = md("<p>hello world</p>");
        assert_eq!(m, "hello world");
    }

    #[test]
    fn link_produces_markdown_link() {
        let m = md("<a href='https://x.com'>click</a>");
        assert!(m.contains("[click](https://x.com/"), "link: {m:?}");
        // url crate 规范化补尾斜杠，属正常行为
    }

    #[test]
    fn link_relative_resolved_against_base() {
        let m = md_base("<a href='/page'>link</a>", "https://site.com/");
        assert!(
            m.contains("[link](https://site.com/page)"),
            "relative link: {m:?}"
        );
    }

    #[test]
    fn strong_emphasis() {
        let m = md("<p>this is <strong>bold</strong> and <em>italic</em></p>");
        assert!(m.contains("**bold**"), "strong: {m:?}");
        assert!(m.contains("*italic*"), "em: {m:?}");
    }

    #[test]
    fn inline_code() {
        let m = md("<p>use <code>fmt</code> module</p>");
        assert!(m.contains("`fmt`"), "code: {m:?}");
    }

    #[test]
    fn code_block_with_pre() {
        let m = md("<pre>let x = 1;</pre>");
        assert!(m.contains("```"), "pre fence: {m:?}");
        assert!(m.contains("let x = 1;"), "pre content: {m:?}");
    }

    #[test]
    fn unordered_list() {
        let m = md("<ul><li>one</li><li>two</li></ul>");
        assert!(m.contains("- one"), "ul li: {m:?}");
        assert!(m.contains("- two"), "ul li: {m:?}");
    }

    #[test]
    fn ordered_list_numbered() {
        let m = md("<ol><li>first</li><li>second</li></ol>");
        assert!(m.contains("1. first"), "ol li: {m:?}");
        assert!(m.contains("2. second"), "ol li: {m:?}");
    }

    #[test]
    fn blockquote_with_gt_prefix() {
        let m = md("<blockquote><p>quoted</p></blockquote>");
        assert!(m.contains("> quoted"), "blockquote: {m:?}");
    }

    #[test]
    fn horizontal_rule() {
        let m = md("<p>before</p><hr><p>after</p>");
        assert!(m.contains("---"), "hr: {m:?}");
    }

    #[test]
    fn image_markdown() {
        let m = md("<img src='https://x.com/a.png' alt='pic'>");
        assert!(m.contains("![pic](https://x.com/a.png)"), "img: {m:?}");
    }

    #[test]
    fn gfm_table_rendered() {
        let m =
            md("<table><tr><th>Name</th><th>Age</th></tr><tr><td>Bob</td><td>30</td></tr></table>");
        assert!(m.contains("| Name | Age |"), "table header: {m:?}");
        assert!(m.contains("| --- |"), "table separator: {m:?}");
        assert!(m.contains("| Bob | 30 |"), "table row: {m:?}");
    }

    #[test]
    fn list_containing_link() {
        let m = md("<ul><li><a href='/a'>link</a></li></ul>");
        // 嵌套：列表项里有链接
        assert!(m.contains("- "), "list marker: {m:?}");
        assert!(m.contains("[link](/a)"), "nested link: {m:?}");
    }

    #[test]
    fn excluded_nodes_are_skipped() {
        let tree = parse("<nav><a href='/n'>nav link</a></nav><article><p>main</p></article>");
        let mut excluded = HashSet::new();
        // 模拟 nav 被排除
        excluded.insert(1); // 1 通常是 html；这里用一个肯定存在的 nav 节点
                            // 找 nav 节点
        let mut nav_id = None;
        tree.traverse(tree.root(), |id, _| {
            if let NodeData::Element { tag, .. } = &tree.get(id).data {
                if tag == "nav" {
                    nav_id = Some(id);
                    return false;
                }
            }
            true
        });
        if let Some(nid) = nav_id {
            excluded.clear();
            excluded.insert(nid);
        }
        let m = to_markdown(&tree, None, None, &excluded).expect("md");
        assert!(m.contains("main"), "main kept: {m:?}");
        assert!(!m.contains("nav link"), "nav excluded: {m:?}");
    }
}
