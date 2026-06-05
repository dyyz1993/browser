//! `browser-html-parser` — wraps `html5ever` to produce `browser-dom::Tree`.
//!
//! M0 placeholder. See `PLAN.md` step M1.3 for the upcoming API:
//! `pub fn parse(html: &str) -> dom::Tree`.

#![forbid(unsafe_code)]

pub const CRATE_NAME: &str = "browser-html-parser";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ping() {
        assert_eq!(CRATE_NAME, "browser-html-parser");
    }
}
