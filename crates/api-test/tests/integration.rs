// Consolidated integration test target.
// Each former `tests/*.rs` is declared as a submodule here so the crate
// links one integration test binary instead of many. This keeps the
// dev-loop link phase O(crates) instead of O(test-files).

#[path = "integration/cli_smoke.rs"]
mod cli_smoke;
#[path = "integration/completion_outside_repo.rs"]
mod completion_outside_repo;
#[path = "integration/e2e.rs"]
mod e2e;
#[path = "integration/grpc_integration.rs"]
mod grpc_integration;
#[path = "integration/progress_contract.rs"]
mod progress_contract;
