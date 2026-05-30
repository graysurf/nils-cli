// Consolidated integration test target.
// Each former `tests/*.rs` is declared as a submodule here so the crate
// links one integration test binary instead of many. This keeps the
// dev-loop link phase O(crates) instead of O(test-files).

#[path = "integration/common.rs"]
pub mod common;

#[path = "integration/catalog_parse.rs"]
mod catalog_parse;
#[path = "integration/command_surface.rs"]
mod command_surface;
#[path = "integration/completion_outside_repo.rs"]
mod completion_outside_repo;
#[path = "integration/content_validation.rs"]
mod content_validation;
#[path = "integration/docs_home.rs"]
mod docs_home;
#[path = "integration/exit_codes.rs"]
mod exit_codes;
#[path = "integration/explain_list_remove.rs"]
mod explain_list_remove;
#[path = "integration/help_snapshot.rs"]
mod help_snapshot;
#[path = "integration/init.rs"]
mod init;
#[path = "integration/preflight.rs"]
mod preflight;
#[path = "integration/resolution.rs"]
mod resolution;
#[path = "integration/when_predicate.rs"]
mod when_predicate;
#[path = "integration/worktree.rs"]
mod worktree;
