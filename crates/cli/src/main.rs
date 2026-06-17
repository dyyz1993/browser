//! `browser-cli` — command-line entry point.
//!
//! Subcommands:
//! - `browser parse <file>`           parse a local HTML file, print DOM tree
//! - `browser get  <url>`             fetch a URL via HTTPS, parse, print DOM tree
//! - `browser render-file <file>`     parse + layout + render a local HTML file
//! - `browser render-script <file>`   parse + execute scripts + layout + render
//! - `browser render-url  <url>`      fetch + parse + execute scripts + render

mod img_ascii;
mod sandbox;
mod screenshot;

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{anyhow, Context, Result};
use browser_cookie::{load_from_file_shared, save_jar_to_file};
use browser_css_engine::{compute_styles, parse as parse_css};
use browser_dom::pretty_print;
use browser_html_parser::parse as parse_html;
use browser_js_runtime::{
    current_cookie_jar, ensure_cookie_jar, run_scripts, run_scripts_with_base, try_csr_fallback,
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
        /// M-cls.1: JS 渲染子进程内存硬上限（MB）。vendor bundle 爆涨时内核
        /// 在子进程内杀掉自己，父进程走 CSR 兜底。0 = 禁用沙箱（进程内渲染）。
        #[arg(long, default_value_t = sandbox::DEFAULT_JS_MEMORY_LIMIT_MB)]
        js_memory_limit_mb: u64,
    },
    /// M59: Fetch a URL, render it (SPA-aware), then extract structured content.
    /// Acts as a curl-like scraper for SPA pages. Output format is controlled
    /// by --format (markdown|html|text|links).
    Fetch {
        url: String,
        /// Output format: markdown (default), html, text, or links.
        #[arg(long, default_value = "markdown")]
        format: String,
        /// CSS selector to extract only matching subtrees (e.g. "table tr").
        #[arg(long)]
        selector: Option<String>,
        /// Strip navigation/footer/ads noise (Firecrawl-style). Default true.
        /// Pass --only-main-content=false to keep full page.
        #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
        only_main_content: bool,
        /// Skip <script> execution (faster for known-static pages).
        #[arg(long)]
        no_js: bool,
        /// Smart mode: try --no-js first (fast SSR), fall back to full JS if
        /// content is too sparse. Best of both worlds for unknown sites.
        #[arg(long)]
        smart: bool,
        /// Output structured JSON {url, title, content} instead of raw content.
        #[arg(long)]
        json: bool,
        /// Render width (affects text wrapping for text/markdown).
        #[arg(long, default_value_t = 80)]
        width: usize,
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
    /// **M42**: Start a CDP (Chrome DevTools Protocol) server. Lets external
    /// tools like Puppeteer/Playwright drive the browser over WebSocket.
    /// Run `browser cdp`, then connect a CDP client to ws://127.0.0.1:9222.
    Cdp {
        /// Port to listen on (Chrome default: 9222).
        #[arg(long, default_value_t = browser_cdp::server::DEFAULT_CDP_PORT)]
        port: u16,
    },
    /// M-cls.1: 内部隐藏子命令 —— 在 RLIMIT_AS 受限的子进程里跑一次 JS
    /// 渲染。父进程（render-url/open）通过 sandbox 模块 spawn 它，stdin 传
    /// `base_url \x1f width \x1f html`，stdout 收回渲染文本。用户不应直接调用。
    #[command(hide = true)]
    JsRender {
        /// 子进程 RLIMIT_AS 上限（MB）。
        #[arg(long, default_value_t = sandbox::DEFAULT_JS_MEMORY_LIMIT_MB)]
        mem_mb: u64,
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
            // M37: render-file 现在执行 JS + 等待异步（setTimeout/fetch/XHR/WS）。
            // 之前 run_js=false 导致 SPA 动态内容永远不渲染。
            let text = render_html_to_string(&html, width, true, None)?;
            print!("{text}");
            if let Some(p) = screenshot {
                let colored = render_html_to_string_colored(&html, width, true, None)?;
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
            js_memory_limit_mb,
        } => {
            ensure_cookie_jar();
            let html = fetch_with_jar(&url).await?;
            let base = if no_js { None } else { Some(url.clone()) };
            // M-cls.1: 网络 HTML 走子进程沙箱（RLIMIT_AS 硬上限）。vendor
            // bundle 爆涨时被内核杀掉，返回 None → 落回进程内渲染（仍会跑
            // CSR 兜底拿到正文）。no_js 或显式禁用沙箱时直接进程内渲染。
            let (text, colored) = if !no_js && js_memory_limit_mb > 0 {
                match sandbox::run_js_render_in_sandbox(&html, &url, width, js_memory_limit_mb) {
                    Ok(Some(text)) => {
                        // 沙箱只返回纯文本；colored 仅截图用，按需在进程内补算。
                        let colored = if screenshot.is_some() {
                            render_html_to_string_colored(&html, width, true, base.clone())?
                        } else {
                            String::new()
                        };
                        (text, colored)
                    }
                    Ok(None) => {
                        // 沙箱失败 = JS 太重（OOM 被 kill）或超时。**不重跑 JS**
                        // （会再次 OOM），改为渲染静态壳 + CSR 数据兜底拿正文。
                        eprintln!("[sandbox] JS render failed/OOM → static shell + CSR fallback (no JS re-run)");
                        render_html_to_string_inner_ex(&html, width, false, true, base.clone())?
                    }
                    Err(e) => {
                        eprintln!("[sandbox] infra error: {e}; falling back to in-process render");
                        render_html_to_string_inner_ex(&html, width, false, true, base.clone())?
                    }
                }
            } else {
                render_html_to_string_inner(&html, width, !no_js, base.clone())?
            };
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
        Cmd::Fetch {
            url,
            format,
            selector,
            only_main_content,
            no_js,
            smart,
            json,
            width: _width,
        } => {
            ensure_cookie_jar();
            let fetch_start = std::time::Instant::now();
            let html = fetch_with_jar(&url).await?;
            // M61: --smart 模式。先 no-js 快速提 SSR（<1s），内容够就直接返回，
            // 不够再跑完整 JS。对有 SSR 的站点省去 JS 执行（快 + 省 memory），
            // 对纯 CSR 站点自动回退跑 JS。兼顾速度和覆盖率。
            let effective_no_js = if smart {
                let quick = extract_fetch(
                    &html,
                    &url,
                    &format,
                    &selector,
                    only_main_content,
                    true,
                    json,
                )?;
                // 阈值：no-js 提取出的可见内容 < 500 字符认为「太稀疏」，需跑 JS。
                if quick.chars().count() >= 500 {
                    eprintln!(
                        "[smart] SSR content sufficient ({} chars), skipping JS",
                        quick.chars().count()
                    );
                    print!("{quick}");
                    println!();
                    use std::io::Write;
                    let _ = std::io::stdout().flush();
                    return Ok(());
                }
                eprintln!(
                    "[smart] SSR too sparse ({} chars < 500), falling back to full JS render",
                    quick.chars().count()
                );
                false // 跑 JS
            } else {
                no_js
            };
            let base = if effective_no_js {
                None
            } else {
                Some(url.clone())
            };
            let tree = parse_html(&html);
            let shared: browser_js_runtime::SharedTree = if effective_no_js {
                use std::cell::RefCell;
                use std::rc::Rc;
                Rc::new(RefCell::new(tree))
            } else {
                let (shared, executed) =
                    browser_js_runtime::run_scripts_with_base(tree, base.clone());
                eprintln!("[browser] {executed} script(s) executed");
                shared
            };
            let out_format = browser_extractor::OutputFormat::parse(&format)
                .map_err(|e| anyhow!("invalid --format: {e}"))?;
            let opts = browser_extractor::FetchOptions {
                format: out_format,
                selector: selector.clone(),
                only_main_content,
            };
            let result = browser_extractor::run_extract(&shared.borrow(), base.as_deref(), &opts)
                .map_err(|e| anyhow!("extract failed: {e}"))?;
            // M59: 启发式提示——重 JS 站 boa 渲染慢/失败时，--no-js 取 SSR 兜底常更快。
            if !no_js {
                let content_len = result.content.trim().len();
                if content_len < 50 {
                    eprintln!(
                        "[hint] output nearly empty after JS — try --no-js for SSR fallback (boa may have failed on this SPA)"
                    );
                } else if fetch_start.elapsed().as_secs() >= 20 {
                    eprintln!(
                        "[hint] render slow — for heavy-JS sites, --no-js may be faster if the site has SSR content"
                    );
                }
            }
            if json {
                let title = json_escape(result.title.as_deref().unwrap_or(""));
                let content_field = json_escape(&result.content);
                println!(
                    "{{\"url\":\"{url}\",\"title\":\"{title}\",\"content\":\"{content_field}\"}}"
                );
            } else {
                print!("{}", result.content);
                println!();
                // M59: 显式 flush stdout，确保管道/重定向场景（> file / | grep）
                // 下输出不丢失。print! 是行缓冲，进程退出通常 flush，但网络/JS
                // 错误路径下可能提前 return，导致缓冲区未刷。
                use std::io::Write;
                let _ = std::io::stdout().flush();
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
        Cmd::Cdp { port } => {
            // M42: start the CDP server. Blocks forever (listen loop).
            browser_cdp::server::CdpServer::listen(port)
                .await
                .map_err(|e| anyhow!("CDP server error: {e}"))?;
            Ok(())
        }
        Cmd::JsRender { mem_mb } => {
            // M-cls.1: 子进程入口。先设内存硬上限，再读 stdin 帧跑渲染。
            sandbox::apply_memory_limit(mem_mb).ok();
            sandbox_child_render()
        }
    }
}

/// M-cls.1: 子进程主体 —— 读 stdin 帧（`base_url \x1f width \x1f html`），
/// 跑一次"解析 + 执行脚本 + CSR 兜底 + 布局 + 渲染"，把纯文本写 stdout。
///
/// 在 RLIMIT_AS 受限的子进程里执行，所以即便页面 JS 在 boa 里爆涨，内核
/// 也会杀掉本进程而不波及父进程。任何错误都打 stderr + 以非 0 退出
/// （父进程据此走 CSR 兜底）。
fn sandbox_child_render() -> Result<()> {
    use std::io::{Read, Write};
    let mut payload = String::new();
    std::io::stdin()
        .read_to_string(&mut payload)
        .map_err(|e| anyhow!("sandbox: read stdin failed: {e}"))?;
    // 解析帧：base_url \x1f width \x1f html。html 末尾不含分隔符。
    let mut parts = payload.splitn(3, '\x1f');
    let base_url = parts.next().unwrap_or("").trim().to_string();
    let width_str = parts.next().unwrap_or("80").trim();
    let html = parts.next().unwrap_or("");
    let width: usize = width_str.parse().unwrap_or(80);
    let base = if base_url.is_empty() || base_url == "about:blank" {
        None
    } else {
        Some(base_url.clone())
    };
    // 复用主渲染管线（含 JS 执行 + CSR 兜底）。失败 → 非 0 退出，父进程兜底。
    let (plain, _colored) = render_html_to_string_inner(html, width, true, base)?;
    // 只写纯文本到 stdout（父进程直接打印）。colored 仅 screenshot 用，沙箱不走截图。
    let mut stdout = std::io::stdout();
    stdout
        .write_all(plain.as_bytes())
        .map_err(|e| anyhow!("sandbox: write stdout failed: {e}"))?;
    stdout
        .flush()
        .map_err(|e| anyhow!("sandbox: flush stdout failed: {e}"))?;
    Ok(())
}

/// Shared render pipeline — prints ASCII to stdout.
/// M15.4: Fetch HTML sharing the current cookie jar (if installed).
/// 主请求带 Cookie 头 + 把响应 Set-Cookie 存入 jar，让后续 JS fetch
/// 能继承会话（解决百度等登录态反爬）。
/// M59: minimal JSON string escaping (avoids serde_json dependency).
/// Escapes quotes, backslash, control chars. Good enough for --json output.
fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}

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
///
/// `run_js` = 是否执行 `<script>`；`csr_fallback` = 是否在 JS 后/跳过 JS 时
/// 尝试 CSR 数据兜底（M-cls.3）。两者解耦：沙箱 JS 失败时父进程会用
/// `run_js=false, csr_fallback=true` 重渲染——既不重跑会 OOM 的 JS，又能
/// 拿到 SSR 兜底正文。
fn render_html_to_string_inner(
    html: &str,
    width: usize,
    run_js: bool,
    base_url: Option<String>,
) -> Result<(String, String)> {
    render_html_to_string_inner_ex(html, width, run_js, run_js, base_url)
}

/// 同 [`render_html_to_string_inner`]，但 CSR 兜底开关独立于 run_js。
/// （M-cls.1 沙箱失败回退路径用 `run_js=false, csr_fallback=true`。）
fn render_html_to_string_inner_ex(
    html: &str,
    width: usize,
    run_js: bool,
    csr_fallback: bool,
    base_url: Option<String>,
) -> Result<(String, String)> {
    let tree = parse_html(html);
    let (shared_tree, executed) = if run_js {
        let (shared, n) = if base_url.is_some() {
            run_scripts_with_base(tree, base_url.clone())
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
    // M-cls.3: CSR 数据兜底。JS 跑完若仍是空壳（或 JS 被跳过），且页面是
    // 已知 CSR 站点，直接拉对应 SSR 数据页注入正文。仅在 csr_fallback 且
    // 有 base_url 时尝试。失败静默（best-effort，不阻断）。
    if csr_fallback {
        if let Some(bu) = base_url.as_deref() {
            match try_csr_fallback(&shared_tree, bu) {
                Ok(true) => eprintln!("[csr-fallback] injected data for {bu}"),
                Ok(false) => {}
                Err(e) => eprintln!("[csr-fallback] {bu}: {e}"),
            }
        }
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

/// M61: 提取 fetch 内容的辅助函数，供 --smart 模式复用。
/// 跑 fetch 管线（可选 JS），返回提取后的内容字符串（非 json 包装）。
fn extract_fetch(
    html: &str,
    url: &str,
    format: &str,
    selector: &Option<String>,
    only_main_content: bool,
    no_js: bool,
    _json: bool,
) -> Result<String, anyhow::Error> {
    let base = if no_js { None } else { Some(url.to_string()) };
    let tree = parse_html(html);
    let shared: browser_js_runtime::SharedTree = if no_js {
        use std::cell::RefCell;
        use std::rc::Rc;
        Rc::new(RefCell::new(tree))
    } else {
        let (shared, _) = browser_js_runtime::run_scripts_with_base(tree, base.clone());
        shared
    };
    let out_format = browser_extractor::OutputFormat::parse(format)
        .map_err(|e| anyhow!("invalid --format: {e}"))?;
    let opts = browser_extractor::FetchOptions {
        format: out_format,
        selector: selector.clone(),
        only_main_content,
    };
    let result = browser_extractor::run_extract(&shared.borrow(), base.as_deref(), &opts)
        .map_err(|e| anyhow!("extract failed: {e}"))?;
    Ok(result.content)
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
