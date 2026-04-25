//! Per-task dispatch record JSON.
//!
//! Defined in `agent-kit/skills/automation/plan-issue-delivery/references/RUNTIME_LAYOUT.md`
//! L48-52 and `plan-issue-cli-contract-v2.md` "Canonical Runtime Artifacts (v2)".
//!
//! The binary writes the ten required keys at sprint start. Optional adapter
//! fields (`runtime_name`, `runtime_role`, `runtime_role_fallback_reason`)
//! are intentionally **absent** — they belong to the active runtime adapter
//! and are added post-emission by the wrapper / main-agent.

use std::path::Path;

use serde::{Deserialize, Serialize};

use nils_common::fs as common_fs;

/// `workflow_role` value emitted by `start-sprint` for every implementation
/// task.
pub const WORKFLOW_ROLE_IMPLEMENTATION: &str = "implementation";

/// Stable, sorted JSON object written to `dispatch-<TASK_ID>.json`.
///
/// Field order matches the canonical contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DispatchRecord {
    pub task_id: String,
    pub task_prompt_path: String,
    pub subagent_init_snapshot_path: String,
    pub plan_snapshot_path: String,
    pub worktree: String,
    pub branch: String,
    pub execution_mode: String,
    pub pr_group: String,
    pub base_branch: String,
    pub workflow_role: String,
}

impl DispatchRecord {
    #[allow(clippy::too_many_arguments)]
    pub fn implementation(
        task_id: impl Into<String>,
        task_prompt_path: impl Into<String>,
        subagent_init_snapshot_path: impl Into<String>,
        plan_snapshot_path: impl Into<String>,
        worktree: impl Into<String>,
        branch: impl Into<String>,
        execution_mode: impl Into<String>,
        pr_group: impl Into<String>,
        base_branch: impl Into<String>,
    ) -> Self {
        Self {
            task_id: task_id.into(),
            task_prompt_path: task_prompt_path.into(),
            subagent_init_snapshot_path: subagent_init_snapshot_path.into(),
            plan_snapshot_path: plan_snapshot_path.into(),
            worktree: worktree.into(),
            branch: branch.into(),
            execution_mode: execution_mode.into(),
            pr_group: pr_group.into(),
            base_branch: base_branch.into(),
            workflow_role: WORKFLOW_ROLE_IMPLEMENTATION.to_string(),
        }
    }

    pub fn to_pretty_json(&self) -> String {
        let mut text = serde_json::to_string_pretty(self)
            .expect("DispatchRecord serializes via serde_json without panicking");
        text.push('\n');
        text
    }
}

/// Pretty-print + write a dispatch record to disk (creating parent dirs).
pub fn write_dispatch_record(
    path: &Path,
    record: &DispatchRecord,
) -> Result<(), common_fs::WriteTextError> {
    common_fs::write_text(path, &record.to_pretty_json())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> DispatchRecord {
        DispatchRecord::implementation(
            "S1T1",
            "/agent-home/out/plan-issue-delivery/owner__repo/issue-7/sprint-1/prompts/S1T1.md",
            "/agent-home/out/plan-issue-delivery/owner__repo/issue-7/sprint-1/prompts/plan-issue-delivery-subagent-init.snapshot.md",
            "/agent-home/out/plan-issue-delivery/owner__repo/issue-7/plan/plan.snapshot.md",
            "/agent-home/out/plan-issue-delivery/owner__repo/issue-7/worktrees/pr-isolated/S1T1",
            "issue/s1-t1",
            "pr-isolated",
            "s1-t1",
            "plan/issue-7",
        )
    }

    #[test]
    fn test_serializes_required_keys() {
        let json = sample().to_pretty_json();
        for key in [
            "\"task_id\"",
            "\"task_prompt_path\"",
            "\"subagent_init_snapshot_path\"",
            "\"plan_snapshot_path\"",
            "\"worktree\"",
            "\"branch\"",
            "\"execution_mode\"",
            "\"pr_group\"",
            "\"base_branch\"",
            "\"workflow_role\"",
        ] {
            assert!(
                json.contains(key),
                "json missing required key {key}: {json}"
            );
        }
        for absent in [
            "\"runtime_name\"",
            "\"runtime_role\"",
            "\"runtime_role_fallback_reason\"",
        ] {
            assert!(
                !json.contains(absent),
                "json must not include adapter key {absent}: {json}"
            );
        }
    }

    #[test]
    fn test_default_workflow_role_is_implementation() {
        let record = sample();
        assert_eq!(record.workflow_role, "implementation");
        let parsed: serde_json::Value = serde_json::from_str(&record.to_pretty_json()).unwrap();
        assert_eq!(parsed["workflow_role"], "implementation");
    }

    #[test]
    fn test_round_trip_equals() {
        let original = sample();
        let json = serde_json::to_string(&original).expect("to_string");
        let restored: DispatchRecord = serde_json::from_str(&json).expect("from_str");
        assert_eq!(restored, original);
    }

    #[test]
    fn write_dispatch_record_creates_parent_dirs() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let path = tmp.path().join("manifests").join("dispatch-S1T1.json");
        write_dispatch_record(&path, &sample()).expect("write");
        let text = std::fs::read_to_string(&path).expect("read");
        assert!(text.contains("\"task_id\": \"S1T1\""), "{text}");
        assert!(text.ends_with('\n'), "trailing newline");
    }
}
