// Consolidated integration test target.
// Each former `tests/*.rs` is declared as a submodule here so the crate
// links one integration test binary instead of many. This keeps the
// dev-loop link phase O(crates) instead of O(test-files).

#[path = "integration/add_and_list.rs"]
mod add_and_list;
#[path = "integration/agent_roundtrip.rs"]
mod agent_roundtrip;
#[path = "integration/apply_metadata_validation.rs"]
mod apply_metadata_validation;
#[path = "integration/completion_outside_repo.rs"]
mod completion_outside_repo;
#[path = "integration/exit_codes.rs"]
mod exit_codes;
#[path = "integration/extension_cleanup_contract.rs"]
mod extension_cleanup_contract;
#[path = "integration/fetch_apply_flow.rs"]
mod fetch_apply_flow;
#[path = "integration/item_id_allocator.rs"]
mod item_id_allocator;
#[path = "integration/json_contract.rs"]
mod json_contract;
#[path = "integration/memo_flow.rs"]
mod memo_flow;
#[path = "integration/metadata_projection.rs"]
mod metadata_projection;
#[path = "integration/report_time_options.rs"]
mod report_time_options;
#[path = "integration/search_and_report.rs"]
mod search_and_report;
#[path = "integration/support/mod.rs"]
pub mod support;
#[path = "integration/text_output.rs"]
mod text_output;
#[path = "integration/update_delete_flow.rs"]
mod update_delete_flow;
