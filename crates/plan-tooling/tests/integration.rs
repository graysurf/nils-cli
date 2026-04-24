// Consolidated integration test target.
// Each former `tests/*.rs` is declared as a submodule here so the crate
// links one integration test binary instead of many. This keeps the
// dev-loop link phase O(crates) instead of O(test-files).

#[path = "integration/batches.rs"]
mod batches;
#[path = "integration/common.rs"]
pub mod common;
#[path = "integration/completion_outside_repo.rs"]
mod completion_outside_repo;
#[path = "integration/scaffold.rs"]
mod scaffold;
#[path = "integration/split_prs.rs"]
mod split_prs;
#[path = "integration/to_json.rs"]
mod to_json;
#[path = "integration/validate.rs"]
mod validate;
