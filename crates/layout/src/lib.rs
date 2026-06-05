//! `browser-layout` — block / inline / flex layout engine.
//!
//! M0 placeholder. Will be wired up in M2.

#![forbid(unsafe_code)]

pub const CRATE_NAME: &str = "browser-layout";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ping() {
        assert_eq!(CRATE_NAME, "browser-layout");
    }
}
