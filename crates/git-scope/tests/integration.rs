// Consolidated integration test target.
// Each former `tests/*.rs` is declared as a submodule here so the crate
// links one integration test binary instead of many. This keeps the
// dev-loop link phase O(crates) instead of O(test-files).

#[path = "integration/characterization_commands.rs"]
mod characterization_commands;
#[path = "integration/characterization_warnings.rs"]
mod characterization_warnings;
#[path = "integration/commit_mode.rs"]
mod commit_mode;
#[path = "integration/common.rs"]
pub mod common;
#[path = "integration/edge_cases.rs"]
mod edge_cases;
#[path = "integration/exit_codes.rs"]
mod exit_codes;
#[path = "integration/help_outside_repo.rs"]
mod help_outside_repo;
#[path = "integration/help_snapshot.rs"]
mod help_snapshot;
#[path = "integration/print_sources.rs"]
mod print_sources;
#[path = "integration/progress_opt_in.rs"]
mod progress_opt_in;
#[path = "integration/rendering.rs"]
mod rendering;
#[path = "integration/tool_degradation.rs"]
mod tool_degradation;
#[path = "integration/tracked_prefix.rs"]
mod tracked_prefix;
