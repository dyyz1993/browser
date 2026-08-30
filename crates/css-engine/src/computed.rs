//! Compute the set of declarations that apply to each element in a tree.
//!
//! Output: a map from element NodeId to its matching [`Declaration`]s,
//! in the order they appear in the stylesheet. (Cascade priority and
//! specificity are not implemented in M2 — every matching declaration
//! contributes, last-write-wins by source order.)

use std::collections::HashMap;

use browser_dom::{NodeData, NodeId, Tree};

use crate::ast::{Declaration, Stylesheet};
use crate::selector::Selector;

/// For every Element in `tree`, collect the declarations whose selector
/// list matches that element. Text / Comment / Doctype nodes are skipped.
///
/// M2 cascade rule: later rules in the stylesheet override earlier ones
/// (we don't implement specificity or `!important` priority yet).
///
/// M70.1: also folds each Element's inline `style="..."` attribute into the
/// computed map. Inline styles are appended last (last-write-wins), giving
/// them the highest priority — matching browser behavior where inline styles
/// beat stylesheet rules (without `!important`).
///
/// M72.1: the built-in UA stylesheet ([`crate::ua::ua_stylesheet`]) is
/// prepended before the page's rules, so author CSS overrides browser
/// defaults while unstyled elements still get them (h1 sizing, p margins,
/// list indents...). Full order, lowest → highest priority:
/// UA sheet → page sheet → inline `style="..."`.
#[must_use]
pub fn compute_styles(tree: &Tree, sheet: &Stylesheet) -> HashMap<NodeId, Vec<Declaration>> {
    // Pre-parse every rule's selector list once. UA rules first (lowest
    // priority), page rules after.
    let ua = crate::ua::ua_stylesheet();
    let mut parsed: Vec<(Selector, &[Declaration])> =
        Vec::with_capacity(ua.rules.len() + sheet.rules.len());
    for r in ua.rules.iter().chain(sheet.rules.iter()) {
        if let Ok(s) = Selector::parse(&r.selectors) {
            parsed.push((s, r.declarations.as_slice()));
        }
    }

    let mut out: HashMap<NodeId, Vec<Declaration>> = HashMap::new();
    tree.traverse(tree.root(), |id, node| {
        if let NodeData::Element { tag: _, attrs } = &node.data {
            let mut decls: Vec<Declaration> = Vec::new();
            // Stylesheet rules (selector-matched).
            for (sel, ds) in &parsed {
                if sel.matches(tree, id) {
                    decls.extend_from_slice(ds);
                }
            }
            // M70.1: inline `style="..."` attribute — appended last so it wins.
            if let Some(style_val) = attrs
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("style"))
                .map(|(_, v)| v.as_str())
            {
                decls.extend(crate::parser::parse_declaration_list(style_val));
            }
            if !decls.is_empty() {
                out.insert(id, decls);
            }
        }
        true
    });

    // M81: CSS 变量（custom properties / var()）基础消费 pass。
    resolve_custom_properties(tree, &mut out);
    out
}

// ---------------------------------------------------------------------------
// M81: CSS 变量（custom properties / var()）
// ---------------------------------------------------------------------------

/// Resolve CSS custom properties for every element in `styles`, in place.
///
/// Three sub-steps (minimal-usable semantics, not full spec):
/// 1. **收集** — each element's `--*` declarations (stylesheet + inline
///    `style="..."`) become that element's variable table;
/// 2. **继承** — a child starts from its parent's table (pre-order traversal
///    guarantees parents are built first); the child's own `--*` declarations
///    override inherited ones. Custom property names are case-sensitive.
/// 3. **替换** — every declaration value's `var(--name)` /
///    `var(--name, fallback)` is expanded (color / font-size / margin /
///    padding / border / shorthand values — anything that carries a value).
///
/// Undefined variable without fallback: the `var(--name)` text is kept as-is
/// (downstream property parsers already treat it as unparseable → ignored,
/// matching pre-M81 behavior). `--*` declarations themselves keep their raw
/// values; nested `var()` chains resolve recursively at consumption time.
fn resolve_custom_properties(tree: &Tree, styles: &mut HashMap<NodeId, Vec<Declaration>>) {
    // Steps 1+2: build per-element variable tables, top-down.
    let mut var_maps: HashMap<NodeId, HashMap<String, String>> = HashMap::new();
    tree.traverse(tree.root(), |id, node| {
        if !matches!(node.data, NodeData::Element { .. }) {
            return true;
        }
        let mut vars: HashMap<String, String> = node
            .parent
            .and_then(|pid| var_maps.get(&pid))
            .cloned()
            .unwrap_or_default();
        if let Some(decls) = styles.get(&id) {
            for d in decls {
                if d.property.starts_with("--") {
                    vars.insert(d.property.clone(), d.value.clone());
                }
            }
        }
        if !vars.is_empty() {
            var_maps.insert(id, vars);
        }
        true
    });

    // Step 3: substitute var() references in declaration values. Elements
    // with an empty table still participate — `var(--x, fallback)` must
    // resolve to the fallback even when nothing is defined.
    let empty_vars: HashMap<String, String> = HashMap::new();
    for (id, decls) in styles.iter_mut() {
        let vars = var_maps.get(id).unwrap_or(&empty_vars);
        for d in decls.iter_mut() {
            if d.property.starts_with("--") || !d.value.contains("var(") {
                continue;
            }
            d.value = substitute_vars(&d.value, vars, 0);
        }
    }
}

/// Custom-property name / ident character (ASCII ident + non-ASCII, so
/// names like `--主色` don't truncate the scan).
fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b >= 0x80
}

/// Parse a `var(` call whose `(` sits at `paren + 1`... i.e. `start` points
/// just past `var(`. Returns `(name, fallback, end_index_past_closing_paren)`,
/// or `None` when the call is malformed (caller keeps the text as-is).
fn parse_var_call(value: &str, start: usize) -> Option<(String, String, usize)> {
    let bytes = value.as_bytes();
    let mut i = start;
    // Name must start with `--`.
    if bytes.get(i) != Some(&b'-') || bytes.get(i + 1) != Some(&b'-') {
        return None;
    }
    let name_start = i;
    i += 2;
    while i < bytes.len() && is_ident_byte(bytes[i]) {
        i += 1;
    }
    if i == name_start + 2 {
        return None; // empty custom property name
    }
    let name = value[name_start..i].to_string();
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    match bytes.get(i) {
        Some(&b')') => Some((name, String::new(), i + 1)),
        Some(&b',') => {
            i += 1;
            // Fallback = everything up to the *matching* `)` (balanced, so
            // `var(--x, rgba(0,0,0,.5))` works).
            let fb_start = i;
            let mut depth = 1usize;
            while i < bytes.len() {
                match bytes[i] {
                    b'(' => depth += 1,
                    b')' => {
                        depth -= 1;
                        if depth == 0 {
                            return Some((name, value[fb_start..i].to_string(), i + 1));
                        }
                    }
                    _ => {}
                }
                i += 1;
            }
            None // unbalanced parentheses
        }
        _ => None,
    }
}

/// Replace every `var(--name)` / `var(--name, fallback)` in `value` with the
/// corresponding entry from `vars`.
///
/// - Substituted values (and fallbacks) may themselves contain `var()` —
///   resolved recursively; `depth` guards reference cycles (over the cap the
///   original text is kept).
/// - Undefined name with fallback → fallback; without → original text kept.
fn substitute_vars(value: &str, vars: &HashMap<String, String>, depth: usize) -> String {
    const MAX_DEPTH: usize = 16;
    if depth > MAX_DEPTH {
        return value.to_string();
    }
    let bytes = value.as_bytes();
    let mut out = String::with_capacity(value.len());
    let mut i = 0;
    while i < bytes.len() {
        // `var(` counts as a function call only when not preceded by an ident
        // character (so e.g. `somevar(` is not a call).
        let is_call = bytes[i] == b'v'
            && value[i..].starts_with("var(")
            && (i == 0 || !is_ident_byte(bytes[i - 1]));
        if is_call {
            if let Some((name, fallback, end)) = parse_var_call(value, i + 4) {
                match vars.get(&name) {
                    Some(v) => out.push_str(&substitute_vars(v, vars, depth + 1)),
                    None if !fallback.trim().is_empty() => {
                        out.push_str(&substitute_vars(fallback.trim(), vars, depth + 1));
                    }
                    None => out.push_str(&value[i..end]), // keep original text
                }
                i = end;
                continue;
            }
        }
        // Copy bytes up to the next potential call start (ASCII 'v' — slice
        // boundaries stay on char boundaries).
        let stop = match bytes[i..].iter().position(|&b| b == b'v') {
            Some(rel) => i + rel,
            None => bytes.len(),
        };
        if stop == i {
            out.push('v'); // lone 'v' that is not a var( call (ASCII, 1 byte)
            i += 1;
        } else {
            out.push_str(&value[i..stop]);
            i = stop;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse as parse_css;
    use browser_dom::{NodeData, Tree};

    /// Tree: html > body > p
    fn fixture() -> Tree {
        let mut t = Tree::with_root(NodeData::Document);
        let root = t.root();
        let html = t.insert(
            Some(root),
            NodeData::Element {
                tag: "html".into(),
                attrs: vec![],
            },
        );
        let body = t.insert(
            Some(html),
            NodeData::Element {
                tag: "body".into(),
                attrs: vec![],
            },
        );
        let _p = t.insert(
            Some(body),
            NodeData::Element {
                tag: "p".into(),
                attrs: vec![("class".into(), "text".into())],
            },
        );
        t
    }

    fn get(out: &HashMap<NodeId, Vec<Declaration>>, id: NodeId, prop: &str) -> Option<String> {
        out.get(&id)
            .and_then(|decls| decls.iter().rev().find(|d| d.property == prop))
            .map(|d| d.value.clone())
    }

    #[test]
    fn compute_styles_applies_matching_rule() {
        let tree = fixture();
        let sheet = parse_css("p { color: red; }");
        let styles = compute_styles(&tree, &sheet);
        // <p> is at id=4 (root=0, html=1, body=2, p=3). Wait — 3 children
        // inserted under root? Let me check: root, html, body, p = ids 0..3.
        assert_eq!(get(&styles, 3, "color").as_deref(), Some("red"));
    }

    #[test]
    fn compute_styles_skips_non_matching() {
        let tree = fixture();
        let sheet = parse_css("h1 { color: red; }");
        let styles = compute_styles(&tree, &sheet);
        // No element is h1, so the page rule matches nothing. M72.1: the
        // <p> still receives UA-default declarations (e.g. margin), but
        // never the page rule's color.
        assert_eq!(get(&styles, 3, "color"), None);
        assert!(get(&styles, 3, "margin").is_some(), "UA p margin expected");
    }

    #[test]
    fn compute_styles_later_rule_overrides() {
        let tree = fixture();
        let sheet = parse_css("p { color: red; } p { color: blue; }");
        let styles = compute_styles(&tree, &sheet);
        // Last-write-wins → blue.
        assert_eq!(get(&styles, 3, "color").as_deref(), Some("blue"));
    }

    #[test]
    fn compute_styles_class_selector() {
        let tree = fixture();
        let sheet = parse_css(".text { font-size: 14px; }");
        let styles = compute_styles(&tree, &sheet);
        assert_eq!(get(&styles, 3, "font-size").as_deref(), Some("14px"));
    }

    #[test]
    fn compute_styles_descendant_selector() {
        let tree = fixture();
        let sheet = parse_css("body p { color: green; }");
        let styles = compute_styles(&tree, &sheet);
        assert_eq!(get(&styles, 3, "color").as_deref(), Some("green"));
        // <body> alone doesn't match.
        assert!(get(&styles, 2, "color").is_none());
    }

    #[test]
    fn compute_styles_text_nodes_not_in_output() {
        let tree = fixture();
        let sheet = parse_css("* { color: red; }");
        let styles = compute_styles(&tree, &sheet);
        // Only Element node ids appear in the output.
        for id in styles.keys() {
            assert!(
                matches!(tree.data(*id), NodeData::Element { .. }),
                "non-Element node {id} in styles map"
            );
        }
    }

    // ---- M70.1: inline `style="..."` attribute parsing ----

    /// Build a tree where <p> has an inline style attribute.
    fn fixture_with_inline_style(style_val: &str) -> Tree {
        let mut t = Tree::with_root(NodeData::Document);
        let root = t.root();
        let html = t.insert(
            Some(root),
            NodeData::Element {
                tag: "html".into(),
                attrs: vec![],
            },
        );
        let body = t.insert(
            Some(html),
            NodeData::Element {
                tag: "body".into(),
                attrs: vec![],
            },
        );
        let _p = t.insert(
            Some(body),
            NodeData::Element {
                tag: "p".into(),
                attrs: vec![("style".into(), style_val.into())],
            },
        );
        t
    }

    #[test]
    fn inline_style_attribute_is_parsed() {
        // <p style="display: grid; color: red">
        let tree = fixture_with_inline_style("display: grid; color: red");
        let sheet = parse_css(""); // empty stylesheet
        let styles = compute_styles(&tree, &sheet);
        // p is at id 3 (root=0, html=1, body=2, p=3)
        assert_eq!(get(&styles, 3, "display").as_deref(), Some("grid"));
        assert_eq!(get(&styles, 3, "color").as_deref(), Some("red"));
    }

    #[test]
    fn inline_style_overrides_stylesheet() {
        // Inline style should win over stylesheet rule (last-write-wins).
        let tree = fixture_with_inline_style("color: blue");
        let sheet = parse_css("p { color: red; }");
        let styles = compute_styles(&tree, &sheet);
        // blue (inline) beats red (stylesheet)
        assert_eq!(get(&styles, 3, "color").as_deref(), Some("blue"));
    }

    #[test]
    fn inline_style_missing_is_noop() {
        // Element without style attribute → no inline decls. M72.1: UA
        // defaults still show up (p margin), but the UA sheet declares no
        // `color` for p, so a missing inline style means no color either.
        let tree = fixture(); // fixture's <p> has class but no style
        let sheet = parse_css("");
        let styles = compute_styles(&tree, &sheet);
        assert_eq!(get(&styles, 3, "color"), None);
    }

    // ---- M81: CSS 变量（custom properties / var()）----

    /// `:root` 定义 + `body` 消费：M81 验证样例（var_test.html 语义）。
    #[test]
    fn var_defined_on_root_consumed_by_body() {
        let tree = fixture();
        let sheet = parse_css(
            ":root { --main-color: #ff0000; --gap: 10px; } \
             body { color: var(--main-color); margin: var(--gap); }",
        );
        let styles = compute_styles(&tree, &sheet);
        // html=1 是 :root（parent 是 Document），body=2 消费变量。
        assert_eq!(get(&styles, 2, "color").as_deref(), Some("#ff0000"));
        assert_eq!(get(&styles, 2, "margin").as_deref(), Some("10px"));
    }

    /// 变量沿树向下继承：孙辈 `<p>`（无自身声明）也能消费 `:root` 的变量。
    #[test]
    fn var_inherits_to_descendants() {
        let tree = fixture();
        let sheet = parse_css(
            ":root { --c: teal; } \
             * { color: var(--c); }",
        );
        let styles = compute_styles(&tree, &sheet);
        // p=3 自身无 --c 声明，值来自 html(1) 的继承。
        assert_eq!(get(&styles, 3, "color").as_deref(), Some("teal"));
    }

    /// 自身定义覆盖继承值。
    #[test]
    fn var_own_definition_overrides_inherited() {
        let tree = fixture();
        let sheet = parse_css(
            ":root { --c: red; } \
             * { color: var(--c); } \
             .text { --c: blue; }",
        );
        let styles = compute_styles(&tree, &sheet);
        // body=2 继承 html 的 red；p=3（.text）自身 --c: blue 覆盖。
        assert_eq!(get(&styles, 2, "color").as_deref(), Some("red"));
        assert_eq!(get(&styles, 3, "color").as_deref(), Some("blue"));
    }

    /// 未定义变量带 fallback → 取 fallback。
    #[test]
    fn var_fallback_used_when_undefined() {
        let tree = fixture();
        let sheet = parse_css("body { margin: var(--nope, 7px); }");
        let styles = compute_styles(&tree, &sheet);
        assert_eq!(get(&styles, 2, "margin").as_deref(), Some("7px"));
    }

    /// 未定义变量无 fallback → 原样保留。
    #[test]
    fn var_undefined_without_fallback_kept_as_is() {
        let tree = fixture();
        let sheet = parse_css("body { color: var(--nope); }");
        let styles = compute_styles(&tree, &sheet);
        assert_eq!(get(&styles, 2, "color").as_deref(), Some("var(--nope)"));
    }

    /// var() 嵌套链：`--a` 指向 `--b`，消费处递归展开。
    #[test]
    fn var_nested_chain_resolves() {
        let tree = fixture();
        let sheet = parse_css(
            ":root { --b: 4px; --a: var(--b); } \
             body { margin: var(--a); }",
        );
        let styles = compute_styles(&tree, &sheet);
        assert_eq!(get(&styles, 2, "margin").as_deref(), Some("4px"));
    }

    /// 混合值：`border: 1px solid var(--c)` 只替换 var() 部分。
    #[test]
    fn var_inside_mixed_value() {
        let tree = fixture();
        let sheet = parse_css(
            ":root { --c: red; } \
             body { border: 1px solid var(--c); }",
        );
        let styles = compute_styles(&tree, &sheet);
        assert_eq!(get(&styles, 2, "border").as_deref(), Some("1px solid red"));
    }

    /// inline style 里的变量定义与消费同样生效。
    #[test]
    fn var_defined_and_consumed_in_inline_style() {
        let tree = fixture_with_inline_style("--x: 9px; margin: var(--x)");
        let sheet = parse_css("");
        let styles = compute_styles(&tree, &sheet);
        assert_eq!(get(&styles, 3, "margin").as_deref(), Some("9px"));
    }

    /// fallback 里的嵌套括号（rgba(...)）按配平解析。
    #[test]
    fn var_fallback_with_balanced_parens() {
        let tree = fixture();
        let sheet = parse_css("body { color: var(--nope, rgba(0, 0, 255, .5)); }");
        let styles = compute_styles(&tree, &sheet);
        assert_eq!(
            get(&styles, 2, "color").as_deref(),
            Some("rgba(0, 0, 255, .5)")
        );
    }
}
