//! `browser-js-runtime` — embeds a JS engine (`boa_engine` initially,
//! possibly `deno_core` later) and provides the JS ↔ DOM bridge.
//!
//! M0 placeholder. Will be wired up in M3.

#![forbid(unsafe_code)]

pub const CRATE_NAME: &str = "browser-js-runtime";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ping() {
        assert_eq!(CRATE_NAME, "browser-js-runtime");
    }
}
