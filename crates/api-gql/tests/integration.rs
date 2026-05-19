// Consolidated integration test target.
// Each former `tests/*.rs` is declared as a submodule here so the crate
// links one integration test binary instead of many. This keeps the
// dev-loop link phase O(crates) instead of O(test-files).

#[path = "integration/cli_smoke.rs"]
mod cli_smoke;
#[path = "integration/completion_outside_repo.rs"]
mod completion_outside_repo;
#[path = "integration/env_and_auth_resolution.rs"]
mod env_and_auth_resolution;
#[path = "integration/help_snapshot.rs"]
mod help_snapshot;
#[path = "integration/history.rs"]
mod history;
#[path = "integration/integration.rs"]
mod integration;
#[path = "integration/schema_command.rs"]
mod schema_command;
