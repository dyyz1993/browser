//! `--format images`: 提取页面中所有 `<img>` 标签的 src + alt。
//!
//! 对标 Firecrawl `formats: ["images"]`。输出格式 `alt text → URL`，
//! 与 format_links.rs 风格一致。

use browser_dom::{NodeData, NodeId, Tree};
use std::collections::HashSet;
use url::Url;

/// 提取图片地图。
pub fn to_images(
    tree: &Tree,
    base_url: Option<&str>,
    _selector: Option<&str>,
    excluded: &HashSet<NodeId>,
) -> Result<String, String> {
    let base = base_url.and_then(|b| Url::parse(b).ok());
    let mut results: Vec<String> = Vec::new();

    tree.traverse(tree.root(), |id, _| {
        if excluded.contains(&id) {
            return true;
        }
        if let NodeData::Element { tag, attrs } = &tree.get(id).data {
            if tag.eq_ignore_ascii_case("img") {
                let src = attrs
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case("src"))
                    .map(|(_, v)| resolve(v, &base));
                let alt = attrs
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case("alt"))
                    .map(|(_, v)| v.as_str())
                    .unwrap_or("");
                if let Some(url) = src {
                    if !url.is_empty() {
                        let line = if alt.is_empty() {
                            format!("(no alt) → {url}")
                        } else {
                            format!("{alt} → {url}")
                        };
                        results.push(line);
                    }
                }
            }
        }
        true
    });

    if results.is_empty() {
        Ok("(no images found)".to_string())
    } else {
        Ok(results.join("\n"))
    }
}

fn resolve(href: &str, base: &Option<Url>) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;
    use browser_html_parser::parse;

    #[test]
    fn extracts_images_with_alt() {
        let html = r#"<img src="https://x.com/a.png" alt="pic">"#;
        let tree = parse(html);
        let out = to_images(&tree, None, None, &HashSet::new()).unwrap();
        assert!(out.contains("pic → https://x.com/a.png"), "got: {out}");
    }

    #[test]
    fn extracts_multiple_images() {
        let html = r#"<img src="a.png" alt="A"><img src="b.png" alt="B">"#;
        let tree = parse(html);
        let out = to_images(&tree, None, None, &HashSet::new()).unwrap();
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 2, "should have 2 images: {out}");
    }

    #[test]
    fn skips_excluded_node() {
        let html = r#"<div><img src="x.png" alt="X"></div>"#;
        let tree = parse(html);
        // find the img node and exclude it
        let mut excluded = HashSet::new();
        tree.traverse(tree.root(), |id, _| {
            if let NodeData::Element { tag, .. } = &tree.get(id).data {
                if tag == "img" {
                    excluded.insert(id);
                    return false;
                }
            }
            true
        });
        let out = to_images(&tree, None, None, &excluded).unwrap();
        assert!(out.contains("no images"), "should be empty: {out}");
    }

    #[test]
    fn handles_relative_url() {
        let html = r#"<img src="/path/img.png" alt="rel">"#;
        let tree = parse(html);
        let out = to_images(&tree, Some("https://site.com/page"), None, &HashSet::new()).unwrap();
        assert!(out.contains("https://site.com/path/img.png"), "got: {out}");
    }
}
