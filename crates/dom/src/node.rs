//! Node data variants for the DOM tree.
//!
//! Mirrors the WHATWG HTML5 node types we care about for M1+.
//! Notably `Document` is just a marker variant; metadata like
//! URL / content-type lives on the `Document` owner struct later.

use std::fmt;

/// A single attribute pair `(name, value)`. Order is preserved
/// to match the HTML source order (which matters for some
/// selectors and for debugging output).
pub type Attr = (String, String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeData {
    /// The root of a document tree.
    Document,
    /// `<!DOCTYPE name>`
    Doctype { name: String },
    /// `<tag attrs...>`
    Element { tag: String, attrs: Vec<Attr> },
    /// Plain text content.
    Text(String),
    /// `<!-- comment -->`
    Comment(String),
}

impl NodeData {
    /// Returns the tag name if this is an [`NodeData::Element`], else `None`.
    #[must_use]
    pub fn as_element(&self) -> Option<(&str, &Vec<Attr>)> {
        if let NodeData::Element { tag, attrs } = self {
            Some((tag, attrs))
        } else {
            None
        }
    }

    /// Returns the text content if this is a [`NodeData::Text`], else `None`.
    #[must_use]
    pub fn as_text(&self) -> Option<&str> {
        if let NodeData::Text(s) = self {
            Some(s)
        } else {
            None
        }
    }
}

impl fmt::Display for NodeData {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NodeData::Document => write!(f, "Document"),
            NodeData::Doctype { name } => write!(f, "Doctype({name})"),
            NodeData::Element { tag, attrs } => {
                if attrs.is_empty() {
                    write!(f, "Element({tag})")
                } else {
                    let attrs_str = attrs
                        .iter()
                        .map(|(k, v)| format!("(\"{k}\", \"{v}\")"))
                        .collect::<Vec<_>>()
                        .join(", ");
                    write!(f, "Element({tag}, attrs=[{attrs_str}])")
                }
            }
            NodeData::Text(s) => write!(f, "Text(\"{s}\")"),
            NodeData::Comment(s) => write!(f, "Comment(\"{s}\")"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_element_no_attrs() {
        assert_eq!(
            format!(
                "{}",
                NodeData::Element {
                    tag: "div".into(),
                    attrs: vec![]
                }
            ),
            "Element(div)"
        );
    }

    #[test]
    fn display_element_with_attrs_preserves_order() {
        let data = NodeData::Element {
            tag: "a".into(),
            attrs: vec![("href".into(), "x".into()), ("class".into(), "c".into())],
        };
        assert_eq!(
            format!("{}", data),
            "Element(a, attrs=[(\"href\", \"x\"), (\"class\", \"c\")])"
        );
    }

    #[test]
    fn as_element_round_trip() {
        let data = NodeData::Element {
            tag: "p".into(),
            attrs: vec![("id".into(), "a".into())],
        };
        let (tag, attrs) = data.as_element().expect("should be element");
        assert_eq!(tag, "p");
        assert_eq!(attrs.len(), 1);
    }

    #[test]
    fn as_text_returns_none_for_non_text() {
        assert!(NodeData::Document.as_text().is_none());
    }
}
