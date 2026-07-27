// Consolidated integration test target.
// Each former `tests/*.rs` is declared as a submodule here so the crate
// links one integration test binary instead of many. This keeps the
// dev-loop link phase O(crates) instead of O(test-files).

#[path = "integration/branch.rs"]
mod branch;
#[path = "integration/ci.rs"]
mod ci;
#[path = "integration/commit.rs"]
mod commit;
#[path = "integration/common.rs"]
pub mod common;
#[path = "integration/completion_outside_repo.rs"]
mod completion_outside_repo;
#[path = "integration/dirty_checkout_adoption.rs"]
mod dirty_checkout_adoption;
#[path = "integration/dispatcher.rs"]
mod dispatcher;
#[path = "integration/open.rs"]
mod open;
#[path = "integration/reset.rs"]
mod reset;
#[path = "integration/trusted_binary_cache.rs"]
mod trusted_binary_cache;
#[path = "integration/utils.rs"]
pub mod utils;
#[path = "integration/worktree.rs"]
mod worktree;
