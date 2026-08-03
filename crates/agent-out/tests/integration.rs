// Consolidated integration test target.
// Each former `tests/*.rs` is declared as a submodule here so the crate
// links one integration test binary instead of many. This keeps the
// dev-loop link phase O(crates) instead of O(test-files).

#[path = "integration/cli.rs"]
mod cli;
#[path = "integration/completion_export.rs"]
mod completion_export;
#[path = "integration/exit_codes.rs"]
mod exit_codes;
#[path = "integration/help_snapshot.rs"]
mod help_snapshot;
