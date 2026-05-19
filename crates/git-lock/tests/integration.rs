// Consolidated integration test target.
// Each former `tests/*.rs` is declared as a submodule here so the crate
// links one integration test binary instead of many. This keeps the
// dev-loop link phase O(crates) instead of O(test-files).

#[path = "integration/common.rs"]
pub mod common;
#[path = "integration/completion_outside_repo.rs"]
mod completion_outside_repo;
#[path = "integration/copy_and_tag.rs"]
mod copy_and_tag;
#[path = "integration/copy_delete.rs"]
mod copy_delete;
#[path = "integration/diff_tag.rs"]
mod diff_tag;
#[path = "integration/edge_cases.rs"]
mod edge_cases;
#[path = "integration/exit_codes.rs"]
mod exit_codes;
#[path = "integration/help_outside_repo.rs"]
mod help_outside_repo;
#[path = "integration/list.rs"]
mod list;
#[path = "integration/lock_unlock.rs"]
mod lock_unlock;
