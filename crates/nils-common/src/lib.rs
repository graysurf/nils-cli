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
//! ## Determinism contract (Resolved Decision #9)
//!
//! `nils-agent-runtime` consumes this crate on its render path, so
//! `std::collections::HashMap`, `std::time::SystemTime::now`, and
//! `chrono::Utc::now` are forbidden inside this crate. The crate-wide
//! `#![deny(...)]` below pairs with `clippy.toml` to make every
//! violation a build failure. Use `IndexMap` or `BTreeMap` for any map
//! that wants stable iteration. Source:
//! `agent-runtime-kit/docs/source/inventory-target-architecture.md`
//! → Resolved Decision #9.
#![deny(clippy::disallowed_types, clippy::disallowed_methods)]

pub mod agent_attribution;
pub mod cli_contract;
pub mod clipboard;
pub mod coordination_projection;
pub mod default_branch_receipt;
pub mod diag_output;
pub mod env;
pub mod execution_effect;
pub mod fs;
pub mod git;
pub mod markdown;
pub mod process;
pub mod provider_payload;
pub mod provider_runtime;
pub mod provider_usage;
pub mod rate_limits_ansi;
pub mod redact;
pub mod shell;
pub mod slug;
pub mod usage_cache_policy;
pub mod usage_time;

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
