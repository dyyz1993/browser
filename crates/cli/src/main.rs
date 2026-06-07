//! `browser-cli` — command-line entry point.
//!
//! Subcommands:
//! - `browser parse <file>`           parse a local HTML file, print DOM tree
//! - `browser get  <url>`             fetch a URL via HTTPS, parse, print DOM tree
//! - `browser render-file <file>`     parse + layout + render a local HTML file
//! - `browser render-script <file>`   parse + execute scripts + layout + render
//! - `browser render-url  <url>`      fetch + parse + execute scripts + render

mod img_ascii;
mod screenshot;

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{anyhow, Context, Result};
use browser_cookie::{load_from_file_shared, save_jar_to_file};
use browser_css_engine::{compute_styles, parse as parse_css};
use browser_dom::pretty_print;
use browser_html_parser::parse as parse_html;
use browser_js_runtime::{
    current_cookie_jar, ensure_cookie_jar, run_scripts, run_scripts_with_base,
};
use browser_layout::{construct_layout_tree, layout as run_layout, LayoutConfig};
use browser_net::HttpClient;
use browser_render::{render_ascii, render_ascii_colored};
use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "browser", version, about = "Cross-platform browser toolkit")]
struct Cli {
    /// M21.2: load cookies from this file at start (if exists), save
    /// updated jar back at exit. Enables login persistence across runs.
    #[arg(long, global = true)]
    cookie_file: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Debug, Subcommand)]
enum Cmd {
    /// Parse a local HTML file and print the DOM tree.
    Parse { file: PathBuf },
    /// M12.3: convert a PNG/JPG image to ASCII art.
    ImageAscii {
        file: PathBuf,
        /// Max output width (characters). Default 80 = terminal width.
        #[arg(long, default_value_t = 80)]
        width: u32,
        /// Max output height (lines).
        #[arg(long, default_value_t = 40)]
        height: u32,
    },
    /// Fetch a URL via HTTPS and print the parsed DOM tree.
    Get { url: String },
    /// Parse, lay out, and render a local HTML file as terminal ASCII.
    RenderFile {
        file: PathBuf,
        #[arg(long, default_value_t = 80)]
        width: usize,
        /// M12.1: write rendered ASCII as a PNG screenshot to this path.
        #[arg(long)]
        screenshot: Option<PathBuf>,
        /// M29.3: limit screenshot height (pixels). If rendered height > max-height,
        /// truncate from top (bottom content discarded).
        #[arg(long)]
        max_height: Option<usize>,
    },
    /// Parse, execute <script> tags, then render. JS can mutate the
    /// DOM via __setBody / __appendBody / __setTitle / __log.
    RenderScript {
        file: PathBuf,
        #[arg(long, default_value_t = 80)]
        width: usize,
        /// M12.1: write rendered ASCII as a PNG screenshot to this path.
        #[arg(long)]
        screenshot: Option<PathBuf>,
        /// M29.3: limit screenshot height (pixels). If rendered height > max-height,
        /// truncate from top (bottom content discarded).
        #[arg(long)]
        max_height: Option<usize>,
        /// M18.2: after rendering, assert network is idle.
        #[arg(long)]
        assert_network_idle: bool,
    },
    /// Fetch a URL, parse, execute <script> tags, then render.
    /// End-to-end SPA rendering pipeline.
    RenderUrl {
        url: String,
        #[arg(long, default_value_t = 80)]
        width: usize,
        /// Skip <script> execution (render only the static HTML).
        #[arg(long)]
        no_js: bool,
        /// M12.1: write rendered ASCII as a PNG screenshot to this path.
        #[arg(long)]
        screenshot: Option<PathBuf>,
        /// M29.3: limit screenshot height (pixels). If rendered height > max-height,
        /// truncate from top (bottom content discarded).
        #[arg(long)]
        max_height: Option<usize>,
        /// M18.2: after rendering, assert network is idle (no pending
        /// timers / fetches). Exits non-zero if SPA left work pending.
        /// No-op with --no-js.
        #[arg(long)]
        assert_network_idle: bool,
    },
    /// Fetch a URL, render it, and display the result in a GUI window.
    /// End-to-end browser-like experience. Requires a display server
    /// (won't work in headless CI / SSH sessions without X forwarding).
    Open {
        url: String,
        #[arg(long, default_value_t = 80)]
        width: usize,
        #[arg(long, default_value_t = 3)]
        scale: usize,
        #[arg(long, default_value_t = 1024)]
        win_width: u32,
        #[arg(long, default_value_t = 768)]
        win_height: u32,
        #[arg(long)]
        no_js: bool,
        /// Don't open a window — just verify the render pipeline
        /// succeeds and print the resulting text to stdout. Used
        /// by e2e tests in headless environments.
        #[arg(long)]
        check: bool,
    },
}

async fn run() -> Result<()> {
    let cli = Cli::parse();
    // M21.2: 加载 cookie 文件（如果 --cookie-file 指定且文件存在）。
    // 用 RAII guard 确保 run 退出时（无论成功还是 ? 提前返回）
    // 都把更新后的 jar save 回文件（登录态持久化）。
    let mut _cookie_guard = cli.cookie_file.as_ref().and_then(|path: &PathBuf| {
        load_cookie_file(path);
        // load 后 current_cookie_jar() 应非空（ensure_cookie_jar 已调）
        current_cookie_jar().map(|jar| CookieFileGuard {
            path: path.clone(),
            jar,
        })
    });
    let result = run_cmd(cli.cmd).await;
    // M21.2: 命令执行后，把最新 jar 同步到 guard（保留新获取的 cookie）。
    if let Some(g) = _cookie_guard.as_mut() {
        g.sync_jar();
    }
    result
}

/// M21.2: RAII guard，Drop 时把 cookie jar save 回文件。
/// 确保 `?` 提前返回时也持久化登录态。
struct CookieFileGuard {
    path: PathBuf,
    /// M21.2: 持有 jar 的 owned clone，不受 thread-local 清空影响。
    jar: browser_cookie::CookieHandle,
}

impl Drop for CookieFileGuard {
    fn drop(&mut self) {
        // M21.2: 始终 save。注意：保存 self.jar（owned clone），而非 current_cookie_jar()，
        // 因为 run_scripts_with_base 的 TreeGuard::drop 会清空 thread-local slot。
        if let Err(e) = save_jar_to_file(&self.jar, &self.path) {
            eprintln!("[cookie] failed to save {}: {e}", self.path.display());
        } else {
            eprintln!("[cookie] saved {}", self.path.display());
        }
    }
}

impl CookieFileGuard {
    /// M21.2: 在 run 退出前手动 flush，把最新 jar 同步到 self.jar。
    /// 因 thread-local jar 可能被 TreeGuard::drop 清空，需在命令执行后、
    /// guard drop 前同步一次。
    fn sync_jar(&mut self) {
        if let Some(jar) = current_cookie_jar() {
            self.jar = jar;
        }
    }
}

/// M21.2: 加载 cookie 文件到当前 jar（文件不存在视为空 jar，返回 false）。
fn load_cookie_file(path: &std::path::Path) -> bool {
    ensure_cookie_jar();
    if !path.exists() {
        eprintln!("[cookie] {} not found, starting fresh", path.display());
        return false;
    }
    if let Some(jar) = current_cookie_jar() {
        match load_from_file_shared(&jar, path) {
            Ok(()) => {
                eprintln!("[cookie] loaded {}", path.display());
                true
            }
            Err(e) => {
                eprintln!("[cookie] load {} failed: {e}", path.display());
                false
            }
        }
    } else {
        false
    }
}

/// 把 Cmd match 拆出，让 run() 能套 cookie guard。
async fn run_cmd(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::ImageAscii {
            file,
            width,
            height,
        } => {
            let ascii = img_ascii::image_to_ascii(&file, width, height)
                .map_err(|e| anyhow!("image-ascii failed: {e}"))?;
            print!("{ascii}");
            Ok(())
        }
        Cmd::Parse { file } => {
            let html = std::fs::read_to_string(&file)
                .with_context(|| format!("failed to read {}", file.display()))?;
            let tree = parse_html(&html);
            println!("{}", pretty_print(&tree));
            Ok(())
        }
        Cmd::Get { url } => {
            ensure_cookie_jar();
            let html = fetch_with_jar(&url).await?;
            let tree = parse_html(&html);
            println!("{}", pretty_print(&tree));
            Ok(())
        }
        Cmd::RenderFile {
            file,
            width,
            screenshot,
            max_height,
        } => {
            let html = std::fs::read_to_string(&file)
                .with_context(|| format!("failed to read {}", file.display()))?;
            let text = render_html_to_string(&html, width, false, None)?;
            print!("{text}");
            if let Some(p) = screenshot {
                let colored = render_html_to_string_colored(&html, width, false, None)?;
                screenshot::render_text_to_png(&colored, &p, max_height)?;
                eprintln!("[screenshot] wrote {}", p.display());
            }
            Ok(())
        }
        Cmd::RenderScript {
            file,
            width,
            screenshot,
            max_height,
            assert_network_idle,
        } => {
            let html = std::fs::read_to_string(&file)
                .with_context(|| format!("failed to read {}", file.display()))?;
            let text = render_html_to_string(&html, width, true, None)?;
            print!("{text}");
            if let Some(p) = screenshot {
                let colored = render_html_to_string_colored(&html, width, true, None)?;
                screenshot::render_text_to_png(&colored, &p, max_height)
                    .map_err(|e| anyhow!("screenshot failed: {e}"))?;
                eprintln!("[screenshot] wrote {}", p.display());
            }
            // M18.2: 断言 networkidle。
            if assert_network_idle && !browser_js_runtime::is_network_idle() {
                eprintln!(
                    "[networkidle] NOT idle: {} pending timers, {} pending requests",
                    browser_js_runtime::pending_timers(),
                    browser_js_runtime::pending_requests()
                );
                return Err(anyhow!("network not idle after render"));
            }
            if assert_network_idle {
                eprintln!("[networkidle] OK");
            }
            Ok(())
        }
        Cmd::RenderUrl {
            url,
            width,
            no_js,
            screenshot,
            max_height,
            assert_network_idle,
        } => {
            ensure_cookie_jar();
            let html = fetch_with_jar(&url).await?;
            let base = if no_js { None } else { Some(url.clone()) };
            let (text, colored) = render_html_to_string_inner(&html, width, !no_js, base.clone())?;
            print!("{text}");
            if let Some(p) = screenshot {
                screenshot::render_text_to_png(&colored, &p, max_height)
                    .map_err(|e| anyhow!("screenshot failed: {e}"))?;
                eprintln!("[screenshot] wrote {}", p.display());
            }
            // M18.2: 断言 networkidle（爬虫调试用）。
            if assert_network_idle && !no_js {
                if !browser_js_runtime::is_network_idle() {
                    eprintln!(
                        "[networkidle] NOT idle: {} pending timers, {} pending requests",
                        browser_js_runtime::pending_timers(),
                        browser_js_runtime::pending_requests()
                    );
                    return Err(anyhow!("network not idle after render"));
                }
                eprintln!("[networkidle] OK");
            }
            Ok(())
        }
        Cmd::Open {
            url,
            width,
            scale,
            win_width,
            win_height,
            no_js,
            check,
        } => {
            ensure_cookie_jar();
            let html = fetch_with_jar(&url).await?;
            let base = if no_js { None } else { Some(url.clone()) };
            let text = render_html_to_string(&html, width, !no_js, base)?;
            if check {
                print!("{text}");
                return Ok(());
            }
            eprintln!("[browser] opening window {win_width}x{win_height}, scale={scale}");
            let config = browser_gui::WindowConfig {
                title: format!("browser — {url}"),
                width: win_width,
                height: win_height,
                scale,
                text,
            };
            browser_gui::run_window(config).map_err(|e| anyhow!("GUI error: {e}"))?;
            Ok(())
        }
    }
}

/// Shared render pipeline — prints ASCII to stdout.
/// M15.4: Fetch HTML sharing the current cookie jar (if installed).
/// 主请求带 Cookie 头 + 把响应 Set-Cookie 存入 jar，让后续 JS fetch
/// 能继承会话（解决百度等登录态反爬）。
async fn fetch_with_jar(url: &str) -> Result<String> {
    let jar = current_cookie_jar();
    let cookie_header = jar
        .as_ref()
        .and_then(|h| {
            url::Url::parse(url)
                .ok()
                .map(|u| h.borrow().to_cookie_header(&u))
        })
        .filter(|h: &String| !h.is_empty());
    let client = HttpClient::new();
    let (bytes, headers) = client
        .get_with_headers(url, cookie_header.as_deref())
        .await
        .with_context(|| format!("failed to fetch {url}"))?;
    // 把 Set-Cookie 存入 jar（如果有 jar）。
    if let Some(jar) = &jar {
        if let Ok(req_url) = url::Url::parse(url) {
            for value in headers.get_all("set-cookie").iter() {
                if let Ok(sc) = value.to_str() {
                    jar.borrow_mut().store_set_cookie(sc, &req_url);
                }
            }
        }
    }
    String::from_utf8(bytes).map_err(|e| anyhow!("response is not valid UTF-8: {e}"))
}

/// Shared render pipeline — returns ASCII text. Used by `render-*` (prints)
/// and `open` (passes to GUI window).
fn render_html_to_string(
    html: &str,
    width: usize,
    run_js: bool,
    base_url: Option<String>,
) -> Result<String> {
    Ok(render_html_to_string_inner(html, width, run_js, base_url)?.0)
}

/// M30: colored variant for screenshots — link text gets ANSI blue/underline,
/// which the PNG renderer parses to paint blue text.
fn render_html_to_string_colored(
    html: &str,
    width: usize,
    run_js: bool,
    base_url: Option<String>,
) -> Result<String> {
    Ok(render_html_to_string_inner(html, width, run_js, base_url)?.1)
}

/// M30: returns `(plain_text, colored_text)`. Parse + layout + JS run
/// exactly **once**; only the final render pass differs (plain vs colored).
/// Avoids double-executing JS when both stdout and screenshot are needed.
fn render_html_to_string_inner(
    html: &str,
    width: usize,
    run_js: bool,
    base_url: Option<String>,
) -> Result<(String, String)> {
    let tree = parse_html(html);
    let (shared_tree, executed) = if run_js {
        let (shared, n) = if base_url.is_some() {
            run_scripts_with_base(tree, base_url)
        } else {
            run_scripts(tree)
        };
        (shared, n)
    } else {
        use std::cell::RefCell;
        use std::rc::Rc;
        (Rc::new(RefCell::new(tree)), 0)
    };
    if run_js {
        eprintln!("[browser] {executed} script(s) executed");
    }
    // M7.1.6: extract <style> tag contents so they participate in
    // computed styles. Walks the DOM in-tree before the immutable
    // borrow for layout.
    let style_text = extract_style_text(&shared_tree.borrow());
    let sheet = parse_css(&style_text);
    let styles = compute_styles(&shared_tree.borrow(), &sheet);
    let mut layout = construct_layout_tree(&shared_tree.borrow(), &styles);
    run_layout(
        &mut layout,
        LayoutConfig {
            viewport_width: width as f32,
        },
    );
    let plain = render_ascii(&layout, width);
    let colored = render_ascii_colored(&layout, width);
    // M22.2: 把 [IMG: src] 占位符替换为本地图像的 ASCII art。
    // http(s) URL 或不存在的文件 → 保留占位符（不报错，爬虫场景容错）。
    Ok((
        post_process_images(&plain, width),
        post_process_images(&colored, width),
    ))
}

/// M22.2: 扫描渲染输出里的 `[IMG: src]` 占位符，尝试把 src 解析为本地
/// 图像文件并解码成 ASCII art 替换。无法解析的保留原占位符。
///
/// base_dir 用 cwd（render-file/render-script 从文件读，cwd 是合理的
/// 相对基准）。max_w 用渲染宽度，max_h 用宽度的 1/3（图像高度通常小于文本）。
fn post_process_images(rendered: &str, width: usize) -> String {
    use browser_render::resolve_local_image_src;
    let cwd = std::env::current_dir().ok();
    let max_h = (width / 3).max(5) as u32;
    // M22.2: 跨行扫描 [IMG: ... ]。src 可能因折行被拆到多行，
    // ] 可能被 width 截断丢失。策略：find("[IMG:")（不带空格），
    // 向后扫描跳过空白（含换行）收集 src 字符，直到 ] 或段落边界
    // （连续 2 个换行 = 下一段文本）。src 去全部空白（路径无空格假设）。
    let chars: Vec<char> = rendered.chars().collect();
    let mut out = String::with_capacity(rendered.len());
    let mut i = 0;
    while i < chars.len() {
        // 匹配 [IMG:
        if i + 5 <= chars.len()
            && chars[i] == '['
            && chars[i + 1] == 'I'
            && chars[i + 2] == 'M'
            && chars[i + 3] == 'G'
            && chars[i + 4] == ':'
        {
            // 扫描 src：跳过空白，收集非空白字符，直到 ] 或段落边界
            let mut j = i + 5;
            let mut src_raw = String::new();
            let mut found_close = false;
            let mut blank_lines = 0u32;
            while j < chars.len() {
                let c = chars[j];
                if c == ']' {
                    found_close = true;
                    break;
                }
                if c == '\n' {
                    // 检测段落边界（连续换行）
                    if j + 1 < chars.len() && chars[j + 1] == '\n' {
                        blank_lines += 1;
                        if blank_lines >= 1 {
                            break; // ] 丢失，到段落边界
                        }
                    }
                    src_raw.push(c);
                    j += 1;
                    continue;
                }
                src_raw.push(c);
                j += 1;
            }
            let src: String = src_raw.chars().filter(|c| !c.is_whitespace()).collect();
            let advance = if found_close { j + 1 } else { j };
            if src.is_empty() {
                // 空 src：原样输出已扫描部分
                out.push_str(&chars[i..advance].iter().collect::<String>());
                i = advance;
                continue;
            }
            // 尝试解析 + 解码
            match resolve_local_image_src(&src, cwd.as_deref()) {
                Some(path) => match browser_render::image_file_to_ascii(&path, width as u32, max_h)
                {
                    Ok(ascii) => {
                        out.push_str("\n┌─ image: ");
                        out.push_str(&src);
                        out.push_str(" ─\n");
                        out.push_str(ascii.trim_end_matches('\n'));
                        out.push_str("\n└──────────────\n");
                        eprintln!("[img] rendered {src} as ASCII");
                    }
                    Err(e) => {
                        eprintln!("[img] decode {src} failed: {e}");
                        out.push_str(&format!("[IMG: {src}]"));
                    }
                },
                None => {
                    out.push_str(&format!("[IMG: {src}]"));
                }
            }
            i = advance;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

/// Walk a DOM tree and concatenate every `<style>` tag's text content
/// into one CSS string. Used by [`render_html_to_string`] so that
/// inline `<style>` rules participate in computed styles.
fn extract_style_text(tree: &browser_dom::Tree) -> String {
    use browser_dom::{NodeData, NodeId};
    let mut buf = String::new();
    let mut stack: Vec<NodeId> = vec![tree.root()];
    while let Some(id) = stack.pop() {
        match tree.data(id) {
            NodeData::Element { tag, .. } if tag.eq_ignore_ascii_case("style") => {
                for &child in tree.children_of(id) {
                    if let NodeData::Text(s) = tree.data(child) {
                        buf.push_str(s);
                        buf.push('\n');
                    }
                }
            }
            _ => {
                for &child in tree.children_of(id) {
                    stack.push(child);
                }
            }
        }
    }
    buf
}

fn main() -> ExitCode {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("failed to build tokio runtime");
    match rt.block_on(run()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn ping() {
        assert!("browser-cli".contains('-'));
    }

    #[test]
    fn render_pipeline_static_html_outputs_text() {
        let html = "<html><body><p>hello</p></body></html>";
        let tree = browser_html_parser::parse(html);
        let sheet = browser_css_engine::parse("");
        let styles = browser_css_engine::compute_styles(&tree, &sheet);
        let mut layout = browser_layout::construct_layout_tree(&tree, &styles);
        browser_layout::layout(
            &mut layout,
            browser_layout::LayoutConfig {
                viewport_width: 80.0,
            },
        );
        let out = browser_render::render_ascii(&layout, 80);
        assert!(out.contains("hello"), "got:\n{out}");
    }

    #[test]
    fn render_pipeline_with_script_runs_js_and_renders_dynamic() {
        let html = r#"<html><body>
            <script>__setBody("dynamic from js")</script>
        </body></html>"#;
        // Use the same pipeline the CLI does.
        let tree = browser_html_parser::parse(html);
        let (shared, executed) = browser_js_runtime::run_scripts(tree);
        assert!(executed >= 1);
        let borrowed = shared.borrow();
        let sheet = browser_css_engine::parse("");
        let styles = browser_css_engine::compute_styles(&borrowed, &sheet);
        let mut layout = browser_layout::construct_layout_tree(&borrowed, &styles);
        browser_layout::layout(
            &mut layout,
            browser_layout::LayoutConfig {
                viewport_width: 80.0,
            },
        );
        let out = browser_render::render_ascii(&layout, 80);
        assert!(out.contains("dynamic from js"), "got:\n{out}");
    }
}
