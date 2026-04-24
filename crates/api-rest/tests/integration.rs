// Consolidated integration test target.
// Each former `tests/*.rs` is declared as a submodule here so the crate
// links one integration test binary instead of many. This keeps the
// dev-loop link phase O(crates) instead of O(test-files).

#[path = "integration/auth_resolution.rs"]
mod auth_resolution;
#[path = "integration/cli_smoke.rs"]
mod cli_smoke;
#[path = "integration/completion_outside_repo.rs"]
mod completion_outside_repo;
#[path = "integration/endpoint_resolution.rs"]
mod endpoint_resolution;
#[path = "integration/history.rs"]
mod history;
#[path = "integration/integration.rs"]
mod integration;
#[path = "integration/report.rs"]
mod report;
#[path = "integration/report_from_cmd.rs"]
mod report_from_cmd;
#[path = "integration/schema_edges.rs"]
mod schema_edges;
#[path = "integration/setup_resolution.rs"]
mod setup_resolution;
