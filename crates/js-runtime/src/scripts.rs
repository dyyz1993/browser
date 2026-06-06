//! Extract and execute `<script>` bodies from a DOM tree.
//!
//! M3.3 scope: walk a parsed tree, pull the text content of every
//! `<script>` element, then run them in order through a boa `Context`
//! (with the bridge installed). The DOM mutations made by JS become
//! visible to the subsequent layout + render passes.

use boa_engine::{Context, Source};
use browser_dom::{NodeData, NodeId, Tree};

use crate::bridge::install;

/// Collect the text content of every `<script>` element in `tree`,
/// in document order. Empty scripts are filtered out.
#[must_use]
pub fn extract_scripts(tree: &Tree) -> Vec<String> {
    let mut scripts = Vec::new();
    let mut stack: Vec<NodeId> = vec![tree.root()];
    while let Some(id) = stack.pop() {
        let node = tree.get(id);
        if let NodeData::Element { tag, .. } = &node.data {
            if tag.eq_ignore_ascii_case("script") {
                let mut text = String::new();
                for &child_id in &node.children {
                    if let NodeData::Text(s) = tree.data(child_id) {
                        text.push_str(s);
                    }
                }
                if !text.trim().is_empty() {
                    scripts.push(text);
                }
                // Don't recurse into scripts (no nested scripts allowed by HTML5).
                continue;
            }
        }
        // Push children in reverse so left-most is popped first
        // (preserves document order).
        for &child in node.children.iter().rev() {
            stack.push(child);
        }
    }
    scripts
}

/// Run all `<script>` bodies in `tree` against `ctx`, with the bridge
/// installed so JS can mutate the DOM. Returns the count of scripts
/// that executed without throwing.
///
/// # Errors
/// Individual script errors are logged to stderr and don't abort the
/// run; the count returned reflects only successful executions.
pub fn execute_scripts(tree_shared: &crate::bridge::SharedTree, ctx: &mut Context) -> usize {
    execute_scripts_with_base(tree_shared, ctx, None)
}

/// Same as [`execute_scripts`], but also installs a base URL used to
/// resolve relative URLs in `__fetchSetBody` / `__fetchAppendBody`.
pub fn execute_scripts_with_base(
    tree_shared: &crate::bridge::SharedTree,
    ctx: &mut Context,
    base_url: Option<String>,
) -> usize {
    let scripts: Vec<String> = {
        let borrowed = tree_shared.borrow();
        extract_scripts(&borrowed)
    };
    let _guard = crate::bridge::install_shared_with_base(tree_shared.clone(), base_url);
    let mut executed = 0;
    for script in scripts {
        match ctx.eval(Source::from_bytes(&script)) {
            Ok(_) => executed += 1,
            Err(e) => {
                eprintln!("[js-runtime] script error: {e}");
            }
        }
    }
    executed
}

/// Convenience: install bridge + execute scripts in one call.
/// Returns (executed_count, SharedTree). The caller can drop the
/// `Rc<RefCell<Tree>>` clones when done rendering.
#[must_use]
pub fn run_scripts(tree: Tree) -> (crate::bridge::SharedTree, usize) {
    run_scripts_with_base(tree, None)
}

/// Same as [`run_scripts`], but installs a base URL for relative-URL
/// resolution in `__fetch*` calls.
#[must_use]
pub fn run_scripts_with_base(
    tree: Tree,
    base_url: Option<String>,
) -> (crate::bridge::SharedTree, usize) {
    use std::cell::RefCell;
    use std::rc::Rc;
    let shared: crate::bridge::SharedTree = Rc::new(RefCell::new(tree));
    let mut ctx = Context::default();
    install(&mut ctx);
    // M13.3: 安装 localStorage / sessionStorage 后端 + JS 对象
    let storage = browser_storage::new_storage();
    crate::bridge::install_storage(storage);
    let _ = crate::storage_shim::install_storage_globals(&mut ctx);
    // M14.3: 安装 history / location 后端 + JS 对象（初始 URL = base_url）
    let initial_url = base_url
        .clone()
        .unwrap_or_else(|| "about:blank".to_string());
    let nav = browser_navigation::new_navigation(&initial_url);
    crate::bridge::install_navigation(nav);
    let _ = crate::navigation_shim::install_navigation_globals(&mut ctx);
    // M15.3/M15.4: 安装 cookie jar 后端。如果 cli 已装（主请求）复用，否则新建。
    crate::bridge::ensure_cookie_jar();
    let count = execute_scripts_with_base(&shared, &mut ctx, base_url);
    (shared, count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::body_text_content;

    fn parse(html: &str) -> Tree {
        browser_html_parser::parse(html)
    }

    #[test]
    fn extract_no_scripts() {
        let tree = parse("<html><body><p>hi</p></body></html>");
        assert!(extract_scripts(&tree).is_empty());
    }

    #[test]
    fn extract_one_inline_script() {
        let tree = parse("<html><body><script>__setBody('x')</script></body></html>");
        let scripts = extract_scripts(&tree);
        assert_eq!(scripts.len(), 1);
        assert_eq!(scripts[0], "__setBody('x')");
    }

    #[test]
    fn extract_multiple_scripts_in_order() {
        let tree = parse(
            "<html><body>\
             <script>a()</script>\
             <script>b()</script>\
             <script>c()</script>\
             </body></html>",
        );
        let scripts = extract_scripts(&tree);
        assert_eq!(scripts.len(), 3);
        assert!(scripts[0].contains("a()"));
        assert!(scripts[1].contains("b()"));
        assert!(scripts[2].contains("c()"));
    }

    #[test]
    fn extract_skips_empty_scripts() {
        let tree = parse(
            "<html><body>\
             <script>  </script>\
             <script>real()</script>\
             <script></script>\
             </body></html>",
        );
        let scripts = extract_scripts(&tree);
        assert_eq!(scripts.len(), 1);
        assert!(scripts[0].contains("real"));
    }

    #[test]
    fn execute_mutates_dom_via_bridge() {
        let html = "<html><body>\
                    <script>__setBody(\"dynamic content\")</script>\
                    </body></html>";
        let (shared, executed) = run_scripts(parse(html));
        assert_eq!(executed, 1);
        assert_eq!(body_text_content(&shared.borrow()), "dynamic content");
    }

    #[test]
    fn execute_chained_scripts_share_dom_state() {
        let html = "<html><body>\
                    <script>__setBody(\"first\")</script>\
                    <script>__appendBody(\" second\")</script>\
                    </body></html>";
        let (shared, executed) = run_scripts(parse(html));
        assert_eq!(executed, 2);
        assert_eq!(body_text_content(&shared.borrow()), "first second");
    }

    #[test]
    fn execute_script_error_does_not_abort_run() {
        let html = "<html><body>\
                    <script>throw new Error('boom')</script>\
                    <script>__setBody('recovered')</script>\
                    </body></html>";
        let (shared, executed) = run_scripts(parse(html));
        // The first script threw, the second succeeded.
        assert_eq!(executed, 1);
        assert_eq!(body_text_content(&shared.borrow()), "recovered");
    }

    #[test]
    fn execute_preserves_non_script_dom() {
        // Static content should remain visible even with a script that
        // mutates a different part of the tree.
        let html = "<html><body>\
                    <p>static</p>\
                    <script>__setTitle('dynamic title')</script>\
                    </body></html>";
        let (shared, _) = run_scripts(parse(html));
        let text = body_text_content(&shared.borrow());
        assert!(text.contains("static"), "got: {text}");
    }
}
