//! Fixture-driven tests for `browser-html-parser`.
//!
//! Every fixture lives under `tests/fixtures/` and is checked into git
//! so the test suite is fully offline and reproducible.

use browser_dom::{NodeData, Tree};
use browser_html_parser::parse;

const SIMPLE: &str = include_str!("fixtures/simple.html");
const WITH_DOCTYPE: &str = include_str!("fixtures/with-doctype.html");
const UNCLOSED: &str = include_str!("fixtures/unclosed.html");
const NESTED: &str = include_str!("fixtures/nested.html");
const ATTRS: &str = include_str!("fixtures/attrs.html");
const EXAMPLE_COM: &str = include_str!("fixtures/example.com.html");

/// Walk the tree and return a flat Vec<(id, kind, tag_or_text)>
/// with text nodes trimmed.
fn flatten(tree: &Tree) -> Vec<(usize, &'static str, String)> {
    let mut out = Vec::new();
    tree.traverse(tree.root(), |id, node| {
        let (kind, payload) = match &node.data {
            NodeData::Document => ("doc", String::new()),
            NodeData::Doctype { name } => ("doctype", name.clone()),
            NodeData::Element { tag, .. } => ("elem", tag.clone()),
            NodeData::Text(s) => ("text", s.trim().to_string()),
            NodeData::Comment(s) => ("comment", s.clone()),
        };
        out.push((id, kind, payload));
        true
    });
    out
}

/// Count nodes of a given kind/tag. Text nodes are matched against
/// their trimmed value so surrounding whitespace doesn't break
/// assertions.
fn count(tree: &Tree, kind: &'static str, payload: &str) -> usize {
    flatten(tree)
        .into_iter()
        .filter(|(_, k, p)| *k == kind && (payload.is_empty() || p == payload))
        .count()
}

#[test]
fn test_simple_html_has_html_head_body_p() {
    let tree = parse(SIMPLE);
    assert!(count(&tree, "elem", "html") >= 1, "missing <html>");
    assert!(count(&tree, "elem", "head") >= 1, "missing <head>");
    assert!(count(&tree, "elem", "body") >= 1, "missing <body>");
    assert!(count(&tree, "elem", "p") >= 1, "missing <p>");
    assert!(count(&tree, "text", "hello") >= 1, "missing text 'hello'");
}

#[test]
fn test_simple_tree_root_is_document() {
    let tree = parse(SIMPLE);
    assert!(matches!(tree.data(tree.root()), NodeData::Document));
}

#[test]
fn test_with_doctype_contains_doctype_node() {
    let tree = parse(WITH_DOCTYPE);
    assert!(
        count(&tree, "doctype", "html") >= 1,
        "expected <!DOCTYPE html>"
    );
}

#[test]
fn test_unclosed_p_tags_are_split() {
    // `<p>foo<p>bar` — html5ever should auto-close the first <p>
    // before the second, producing two sibling <p> elements.
    let tree = parse(UNCLOSED);
    let p_count = count(&tree, "elem", "p");
    assert!(
        p_count >= 2,
        "expected >=2 <p> elements for unclosed tags, got {p_count}"
    );
    assert!(count(&tree, "text", "foo") >= 1);
    assert!(count(&tree, "text", "bar") >= 1);

    // Debug aid if this ever regresses.
    let flat = flatten(&tree);
    eprintln!("unclosed tree:");
    for (id, kind, payload) in &flat {
        eprintln!("  {id}: {kind} {payload:?}");
    }
}

#[test]
fn test_nested_5_levels_of_div() {
    let tree = parse(NESTED);
    let div_count = count(&tree, "elem", "div");
    assert!(
        div_count >= 5,
        "expected >=5 nested <div> elements, got {div_count}"
    );
    assert!(count(&tree, "text", "deep") >= 1, "missing deepest text");
}

#[test]
fn test_attrs_preserved_in_order() {
    let tree = parse(ATTRS);
    // Find the <a> element.
    let mut found = None;
    tree.traverse(tree.root(), |id, node| {
        if let NodeData::Element { tag, attrs } = &node.data {
            if tag == "a" {
                found = Some((id, attrs.clone()));
                return false;
            }
        }
        true
    });
    let (_id, attrs) = found.expect("<a> element must exist");
    assert_eq!(attrs.len(), 2, "expected 2 attrs, got {:?}", attrs);
    assert_eq!(attrs[0].0, "href", "href should be first");
    assert_eq!(attrs[0].1, "x");
    assert_eq!(attrs[1].0, "class", "class should be second");
    assert_eq!(attrs[1].1, "c");
}

#[test]
fn test_example_com_real_snapshot() {
    let tree = parse(EXAMPLE_COM);
    assert!(matches!(tree.data(tree.root()), NodeData::Document));
    assert!(count(&tree, "elem", "html") >= 1);
    assert!(count(&tree, "elem", "head") >= 1);
    assert!(count(&tree, "elem", "body") >= 1);
    assert!(count(&tree, "elem", "h1") >= 1, "example.com has an <h1>");
    // Title text appears inside <title> / page heading.
    let flat = flatten(&tree);
    assert!(
        flat.iter().any(|(_, _, p)| p.contains("Example Domain")),
        "expected page to contain 'Example Domain'"
    );
}

#[test]
fn test_empty_string_produces_document_root() {
    let tree = parse("");
    assert!(matches!(tree.data(tree.root()), NodeData::Document));
    // html5ever should inject at least <html>.
    assert!(count(&tree, "elem", "html") >= 1);
}

#[test]
fn test_text_only_input_wraps_in_html_body() {
    let tree = parse("just text");
    assert!(count(&tree, "elem", "html") >= 1);
    assert!(count(&tree, "elem", "body") >= 1);
    assert!(count(&tree, "text", "just text") >= 1);
}
