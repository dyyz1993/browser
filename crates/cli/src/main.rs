//! `browser-cli` — command-line entry point.
//!
//! Subcommands:
//! - `browser parse <file>`           parse a local HTML file, print DOM tree
//! - `browser get  <url>`             fetch a URL via HTTPS, parse, print DOM tree
//! - `browser render-file <file>`     parse + layout + render a local HTML file
//! - `browser render-script <file>`   parse + execute scripts + layout + render
//! - `browser render-url  <url>`      fetch + parse + execute scripts + render

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{anyhow, Context, Result};
use browser_css_engine::{compute_styles, parse as parse_css};
use browser_dom::pretty_print;
use browser_html_parser::parse as parse_html;
use browser_js_runtime::{run_scripts, run_scripts_with_base};
use browser_layout::{construct_layout_tree, layout as run_layout, LayoutConfig};
use browser_net::get;
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
    /// Fetch a URL via HTTPS and print the parsed DOM tree.
    Get { url: String },
    /// Parse, lay out, and render a local HTML file as terminal ASCII.
    RenderFile {
        file: PathBuf,
        #[arg(long, default_value_t = 80)]
        width: usize,
    },
    /// Parse, execute <script> tags, then render. JS can mutate the
    /// DOM via __setBody / __appendBody / __setTitle / __log.
    RenderScript {
        file: PathBuf,
        #[arg(long, default_value_t = 80)]
        width: usize,
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
    },
}

async fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Parse { file } => {
            let html = std::fs::read_to_string(&file)
                .with_context(|| format!("failed to read {}", file.display()))?;
            let tree = parse_html(&html);
            println!("{}", pretty_print(&tree));
            Ok(())
        }
        Cmd::Get { url } => {
            let bytes = get(&url).await.context("fetch failed")?;
            let html = String::from_utf8(bytes)
                .map_err(|e| anyhow!("response is not valid UTF-8: {e}"))?;
            let tree = parse_html(&html);
            println!("{}", pretty_print(&tree));
            Ok(())
        }
        Cmd::RenderFile { file, width } => {
            let html = std::fs::read_to_string(&file)
                .with_context(|| format!("failed to read {}", file.display()))?;
            render_html_to_stdout(&html, width, false, None)?;
            Ok(())
        }
        Cmd::RenderScript { file, width } => {
            let html = std::fs::read_to_string(&file)
                .with_context(|| format!("failed to read {}", file.display()))?;
            render_html_to_stdout(&html, width, true, None)?;
            Ok(())
        }
        Cmd::RenderUrl { url, width, no_js } => {
            let bytes = get(&url)
                .await
                .with_context(|| format!("failed to fetch {url}"))?;
            let html = String::from_utf8(bytes)
                .map_err(|e| anyhow!("response is not valid UTF-8: {e}"))?;
            let base = if no_js { None } else { Some(url.clone()) };
            render_html_to_stdout(&html, width, !no_js, base)?;
            Ok(())
        }
        Cmd::Open {
            url,
            width,
            scale,
            win_width,
            win_height,
            no_js,
        } => {
            let bytes = get(&url)
                .await
                .with_context(|| format!("failed to fetch {url}"))?;
            let html = String::from_utf8(bytes)
                .map_err(|e| anyhow!("response is not valid UTF-8: {e}"))?;
            let base = if no_js { None } else { Some(url.clone()) };
            let text = render_html_to_string(&html, width, !no_js, base)?;
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
fn render_html_to_stdout(
    html: &str,
    width: usize,
    run_js: bool,
    base_url: Option<String>,
) -> Result<()> {
    let out = render_html_to_string(html, width, run_js, base_url)?;
    print!("{out}");
    Ok(())
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
    let tree = shared_tree.borrow();
    let sheet = parse_css("");
    let styles = compute_styles(&tree, &sheet);
    let mut layout = construct_layout_tree(&tree, &styles);
    run_layout(
        &mut layout,
        LayoutConfig {
            viewport_width: width as f32,
        },
    );
    Ok(render_ascii(&layout, width))
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
