// Consolidated integration test target.
// Each former `tests/*.rs` is declared as a submodule here so the crate
// links one integration test binary instead of many. This keeps the
// dev-loop link phase O(crates) instead of O(test-files).

#[path = "integration/bin_cmd.rs"]
mod bin_cmd;
#[path = "integration/fixtures.rs"]
mod fixtures;
#[path = "integration/fs.rs"]
mod fs;
#[path = "integration/git.rs"]
mod git;
#[path = "integration/guards.rs"]
mod guards;
#[path = "integration/http.rs"]
mod http;
