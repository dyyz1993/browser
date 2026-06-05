//! `browser-cli` — command-line entry point.
//!
//! Subcommands (M1.5):
//! - `browser parse <file>` — parse a local HTML file and print the DOM
//! - `browser get  <url>`   — fetch a URL, parse, and print the DOM

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{anyhow, Context, Result};
use browser_dom::pretty_print;
use browser_html_parser::parse;
use browser_net::get;
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
}

async fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Parse { file } => {
            let html = std::fs::read_to_string(&file)
                .with_context(|| format!("failed to read {}", file.display()))?;
            let tree = parse(&html);
            println!("{}", pretty_print(&tree));
            Ok(())
        }
        Cmd::Get { url } => {
            let bytes = get(&url).await.context("fetch failed")?;
            let html = String::from_utf8(bytes)
                .map_err(|e| anyhow!("response is not valid UTF-8: {e}"))?;
            let tree = parse(&html);
            println!("{}", pretty_print(&tree));
            Ok(())
        }
    }
}

fn main() -> ExitCode {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("failed to build tokio runtime");
    match rt.block_on(run()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            // Print the error chain in full so users see the root cause.
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    /// Smoke test: the crate compiles and the binary is wired into
    /// the workspace.
    #[test]
    fn ping() {
        assert!("browser-cli".contains('-'));
    }
}
