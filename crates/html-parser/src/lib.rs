//! `browser-html-parser` — wraps `html5ever` to produce `browser_dom::Tree`.
//!
//! Single entry point: [`parse`].

#![forbid(unsafe_code)]

pub mod parser;

pub use parser::{parse, parse_fragment};

#[cfg(test)]
mod tests {
    #[test]
    fn ping() {
        assert_eq!(env!("CARGO_PKG_NAME"), "browser-html-parser");
    }
}
