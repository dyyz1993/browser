//! Regression tests for `parse_fragment` (innerHTML semantics) and
//! long-script fidelity of both parse entry points.
//!
//! Background: `parse` uses document tree-construction modes, which relocate
//! a leading `<script>` into `<head>`. `innerHTML` setters that copy only the
//! `body` children therefore lost the script entirely. `parse_fragment` uses
//! the HTML5 fragment parsing algorithm (body context) so every fragment node
//! stays in place.

use browser_dom::{NodeData, NodeId};
use browser_html_parser::{parse, parse_fragment};

/// Collect `(tag, concatenated_text)` for every script element, in document order.
fn scripts(tree: &browser_dom::Tree) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut stack: Vec<NodeId> = vec![tree.root()];
    while let Some(id) = stack.pop() {
        let node = tree.get(id);
        if let NodeData::Element { tag, attrs } = &node.data {
            if tag.eq_ignore_ascii_case("script") {
                let mut text = String::new();
                for &child in &node.children {
                    if let NodeData::Text(s) = tree.data(child) {
                        text.push_str(s);
                    }
                }
                out.push((
                    attrs
                        .iter()
                        .find(|(n, _)| n.eq_ignore_ascii_case("src"))
                        .map_or_else(|| tag.clone(), |(_, v)| format!("{tag}[src={v}]")),
                    text,
                ));
                continue; // no nested scripts per HTML5
            }
        }
        for &child in node.children.iter().rev() {
            stack.push(child);
        }
    }
    out
}

fn body_children_tags(tree: &browser_dom::Tree) -> Vec<String> {
    let mut tags = Vec::new();
    let mut stack: Vec<NodeId> = vec![tree.root()];
    while let Some(id) = stack.pop() {
        let node = tree.get(id);
        if let NodeData::Element { tag, .. } = &node.data {
            if tag == "body" {
                for &c in &node.children {
                    if let NodeData::Element { tag: t, .. } = tree.data(c) {
                        tags.push(t.clone());
                    }
                }
                break;
            }
        }
        for &c in &node.children {
            stack.push(c);
        }
    }
    tags
}

// ---------------------------------------------------------------- 修复点

#[test]
fn fragment_starting_with_script_keeps_script_node() {
    // document 解析会把开头的 <script> 挪进 <head>；片段解析必须保留为 body 子节点
    let tree = parse_fragment("<script>var a = 1;</script>");
    assert_eq!(body_children_tags(&tree), vec!["script".to_string()]);
    let s = scripts(&tree);
    assert_eq!(s.len(), 1);
    assert_eq!(s[0].1, "var a = 1;");
}

#[test]
fn fragment_starting_with_style_keeps_style_node() {
    let tree = parse_fragment("<style>p { color: red; }</style><p>t</p>");
    let tags = body_children_tags(&tree);
    assert_eq!(tags.first().map(String::as_str), Some("style"));
    assert!(tags.contains(&"p".to_string()));
}

#[test]
fn fragment_mixed_content_order_preserved() {
    let tree = parse_fragment("<div>head</div><script>var b = 2;</script>tail");
    let tags = body_children_tags(&tree);
    assert_eq!(tags, vec!["div".to_string(), "script".to_string()]);
    // 尾部裸文本也在 body 子节点里
    let has_tail_text = {
        let mut found = false;
        let mut stack = vec![tree.root()];
        while let Some(id) = stack.pop() {
            let node = tree.get(id);
            if let NodeData::Element { tag, .. } = &node.data {
                if tag == "body" {
                    for &c in &node.children {
                        if let NodeData::Text(s) = tree.data(c) {
                            found = found || s == "tail";
                        }
                    }
                }
            }
            for &c in &node.children {
                stack.push(c);
            }
        }
        found
    };
    assert!(has_tail_text, "trailing text node missing under body");
    let s = scripts(&tree);
    assert_eq!(s.len(), 1);
    assert_eq!(s[0].1, "var b = 2;");
}

// ---------------------------------------------------------------- 长 script 完整性

fn assert_long_script_preserved(html: &str, expected: &str) {
    for (label, tree) in [
        ("fragment", parse_fragment(html)),
        ("document", parse(html)),
    ] {
        let s = scripts(&tree);
        assert_eq!(s.len(), 1, "{label}: script element missing");
        assert_eq!(s[0].1.len(), expected.len(), "{label}: length mismatch");
        assert_eq!(s[0].1, expected, "{label}: content mismatch");
    }
}

#[test]
fn long_script_64k_preserved_byte_for_byte() {
    // >64KB：跨 html5ever tendril 缓冲块边界（默认缓冲 8KB/64KB 量级）
    let filler = "x".repeat(65_536);
    let code = format!("var a = 1;\n// {filler}\nfunction f() {{}}(1);");
    let html = format!("<html><body><script>{code}</script></body></html>");
    assert_long_script_preserved(&html, &code);
}

#[test]
fn long_script_1mb_preserved_byte_for_byte() {
    let filler = "y".repeat(1_048_576);
    let code = format!("var a = 2;/*{filler}*/var b = 3;");
    let html = format!("<div><script>{code}</script></div>");
    assert_long_script_preserved(&html, &code);
}

#[test]
fn script_at_exact_power_of_two_lengths() {
    // 精确落在 4096 / 8192 / 16384 字节边界的内容
    for size in [4094usize, 8190, 16_382] {
        let filler = "z".repeat(size);
        let code = format!("var c = 0;/*{filler}*/");
        let html = format!("<script>{code}</script>");
        assert_long_script_preserved(&html, &code);
    }
}

// ---------------------------------------------------------------- </script> 边界

#[test]
fn fragment_script_ends_at_first_close_tag() {
    // HTML5 规范：原始文本模式在第一个 </script> 结束（与 Chrome 一致）
    let tree = parse_fragment("<script>var s = '</b>';</script><b>x</b>");
    let s = scripts(&tree);
    assert_eq!(s.len(), 1);
    assert_eq!(s[0].1, "var s = '</b>';");
    assert!(body_children_tags(&tree).contains(&"b".to_string()));
}

#[test]
fn fragment_script_with_escaped_close_tag_keeps_content() {
    // JS 层转义 `<\/script>` 不构成真实的结束标签——整段保留在脚本文本里
    let tree = parse_fragment(r#"<script>var s = "<\/script>"; if (s) { ok(); }</script>"#);
    let s = scripts(&tree);
    assert_eq!(s.len(), 1);
    assert_eq!(s[0].1, r#"var s = "<\/script>"; if (s) { ok(); }"#);
}

#[test]
fn fragment_two_scripts_both_collected() {
    let tree = parse_fragment("<script>a();</script><script>b();</script>");
    let s = scripts(&tree);
    assert_eq!(s.len(), 2);
    assert_eq!(s[0].1, "a();");
    assert_eq!(s[1].1, "b();");
}

// ---------------------------------------------------------------- 属性特殊字符

#[test]
fn fragment_attr_special_chars_preserved() {
    let tree = parse_fragment(r#"<div data-x="a<b>c&amp;d" data-empty='' title='q"q'>t</div>"#);
    let mut found = None;
    let mut stack = vec![tree.root()];
    while let Some(id) = stack.pop() {
        let node = tree.get(id);
        if let NodeData::Element { tag, attrs } = &node.data {
            if tag == "div" {
                found = Some(attrs.clone());
            }
        }
        for &c in &node.children {
            stack.push(c);
        }
    }
    let attrs = found.expect("div missing");
    assert_eq!(
        attrs,
        vec![
            ("data-x".to_string(), "a<b>c&d".to_string()),
            ("data-empty".to_string(), String::new()),
            ("title".to_string(), "q\"q".to_string()),
        ]
    );
}

// ---------------------------------------------------------------- parse() 语义不回归

#[test]
fn document_parse_still_moves_leading_script_to_head() {
    // parse() 保持 document 语义：开头的 <script> 属于 <head>（不回归到片段行为）
    let tree = parse("<script>var a = 1;</script>");
    let head_script_text = (|| {
        let mut stack: Vec<NodeId> = vec![tree.root()];
        while let Some(id) = stack.pop() {
            let node = tree.get(id);
            if let NodeData::Element { tag, .. } = &node.data {
                if tag == "head" {
                    let mut text = String::new();
                    for &c in &node.children {
                        if let NodeData::Element { tag: t, .. } = tree.data(c) {
                            if t == "script" {
                                for &cc in &tree.get(c).children {
                                    if let NodeData::Text(s) = tree.data(cc) {
                                        text.push_str(s);
                                    }
                                }
                            }
                        }
                    }
                    return Some(text);
                }
            }
            for &c in &node.children {
                stack.push(c);
            }
        }
        None
    })();
    assert_eq!(head_script_text.as_deref(), Some("var a = 1;"));
}
