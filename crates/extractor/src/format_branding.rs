//! `--format branding`: 提取页面品牌/SEO 元数据。
//!
//! 对标 Firecrawl `formats: ["branding"]`。从 `<head>` 提取 title、meta
//! description、Open Graph 标签、favicon 等。

use browser_dom::{NodeData, NodeId, Tree};
use std::collections::HashSet;
use url::Url;

/// 提取品牌信息。
pub fn to_branding(
    tree: &Tree,
    base_url: Option<&str>,
    _selector: Option<&str>,
    _excluded: &HashSet<NodeId>,
) -> Result<String, String> {
    let base = base_url.and_then(|b| Url::parse(b).ok());
    let mut pairs: Vec<(String, String)> = Vec::new();

    tree.traverse(tree.root(), |id, _| {
        if let NodeData::Element { tag, attrs } = &tree.get(id).data {
            let lower = tag.to_ascii_lowercase();
            match lower.as_str() {
                "title" => {
                    let text = collect_text(tree, id);
                    if !text.is_empty() {
                        pairs.push(("title".into(), text));
                    }
                }
                "meta" => {
                    // description → name="description"
                    let name = attrs.iter().find(|(k, v)| {
                        k.eq_ignore_ascii_case("name") && v.eq_ignore_ascii_case("description")
                    });
                    if name.is_some() {
                        if let Some(content) = attrs.iter().find_map(|(k, v)| {
                            if k.eq_ignore_ascii_case("content") {
                                Some(v.clone())
                            } else {
                                None
                            }
                        }) {
                            pairs.push(("description".into(), content));
                        }
                    }
                    // Open Graph: <meta property="og:*" content="...">
                    if let Some(prop) = attrs.iter().find_map(|(k, v)| {
                        if k.eq_ignore_ascii_case("property") && v.starts_with("og:") {
                            Some(v.clone())
                        } else {
                            None
                        }
                    }) {
                        if let Some(content) = attrs.iter().find_map(|(k, v)| {
                            if k.eq_ignore_ascii_case("content") {
                                Some(v.clone())
                            } else {
                                None
                            }
                        }) {
                            pairs.push((prop, content));
                        }
                    }
                }
                "link" => {
                    // <link rel="icon" href="...">
                    let is_icon = attrs.iter().any(|(k, v)| {
                        k.eq_ignore_ascii_case("rel")
                            && (v.eq_ignore_ascii_case("icon")
                                || v.eq_ignore_ascii_case("shortcut icon")
                                || v.eq_ignore_ascii_case("apple-touch-icon"))
                    });
                    if is_icon {
                        if let Some(href) = attrs.iter().find_map(|(k, v)| {
                            if k.eq_ignore_ascii_case("href") {
                                Some(resolve(v, &base))
                            } else {
                                None
                            }
                        }) {
                            let rel = attrs
                                .iter()
                                .find_map(|(k, v)| {
                                    if k.eq_ignore_ascii_case("rel") {
                                        Some(v.clone())
                                    } else {
                                        None
                                    }
                                })
                                .unwrap_or_else(|| "icon".into());
                            pairs.push((format!("icon: {rel}"), href));
                        }
                    }
                }
                _ => {}
            }
        }
        true
    });

    if pairs.is_empty() {
        return Ok("(no branding found)".to_string());
    }

    // 最大 key 宽度对齐
    let max_key = pairs.iter().map(|(k, _)| k.len()).max().unwrap_or(0);
    let out: Vec<String> = pairs
        .iter()
        .map(|(k, v)| format!("{:max_key$}  {v}", k, max_key = max_key))
        .collect();
    Ok(out.join("\n"))
}

fn collect_text(tree: &Tree, id: NodeId) -> String {
    let mut out = String::new();
    for &child in tree.children_of(id) {
        if let NodeData::Text(s) = &tree.get(child).data {
            out.push_str(s);
        }
    }
    out.trim().to_string()
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
    fn extracts_title() {
        let html = r#"<html><head><title>My Site</title></head><body></body></html>"#;
        let tree = parse(html);
        let out = to_branding(&tree, None, None, &HashSet::new()).unwrap();
        assert!(out.contains("title"), "got: {out}");
        assert!(out.contains("My Site"), "got: {out}");
    }

    #[test]
    fn extracts_og_tags() {
        let html = r#"<html><head><meta property="og:title" content="Vue"><meta property="og:image" content="https://x.com/img.png"></head><body></body></html>"#;
        let tree = parse(html);
        let out = to_branding(&tree, None, None, &HashSet::new()).unwrap();
        assert!(out.contains("og:title"), "got: {out}");
        assert!(out.contains("og:image"), "got: {out}");
        assert!(out.contains("Vue"), "got: {out}");
    }

    #[test]
    fn extracts_icon() {
        let html = r#"<html><head><link rel="icon" href="/favicon.ico" type="image/x-icon"></head><body></body></html>"#;
        let tree = parse(html);
        let out = to_branding(&tree, Some("https://site.com"), None, &HashSet::new()).unwrap();
        assert!(out.contains("icon"), "got: {out}");
        assert!(out.contains("favicon.ico"), "got: {out}");
    }
}
