//! Selector parsing + matching for the M2 subset.
//!
//! Supported selector forms:
//! - `*`                  universal
//! - `tag`                type (e.g. `p`, `h1`)
//! - `.cls`               class
//! - `#id`                identifier
//! - `tag.cls#id`         compound (any combination)
//! - `A B`                descendant (whitespace combinator)
//! - `A, B`               selector list
//!
//! Explicitly out of scope:
//! attribute selectors, pseudo-classes/elements, `>`, `+`, `~`.

use std::fmt;

use browser_dom::{NodeData, NodeId, Tree};

/// A selector list (the right-hand side of a CSS rule).
#[derive(Debug, Clone)]
pub struct Selector {
    /// The original source string, for debugging and Display.
    pub source: String,
    /// Comma-separated list of compound+combinator chains.
    pub selectors: Vec<SelectorChain>,
}

/// A chain of compound selectors connected by descendant combinators,
/// e.g. `body p.title`.
#[derive(Debug, Clone)]
pub struct SelectorChain {
    pub parts: Vec<CompoundSelector>,
}

/// A single compound selector with no whitespace, e.g. `p.title#main`.
#[derive(Debug, Clone, Default)]
pub struct CompoundSelector {
    pub tag: Option<String>, // None = universal / not specified
    pub classes: Vec<String>,
    pub id: Option<String>,
}

impl fmt::Display for CompoundSelector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.tag {
            Some(t) => write!(f, "{t}")?,
            None => write!(f, "*")?,
        }
        for c in &self.classes {
            write!(f, ".{c}")?;
        }
        if let Some(id) = &self.id {
            write!(f, "#{id}")?;
        }
        Ok(())
    }
}

impl Selector {
    /// Parse a selector list like `"h1, .title, body p"`.
    ///
    /// # Errors
    /// Returns an error string for malformed input.
    pub fn parse(input: &str) -> Result<Self, String> {
        let mut selectors = Vec::new();
        for chunk in input.split(',') {
            let chain = parse_chain(chunk.trim())?;
            selectors.push(chain);
        }
        Ok(Self {
            source: input.to_string(),
            selectors,
        })
    }

    /// Whether this selector list matches the element at `element_id`
    /// in `tree`. `element_id` must point to an Element node.
    #[must_use]
    pub fn matches(&self, tree: &Tree, element_id: NodeId) -> bool {
        if !matches!(tree.data(element_id), NodeData::Element { .. }) {
            return false;
        }
        self.selectors
            .iter()
            .any(|chain| chain_matches(tree, element_id, chain))
    }
}

fn parse_chain(input: &str) -> Result<SelectorChain, String> {
    let mut parts = Vec::new();
    for chunk in input.split_whitespace() {
        if chunk.is_empty() {
            continue;
        }
        parts.push(parse_compound(chunk)?);
    }
    if parts.is_empty() {
        return Err(format!("empty selector chain: {input:?}"));
    }
    Ok(SelectorChain { parts })
}

fn parse_compound(input: &str) -> Result<CompoundSelector, String> {
    let mut out = CompoundSelector::default();
    let mut chars = input.chars().peekable();
    // Optional tag at the start.
    let mut tag = String::new();
    while let Some(&c) = chars.peek() {
        if c == '.' || c == '#' {
            break;
        }
        tag.push(c);
        chars.next();
    }
    if !tag.is_empty() && tag != "*" {
        out.tag = Some(tag);
    }
    // Class / id parts.
    while let Some(&c) = chars.peek() {
        match c {
            '.' => {
                chars.next();
                let mut name = String::new();
                while let Some(&c) = chars.peek() {
                    if c == '.' || c == '#' {
                        break;
                    }
                    name.push(c);
                    chars.next();
                }
                if name.is_empty() {
                    return Err(format!("empty class name in {input:?}"));
                }
                out.classes.push(name);
            }
            '#' => {
                chars.next();
                let mut name = String::new();
                while let Some(&c) = chars.peek() {
                    if c == '.' || c == '#' {
                        break;
                    }
                    name.push(c);
                    chars.next();
                }
                if name.is_empty() {
                    return Err(format!("empty id in {input:?}"));
                }
                out.id = Some(name);
            }
            _ => {
                return Err(format!(
                    "unexpected char {c:?} in compound selector {input:?}"
                ));
            }
        }
    }
    Ok(out)
}

fn compound_matches(tree: &Tree, id: NodeId, sel: &CompoundSelector) -> bool {
    let data = tree.data(id);
    let (tag, attrs): (&str, &[(String, String)]) = match data {
        NodeData::Element { tag, attrs } => (tag.as_str(), attrs.as_slice()),
        _ => return false,
    };
    if let Some(s) = &sel.tag {
        if s != tag {
            return false;
        }
    }
    if let Some(want_id) = &sel.id {
        let has = attrs.iter().find(|(k, _)| k == "id");
        if !matches!(has, Some((_, v)) if v == want_id) {
            return false;
        }
    }
    if !sel.classes.is_empty() {
        let class_attr = attrs
            .iter()
            .find(|(k, _)| k == "class")
            .map(|(_, v)| v.as_str())
            .unwrap_or("");
        let present: std::collections::HashSet<&str> = class_attr.split_whitespace().collect();
        for c in &sel.classes {
            if !present.contains(c.as_str()) {
                return false;
            }
        }
    }
    true
}

fn chain_matches(tree: &Tree, id: NodeId, chain: &SelectorChain) -> bool {
    // The last part matches `id`; each preceding part must match some
    // ancestor (descendant combinator).
    let parts = &chain.parts;
    let last_idx = parts.len() - 1;
    if !compound_matches(tree, id, &parts[last_idx]) {
        return false;
    }
    let mut current_id = id;
    for part in parts[..last_idx].iter().rev() {
        // Walk ancestors until we find one that matches `part`.
        let mut ancestor = tree.get(current_id).parent;
        let mut found = false;
        while let Some(aid) = ancestor {
            if compound_matches(tree, aid, part) {
                current_id = aid;
                found = true;
                break;
            }
            ancestor = tree.get(aid).parent;
        }
        if !found {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use browser_dom::{NodeData, Tree};

    /// Build a tiny fixture:
    /// ```text
    /// Document
    ///   └─ html
    ///      └─ body
    ///         ├─ div.container
    ///         │  └─ p.text#main  ("hello")
    ///         └─ a.link          ("more")
    /// ```
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
        let div = t.insert(
            Some(body),
            NodeData::Element {
                tag: "div".into(),
                attrs: vec![("class".into(), "container".into())],
            },
        );
        let p = t.insert(
            Some(div),
            NodeData::Element {
                tag: "p".into(),
                attrs: vec![
                    ("class".into(), "text greeting".into()),
                    ("id".into(), "main".into()),
                ],
            },
        );
        let _ = t.insert(Some(p), NodeData::Text("hello".into()));
        let a = t.insert(
            Some(body),
            NodeData::Element {
                tag: "a".into(),
                attrs: vec![("class".into(), "link".into())],
            },
        );
        let _ = t.insert(Some(a), NodeData::Text("more".into()));
        t
    }

    #[test]
    fn parse_universal_matches_everything() {
        let tree = fixture();
        let sel = Selector::parse("*").unwrap();
        // Element ids: html=1, body=2, div=3, p=4, a=6.
        for id in [1, 2, 3, 4, 6] {
            assert!(sel.matches(&tree, id), "expected * to match id={id}");
        }
        // Text nodes must NOT match.
        assert!(!sel.matches(&tree, 5));
    }

    #[test]
    fn parse_type_selector_matches_tag_only() {
        let tree = fixture();
        let sel = Selector::parse("p").unwrap();
        // <p> lives at id=4; other element ids must not match.
        assert!(sel.matches(&tree, 4));
        assert!(!sel.matches(&tree, 1));
        assert!(!sel.matches(&tree, 2));
        assert!(!sel.matches(&tree, 3));
        assert!(!sel.matches(&tree, 6));
    }

    #[test]
    fn parse_class_selector() {
        let tree = fixture();
        let sel = Selector::parse(".container").unwrap();
        assert!(sel.matches(&tree, 3)); // div.container
        assert!(!sel.matches(&tree, 4)); // p.text#main
    }

    #[test]
    fn parse_class_match_when_class_attr_has_multiple() {
        let tree = fixture();
        let sel = Selector::parse(".text").unwrap();
        assert!(sel.matches(&tree, 4));
        let sel2 = Selector::parse(".greeting").unwrap();
        assert!(sel2.matches(&tree, 4));
    }

    #[test]
    fn parse_id_selector() {
        let tree = fixture();
        let sel = Selector::parse("#main").unwrap();
        assert!(sel.matches(&tree, 4));
        assert!(!sel.matches(&tree, 3));
    }

    #[test]
    fn parse_compound_tag_class_id() {
        let tree = fixture();
        let sel = Selector::parse("p.text#main").unwrap();
        assert!(sel.matches(&tree, 4));
        let sel2 = Selector::parse("p.other").unwrap();
        assert!(!sel2.matches(&tree, 4));
    }

    #[test]
    fn parse_list_of_selectors() {
        let tree = fixture();
        let sel = Selector::parse("a, .container").unwrap();
        assert!(sel.matches(&tree, 3)); // div.container
        assert!(sel.matches(&tree, 6)); // a (id=6, not 5)
        assert!(!sel.matches(&tree, 4)); // p
    }

    #[test]
    fn parse_descendant_combinator() {
        let tree = fixture();
        // body p — matches the <p> inside body (via div).
        let sel = Selector::parse("body p").unwrap();
        assert!(sel.matches(&tree, 4));
        // body a — matches the <a> directly under body.
        let sel2 = Selector::parse("body a").unwrap();
        assert!(sel2.matches(&tree, 6));
        // div a — does NOT match <a> (a is sibling of div, not descendant).
        let sel3 = Selector::parse("div a").unwrap();
        assert!(!sel3.matches(&tree, 6));
    }

    #[test]
    fn parse_invalid_selector_returns_err() {
        assert!(Selector::parse(".").is_err());
        assert!(Selector::parse("#").is_err());
    }

    #[test]
    fn parse_universal_with_classes_matches_correctly() {
        let tree = fixture();
        let sel = Selector::parse("*.link").unwrap();
        assert!(sel.matches(&tree, 6));
        assert!(!sel.matches(&tree, 4));
    }
}
