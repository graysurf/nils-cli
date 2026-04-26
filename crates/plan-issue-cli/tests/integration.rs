// Consolidated integration test target.
// Each former `tests/*.rs` is declared as a submodule here so the crate
// links one integration test binary instead of many. This keeps the
// dev-loop link phase O(crates) instead of O(test-files).

#[path = "integration/auto_single_lane_runtime_truth.rs"]
mod auto_single_lane_runtime_truth;
#[path = "integration/cli_contract.rs"]
mod cli_contract;
#[path = "integration/common.rs"]
pub mod common;
#[path = "integration/grouping_default.rs"]
mod grouping_default;
#[path = "integration/link_pr_flow.rs"]
mod link_pr_flow;
#[path = "integration/live_issue_ops.rs"]
mod live_issue_ops;
#[path = "integration/live_start_sprint_runtime_truth.rs"]
mod live_start_sprint_runtime_truth;
#[path = "integration/output_contract.rs"]
mod output_contract;
#[path = "integration/parity_guardrails.rs"]
mod parity_guardrails;
#[path = "integration/runtime_layout_parity.rs"]
mod runtime_layout_parity;
#[path = "integration/runtime_truth_plan_and_sprint_flow.rs"]
mod runtime_truth_plan_and_sprint_flow;
#[path = "integration/start_plan_canonical.rs"]
mod start_plan_canonical;
#[path = "integration/start_sprint_canonical.rs"]
mod start_sprint_canonical;
#[path = "integration/task_spec_flow.rs"]
mod task_spec_flow;
