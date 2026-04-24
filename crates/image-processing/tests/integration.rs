// Consolidated integration test target.
// Each former `tests/*.rs` is declared as a submodule here so the crate
// links one integration test binary instead of many. This keeps the
// dev-loop link phase O(crates) instead of O(test-files).

#[path = "integration/common.rs"]
pub mod common;
#[path = "integration/core_flows.rs"]
mod core_flows;
#[path = "integration/dry_run_paths.rs"]
mod dry_run_paths;
#[path = "integration/edge_cases.rs"]
mod edge_cases;
#[path = "integration/version.rs"]
mod version;
