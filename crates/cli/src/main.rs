//! `browser-cli` — command-line entry point.
//!
//! Subcommands:
//! - `browser parse <file>`        parse a local HTML file, print DOM tree
//! - `browser get  <url>`          fetch a URL via HTTPS, parse, print DOM tree
//! - `browser render-file <file>`  parse + layout + render a local HTML file
//!   to terminal ASCII (M2.8)

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{anyhow, Context, Result};
use browser_css_engine::{compute_styles, parse as parse_css};
use browser_dom::pretty_print;
use browser_html_parser::parse as parse_html;
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
    Parse {
        /// Path to the HTML file.
        file: PathBuf,
    },
    /// Fetch a URL via HTTPS and print the parsed DOM tree.
    Get {
        /// URL to fetch. Must be http(s)://.
        url: String,
    },
    /// Parse, lay out, and render a local HTML file as terminal ASCII.
    RenderFile {
        /// Path to the HTML file.
        file: PathBuf,
        /// Viewport width in characters (default: 80).
        #[arg(long, default_value_t = 80)]
        width: usize,
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
            render_html_to_stdout(&html, width)?;
            Ok(())
        }
    }
}

/// Shared render pipeline: html → dom → layout → ascii → stdout.
fn render_html_to_stdout(html: &str, width: usize) -> Result<()> {
    let tree = parse_html(html);
    let sheet = parse_css("");
    let styles = compute_styles(&tree, &sheet);
    let mut layout = construct_layout_tree(&tree, &styles);
    run_layout(
        &mut layout,
        LayoutConfig {
            viewport_width: width as f32,
        },
    );
    let out = render_ascii(&layout, width);
    print!("{out}");
    Ok(())
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

    use super::render_html_to_stdout;

    #[test]
    fn render_pipeline_simple_html_outputs_hello() {
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
    fn render_pipeline_empty_html_returns_no_panic() {
        let html = "";
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
        let _ = browser_render::render_ascii(&layout, 80);
        // Just exercising — no panic is the pass condition.
        let _ = render_html_to_stdout;
    }
}
