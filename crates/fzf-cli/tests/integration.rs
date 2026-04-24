// Consolidated integration test target.
// Each former `tests/*.rs` is declared as a submodule here so the crate
// links one integration test binary instead of many. This keeps the
// dev-loop link phase O(crates) instead of O(test-files).

#[path = "integration/common.rs"]
pub mod common;
#[path = "integration/completion_outside_repo.rs"]
mod completion_outside_repo;
#[path = "integration/edge_cases.rs"]
mod edge_cases;
#[path = "integration/git_commands.rs"]
mod git_commands;
#[path = "integration/git_commit.rs"]
mod git_commit;
#[path = "integration/help_and_dispatch.rs"]
mod help_and_dispatch;
#[path = "integration/open_and_file.rs"]
mod open_and_file;
