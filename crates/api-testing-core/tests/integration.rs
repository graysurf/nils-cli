// Consolidated integration test target.
// Each former `tests/*.rs` is declared as a submodule here so the crate
// links one integration test binary instead of many. This keeps the
// dev-loop link phase O(crates) instead of O(test-files).

#[path = "integration/cli_endpoint.rs"]
mod cli_endpoint;
#[path = "integration/cli_history.rs"]
mod cli_history;
#[path = "integration/cli_io.rs"]
mod cli_io;
#[path = "integration/cli_report.rs"]
mod cli_report;
#[path = "integration/cli_util.rs"]
mod cli_util;
#[path = "integration/config_resolution.rs"]
mod config_resolution;
#[path = "integration/fixtures_smoke.rs"]
mod fixtures_smoke;
#[path = "integration/report_history.rs"]
mod report_history;
#[path = "integration/suite_cleanup_graphql.rs"]
mod suite_cleanup_graphql;
#[path = "integration/suite_rest_graphql_matrix.rs"]
mod suite_rest_graphql_matrix;
#[path = "integration/suite_runner_grpc_matrix.rs"]
mod suite_runner_grpc_matrix;
#[path = "integration/suite_runner_loopback.rs"]
mod suite_runner_loopback;
#[path = "integration/suite_runner_websocket_matrix.rs"]
mod suite_runner_websocket_matrix;
#[path = "integration/support/mod.rs"]
pub mod support;
