// Consolidated integration test target.
// Each former `tests/*.rs` is declared as a submodule here so the crate
// links one integration test binary instead of many. This keeps the
// dev-loop link phase O(crates) instead of O(test-files).

#[path = "integration/add.rs"]
mod add;
#[path = "integration/baseline.rs"]
mod baseline;
#[path = "integration/common.rs"]
pub mod common;
#[path = "integration/completion_outside_repo.rs"]
mod completion_outside_repo;
#[path = "integration/config.rs"]
mod config;
#[path = "integration/contexts_checklist.rs"]
mod contexts_checklist;
#[path = "integration/env_paths.rs"]
mod env_paths;
#[path = "integration/exit_codes.rs"]
mod exit_codes;
#[path = "integration/resolve_builtin.rs"]
mod resolve_builtin;
#[path = "integration/resolve_checklist.rs"]
mod resolve_checklist;
#[path = "integration/resolve_toml.rs"]
mod resolve_toml;
#[path = "integration/scaffold_agents.rs"]
mod scaffold_agents;
#[path = "integration/scaffold_baseline.rs"]
mod scaffold_baseline;
