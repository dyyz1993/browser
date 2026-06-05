//! `browser-render` — rasterizes the layout tree to pixels via `tiny-skia`.
//!
//! M0 placeholder. Will be wired up in M5 (GUI window).

#![forbid(unsafe_code)]

pub const CRATE_NAME: &str = "browser-render";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ping() {
        assert_eq!(CRATE_NAME, "browser-render");
    }
}
