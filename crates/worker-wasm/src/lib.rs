//! WASM 入口：给 Cloudflare Worker 调用。
//!
//! 提供 `extract_markdown(html, base_url)` 等 JS 可调函数。

use browser_extractor::{run_extract, FetchOptions, OutputFormat};
use wasm_bindgen::prelude::*;

/// 解析 HTML + 提取 markdown。
#[wasm_bindgen]
pub fn extract_markdown(html: &str, base_url: Option<String>) -> String {
    let tree = browser_html_parser::parse(html);
    let opts = FetchOptions {
        format: OutputFormat::Markdown,
        only_main_content: true,
        ..Default::default()
    };
    match run_extract(&tree, base_url.as_deref(), &opts) {
        Ok(result) => result.content,
        Err(e) => format!("error: {e}"),
    }
}

/// 解析 HTML + 提取纯文本。
#[wasm_bindgen]
pub fn extract_text(html: &str, base_url: Option<String>) -> String {
    let tree = browser_html_parser::parse(html);
    let opts = FetchOptions {
        format: OutputFormat::Text,
        only_main_content: true,
        ..Default::default()
    };
    match run_extract(&tree, base_url.as_deref(), &opts) {
        Ok(result) => result.content,
        Err(e) => format!("error: {e}"),
    }
}

/// 解析 HTML + 提取链接地图。
#[wasm_bindgen]
pub fn extract_links(html: &str, base_url: Option<String>) -> String {
    let tree = browser_html_parser::parse(html);
    let opts = FetchOptions {
        format: OutputFormat::Links,
        only_main_content: true,
        ..Default::default()
    };
    match run_extract(&tree, base_url.as_deref(), &opts) {
        Ok(result) => result.content,
        Err(e) => format!("error: {e}"),
    }
}

/// 解析 HTML + 提取完整 HTML（序列化 DOM）。
#[wasm_bindgen]
pub fn extract_html(html: &str, base_url: Option<String>) -> String {
    let tree = browser_html_parser::parse(html);
    let opts = FetchOptions {
        format: OutputFormat::Html,
        only_main_content: true,
        ..Default::default()
    };
    match run_extract(&tree, base_url.as_deref(), &opts) {
        Ok(result) => result.content,
        Err(e) => format!("error: {e}"),
    }
}

/// 提取图片（alt text → URL）。
#[wasm_bindgen]
pub fn extract_images(html: &str, base_url: Option<String>) -> String {
    let tree = browser_html_parser::parse(html);
    let opts = FetchOptions {
        format: OutputFormat::Images,
        only_main_content: true,
        ..Default::default()
    };
    match run_extract(&tree, base_url.as_deref(), &opts) {
        Ok(result) => result.content,
        Err(e) => format!("error: {e}"),
    }
}

/// 提取高亮文本（mark/strong/em/b）。
#[wasm_bindgen]
pub fn extract_highlights(html: &str, base_url: Option<String>) -> String {
    let tree = browser_html_parser::parse(html);
    let opts = FetchOptions {
        format: OutputFormat::Highlights,
        only_main_content: true,
        ..Default::default()
    };
    match run_extract(&tree, base_url.as_deref(), &opts) {
        Ok(result) => result.content,
        Err(e) => format!("error: {e}"),
    }
}

/// 提取品牌信息（title/description/og:tags/icon）。
#[wasm_bindgen]
pub fn extract_branding(html: &str, base_url: Option<String>) -> String {
    let tree = browser_html_parser::parse(html);
    let opts = FetchOptions {
        format: OutputFormat::Branding,
        only_main_content: true,
        ..Default::default()
    };
    match run_extract(&tree, base_url.as_deref(), &opts) {
        Ok(result) => result.content,
        Err(e) => format!("error: {e}"),
    }
}

/// 提取页面标题。
#[wasm_bindgen]
pub fn extract_title(html: &str) -> String {
    let tree = browser_html_parser::parse(html);
    let opts = FetchOptions::default();
    match run_extract(&tree, None, &opts) {
        Ok(result) => result.title.unwrap_or_default(),
        Err(_) => String::new(),
    }
}
