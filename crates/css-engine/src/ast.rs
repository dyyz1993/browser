//! CSS data model: a minimal subset of CSSOM suitable for M2.
//!
//! We support only the bits needed for text-mode rendering:
//! - Stylesheets = list of rules
//! - Rules = list of simple selectors + list of declarations
//! - Declarations = property:value pairs (with `!important` flag)
//!
//! At-rule support is intentionally absent; M3+ will revisit.

use std::fmt;

/// A parsed stylesheet.
#[derive(Debug, Default, Clone)]
pub struct Stylesheet {
    pub rules: Vec<Rule>,
}

/// A single CSS rule: `selector-list { declarations }`.
#[derive(Debug, Clone)]
pub struct Rule {
    /// Raw selector list as authored in CSS (e.g. `"h1, .title"`).
    /// M2.2 will parse this into structured `Selector`s; for now we
    /// keep the source string to keep M2.1 self-contained.
    pub selectors: String,
    pub declarations: Vec<Declaration>,
}

/// A `property: value;` declaration, optionally flagged `!important`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Declaration {
    pub property: String,
    pub value: String,
    pub important: bool,
}

impl fmt::Display for Declaration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.important {
            write!(f, "{}: {} !important", self.property, self.value)
        } else {
            write!(f, "{}: {}", self.property, self.value)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declaration_display_no_important() {
        let d = Declaration {
            property: "color".into(),
            value: "red".into(),
            important: false,
        };
        assert_eq!(format!("{d}"), "color: red");
    }

    #[test]
    fn declaration_display_with_important() {
        let d = Declaration {
            property: "color".into(),
            value: "red".into(),
            important: true,
        };
        assert_eq!(format!("{d}"), "color: red !important");
    }
}
