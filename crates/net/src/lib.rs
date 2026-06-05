//! `browser-net` — HTTP / HTTPS / WebSocket client.
//!
//! M0 placeholder. See `PLAN.md` step M1.1 for the upcoming API:
//! `pub async fn get(url: &str) -> Result<Vec<u8>, NetError>`.

#![forbid(unsafe_code)]

/// Crate version marker. Used by the M0 smoke test to verify the crate
/// compiles and is wired into the workspace.
pub const CRATE_NAME: &str = "browser-net";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ping() {
        assert_eq!(CRATE_NAME, "browser-net");
    }
}
