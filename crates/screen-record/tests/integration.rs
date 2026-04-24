// Consolidated integration test target.
// Each former `tests/*.rs` is declared as a submodule here so the crate
// links one integration test binary instead of many. This keeps the
// dev-loop link phase O(crates) instead of O(test-files).

#[path = "integration/cli_smoke.rs"]
mod cli_smoke;
#[path = "integration/common.rs"]
pub mod common;
#[path = "integration/completion_outside_repo.rs"]
mod completion_outside_repo;
#[path = "integration/coverage_stubs.rs"]
mod coverage_stubs;
#[path = "integration/error.rs"]
mod error;
#[path = "integration/linux_portal_unit.rs"]
mod linux_portal_unit;
#[path = "integration/linux_request_permission.rs"]
mod linux_request_permission;
#[path = "integration/linux_unit.rs"]
mod linux_unit;
#[path = "integration/linux_x11_integration.rs"]
mod linux_x11_integration;
#[path = "integration/non_macos.rs"]
mod non_macos;
#[path = "integration/recording_test_mode.rs"]
mod recording_test_mode;
#[path = "integration/selection.rs"]
mod selection;
#[path = "integration/writer.rs"]
mod writer;
