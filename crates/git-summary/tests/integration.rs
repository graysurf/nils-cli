// Consolidated integration test target.
// Each former `tests/*.rs` is declared as a submodule here so the crate
// links one integration test binary instead of many. This keeps the
// dev-loop link phase O(crates) instead of O(test-files).

#[path = "integration/cli_paths.rs"]
mod cli_paths;
#[path = "integration/common.rs"]
pub mod common;
#[path = "integration/completion_outside_repo.rs"]
mod completion_outside_repo;
#[path = "integration/edge_cases.rs"]
mod edge_cases;
#[path = "integration/exit_codes.rs"]
mod exit_codes;
#[path = "integration/summary_counts.rs"]
mod summary_counts;
