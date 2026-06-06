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
use browser_css_engine::{compute_styles, parse as parse_css};
use browser_dom::pretty_print;
use browser_html_parser::parse as parse_html;
use browser_js_runtime::{
    current_cookie_jar, ensure_cookie_jar, run_scripts, run_scripts_with_base,
};
use browser_layout::{construct_layout_tree, layout as run_layout, LayoutConfig};
use browser_net::HttpClient;
use browser_render::render_ascii;
use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "browser", version, about = "Cross-platform browser toolkit")]
struct Cli {
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
    match cli.cmd {
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
        } => {
            let html = std::fs::read_to_string(&file)
                .with_context(|| format!("failed to read {}", file.display()))?;
            let text = render_html_to_string(&html, width, false, None)?;
            print!("{text}");
            if let Some(p) = screenshot {
                screenshot::render_text_to_png(&text, &p)?;
                eprintln!("[screenshot] wrote {}", p.display());
            }
            Ok(())
        }
        Cmd::RenderScript {
            file,
            width,
            screenshot,
            assert_network_idle,
        } => {
            let html = std::fs::read_to_string(&file)
                .with_context(|| format!("failed to read {}", file.display()))?;
            let text = render_html_to_string(&html, width, true, None)?;
            print!("{text}");
            if let Some(p) = screenshot {
                screenshot::render_text_to_png(&text, &p)
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
            assert_network_idle,
        } => {
            ensure_cookie_jar();
            let html = fetch_with_jar(&url).await?;
            let base = if no_js { None } else { Some(url.clone()) };
            let text = render_html_to_string(&html, width, !no_js, base)?;
            print!("{text}");
            if let Some(p) = screenshot {
                screenshot::render_text_to_png(&text, &p)
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
    Ok(render_ascii(&layout, width))
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
