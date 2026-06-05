//! `browser-cli` — command-line entry point.
//!
//! M0 placeholder. Real subcommands (`get`, `parse`) will be added in M1.5.

/// Entry point. Kept minimal in M0; subcommands land in M1.5.
fn main() {
    println!("browser v0.0.1 — M0 skeleton");
    println!("run `cargo test --workspace` to verify the skeleton.");
}

#[cfg(test)]
mod tests {
    /// Smoke test: ensures the binary crate compiles and is wired into
    /// the workspace. Mirrors the `ping` tests in the library crates.
    #[test]
    fn ping() {
        // If the crate compiles, the binary exists. Assertion kept for
        // consistency with sibling crates.
        assert!("browser-cli".contains('-'));
    }
}
