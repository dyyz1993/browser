//! `browser-dom` — arena-backed DOM data structures.
//!
//! M0 placeholder. See `PLAN.md` step M1.2 for the upcoming API:
//! `NodeData`, `Node`, `Tree` (arena of `Vec<Node>` + `NodeId`).

#![forbid(unsafe_code)]

pub const CRATE_NAME: &str = "browser-dom";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ping() {
        assert_eq!(CRATE_NAME, "browser-dom");
    }
}
