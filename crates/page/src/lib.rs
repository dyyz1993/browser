//! `browser-page` — Page / Frame / Navigation lifecycle orchestration.
//!
//! M0 placeholder. Will be wired up in M4.

#![forbid(unsafe_code)]

pub const CRATE_NAME: &str = "browser-page";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ping() {
        assert_eq!(CRATE_NAME, "browser-page");
    }
}
