//! Foundation crate shared across nils-* CLIs.
//!
//! Each public module documents its own surface; `crates/nils-common/README.md` carries the
//! per-module narrative and the consumer index, and
//! `docs/specs/workspace-shared-crate-boundary-v1.md` carries the boundary contract.
//!
//! ## Compatibility rules
//! - Returns structured results only; user-facing warning/error text stays in caller adapters.
//! - Exit-code mapping stays in caller crates.
//! - APIs stay domain-neutral and must not encode crate-specific UX policies.
//! - Quoting and ANSI differences are expressed via explicit mode/policy parameters.
//!
pub mod cli_contract;
pub mod clipboard;
pub mod env;
pub mod fs;
pub mod git;
pub mod markdown;
pub mod process;
pub mod provider_runtime;
pub mod rate_limits_ansi;
pub mod shell;

pub fn greeting(name: &str) -> String {
    format!("Hello, {name}!")
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn greeting_formats_name() {
        let result = greeting("Nils");
        assert_eq!(result, "Hello, Nils!");
    }
}
