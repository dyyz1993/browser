//! `browser-cli` — command-line entry point.
//!
//! Subcommands:
//! - `browser parse <file>`           parse a local HTML file, print DOM tree
//! - `browser get  <url>`             fetch a URL via HTTPS, parse, print DOM tree
//! - `browser render-file <file>`     parse + layout + render a local HTML file
//! - `browser render-script <file>`   parse + execute scripts + layout + render
//! - `browser render-url  <url>`      fetch + parse + execute scripts + render

mod ai;
mod img_ascii;
mod sandbox;
mod screenshot;

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{anyhow, Context, Result};
use browser_cookie::{load_from_file_shared, save_jar_to_file};
use browser_css_engine::{compute_styles, parse as parse_css};
use browser_dom::pretty_print;
use browser_html_parser::parse as parse_html;
use browser_js_runtime::{
    current_cookie_jar, drain_captured_console_events, drain_captured_js_errors,
    drain_captured_network_events, ensure_cookie_jar, try_csr_fallback,
};
use browser_layout::{layout as run_layout, LayoutConfig};
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
    /// M70.7: HTTP/HTTPS proxy URL (e.g. http://127.0.0.1:7890).
    /// Also respects https_proxy/http_proxy environment variables.
    #[arg(long, global = true)]
    proxy: Option<String>,
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
        /// M80: 截图渲染模式（ascii | pixel）。ascii = 字符网格 PNG（爬虫
        /// 契约，默认）；pixel = 2D 画布近似渲染，此时 --width 按 CSS px 解释。
        #[arg(long, default_value = "ascii")]
        render_mode: String,
        /// M81: 合成点击（可多次）。JS 跑完后、渲染/截图前按序执行：
        /// querySelector → 合成 MouseEvent click → 泵事件循环。
        /// 支持 CSS 选择器与 `text=xxx`（textContent 包含匹配）。
        #[arg(long = "click")]
        clicks: Vec<String>,
        /// M81: 合成悬停（可多次）。含义同 --click，事件换成浏览器标准
        /// hover 序列：mouseover → mouseenter（不冒泡）→ mousemove；
        /// 若此前 hover 过别的元素，先对旧元素派 mouseout/mouseleave。
        /// hover 后泵事件循环（展开菜单的异步内容需要 drain）。
        #[arg(long = "hover")]
        hovers: Vec<String>,
        /// M81.4: 合成聚焦（可多次）。focus → focusin（冒泡），
        /// 前一焦点元素派 blur → focusout。
        #[arg(long = "focus")]
        focuses: Vec<String>,
        /// M81.5: 键盘输入（可多次）。格式：--type "SELECTOR=TEXT"。
        /// 序列：focus → 逐字符 keydown/keypress/input/keyup → change。
        #[arg(long = "type")]
        type_args: Vec<String>,
        /// M81.6: 勾选/取消 checkbox/radio（可多次）。派发 click+change。
        #[arg(long = "check")]
        checks: Vec<String>,
        /// M81.6: 下拉选择（可多次）。格式：--select "SELECTOR=VALUE"。
        /// 派发 change 事件。
        #[arg(long = "select")]
        select_args: Vec<String>,
        /// M81.7: 滚动到元素/像素位置（可多次）。触发 scroll 事件 +
        /// IntersectionObserver 回调（lazy 内容加载）。
        #[arg(long = "scroll-to")]
        scroll_tos: Vec<String>,
        /// M81.8: 双击（可多次）。序列：click ×2 + dblclick。
        #[arg(long = "dblclick")]
        dblclicks: Vec<String>,
        /// M81.8: 右键（可多次）。派发 contextmenu 事件。
        #[arg(long = "contextmenu")]
        contextmenus: Vec<String>,
        /// M81.9: 拖放（可多次）。格式："源选择器>目标选择器"。
        /// 序列：dragstart→dragover→drop→dragend。
        #[arg(long = "drag")]
        drags: Vec<String>,
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
        /// M80: 截图渲染模式（ascii | pixel）。含义同 render-file。
        #[arg(long, default_value = "ascii")]
        render_mode: String,
        /// M81: 合成点击（可多次）。含义同 render-file。
        #[arg(long = "click")]
        clicks: Vec<String>,
        /// M81: 合成悬停（可多次）。含义同 render-file --hover。
        #[arg(long = "hover")]
        hovers: Vec<String>,
        /// M81.4: 合成聚焦（可多次）。
        #[arg(long = "focus")]
        focuses: Vec<String>,
        /// M81.5: 键盘输入（可多次，两个值：selector 和 text）。
        #[arg(long = "type")]
        type_args: Vec<String>,
        /// M81.6: 勾选/取消 checkbox/radio（可多次）。派发 click+change。
        #[arg(long = "check")]
        checks: Vec<String>,
        /// M81.6: 下拉选择（可多次）。格式：--select "SELECTOR=VALUE"。
        /// 派发 change 事件。
        #[arg(long = "select")]
        select_args: Vec<String>,
        /// M81.7: 滚动到元素/像素位置（可多次）。触发 scroll 事件 +
        /// IntersectionObserver 回调（lazy 内容加载）。
        #[arg(long = "scroll-to")]
        scroll_tos: Vec<String>,
        /// M81.8: 双击（可多次）。序列：click ×2 + dblclick。
        #[arg(long = "dblclick")]
        dblclicks: Vec<String>,
        /// M81.8: 右键（可多次）。派发 contextmenu 事件。
        #[arg(long = "contextmenu")]
        contextmenus: Vec<String>,
        /// M81.9: 拖放（可多次）。格式："源选择器>目标选择器"。
        /// 序列：dragstart→dragover→drop→dragend。
        #[arg(long = "drag")]
        drags: Vec<String>,
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
        /// M80: 截图渲染模式（ascii | pixel）。含义同 render-file。
        #[arg(long, default_value = "ascii")]
        render_mode: String,
        /// M18.2: after rendering, assert network is idle (no pending
        /// timers / fetches). Exits non-zero if SPA left work pending.
        /// No-op with --no-js.
        #[arg(long)]
        assert_network_idle: bool,
        /// M-cls.1: JS 渲染子进程内存硬上限（MB）。vendor bundle 爆涨时内核
        /// 在子进程内杀掉自己，父进程走 CSR 兜底。0 = 禁用沙箱（进程内渲染）。
        #[arg(long, default_value_t = sandbox::DEFAULT_JS_MEMORY_LIMIT_MB)]
        js_memory_limit_mb: u64,
        /// M66: JS engine (boa | quickjs). Default: quickjs.
        #[arg(long, default_value = "quickjs")]
        js_engine: String,
        /// M81: 合成点击（可多次）。JS 跑完后、渲染/截图前按序执行：
        /// querySelector → 合成 MouseEvent click → 泵事件循环。
        /// 支持 CSS 选择器与 `text=xxx`。仅 QuickJS 引擎生效。
        #[arg(long = "click")]
        clicks: Vec<String>,
        /// M81: 合成悬停（可多次）。含义同 render-file --hover。
        /// 仅 QuickJS 引擎生效。
        #[arg(long = "hover")]
        hovers: Vec<String>,
        /// M81.4: 合成聚焦（可多次）。
        #[arg(long = "focus")]
        focuses: Vec<String>,
        /// M81.5: 键盘输入（可多次，两个值：selector 和 text）。
        /// 序列：focus → 逐字符 keydown/keypress/input/keyup → change。
        #[arg(long = "type")]
        type_args: Vec<String>,
        /// M81.6: 勾选/取消 checkbox/radio（可多次）。派发 click+change。
        #[arg(long = "check")]
        checks: Vec<String>,
        /// M81.6: 下拉选择（可多次）。格式：--select "SELECTOR=VALUE"。
        /// 派发 change 事件。
        #[arg(long = "select")]
        select_args: Vec<String>,
        /// M81.7: 滚动到元素/像素位置（可多次）。触发 scroll 事件 +
        /// IntersectionObserver 回调（lazy 内容加载）。
        #[arg(long = "scroll-to")]
        scroll_tos: Vec<String>,
        /// M81.8: 双击（可多次）。序列：click ×2 + dblclick。
        #[arg(long = "dblclick")]
        dblclicks: Vec<String>,
        /// M81.8: 右键（可多次）。派发 contextmenu 事件。
        #[arg(long = "contextmenu")]
        contextmenus: Vec<String>,
        /// M81.9: 拖放（可多次）。格式："源选择器>目标选择器"。
        /// 序列：dragstart→dragover→drop→dragend。
        #[arg(long = "drag")]
        drags: Vec<String>,
    },
    /// M59: Fetch a URL, render it (SPA-aware), then extract structured content.
    /// Acts as a curl-like scraper for SPA pages. Output format is controlled
    /// by --format (markdown|html|text|links).
    Fetch {
        url: String,
        /// Output format: markdown (default), html, text, links, images, highlights, branding.
        #[arg(long, default_value = "markdown")]
        format: String,
        /// M70.8: AI summary (calls external pi CLI). Equivalent to Firecrawl "Summary" format.
        #[arg(long)]
        ai_summarize: bool,
        /// M70.8: AI question-answering (calls external pi CLI). Equivalent to Firecrawl "Question" format.
        #[arg(long)]
        ai_question: Option<String>,
        /// M70.8: AI provider (env: AI_PROVIDER, default: opencode-go).
        #[arg(long)]
        ai_provider: Option<String>,
        /// M70.8: AI model (env: AI_MODEL, default: deepseek-v4-flash).
        #[arg(long)]
        ai_model: Option<String>,
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
        /// Output structured JSON {url, title, content} instead of raw content.
        #[arg(long)]
        json: bool,
        /// Render width (affects text wrapping for text/markdown).
        #[arg(long, default_value_t = 80)]
        width: usize,
        /// M65: Profile mode——报告各阶段 RSS 内存 + 耗时到 stderr。
        /// 阶段：fetch HTML / parse / JS eval / event loop / serialize / total
        #[arg(long)]
        profile: bool,
        /// M66: JS engine selection (boa | quickjs). Default: quickjs.
        #[arg(long, default_value = "quickjs")]
        js_engine: String,
        /// M57.6: Wait strategy for JS execution. Options: load (full event loop),
        /// dom-ready (after initial script execution), timeout (with --timeout-ms limit).
        #[arg(long, default_value = "load")]
        wait_strategy: String,
        /// M57.6: Max wait duration in ms for JS execution (only effective with
        /// --wait-strategy timeout). Default: 30000 (30s).
        #[arg(long, default_value_t = 30000)]
        timeout_ms: u64,
    },
    /// Fetch a URL, render it, and display the result in a GUI window.
    /// Requires the `gui` feature (`--features gui`) and a display server
    /// (won't work in headless CI / SSH sessions without X forwarding).
    /// Without the feature, `--check` still works; window mode errors out.
    /// M81.E1: gui 默认不编译——容器部署零图形依赖。
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
        /// M66: JS engine (boa | quickjs). Default: quickjs.
        #[arg(long, default_value = "quickjs")]
        js_engine: String,
    },
    /// **M42**: Start a CDP (Chrome DevTools Protocol) server. Lets external
    /// tools like Puppeteer/Playwright drive the browser over WebSocket.
    /// Run `browser cdp`, then connect a CDP client to ws://127.0.0.1:9222.
    Cdp {
        /// Port to listen on (Chrome default: 9222).
        #[arg(long, default_value_t = browser_cdp::server::DEFAULT_CDP_PORT)]
        port: u16,
        /// M67: JS engine for Runtime.evaluate/callFunctionOn (boa | quickjs).
        /// Default: quickjs (aligns with render-url/fetch/open). The CDP Runtime
        /// domain is the last path still bound to boa pre-M67.
        #[arg(long, default_value = "quickjs")]
        js_engine: String,
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

    /// M70.12: 启动 HTTP API 服务。
    Serve {
        #[arg(long, default_value_t = 8080)]
        port: u16,
        #[arg(long, default_value = "0.0.0.0")]
        bind: String,
        /// M70.17: 最大并发请求数。每并发 = 1 个 serve-child 子进程（~22MB）。
        /// 默认 3（峰值 ~86MB = 主 20MB + 3×22MB）。
        #[arg(long, default_value_t = 3)]
        max_concurrency: usize,
    },
    /// M70.14: serve 子进程——RLIMIT_AS 隔离 QuickJS C 层 abort。
    #[command(hide = true)]
    ServeChild {
        #[arg(long, default_value_t = sandbox::DEFAULT_JS_MEMORY_LIMIT_MB)]
        mem_mb: u64,
    },
}

async fn run() -> Result<()> {
    let cli = Cli::parse();
    // M70.7: 应用 --proxy（若指定）。reqwest 默认读环境变量，这里设了后续所有
    // HTTP 请求都会走代理。优先级：--proxy > 已有环境变量。
    if let Some(ref proxy_url) = cli.proxy {
        if std::env::var("https_proxy").is_err() {
            std::env::set_var("https_proxy", proxy_url);
        }
        if std::env::var("http_proxy").is_err() {
            std::env::set_var("http_proxy", proxy_url);
        }
    }
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
            render_mode,
            clicks,
            hovers,
            focuses,
            type_args,
            checks,
            select_args,
            scroll_tos,
            dblclicks,
            contextmenus,
            drags,
        } => {
            let html = std::fs::read_to_string(&file)
                .with_context(|| format!("failed to read {}", file.display()))?;
            // M81: --click/--hover 表达式（同会话合成事件；空列表 = 原行为）。
            let post_exprs = click_post_exprs(&clicks)
                .into_iter()
                .chain(hover_post_exprs(&hovers))
                .chain(focus_post_exprs(&focuses))
                .chain(type_post_exprs(&type_args))
                .chain(check_post_exprs(&checks))
                .chain(select_post_exprs(&select_args))
                .chain(scroll_post_exprs(&scroll_tos))
                .chain(dblclick_post_exprs(&dblclicks))
                .chain(contextmenu_post_exprs(&contextmenus))
                .chain(drag_post_exprs(&drags))
                .collect::<Vec<_>>();
            // M80: pixel 模式——width 按 CSS px 解释，单次 JS，stdout 仍输出
            // ASCII 便于 pipe。无 --screenshot 时 pixel 无意义，退回 ASCII。
            if render_mode == "pixel" {
                let text = match &screenshot {
                    Some(p) => render_pixel_screenshot(
                        &html,
                        width,
                        true,
                        None,
                        "boa",
                        p,
                        max_height,
                        &post_exprs,
                    )?,
                    None => render_html_to_string_with_post_exprs(
                        &html,
                        width,
                        true,
                        None,
                        "boa",
                        &post_exprs,
                    )?,
                };
                print!("{text}");
                return Ok(());
            }
            // M37: render-file 现在执行 JS + 等待异步（setTimeout/fetch/XHR/WS）。
            // 之前 run_js=false 导致 SPA 动态内容永远不渲染。
            let text = render_html_to_string_with_post_exprs(
                &html,
                width,
                true,
                None,
                "boa",
                &post_exprs,
            )?;
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
            render_mode,
            clicks,
            hovers,
            focuses,
            type_args,
            checks,
            select_args,
            scroll_tos,
            dblclicks,
            contextmenus,
            drags,
            assert_network_idle,
        } => {
            let html = std::fs::read_to_string(&file)
                .with_context(|| format!("failed to read {}", file.display()))?;
            // M81: --click/--hover 表达式（同会话合成事件；空列表 = 原行为）。
            let post_exprs = click_post_exprs(&clicks)
                .into_iter()
                .chain(hover_post_exprs(&hovers))
                .chain(focus_post_exprs(&focuses))
                .chain(type_post_exprs(&type_args))
                .chain(check_post_exprs(&checks))
                .chain(select_post_exprs(&select_args))
                .chain(scroll_post_exprs(&scroll_tos))
                .chain(dblclick_post_exprs(&dblclicks))
                .chain(contextmenu_post_exprs(&contextmenus))
                .chain(drag_post_exprs(&drags))
                .collect::<Vec<_>>();
            // M80: pixel 模式（含义同 render-file；网络空闲断言两条路都跑）。
            if render_mode == "pixel" {
                let text = match &screenshot {
                    Some(p) => render_pixel_screenshot(
                        &html,
                        width,
                        true,
                        None,
                        "boa",
                        p,
                        max_height,
                        &post_exprs,
                    )?,
                    None => render_html_to_string_with_post_exprs(
                        &html,
                        width,
                        true,
                        None,
                        "boa",
                        &post_exprs,
                    )?,
                };
                print!("{text}");
            } else {
                let text = render_html_to_string_with_post_exprs(
                    &html,
                    width,
                    true,
                    None,
                    "boa",
                    &post_exprs,
                )?;
                print!("{text}");
                if let Some(p) = screenshot {
                    let colored = render_html_to_string_colored(&html, width, true, None)?;
                    screenshot::render_text_to_png(&colored, &p, max_height)
                        .map_err(|e| anyhow!("screenshot failed: {e}"))?;
                    eprintln!("[screenshot] wrote {}", p.display());
                }
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
            render_mode,
            assert_network_idle,
            js_memory_limit_mb,
            js_engine,
            clicks,
            hovers,
            focuses,
            type_args,
            checks,
            select_args,
            scroll_tos,
            dblclicks,
            contextmenus,
            drags,
        } => {
            ensure_cookie_jar();
            let html = fetch_with_jar(&url).await?;
            let base = if no_js { None } else { Some(url.clone()) };
            // M81: --click/--hover 表达式（同会话合成事件；空列表 = 原行为）。
            let post_exprs = click_post_exprs(&clicks)
                .into_iter()
                .chain(hover_post_exprs(&hovers))
                .chain(focus_post_exprs(&focuses))
                .chain(type_post_exprs(&type_args))
                .chain(check_post_exprs(&checks))
                .chain(select_post_exprs(&select_args))
                .chain(scroll_post_exprs(&scroll_tos))
                .chain(dblclick_post_exprs(&dblclicks))
                .chain(contextmenu_post_exprs(&contextmenus))
                .chain(drag_post_exprs(&drags))
                .collect::<Vec<_>>();
            // M80: pixel 模式——width 按 CSS px 解释；走进程内渲染
            // （沙箱子进程只回传文本、没有布局树，无法做像素光栅化）。
            if render_mode == "pixel" {
                let text = match &screenshot {
                    Some(p) => render_pixel_screenshot(
                        &html,
                        width,
                        !no_js,
                        base.clone(),
                        &js_engine,
                        p,
                        max_height,
                        &post_exprs,
                    )?,
                    None => {
                        let cols = browser_render::layout_columns_for_px(width);
                        let (layout, _styles) = layout_tree_after_js_engine(
                            &html,
                            cols,
                            !no_js,
                            false,
                            base.clone(),
                            &js_engine,
                            true,
                            browser_render::pixel::cell_metrics().1,
                            &post_exprs,
                        )?;
                        render_ascii(&layout, cols)
                    }
                };
                print!("{text}");
            } else {
                // M-cls.1: 网络 HTML 走子进程沙箱（RLIMIT_AS 硬上限）。
                // M66: QuickJS 引擎内存效率高，跳过沙箱直接进程内渲染。
                // M81: --click/--hover 需要同会话 post eval，跳过沙箱（子进程无法回传）。
                let (text, colored) = if !no_js
                    && js_memory_limit_mb > 0
                    && js_engine == "boa"
                    && post_exprs.is_empty()
                {
                    match sandbox::run_js_render_in_sandbox(&html, &url, width, js_memory_limit_mb)
                    {
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
                            eprintln!(
                                "[sandbox] infra error: {e}; falling back to in-process render"
                            );
                            render_html_to_string_inner_ex(&html, width, false, true, base.clone())?
                        }
                    }
                } else {
                    // M66: QuickJS 或 no_js → 直接进程内渲染
                    render_html_to_string_inner_ex_engine(
                        &html,
                        width,
                        !no_js,
                        false,
                        base.clone(),
                        &js_engine,
                        &post_exprs,
                    )?
                };
                print!("{text}");
                if let Some(p) = screenshot {
                    screenshot::render_text_to_png(&colored, &p, max_height)
                        .map_err(|e| anyhow!("screenshot failed: {e}"))?;
                    eprintln!("[screenshot] wrote {}", p.display());
                }
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
            json,
            width: _width,
            profile,
            js_engine,
            ai_summarize,
            ai_question,
            ai_provider,
            ai_model,
            wait_strategy,
            timeout_ms,
        } => {
            ensure_cookie_jar();
            let fetch_start = std::time::Instant::now();
            let html = fetch_with_jar(&url).await?;
            // M70.14: base_url 去掉 hash fragment——#/?id=xxx 是客户端路由，
            // 传给 JS 的 location.href 不应带 hash（否则相对路径 XHR 会拼到 hash 上）。
            // 与 serve / render_and_extract 保持一致。
            let base = if no_js {
                None
            } else {
                Some(
                    url::Url::parse(&url)
                        .map(|mut u| {
                            u.set_fragment(None);
                            u.to_string()
                        })
                        .unwrap_or_else(|_| url.clone()),
                )
            };
            let tree = parse_html(&html);
            // M70.5: `--format original-html` → 直接输出原始 HTML（JS 执行前），跳过整个渲染管线。
            if format.eq_ignore_ascii_case("original-html")
                || format.eq_ignore_ascii_case("original_html")
                || format.eq_ignore_ascii_case("pre-js")
            {
                println!("{html}");
                return Ok(());
            }
            // M65: profile 打点
            let rss = || -> u64 {
                // macOS: 用 mach_task_basic_info 拿 RSS（不依赖 /proc）
                #[cfg(target_os = "macos")]
                {
                    use std::mem;
                    let mut info: libc::mach_task_basic_info_data_t = unsafe { mem::zeroed() };
                    let mut count = (mem::size_of_val(&info) / mem::size_of::<libc::natural_t>())
                        as libc::mach_msg_type_number_t;
                    unsafe {
                        let kr = libc::task_info(
                            task_self(),
                            libc::MACH_TASK_BASIC_INFO,
                            &mut info as *mut _ as libc::task_info_t,
                            &mut count,
                        );
                        if kr == libc::KERN_SUCCESS {
                            return info.resident_size as u64 / 1024 / 1024;
                        }
                    }
                    0
                }
                #[cfg(not(target_os = "macos"))]
                {
                    // Linux: /proc/self/status VmRSS
                    std::fs::read_to_string("/proc/self/status")
                        .ok()
                        .and_then(|s| {
                            s.lines().find(|l| l.starts_with("VmRSS:")).and_then(|l| {
                                l.split_whitespace()
                                    .nth(1)
                                    .and_then(|n| n.parse::<u64>().ok())
                            })
                        })
                        .map(|kb| kb / 1024)
                        .unwrap_or(0)
                }
            };
            if profile {
                eprintln!(
                    "[profile] {:<20} {:>6}MB  {:>6.2}s",
                    "fetch+parse",
                    rss(),
                    fetch_start.elapsed().as_secs_f64()
                );
            }
            // M57.6: 解析等待策略。load = 全事件循环（默认），dom-ready = 初始脚本执行，
            // timeout = 带 --timeout-ms 时间上限。
            let wait_strategy_parsed = match wait_strategy.as_str() {
                "dom-ready" | "domready" => "dom-ready",
                "load" => "load",
                "timeout" => "timeout",
                other => {
                    eprintln!("[wait] unknown strategy '{other}', falling back to 'load'");
                    "load"
                }
            };
            if profile || wait_strategy_parsed != "load" {
                eprintln!("[wait] strategy={wait_strategy_parsed}, timeout={timeout_ms}ms");
            }
            let js_start = std::time::Instant::now();
            let shared: browser_js_runtime::SharedTree = if no_js {
                use std::cell::RefCell;
                use std::rc::Rc;
                Rc::new(RefCell::new(tree))
            } else {
                use std::panic::AssertUnwindSafe;
                let base_for_panic = base.clone();
                let engine_kind = browser_js_runtime::EngineKind::parse_str(&js_engine);
                #[cfg(feature = "quickjs")]
                let (shared, executed) = std::panic::catch_unwind(AssertUnwindSafe(|| {
                    browser_js_runtime::run_scripts_with_base_engine(
                        tree,
                        base_for_panic,
                        &engine_kind,
                    )
                }))
                .unwrap_or_else(|payload| {
                    let msg = payload
                        .downcast_ref::<String>()
                        .cloned()
                        .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_string()))
                        .unwrap_or_else(|| "unknown panic".to_string());
                    eprintln!("[fetch] JS engine panicked: {msg}");
                    let static_tree = parse_html(&html);
                    use std::cell::RefCell;
                    use std::rc::Rc;
                    (Rc::new(RefCell::new(static_tree)), 0)
                });
                #[cfg(not(feature = "quickjs"))]
                let (shared, executed) = std::panic::catch_unwind(AssertUnwindSafe(|| {
                    let _ = &engine_kind;
                    browser_js_runtime::run_scripts_with_base(tree, base_for_panic)
                }))
                .unwrap_or_else(|payload| {
                    let msg = payload
                        .downcast_ref::<String>()
                        .cloned()
                        .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_string()))
                        .unwrap_or_else(|| "unknown panic".to_string());
                    eprintln!("[fetch] JS engine panicked: {msg}");
                    let static_tree = parse_html(&html);
                    use std::cell::RefCell;
                    use std::rc::Rc;
                    (Rc::new(RefCell::new(static_tree)), 0)
                });
                eprintln!("[browser] {executed} script(s) executed");
                shared
            };
            // M57.6: 超时检查——若策略为 timeout 且执行超过限制，打警告不阻断。
            let js_elapsed = js_start.elapsed();
            if wait_strategy_parsed == "timeout" && js_elapsed.as_millis() > timeout_ms as u128 {
                eprintln!(
                    "[wait] timeout exceeded: {}ms > {}ms limit, returning current content",
                    js_elapsed.as_millis(),
                    timeout_ms
                );
            }
            if profile {
                eprintln!(
                    "[profile] {:<20} {:>6}MB  {:>6.2}s",
                    "JS eval+eventloop",
                    rss(),
                    js_elapsed.as_secs_f64()
                );
            }
            let extract_start = std::time::Instant::now();
            let out_format = if json {
                // --json 输出自身就是结构化格式，用 Text 格式提取内容
                // 避免 extractor JSON 在 CLI JSON 内被双重编码。
                browser_extractor::OutputFormat::Text
            } else {
                browser_extractor::OutputFormat::parse(&format)
                    .map_err(|e| anyhow!("invalid --format: {e}"))?
            };
            let opts = browser_extractor::FetchOptions {
                format: out_format,
                selector: selector.clone(),
                only_main_content,
            };
            let mut result =
                browser_extractor::run_extract(&shared.borrow(), base.as_deref(), &opts)
                    .map_err(|e| anyhow!("extract failed: {e}"))?;
            if profile {
                eprintln!(
                    "[profile] {:<20} {:>6}MB  {:>6.2}s",
                    "extract+serialize",
                    rss(),
                    extract_start.elapsed().as_secs_f64()
                );
                eprintln!(
                    "[profile] {:<20} {:>6}MB  {:>6.2}s",
                    "TOTAL",
                    rss(),
                    fetch_start.elapsed().as_secs_f64()
                );
                eprintln!("[profile] DOM nodes: {}", shared.borrow().len());
            }
            // M70.6: 启发式检测——JS 执行后输出比原始 HTML 还差（JS 搞坏了页面），
            // 自动回退到原始 HTML。
            if !no_js {
                // M71.5: 同量纲比较——JS 后纯文本 vs JS 前纯文本，
                // 而非 vs 原始 HTML 字节（HTML 标签开销通常占 80%+，
                // 旧逻辑 content < raw/5 对正常页面也成立，会误判 JS 搞坏了页面）。
                let content_len = result.content.trim().len();
                let static_tree = parse_html(&html);
                let static_res =
                    browser_extractor::run_extract(&static_tree, base.as_deref(), &opts);
                let static_len = static_res
                    .as_ref()
                    .map(|r| r.content.trim().len())
                    .unwrap_or(0);
                // 仅当 JS 后内容明显比 JS 前还少（丢了已有文本）才判定 JS 搞坏页面。
                // 阈值：static 文本足够（>200B）且 JS 后不足 static 的 1/3。
                if static_len > 200 && content_len < static_len / 3 {
                    eprintln!(
                        "[hint] JS output ({content_len}B) << pre-JS text ({static_len}B) \
                         — JS likely broke the page; falling back to original HTML"
                    );
                    if let Ok(static_r) = static_res {
                        if static_r.content.trim().len() > content_len {
                            result.content = static_r.content;
                            result.title = static_r.title;
                            eprintln!(
                                "[hint] JS output ({content_len}B) → static ({static}B)",
                                static = result.content.trim().len()
                            );
                        }
                    }
                } else if content_len < 50 {
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
                // M74: Drain capture queues after JS execution.
                let console_events = drain_captured_console_events();
                let js_errors = drain_captured_js_errors();
                let network_events = drain_captured_network_events();

                // Build JSON using serde_json (available in cli deps).
                let map = serde_json::json!({
                    "url": url,
                    "title": result.title,
                    "content": {
                        "text": result.content,
                    },
                    "console": console_events.iter().map(|e| serde_json::json!({
                        "level": e.level,
                        "text": e.text,
                    })).collect::<Vec<_>>(),
                    "errors": js_errors.iter().map(|e| serde_json::json!({
                        "message": e.message,
                        "stack": e.stack,
                    })).collect::<Vec<_>>(),
                    "network": network_events.iter().map(|e| serde_json::json!({
                        "url": e.url,
                        "method": e.method,
                        "status": e.status,
                        "mime_type": e.mime_type,
                        "body_size": e.body_size,
                    })).collect::<Vec<_>>(),
                });
                println!("{}", serde_json::to_string_pretty(&map).unwrap_or_default());
            } else {
                print!("{}", result.content);
                println!();
                // M59: 显式 flush stdout，确保管道/重定向场景（> file / | grep）
                // 下输出不丢失。print! 是行缓冲，进程退出通常 flush，但网络/JS
                // 错误路径下可能提前 return，导致缓冲区未刷。
                use std::io::Write;
                let _ = std::io::stdout().flush();
            }
            // M70.8: AI 后处理（摘要/问答），在输出完成后追加。
            if ai_summarize || ai_question.is_some() {
                let content = &result.content;
                if let Some(q) = &ai_question {
                    match crate::ai::ask_question(
                        content,
                        q,
                        ai_provider.as_deref(),
                        ai_model.as_deref(),
                    ) {
                        Ok(answer) => {
                            eprintln!("\n[AI] Question: {q}\n[AI] Answer: {answer}");
                        }
                        Err(e) => eprintln!("[AI] question failed: {e}"),
                    }
                }
                if ai_summarize {
                    match crate::ai::summarize(content, ai_provider.as_deref(), ai_model.as_deref())
                    {
                        Ok(summary) => {
                            eprintln!("\n[AI] Summary:\n{summary}");
                        }
                        Err(e) => eprintln!("[AI] summarize failed: {e}"),
                    }
                }
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
            js_engine,
        } => {
            ensure_cookie_jar();
            let html = fetch_with_jar(&url).await?;
            let base = if no_js { None } else { Some(url.clone()) };
            let (text, _colored) = render_html_to_string_inner_ex_engine(
                &html,
                width,
                !no_js,
                false,
                base,
                &js_engine,
                &[],
            )?;
            if check {
                print!("{text}");
                return Ok(());
            }
            #[cfg(feature = "gui")]
            {
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
            #[cfg(not(feature = "gui"))]
            {
                let _ = (scale, win_width, win_height);
                anyhow::bail!(
                    "本二进制未编译 GUI（M81.E1 起默认 headless）。\
                     窗口模式请重新构建：cargo build --release -p browser-cli --features gui"
                )
            }
        }
        Cmd::Cdp { port, js_engine } => {
            // M42: start the CDP server. Blocks forever (listen loop).
            // M67: engine_kind 透传到 Runtime domain（默认 quickjs）。
            let engine_kind = browser_js_runtime::EngineKind::parse_str(&js_engine);
            browser_cdp::server::CdpServer::listen(port, engine_kind)
                .await
                .map_err(|e| anyhow!("CDP server error: {e}"))?;
            Ok(())
        }
        Cmd::JsRender { mem_mb } => {
            // M-cls.1: 子进程入口。先设内存硬上限，再读 stdin 帧跑渲染。
            sandbox::apply_memory_limit(mem_mb).ok();
            sandbox_child_render()
        }
        Cmd::Serve {
            port,
            bind,
            max_concurrency,
        } => {
            serve(&bind, port, max_concurrency).await?;
            Ok(())
        }
        Cmd::ServeChild { mem_mb } => {
            sandbox::apply_memory_limit(mem_mb).ok();
            serve_child().await
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
#[allow(dead_code)]
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
    // M70.14: 网络重试——最多 3 次，间隔 500ms。
    // 底层通用方案，任何站点网络抖动都受益。
    let mut last_err = None;
    for attempt in 0..3u32 {
        match fetch_with_jar_once(url).await {
            Ok(html) => return Ok(html),
            Err(e) => {
                eprintln!("[net] attempt {attempt} failed: {e}");
                last_err = Some(e);
                if attempt < 2 {
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                }
            }
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow!("fetch failed after 3 attempts")))
}

async fn fetch_with_jar_once(url: &str) -> Result<String> {
    // M70.14: 去掉 hash fragment——#/?id=xxx 是客户端路由，服务端 fetch 不需要。
    let clean_url = url::Url::parse(url)
        .map(|mut u| {
            u.set_fragment(None);
            u.to_string()
        })
        .unwrap_or_else(|_| url.to_string());
    let jar = current_cookie_jar();
    let cookie_header = jar
        .as_ref()
        .and_then(|h| {
            url::Url::parse(&clean_url)
                .ok()
                .map(|u| h.borrow().to_cookie_header(&u))
        })
        .filter(|h: &String| !h.is_empty());
    let client = HttpClient::new();
    let (bytes, headers) = client
        .get_with_headers(&clean_url, cookie_header.as_deref())
        .await
        .with_context(|| format!("failed to fetch {url}"))?;
    // 把 Set-Cookie 存入 jar（如果有 jar）。
    if let Some(jar) = &jar {
        if let Ok(req_url) = url::Url::parse(&clean_url) {
            for value in headers.get_all("set-cookie").iter() {
                if let Ok(sc) = value.to_str() {
                    jar.borrow_mut().store_set_cookie(sc, &req_url);
                }
            }
        }
    }
    String::from_utf8(bytes).map_err(|e| anyhow!("response is not valid UTF-8: {e}"))
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
    render_html_to_string_inner_ex_engine(html, width, run_js, csr_fallback, base_url, "boa", &[])
}

/// M81: 带 `--click`/`--hover` 的渲染入口——post_exprs（合成点击/悬停
/// 表达式）传入 JS 会话，页面脚本 + 事件循环跑完后在同一会话内按序 eval
/// （addEventListener 监听器注册在会话内，引擎 drop 即失效），再 layout +
/// ASCII 渲染。
#[allow(clippy::too_many_arguments)]
fn render_html_to_string_with_post_exprs(
    html: &str,
    width: usize,
    run_js: bool,
    base_url: Option<String>,
    js_engine: &str,
    post_exprs: &[String],
) -> Result<String> {
    // 走 inner_ex_engine 完整渲染（含 [IMG]/[SVG] 占位符后处理，
    // 与无点击路径产物一致）。
    let (text, _colored) = render_html_to_string_inner_ex_engine(
        html, width, run_js, run_js, base_url, js_engine, post_exprs,
    )?;
    Ok(text)
}

/// M66: 引擎可切换版本的渲染入口。
#[allow(clippy::too_many_arguments)]
fn render_html_to_string_inner_ex_engine(
    html: &str,
    width: usize,
    run_js: bool,
    csr_fallback: bool,
    base_url: Option<String>,
    js_engine: &str,
    post_exprs: &[String],
) -> Result<(String, String)> {
    let (layout, _styles) = layout_tree_after_js_engine(
        html,
        width,
        run_js,
        csr_fallback,
        base_url,
        js_engine,
        false,
        1.0,
        post_exprs,
    )?;
    let plain = render_ascii(&layout, width);
    let colored = render_ascii_colored(&layout, width);
    // M22.2/M70.2: 把 [IMG: src] 占位符替换为本地图像的 ASCII art。
    // colored 路径用彩色 ASCII（每字符带像素 RGB），plain 路径用灰度（爬虫安全）。
    // http(s) URL 或不存在的文件 → 保留占位符（不报错，爬虫场景容错）。
    let plain = post_process_images(&plain, width, false);
    let colored = post_process_images(&colored, width, true);
    // M70.3: 把 [SVG: ...] 占位符替换为 ASCII art。
    Ok((
        post_process_svgs(&plain, width),
        post_process_svgs(&colored, width),
    ))
}

/// M81: 把 `--click` 选择器列表转成同会话合成点击表达式（post_exprs）。
/// 每个表达式返回数字：`-1` = 未命中；否则目标 nodeId（js-runtime 的
/// `run_post_exprs_quickjs` 用 eval_i32 读回打日志）。支持两种形式：
/// - CSS 选择器（主路径）：`querySelector → 合成 MouseEvent('click',
///   {bubbles, cancelable, clientX, clientY, view}) → dispatchEvent`；
///   坐标取 getBoundingClientRect 中心（本引擎 rect 是零桩，坐标 0 ——
///   合成点击 isTrusted=false，handler 一般不校验坐标）。
/// - `text=xxx`：遍历全元素，取 textContent 包含 xxx 且文本最短的元素
///   （避开 html/body 祖先先命中）。
fn click_post_exprs(selectors: &[String]) -> Vec<String> {
    selectors
        .iter()
        .map(|sel| {
            if let Some(text) = sel.strip_prefix("text=") {
                let esc = text.replace('\\', "\\\\").replace('\'', "\\'");
                format!(
                    "(function(){{var els=document.querySelectorAll('*');\
                     var best=null,bl=Infinity;for(var i=0;i<els.length;i++){{\
                     var t=(els[i].textContent||'').trim();\
                     if(t.length>0&&t.indexOf('{esc}')>=0&&t.length<bl){{best=els[i];bl=t.length;}}}}\
                     if(!best||typeof best.__nodeId!=='number'){{return -1;}}\
                     var ev=new MouseEvent('click',{{bubbles:true,cancelable:true,view:window}});\
                     var nc=best.dispatchEvent(ev);                     if(nc&&best.tagName==='A'){{var h=best.getAttribute('href');                     if(h){{if(h.charAt(0)==='#'){{location.hash=h.slice(1);}}else{{location.href=h;}}}}}}                     return best.__nodeId;}})()"
                )
            } else {
                let esc = sel.replace('\\', "\\\\").replace('\'', "\\'");
                format!(
                    "(function(){{var el=null;try{{el=document.querySelector('{esc}');}}\
                     catch(e){{return -1;}}\
                     if(!el||typeof el.__nodeId!=='number'){{return -1;}}\
                     var cx=0,cy=0;try{{var r=el.getBoundingClientRect();\
                     if(r){{cx=(r.left||0)+(r.width||0)/2;cy=(r.top||0)+(r.height||0)/2;}}}}catch(re){{}}\
                     var ev=new MouseEvent('click',{{bubbles:true,cancelable:true,view:window,\
                     clientX:cx,clientY:cy}});var nc=el.dispatchEvent(ev);                     if(nc&&el.tagName==='A'){{var h=el.getAttribute('href');                     if(h){{if(h.charAt(0)==='#'){{location.hash=h.slice(1);}}else{{location.href=h;}}}}}}                     return el.__nodeId;}})()"
                )
            }
        })
        .collect()
}

/// M81: 把 `--hover` 选择器列表转成同会话合成悬停表达式（post_exprs）。
/// 浏览器标准进入序列（指针落到元素上）：`mouseover`（bubbles）→
/// `mouseenter`（不冒泡）→ `mousemove`（bubbles）；若本会话此前 hover 过
/// 别的元素，先对旧元素派 `mouseout`（bubbles）→ `mouseleave`（不冒泡）
/// ——out/over、enter/leave 成对，顺序对齐真实指针从旧元素移入新元素。
/// 坐标取 getBoundingClientRect 中心（本引擎 rect 是零桩，坐标 0——合成
/// 事件 isTrusted=false，菜单 handler 一般不校验坐标）。
/// 上个悬停目标只存 nodeId（数字，`window.__hoverNodeId`）再经
/// `__makeElement` 重包装——QuickJS GC 安全（禁止全局变量存 JS 对象引用）。
/// 返回数字约定同 [`click_post_exprs`]：`-1` = 未命中；否则目标 nodeId。
/// 选择器形式同 [`click_post_exprs`]（CSS 选择器 / `text=xxx`）。
/// M81.4: focus 合成表达式——同 hover 模式，事件为 focus→focusin（冒泡），
/// 前一焦点元素派 blur→focusout（利用 Element.prototype.focus 内建逻辑）。
fn focus_post_exprs(selectors: &[String]) -> Vec<String> {
    selectors
        .iter()
        .map(|sel| {
            let find = if let Some(text) = sel.strip_prefix("text=") {
                let esc = text.replace('\\', "\\\\").replace('\'', "\\'");
                format!(
                    "var els=document.querySelectorAll('*');\
                     for(var i=0;i<els.length;i++){{\
                     var t=(els[i].textContent||'').trim();\
                     if(t.length>0&&t.indexOf('{esc}')>=0&&t.length<bl){{el=els[i];bl=t.length;}}}}"
                )
            } else {
                let esc = sel.replace('\\', "\\\\").replace('\'', "\\'");
                format!("try{{el=document.querySelector('{esc}');}}catch(e){{return -1;}}")
            };
            format!(
                "(function(){{var el=null,bl=Infinity;{find}\
                 if(!el||typeof el.__nodeId!=='number'){{return -1;}}\
                 if(typeof el.focus==='function'){{el.focus();return el.__nodeId;}}\
                 return -1;}})()"
            )
        })
        .collect()
}

/// M81.5: --type <selector> <text>——键盘输入序列合成（聚焦→逐字符
/// keydown/keypress/input/keyup + value 注入）。爬虫搜索框场景：
/// --type "#search" "query" --click "#go"。
fn type_post_exprs(type_args: &[String]) -> Vec<String> {
    type_args
        .iter()
        .map(|arg| {
            // 格式："SELECTOR=TEXT"（selector 里不含 =，text 可含 =）
            let (sel, text) = match arg.find('=') {
                Some(i) => (arg[..i].to_string(), arg[i+1..].to_string()),
                None => (arg.clone(), String::new()),
            };
            let find = if let Some(t) = sel.strip_prefix("text=") {
                let esc = t.replace('\\', "\\\\").replace('\'', "\\'");
                format!(
                    "var els=document.querySelectorAll('*');\
                     for(var i=0;i<els.length;i++){{\
                     var tt=(els[i].textContent||'').trim();\
                     if(tt.length>0&&tt.indexOf('{esc}')>=0&&tt.length<bl){{el=els[i];bl=tt.length;}}}}"
                )
            } else {
                let esc = sel.replace('\\', "\\\\").replace('\'', "\\'");
                format!("try{{el=document.querySelector('{esc}');}}catch(e){{return -1;}}")
            };
            let text_esc = text.replace('\\', "\\\\").replace('\'', "\\'");
            format!(
                "(function(){{var el=null,bl=Infinity;{find}\
                 if(!el||typeof el.__nodeId!=='number'){{return -1;}}\
                 if(typeof el.focus==='function')el.focus();\
                 var text='{text_esc}';\
                 try{{el.value='';}}catch(e){{}}\
                 for(var i=0;i<text.length;i++){{\
                   var ch=text.charAt(i);\
                   var kd=new KeyboardEvent('keydown',{{key:ch,bubbles:true,cancelable:true}});\
                   try{{el.dispatchEvent(kd);}}catch(e){{}}\
                   var kp=new KeyboardEvent('keypress',{{key:ch,bubbles:true,cancelable:true}});\
                   try{{el.dispatchEvent(kp);}}catch(e){{}}\
                   try{{\
                     if(typeof el.value==='string'){{el.value+=ch;}}\
                     else{{el.textContent=(el.textContent||'')+ch;}}\
                   }}catch(e){{}}\
                   var ie=new Event('input',{{bubbles:true}});\
                   try{{ie.data=ch;}}catch(e){{}}\
                   try{{el.dispatchEvent(ie);}}catch(e){{}}\
                   var ku=new KeyboardEvent('keyup',{{key:ch,bubbles:true,cancelable:true}});\
                   try{{el.dispatchEvent(ku);}}catch(e){{}}\
                 }}\
                 try{{el.dispatchEvent(new Event('change',{{bubbles:true}}));}}catch(e){{}}\
                 return el.__nodeId;}})()"
            )
        })
        .collect()
}

/// M81.6: --check 表达式——checkbox/radio 勾选（toggle：已勾则取消）。
/// 派发 click（含 checked 状态切换）+ change。
fn check_post_exprs(selectors: &[String]) -> Vec<String> {
    selectors
        .iter()
        .map(|sel| {
            let find = if let Some(text) = sel.strip_prefix("text=") {
                let esc = text.replace('\\', "\\\\").replace('\'', "\\'");
                format!(
                    "var els=document.querySelectorAll('input[type=checkbox],input[type=radio]');\
                     for(var i=0;i<els.length;i++){{\
                     var tt=(els[i].value||'').trim();\
                     if(tt.length>0&&tt.indexOf('{esc}')>=0&&tt.length<bl){{el=els[i];bl=tt.length;}}}}"
                )
            } else {
                let esc = sel.replace('\\', "\\\\").replace('\'', "\\'");
                format!("try{{el=document.querySelector('{esc}');}}catch(e){{return -1;}}")
            };
            format!(
                "(function(){{var el=null,bl=Infinity;{find}\
                 if(!el||typeof el.__nodeId!=='number'){{return -1;}}\
                 el.checked=!el.checked;\
                 var ce=new MouseEvent('click',{{bubbles:true,cancelable:true,view:window}});\
                 try{{el.dispatchEvent(ce);}}catch(e){{}}\
                 var ch=new Event('change',{{bubbles:true}});\
                 try{{el.dispatchEvent(ch);}}catch(e){{}}\
                 return el.__nodeId;}})()"
            )
        })
        .collect()
}

/// M81.6: --select 表达式——下拉选择（设 select.value + 派发 change）。
fn select_post_exprs(select_args: &[String]) -> Vec<String> {
    select_args
        .iter()
        .map(|arg| {
            // 格式："SELECTOR=VALUE"
            let (sel, val) = match arg.find('=') {
                Some(i) => (arg[..i].to_string(), arg[i+1..].to_string()),
                None => (arg.clone(), String::new()),
            };
            let sel_esc = sel.replace('\\', "\\\\").replace('\'', "\\'");
            let val_esc = val.replace('\\', "\\\\").replace('\'', "\\'").replace('"', "\\\"");
            format!(
                "(function(){{var el=null;try{{el=document.querySelector('{sel_esc}');}}catch(e){{}}\
                 if(!el||el.tagName!=='SELECT'){{return -1;}}\
                 el.value='{val_esc}';\
                 var ch=new Event('change',{{bubbles:true}});\
                 try{{el.dispatchEvent(ch);}}catch(e){{}}\
                 return el.__nodeId;}})()"
            )
        })
        .collect()
}

/// M81.7: --scroll-to 表达式——滚动到元素位置或绝对像素，派发 scroll
/// 事件 + window 派发（lazy 内容/无限滚动的 IntersectionObserver 已在
/// M80.27 激活，滚动后 observe 的元素立即回调）。
/// 格式：CSS 选择器 或 纯数字（像素位置）。
fn scroll_post_exprs(targets: &[String]) -> Vec<String> {
    targets
        .iter()
        .map(|t| {
            if let Ok(px) = t.parse::<f64>() {
                format!(
                    "(function(){{\
                     window.scrollTo(0,{px});\
                     var se=new Event('scroll');try{{window.dispatchEvent(se);}}catch(e){{}}\
                     try{{document.dispatchEvent(se);}}catch(e){{}}\
                     return {px};}})()"
                )
            } else {
                let esc = t.replace('\\', "\\\\").replace('\'', "\\'");
                format!(
                    "(function(){{var el=null;try{{el=document.querySelector('{esc}');}}catch(e){{}}\
                     if(!el||typeof el.__nodeId!=='number'){{return -1;}}\
                     try{{el.scrollIntoView({{behavior:'auto',block:'start'}});}}catch(e){{}}\
                     var se=new Event('scroll');try{{window.dispatchEvent(se);}}catch(e){{}}\
                     try{{el.dispatchEvent(new Event('scroll'));}}catch(e){{}}\
                     return el.__nodeId;}})()"
                )
            }
        })
        .collect()
}

/// M81.8: --dblclick 表达式——click ×2 + dblclick 事件（浏览器标准序列）。
fn dblclick_post_exprs(selectors: &[String]) -> Vec<String> {
    selectors
        .iter()
        .map(|sel| {
            let find = if let Some(text) = sel.strip_prefix("text=") {
                let esc = text.replace('\\', "\\\\").replace('\'', "\\'");
                format!(
                    "var els=document.querySelectorAll('*');\
                     for(var i=0;i<els.length;i++){{\
                     var t=(els[i].textContent||'').trim();\
                     if(t.length>0&&t.indexOf('{esc}')>=0&&t.length<bl){{el=els[i];bl=t.length;}}}}"
                )
            } else {
                let esc = sel.replace('\\', "\\\\").replace('\'', "\\'");
                format!("try{{el=document.querySelector('{esc}');}}catch(e){{return -1;}}")
            };
            format!(
                "(function(){{var el=null,bl=Infinity;{find}\
                 if(!el||typeof el.__nodeId!=='number'){{return -1;}}\
                 var opt={{bubbles:true,cancelable:true,view:window,detail:2}};\
                 try{{el.dispatchEvent(new MouseEvent('click',opt));}}catch(e){{}}\
                 try{{el.dispatchEvent(new MouseEvent('click',opt));}}catch(e){{}}\
                 try{{el.dispatchEvent(new MouseEvent('dblclick',opt));}}catch(e){{}}\
                 return el.__nodeId;}})()"
            )
        })
        .collect()
}

/// M81.8: --contextmenu 表达式——右键 contextmenu 事件（不冒泡到 window 的
/// 特殊性忽略，爬虫场景菜单展开靠 JS 监听 contextmenu）。
fn contextmenu_post_exprs(selectors: &[String]) -> Vec<String> {
    selectors
        .iter()
        .map(|sel| {
            let find = if let Some(text) = sel.strip_prefix("text=") {
                let esc = text.replace('\\', "\\\\").replace('\'', "\\'");
                format!(
                    "var els=document.querySelectorAll('*');\
                     for(var i=0;i<els.length;i++){{\
                     var t=(els[i].textContent||'').trim();\
                     if(t.length>0&&t.indexOf('{esc}')>=0&&t.length<bl){{el=els[i];bl=t.length;}}}}"
                )
            } else {
                let esc = sel.replace('\\', "\\\\").replace('\'', "\\'");
                format!("try{{el=document.querySelector('{esc}');}}catch(e){{return -1;}}")
            };
            format!(
                "(function(){{var el=null,bl=Infinity;{find}\
                 if(!el||typeof el.__nodeId!=='number'){{return -1;}}\
                 var ev=new MouseEvent('contextmenu',{{bubbles:true,cancelable:true,view:window,button:2}});\
                 try{{el.dispatchEvent(ev);}}catch(e){{}}\
                 return el.__nodeId;}})()"
            )
        })
        .collect()
}

/// M81.9: --drag 表达式——"源>目标" 拖放序列。
/// 事件链：dragstart(源) → dragenter/dragover(目标) → drop(目标) → dragend(源)。
/// DataTransfer 用 text/plain 传递源文本。
fn drag_post_exprs(drags: &[String]) -> Vec<String> {
    drags
        .iter()
        .map(|d| {
            let (src, dst) = match d.find('>') {
                Some(i) => (d[..i].trim().to_string(), d[i+1..].trim().to_string()),
                None => return "(function(){return -2;})()".to_string(), // 无 > 分隔：跳过
            };
            let se = src.replace('\\', "\\\\").replace('\'', "\\'");
            let de = dst.replace('\\', "\\\\").replace('\'', "\\'");
            format!(
                "(function(){{\
                 var src=null,dst=null;\
                 try{{src=document.querySelector('{se}');}}catch(e){{}}\
                 try{{dst=document.querySelector('{de}');}}catch(e){{}}\
                 if(!src||!dst||typeof src.__nodeId!=='number'){{return -1;}}\
                 var dt=new DataTransfer();\
                 try{{dt.setData('text/plain',src.textContent||'');}}catch(e){{}}\
                 var mk=function(type,tgt){{var e=new MouseEvent(type,{{bubbles:true,cancelable:true,view:window}});\
                   try{{e.dataTransfer=dt;}}catch(x){{}}return e;}};\
                 try{{src.dispatchEvent(mk('dragstart',src));}}catch(e){{}}\
                 try{{src.dispatchEvent(mk('drag',src));}}catch(e){{}}\
                 try{{dst.dispatchEvent(mk('dragenter',dst));}}catch(e){{}}\
                 try{{dst.dispatchEvent(mk('dragover',dst));}}catch(e){{}}\
                 try{{dst.dispatchEvent(mk('drop',dst));}}catch(e){{}}\
                 try{{src.dispatchEvent(mk('dragend',src));}}catch(e){{}}\
                 return src.__nodeId;}})()"
            )
        })
        .collect()
}

fn hover_post_exprs(selectors: &[String]) -> Vec<String> {
    selectors
        .iter()
        .map(|sel| {
            let find = if let Some(text) = sel.strip_prefix("text=") {
                let esc = text.replace('\\', "\\\\").replace('\'', "\\'");
                format!(
                    "var els=document.querySelectorAll('*');\
                     for(var i=0;i<els.length;i++){{\
                     var t=(els[i].textContent||'').trim();\
                     if(t.length>0&&t.indexOf('{esc}')>=0&&t.length<bl){{el=els[i];bl=t.length;}}}}"
                )
            } else {
                let esc = sel.replace('\\', "\\\\").replace('\'', "\\'");
                format!("try{{el=document.querySelector('{esc}');}}catch(e){{return -1;}}")
            };
            format!(
                "(function(){{var el=null,bl=Infinity;{find}\
                 if(!el||typeof el.__nodeId!=='number'){{return -1;}}\
                 var cx=0,cy=0;try{{var r=el.getBoundingClientRect();\
                 if(r){{cx=(r.left||0)+(r.width||0)/2;cy=(r.top||0)+(r.height||0)/2;}}}}catch(re){{}}\
                 try{{var pid=window.__hoverNodeId;\
                 if(typeof pid==='number'&&pid>=0&&pid!==el.__nodeId&&typeof __makeElement==='function'){{\
                 var prev=__makeElement(pid);\
                 if(prev){{\
                 var oe=new MouseEvent('mouseout',{{bubbles:true,cancelable:true,view:window,\
                 clientX:cx,clientY:cy}});try{{oe.relatedTarget=el;}}catch(o1){{}}prev.dispatchEvent(oe);\
                 var le=new MouseEvent('mouseleave',{{bubbles:false,cancelable:false,view:window,\
                 clientX:cx,clientY:cy}});try{{le.relatedTarget=el;}}catch(o2){{}}prev.dispatchEvent(le);}}}}}}catch(pe){{}}\
                 window.__hoverNodeId=el.__nodeId;\
                 var e1=new MouseEvent('mouseover',{{bubbles:true,cancelable:true,view:window,\
                 clientX:cx,clientY:cy}});el.dispatchEvent(e1);\
                 var e2=new MouseEvent('mouseenter',{{bubbles:false,cancelable:false,view:window,\
                 clientX:cx,clientY:cy}});el.dispatchEvent(e2);\
                 var e3=new MouseEvent('mousemove',{{bubbles:true,cancelable:true,view:window,\
                 clientX:cx,clientY:cy}});el.dispatchEvent(e3);\
                 return el.__nodeId;}})()"
            )
        })
        .collect()
}

/// M80: 共用布局管线（parse → JS → style → construct → run_layout），
/// 返回布局树 + computed styles。ASCII（爬虫契约）与 pixel（近似像素
/// 渲染）两条渲染路径共用此前半段；JS 只执行一次。
/// M81: `post_exprs` —— --click/--hover 合成事件表达式，在同一 JS 会话内
/// 于页面脚本 + 事件循环之后按序 eval（空切片 = 原行为）。
#[allow(clippy::too_many_arguments)]
fn layout_tree_after_js_engine(
    html: &str,
    width: usize,
    run_js: bool,
    csr_fallback: bool,
    base_url: Option<String>,
    js_engine: &str,
    pixel: bool,
    unit_scale: f32,
    post_exprs: &[String],
) -> Result<(browser_layout::LayoutTree, browser_render::StyleMap)> {
    let tree = parse_html(html);
    let (shared_tree, executed) = if run_js {
        let engine_kind = browser_js_runtime::EngineKind::parse_str(js_engine);
        #[cfg(feature = "quickjs")]
        let (shared, n) = if base_url.is_some() || !post_exprs.is_empty() {
            // M81: 有 --click/--hover 时必须走 post_exprs 变体（同会话合成
            // 事件）；无 base_url 的本地文件 QuickJS 也支持（base_url 仅影响
            // 相对 URL 解析）。
            browser_js_runtime::run_scripts_with_post_exprs(
                tree,
                base_url.clone(),
                &engine_kind,
                post_exprs,
            )
        } else {
            // 无 base_url 时用 boa 默认路径（QuickJS 需要 base_url 解析 URL）
            let tree2 = tree;
            browser_js_runtime::run_scripts_with_base(tree2, base_url.clone())
        };
        #[cfg(not(feature = "quickjs"))]
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
    // M80.2: pixel 模式跳过 ASCII 文本改写；M80.6: unit_scale=行高 px
    // （margin px→格换算，防巨隙）。
    let opts = if pixel {
        browser_layout::ConstructOptions::pixel_with_scale(unit_scale)
    } else {
        browser_layout::ConstructOptions::default()
    };
    let mut layout =
        browser_layout::construct_layout_tree_with(&shared_tree.borrow(), &styles, opts);
    run_layout(
        &mut layout,
        LayoutConfig {
            viewport_width: width as f32,
        },
    );
    Ok((layout, styles))
}

/// M80: pixel 模式截图管线。`width_px` 按 CSS px 解释：先换算布局列数
/// 喂给布局（折行按格数），再把 px 宽度交给像素光栅化。JS 只执行一次。
/// 返回 ASCII 纯文本供 stdout（与截图解耦，方便 pipe）。
/// M81: `post_exprs` —— --click/--hover 合成事件表达式（同 JS 会话）。
#[allow(clippy::too_many_arguments)]
fn render_pixel_screenshot(
    html: &str,
    width_px: usize,
    run_js: bool,
    base_url: Option<String>,
    js_engine: &str,
    path: &PathBuf,
    max_height: Option<usize>,
    post_exprs: &[String],
) -> Result<String> {
    let cols = browser_render::layout_columns_for_px(width_px);
    let cm = browser_render::pixel::cell_metrics();
    eprintln!("[dbg-cm] cell_metrics=({:.2},{:.2})", cm.0, cm.1);
    let (layout, styles) = layout_tree_after_js_engine(
        html, cols, run_js, false, base_url, js_engine, true, cm.1, post_exprs,
    )?;
    let (w, h, rgba) = browser_render::render_pixel(&layout, &styles, width_px, 1.0);
    screenshot::render_rgba_to_png(&rgba, w, h, path, max_height)
        .map_err(|e| anyhow!("pixel screenshot failed: {e}"))?;
    eprintln!(
        "[screenshot] wrote {} ({}x{} px, pixel mode)",
        path.display(),
        w,
        h
    );
    Ok(render_ascii(&layout, cols))
}

/// M22.2/M70.2: 扫描渲染输出里的 `[IMG: src]` 占位符，尝试把 src 解析为本地
/// 图像文件并解码成 ASCII art 替换。无法解析的保留原占位符。
///
/// `colored`: true 时用彩色 ASCII（每字符带 ANSI 38;2 前景色，对应像素 RGB），
/// false 时用灰度 ASCII（纯字符，爬虫/终端安全）。
///
/// base_dir 用 cwd（render-file/render-script 从文件读，cwd 是合理的
/// 相对基准）。max_w 用渲染宽度，max_h 用宽度的 1/3（图像高度通常小于文本）。
fn post_process_images(rendered: &str, width: usize, colored: bool) -> String {
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
            // M72 噪声治理：data-URI 绝不进输出（防御性兜底——construct
            // 层已跳过 data: src，这里防 JS 动态注入等其他来源）。
            if src.is_empty() || src.to_ascii_lowercase().starts_with("data:") {
                if src.is_empty() {
                    // 空 src：原样输出已扫描部分
                    out.push_str(&chars[i..advance].iter().collect::<String>());
                }
                i = advance;
                continue;
            }
            // 尝试解析 + 解码（M70.2: colored 路径用彩色版）
            let decode_result = if colored {
                resolve_local_image_src(&src, cwd.as_deref()).map(|path| {
                    browser_render::image_file_to_ascii_colored(&path, width as u32, max_h)
                })
            } else {
                resolve_local_image_src(&src, cwd.as_deref())
                    .map(|path| browser_render::image_file_to_ascii(&path, width as u32, max_h))
            };
            match decode_result {
                Some(Ok(ascii)) => {
                    out.push_str("\n┌─ image: ");
                    out.push_str(&src);
                    out.push_str(" ─\n");
                    out.push_str(ascii.trim_end_matches('\n'));
                    out.push_str("\n└──────────────\n");
                    eprintln!("[img] rendered {src} as ASCII");
                }
                // M72 噪声治理：解析失败（文件不存在）或解码失败时不再
                // 回显 `[IMG: {src}]`——路径/URL 留在输出里就是截图噪声。
                // 丢弃占位符（stderr 记日志，stdout/PNG 保持干净）。
                Some(Err(e)) => {
                    eprintln!("[img] decode {src} failed: {e} (placeholder dropped)");
                }
                None => {
                    eprintln!("[img] unresolvable src, placeholder dropped: {src}");
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

/// M70.3: 扫描渲染输出里的 `[SVG: w=.. h=.. | shapes]` 占位符，解析出
/// viewBox 尺寸 + 形状列表，调 `svg_to_ascii` 渲染成 ASCII art 替换。
/// 解析失败的保留原占位符。
fn post_process_svgs(rendered: &str, width: usize) -> String {
    let chars: Vec<char> = rendered.chars().collect();
    let mut out = String::with_capacity(rendered.len());
    let mut i = 0;
    while i < chars.len() {
        // 匹配 [SVG:
        if i + 5 <= chars.len()
            && chars[i] == '['
            && chars[i + 1] == 'S'
            && chars[i + 2] == 'V'
            && chars[i + 3] == 'G'
            && chars[i + 4] == ':'
        {
            // 找到对应的 ]（SVG 占位符内不含 ]）
            let mut j = i + 5;
            while j < chars.len() && chars[j] != ']' {
                j += 1;
            }
            if j >= chars.len() {
                // 未闭合，原样输出
                out.push(chars[i]);
                i += 1;
                continue;
            }
            let placeholder: String = chars[i + 5..j].iter().collect();
            match parse_svg_placeholder(&placeholder, width) {
                Some(ascii) => {
                    out.push_str("\n┌─ svg");
                    out.push_str(" ─\n");
                    out.push_str(ascii.trim_end_matches('\n'));
                    out.push_str("\n└──────────────\n");
                }
                None => {
                    out.push_str(&format!("[SVG:{placeholder}]"));
                }
            }
            i = j + 1;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

/// M70.3: 解析 `[SVG: w=W h=H | tag k=v k=v; tag k=v]` 占位符内容（不含
/// 外层 `[SVG:` 和 `]`），重建 SvgShape 列表并渲染成 ASCII art。
fn parse_svg_placeholder(body: &str, width: usize) -> Option<String> {
    use browser_render::{parse_svg_shapes, svg_to_ascii};
    // 分割 " w=W h=H | shapes"
    let (dims_part, shapes_part) = body.split_once('|')?;
    let mut vb_w = 100.0_f32;
    let mut vb_h = 100.0_f32;
    for tok in dims_part.split_whitespace() {
        if let Some(v) = tok.strip_prefix("w=") {
            vb_w = v.parse().ok()?;
        } else if let Some(v) = tok.strip_prefix("h=") {
            vb_h = v.parse().ok()?;
        }
    }
    // 解析 shapes：每个 "; " 分隔一个 shape，shape 内 "tag k=v k=v"
    let mut elements: Vec<(String, Vec<(String, String)>)> = Vec::new();
    for shape_str in shapes_part.split("; ") {
        let mut parts = shape_str.split_whitespace();
        let tag = parts.next()?;
        let mut attrs = Vec::new();
        for kv in parts {
            if let Some((k, v)) = kv.split_once('=') {
                attrs.push((k.to_string(), v.to_string()));
            }
        }
        elements.push((tag.to_string(), attrs));
    }
    if elements.is_empty() {
        return None;
    }
    // 转 parse_svg_shapes 需要的 &[(&str, &[(String,String)])]
    let refs: Vec<(&str, &[(String, String)])> = elements
        .iter()
        .map(|(t, a)| (t.as_str(), a.as_slice()))
        .collect();
    let shapes = parse_svg_shapes(&refs);
    let max_h = ((width as f32) * (vb_h / vb_w) * 0.5).max(5.0) as u32;
    let ascii = svg_to_ascii(&shapes, vb_w, vb_h, width as u32, max_h);
    if ascii.is_empty() {
        None
    } else {
        Some(ascii)
    }
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

/// M65: 获取当前进程的 mach task self port（封装 deprecated + unsafe）。
#[cfg(target_os = "macos")]
#[allow(deprecated)]
unsafe fn task_self() -> libc::mach_port_t {
    libc::mach_task_self()
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

    // ── M70.14: has_visible_body_content —— SSR 检测通用逻辑回归 ──
    // 这组测试固化 docsify.js.org 修复的核心判定：
    // HTML 字节数大不等于有正文（meta/CSS/link 可能让 HTML > 5KB 但正文只有 "Loading"）。
    // 判定依据必须是 body 下「可见文本」长度（排除 script/style/svg/noscript/template）。

    /// 真正的 SSR 站：body 下有实质正文 → 应判为 true（跳过 JS，直接提取）。
    #[test]
    fn has_visible_body_content_true_for_real_ssr() {
        use crate::has_visible_body_content;
        let html = r#"<html><head>
            <title>Real SSR Site</title>
            <link rel="stylesheet" href="/style.css">
        </head><body>
            <h1>Welcome to Example</h1>
            <p>This is a fully server-rendered page with plenty of body text
            that should be detected as having visible content well beyond
            the minimum threshold we use to decide whether to run JS.</p>
        </body></html>"#;
        assert!(has_visible_body_content(html));
    }

    /// docsify 式 SPA 壳：HTML 体积大（meta + CSS 链接），但 body 正文
    /// 只有 "Loading..." → 必须判为 false（走 JS 渲染）。
    /// 这是修复 docsify.js.org 的精确回归点。
    #[test]
    fn has_visible_body_content_false_for_spa_shell_with_loading() {
        use crate::has_visible_body_content;
        // 模拟 docsify.js.org：HTML 7KB+，但正文只有占位符。
        let html = r#"<html><head>
            <meta charset="utf-8">
            <meta name="viewport" content="width=device-width,initial-scale=1">
            <meta name="description" content="A magical documentation site generator.">
            <link rel="stylesheet" href="//cdn.jsdelivr.net/npm/docsify/themes/vue.css">
            <link rel="stylesheet" href="//cdn.jsdelivr.net/npm/docsify/lib/themes/vue.css">
            <link rel="alternate" hreflang="en" href="https://docsify.js.org/">
            <link rel="alternate" hreflang="zh-cn" href="https://docsify.js.org/#/zh-cn/">
            <link rel="alternate" hreflang="de" href="https://docsify.js.org/#/de-de/">
            <style>.placeholder { color: gray; }</style>
        </head><body>
            <div id="app">Loading ...</div>
            <script src="//cdn.jsdelivr.net/npm/docsify/lib/docsify.min.js"></script>
        </body></html>"#;
        // 体积大（>5KB 的旧阈值），但可见正文只有 "Loading ..." → false。
        assert!(
            !has_visible_body_content(html),
            "SPA shell with only 'Loading ...' must NOT be detected as SSR"
        );
    }

    /// 边界：body 下文本刚好超阈值（>200B）→ true。
    #[test]
    fn has_visible_body_content_threshold_boundary() {
        use crate::has_visible_body_content;
        // 拼一段 ~250 字节的可见正文。
        let para = "This is a paragraph of server-rendered body text. ";
        let mut body = String::new();
        while body.len() < 250 {
            body.push_str(para);
        }
        let html = format!("<html><body><div>{body}</div></body></html>");
        assert!(has_visible_body_content(&html));
    }

    /// 噪声子树（script/style/svg/template/noscript）不计入可见正文。
    /// 即使 script/style 里有大段文本，body 仍应判为 false（无可见正文）。
    #[test]
    fn has_visible_body_content_ignores_noise_subtrees() {
        use crate::has_visible_body_content;
        let big_script = "x".repeat(2000);
        let html = format!(
            r#"<html><body>
            <script>var big = "{big_script}";</script>
            <style>.x {{ content: "{big_script}"; }}</style>
            <svg><text>{big_script}</text></svg>
            <noscript>enable js</noscript>
            <div>Loading</div>
        </body></html>"#
        );
        assert!(
            !has_visible_body_content(&html),
            "noise in script/style/svg must not count as visible content"
        );
    }

    /// 空 body（无可见文本）→ false（应走 JS 渲染）。
    /// html5ever 会自动补全 body 节点，此时可见文本为 0 < 200 → false。
    #[test]
    fn has_visible_body_content_false_for_empty_body() {
        use crate::has_visible_body_content;
        // 只有 head 内容、body 为空 —— 应走 JS 渲染。
        assert!(!has_visible_body_content(
            "<html><head><title>x</title></head></html>"
        ));
        // 完全空文档也应判为 false（无正文可提取）。
        assert!(!has_visible_body_content(""));
    }
}

/// M70.14: serve 请求结构（跨 serve/serve-child 共享）。
#[derive(serde::Serialize, serde::Deserialize)]
struct ServeRequest {
    url: String,
    #[serde(default = "default_format")]
    format: String,
    #[serde(default = "default_engine")]
    js_engine: String,
}
fn default_format() -> String {
    "markdown".to_string()
}
fn default_engine() -> String {
    "quickjs".to_string()
}

// ── M70.12: HTTP API 服务 ─────────────────────────────────

/// 启动 HTTP API 服务。每个请求跑一次 fetch + JS + extract 管线。
///
/// M70.17: 并发支持——每个连接 spawn 一个独立 `std::thread`，内建嵌套
/// `current_thread` tokio runtime 跑请求处理。这样彻底隔离 thread_local
/// 状态（cookie jar 是 `Rc<RefCell<>>` 即 `!Send`，无法跨 tokio task 共享）。
/// 用 `Semaphore` 限制最大并发数（默认 3），防止 N 个 SPA 同时 fork
/// serve-child 子进程导致 N×22MB 内存爆。
async fn serve(bind: &str, port: u16, max_concurrency: usize) -> Result<()> {
    use std::sync::Arc;
    let addr = format!("{bind}:{port}");
    let listener = TcpListener::bind(&addr).with_context(|| format!("failed to bind {addr}"))?;
    listener.set_nonblocking(false)?;
    let max_concurrency = max_concurrency.clamp(1, 16);
    eprintln!("[serve] listening on http://{addr} (max_concurrency={max_concurrency})");

    // 并发信号量：限制同时处理的请求数（每请求 = 1 个 serve-child ~22MB）。
    let semaphore = Arc::new(tokio::sync::Semaphore::new(max_concurrency));

    for stream in listener.incoming() {
        let stream = match stream {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[serve] accept error: {e}");
                continue;
            }
        };
        let peer = stream.peer_addr().ok();
        eprintln!("[serve] ← {peer:?}");

        let sem = semaphore.clone();
        // 每个连接一个独立 OS 线程 + 嵌套 runtime，彻底隔离 thread_local。
        // handle_request 内的所有 async 调用（fetch_with_jar 等）在这个线程的
        // 独立 runtime 上跑，cookie jar / DOM slot 惰性初始化，互不干扰。
        std::thread::spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    eprintln!("[serve] nested runtime build failed: {e}");
                    return;
                }
            };
            rt.block_on(async move {
                // 获取并发许可（阻塞等待，超过 max_concurrency 的请求在此排队）。
                // _permit drop 时自动释放并发槽。
                let _permit = match sem.acquire_owned().await {
                    Ok(p) => p,
                    Err(e) => {
                        eprintln!("[serve] semaphore closed: {e}");
                        return;
                    }
                };
                if let Err(e) = handle_request(stream).await {
                    eprintln!("[serve] request error: {e}");
                }
            });
        });
    }
    Ok(())
}

/// 处理单个 HTTP 请求：读请求 → 渲染管线 → 响应。
/// 在调用线程的 thread_local 上运行（cookie jar 等惰性初始化，线程间隔离）。
async fn handle_request(mut stream: std::net::TcpStream) -> Result<()> {
    use std::io::{BufRead, BufReader};

    // 解析 HTTP 请求
    let mut reader = BufReader::new(&stream);
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).is_err() {
        eprintln!("[serve] failed to read request line");
        return Ok(());
    }
    // 只处理 POST
    if !request_line.starts_with("POST") {
        send_response(&mut stream, 405, "Method Not Allowed");
        return Ok(());
    }

    // 读请求头 → 找 Content-Length
    let mut content_length: usize = 0;
    let mut body = String::new();
    loop {
        let mut header_line = String::new();
        if reader.read_line(&mut header_line).is_err() {
            break;
        }
        let trimmed = header_line.trim();
        if trimmed.is_empty() {
            // 空行 = header 结束
            break;
        }
        if let Some(len_str) = trimmed
            .strip_prefix("Content-Length:")
            .or_else(|| trimmed.strip_prefix("content-length:"))
        {
            content_length = len_str.trim().parse().unwrap_or(0);
        }
    }

    // 读请求体
    if content_length > 0 {
        let mut buf = vec![0u8; content_length];
        if reader.read_exact(&mut buf).is_ok() {
            body = String::from_utf8_lossy(&buf).to_string();
        }
    }

    // 解析 JSON
    let req: ServeRequest = match serde_json::from_str(&body) {
        Ok(r) => r,
        Err(e) => {
            send_response(
                &mut stream,
                400,
                &format!(r#"{{"error":"bad request: {e}"}}"#),
            );
            return Ok(());
        }
    };

    eprintln!("[serve] rendering url={} format={}", req.url, req.format);

    // M70.14: 去掉 hash fragment——#/?id=xxx 是客户端路由，服务端不需要。
    let clean_url = url::Url::parse(&req.url)
        .map(|mut u| {
            u.set_fragment(None);
            u.to_string()
        })
        .unwrap_or_else(|_| req.url.clone());

    // ── 执行渲染管线 ──
    let t0 = std::time::Instant::now();
    let html = match fetch_with_jar(&clean_url).await {
        Ok(h) => h,
        Err(e) => {
            send_response(
                &mut stream,
                502,
                &format!(
                    r#"{{"url":"{}","error":"fetch failed: {}","content":""}}"#,
                    clean_url,
                    e.to_string().replace('"', "'")
                ),
            );
            return Ok(());
        }
    };
    // M70.14: 检查 body 是否有关键正文（排除 meta/CSS/script 的干扰）。
    // docsify.js.org 的 HTML 7354B（meta 标签+CSS 链接）但正文只有"Loading ..."。
    // 用 DOM 解析提取 body 纯文本长度 > 200 才认为是 SSR。
    let has_content = has_visible_body_content(&html);
    let output_json = if has_content {
        // SSR/SSG：直接提取，不走子进程
        let tree = parse_html(&html);
        let out_format = match browser_extractor::OutputFormat::parse(&req.format) {
            Ok(f) => f,
            Err(_) => browser_extractor::OutputFormat::Markdown,
        };
        let opts = browser_extractor::FetchOptions {
            format: out_format,
            selector: None,
            only_main_content: false,
        };
        let result = match browser_extractor::run_extract(&tree, Some(&clean_url), &opts) {
            Ok(r) => r,
            Err(_) => browser_extractor::ExtractResult {
                content: String::new(),
                title: None,
            },
        };
        let escaped_c =
            serde_json::to_string(&result.content).unwrap_or_else(|_| "\"\"".to_string());
        let escaped_t = serde_json::to_string(&result.title).unwrap_or_else(|_| "null".to_string());
        let total_ms = t0.elapsed().as_millis();
        format!(
            r#"{{"url":"{}","title":{},"content":{},"format":"{}","_timing":{{"fetch_ms":{},"js_ms":0,"extract_ms":0,"total_ms":{}}},"_scripts":0}}"#,
            clean_url, escaped_t, escaped_c, req.format, total_ms, total_ms,
        )
    } else {
        // 纯 SPA：预取外链 JS 脚本（写入全局缓存，子进程 fork 后继承）
        prefetch_external_scripts(&html, &clean_url).await;
        // 子进程隔离 QuickJS（传入已 fetch 的 HTML，外链 JS 已在缓存中）
        let clean_req = ServeRequest {
            url: clean_url.clone(),
            format: req.format.clone(),
            js_engine: req.js_engine.clone(),
        };
        match render_in_subprocess(&clean_req, &html) {
            Ok(json) => json,
            Err(e) => {
                eprintln!("[serve] render failed: {e}");
                format!(
                    r#"{{"url":"{}","error":"render failed: {}","content":""}}"#,
                    clean_url,
                    e.to_string().replace('"', "'")
                )
            }
        }
    };
    eprintln!("[serve] done: {} bytes", output_json.len());
    send_response(&mut stream, 200, &output_json);
    Ok(())
}

/// M70.14: serve 子进程入口——读 stdin JSON，跑渲染管线，写 stdout JSON。
/// 在 RLIMIT_AS 受限的子进程里执行。QuickJS C 层 abort 只杀子进程。
async fn serve_child() -> Result<()> {
    use std::io::{Read, Write};
    let mut input = String::new();
    std::io::stdin()
        .read_to_string(&mut input)
        .map_err(|e| anyhow!("serve_child: read stdin failed: {e}"))?;

    // 解析 stdin payload：url \x1f format \x1f html
    let mut parts = input.splitn(3, '\x1f');
    let url = parts.next().unwrap_or("").trim().to_string();
    let format = parts.next().unwrap_or("markdown").trim().to_string();
    let html = parts.next().unwrap_or("");

    // 子进程不重复 fetch——用主进程传入的 HTML 直接跑 JS + 提取
    let base = if url.is_empty() {
        None
    } else {
        Some(url.clone())
    };
    let tree = parse_html(html);
    let engine_kind = browser_js_runtime::EngineKind::parse_str("quickjs");
    let t_js = std::time::Instant::now();
    let (shared, executed) = {
        use std::cell::RefCell;
        use std::panic::AssertUnwindSafe;
        use std::rc::Rc;
        let base_for_panic = base.clone();
        #[cfg(feature = "quickjs")]
        let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
            browser_js_runtime::run_scripts_with_base_engine(tree, base_for_panic, &engine_kind)
        }))
        .unwrap_or_else(|_| {
            let static_tree = parse_html(html);
            (Rc::new(RefCell::new(static_tree)), 0)
        });
        #[cfg(not(feature = "quickjs"))]
        let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
            browser_js_runtime::run_scripts_with_base(parse_html(html), base_for_panic)
        }))
        .unwrap_or_else(|_| {
            let static_tree = parse_html(html);
            (Rc::new(RefCell::new(static_tree)), 0)
        });
        result
    };
    let js_ms = t_js.elapsed().as_millis() as u64;

    let out_format = browser_extractor::OutputFormat::parse(&format)
        .unwrap_or(browser_extractor::OutputFormat::Markdown);
    let opts = browser_extractor::FetchOptions {
        format: out_format,
        selector: None,
        only_main_content: false,
    };
    let result = browser_extractor::run_extract(&shared.borrow(), base.as_deref(), &opts)
        .unwrap_or(browser_extractor::ExtractResult {
            content: String::new(),
            title: None,
        });

    let escaped_c = serde_json::to_string(&result.content).unwrap_or_else(|_| "\"\"".to_string());
    let escaped_t = serde_json::to_string(&result.title).unwrap_or_else(|_| "null".to_string());
    let json = format!(
        r#"{{"url":"{}","title":{},"content":{},"format":"{}","_timing":{{"fetch_ms":0,"js_ms":{},"extract_ms":0,"total_ms":{}}},"_scripts":{}}}"#,
        url, escaped_t, escaped_c, format, js_ms, js_ms, executed,
    );

    let mut stdout = std::io::stdout();
    stdout
        .write_all(json.as_bytes())
        .map_err(|e| anyhow!("serve_child: write stdout failed: {e}"))?;
    stdout.flush()?;
    Ok(())
}

/// M70.14: 在子进程里跑渲染管线，返回 JSON 字符串。
/// 任何错误都返回 Err（不会 panic 或 abort serve 主进程）。
/// M70.14: 子进程渲染。主进程已 fetch HTML，传给子进程避免重复请求。
fn render_in_subprocess(req: &ServeRequest, html: &str) -> Result<String> {
    let exe = std::env::current_exe().map_err(|e| anyhow!("cannot resolve current_exe: {e}"))?;

    // 构造 stdin payload：URL \x1f format \x1f html（用分隔符避免 JSON 转义问题）
    let mut payload = String::with_capacity(html.len() + req.url.len() + 64);
    payload.push_str(&req.url);
    payload.push('\x1f');
    payload.push_str(&req.format);
    payload.push('\x1f');
    payload.push_str(html);

    let mut child = std::process::Command::new(&exe)
        .arg("serve-child")
        .arg("--mem-mb")
        .arg("400")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .map_err(|e| anyhow!("spawn serve-child failed: {e}"))?;

    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write;
        let _ = stdin.write_all(payload.as_bytes());
    }

    let mut child_stdout = child.stdout.take();
    let child_pid = child.id();
    match sandbox::ChildWaitTimeoutExt::wait_timeout_mem(
        &mut child,
        std::time::Duration::from_secs(20),
        child_pid,
        400,
    ) {
        Ok(Some(status)) if status.success() => {
            let mut buf = String::new();
            if let Some(ref mut s) = child_stdout {
                use std::io::Read;
                let _ = s.read_to_string(&mut buf);
            }
            Ok(buf)
        }
        Ok(Some(_)) => Ok(String::from(r#"{"error":"child crashed","content":""}"#)),
        Ok(None) => {
            let _ = child.kill();
            Ok(String::from(
                r#"{"error":"child timeout (20s)","content":""}"#,
            ))
        }
        Err(e) => {
            let _ = child.kill();
            Ok(format!(r#"{{"error":"child error: {}","content":""}}"#, e))
        }
    }
}

/// M70.14: 预取 HTML 中所有外链 <script src>，写入全局 SCRIPT_CACHE。
/// 子进程 fork 后继承缓存，fetch_external_script 直接命中（省 CDN 网络等待）。
/// 并行 fetch 所有脚本（用 tokio spawn），不串行等待。
async fn prefetch_external_scripts(html: &str, base_url: &str) {
    use std::sync::Arc;
    // 解析所有 <script src="...">
    let srcs: Vec<String> = extract_script_srcs(html, base_url);
    if srcs.is_empty() {
        return;
    }
    eprintln!("[serve] prefetching {} external scripts", srcs.len());
    // 并行 fetch 所有脚本
    let mut handles = Vec::new();
    for url in srcs {
        let url = Arc::new(url);
        handles.push(tokio::spawn(async move {
            // 检查缓存是否已有
            if let Ok(cache) = browser_js_runtime::script_cache_public().lock() {
                if cache.contains_key(&*url) {
                    return; // 已缓存
                }
            }
            // 锁在 if let 作用域结束时已释放，无需显式 drop
            // fetch 脚本
            let client = browser_net::HttpClient::new();
            if let Ok(Ok(bytes)) =
                tokio::time::timeout(std::time::Duration::from_secs(8), client.get(&url)).await
            {
                if let Ok(code) = String::from_utf8(bytes) {
                    if let Ok(mut cache) = browser_js_runtime::script_cache_public().lock() {
                        cache.insert((*url).clone(), code);
                    }
                }
            }
        }));
    }
    // 等待所有预取完成（最多 8s）
    for h in handles {
        let _ = h.await;
    }
}

/// 从 HTML 中提取所有 <script src="..."> 的绝对 URL。
fn extract_script_srcs(html: &str, base_url: &str) -> Vec<String> {
    let tree = parse_html(html);
    let mut srcs = Vec::new();
    let mut stack: Vec<browser_dom::NodeId> = vec![tree.root()];
    while let Some(id) = stack.pop() {
        let node = tree.get(id);
        if let browser_dom::NodeData::Element { tag, attrs } = &node.data {
            if tag.eq_ignore_ascii_case("script") {
                for (k, v) in attrs {
                    if k.eq_ignore_ascii_case("src") && !v.is_empty() {
                        // 解析为绝对 URL
                        let resolved = if v.starts_with("http://") || v.starts_with("https://") {
                            v.clone()
                        } else if v.starts_with("//") {
                            format!("https:{}", v)
                        } else if let Ok(base) = url::Url::parse(base_url) {
                            base.join(v).map(|u| u.to_string()).unwrap_or(v.clone())
                        } else {
                            v.clone()
                        };
                        // 跳过 analytics
                        let lower = resolved.to_lowercase();
                        if lower.contains("cloudflareinsights")
                            || lower.contains("google-analytics")
                            || lower.contains("googletagmanager")
                            || lower.contains("doubleclick")
                            || lower.contains("facebook.net")
                            || lower.contains("sentry.io")
                            || lower.contains("hotjar")
                            || lower.contains("fullstory")
                            || lower.contains("usefathom")
                        {
                            continue;
                        }
                        srcs.push(resolved);
                    }
                }
            }
        }
        for &child in &node.children {
            stack.push(child);
        }
    }
    srcs
}

/// M70.14: 判断 HTML 是否有可见正文（DOM 解析，排除 script/style/meta/CSS）。
fn has_visible_body_content(html: &str) -> bool {
    let tree = parse_html(html);
    // 遍历找 body 节点
    let mut body_id = None;
    let mut stack = vec![tree.root()];
    while let Some(id) = stack.pop() {
        if let browser_dom::NodeData::Element { tag, .. } = tree.data(id) {
            if tag == "body" {
                body_id = Some(id);
                break;
            }
        }
        for &child in tree.children_of(id) {
            stack.push(child);
        }
    }
    let body_id = match body_id {
        Some(id) => id,
        None => return true,
    };
    // 统计 body 下可见文本长度（排除 script/style/svg/noscript）
    let mut text_len = 0usize;
    let mut stack = vec![body_id];
    while let Some(id) = stack.pop() {
        match tree.data(id) {
            browser_dom::NodeData::Text(s) => {
                text_len += s.trim().len();
            }
            browser_dom::NodeData::Element { tag, .. } => {
                if tag == "script"
                    || tag == "style"
                    || tag == "svg"
                    || tag == "noscript"
                    || tag == "template"
                {
                    continue;
                }
                for &child in tree.children_of(id) {
                    stack.push(child);
                }
            }
            _ => {}
        }
        if text_len > 500 {
            return true;
        }
    }
    text_len > 200
}

fn send_response(stream: &mut std::net::TcpStream, status: u16, body: &str) {
    let status_text = match status {
        200 => "OK",
        400 => "Bad Request",
        405 => "Method Not Allowed",
        500 => "Internal Server Error",
        _ => "Unknown",
    };
    let headers = format!(
        "HTTP/1.1 {status} {status_text}\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         Access-Control-Allow-Origin: *\r\n\
         \r\n{}",
        body.len(),
        body
    );
    let _ = stream.write_all(headers.as_bytes());
    let _ = stream.flush();
}
