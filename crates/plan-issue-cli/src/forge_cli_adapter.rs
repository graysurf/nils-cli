//! GitLab-backed `ProviderAdapter` that routes through `forge-cli`'s
//! provider-neutral surface (Sprint 2 Task 2.1 stub; Task 2.2 fills in).
//!
//! In Sprint 2.1 every method returns a typed `provider_not_implemented`
//! error so the routing layer compiles, the GitHub path keeps working
//! unchanged, and downstream callers learn that GitLab is wired up but
//! not yet functional. Sprint 2.2 wires `create_issue`, `comment_issue`,
//! `edit_issue_body`, and `edit_issue_labels` to actual `forge-cli`
//! subprocess calls so `record open` works end-to-end on GitLab.
//!
//! The `#[allow(dead_code)]` on the impl is temporary: Task 2.1 ships only
//! the early-rejection at `resolve_repo_for_live` boundary; Task 2.2 wires
//! the constructor into the dispatcher call sites via
//! `crate::provider::select_adapter`.
#![allow(dead_code)]

use std::path::Path;

use serde_json::Value;

use crate::commands::plan::CloseReason;
use crate::github::{PrMergeSummary, ProviderAdapter};

#[derive(Debug, Clone, Copy)]
pub struct ForgeCliAdapter {
    #[allow(dead_code)]
    // Sprint 2.2 will read this to forward `--force` to forge-cli mutation calls.
    force: bool,
}

impl ForgeCliAdapter {
    pub const fn new(force: bool) -> Self {
        Self { force }
    }
}

fn not_implemented(op: &str) -> String {
    format!(
        "provider_not_implemented: GitLab `{op}` is wired up to the routing layer but not yet implemented (Sprint 2.1 stub). Sprint 2.2 lands the GitLab path; track sympoies/nils-cli#490."
    )
}

impl ProviderAdapter for ForgeCliAdapter {
    fn issue_body(&self, _repo: &str, _issue: u64) -> Result<String, String> {
        Err(not_implemented("issue body"))
    }

    fn issue_evidence(&self, _repo: &str, _issue: u64) -> Result<(String, String), String> {
        Err(not_implemented("issue evidence (body + comments)"))
    }

    fn create_issue(
        &self,
        _repo: &str,
        _title: &str,
        _body_file: &Path,
        _labels: &[String],
    ) -> Result<(u64, String), String> {
        Err(not_implemented("issue create"))
    }

    fn edit_issue_body(&self, _repo: &str, _issue: u64, _body_file: &Path) -> Result<(), String> {
        Err(not_implemented("issue edit body"))
    }

    fn comment_issue(&self, _repo: &str, _issue: u64, _body_file: &Path) -> Result<String, String> {
        Err(not_implemented("issue comment"))
    }

    fn edit_issue_labels(
        &self,
        _repo: &str,
        _issue: u64,
        _add: &[String],
        _remove: &[String],
    ) -> Result<(), String> {
        Err(not_implemented("issue edit labels"))
    }

    fn close_issue(
        &self,
        _repo: &str,
        _issue: u64,
        _reason: CloseReason,
        _close_comment: Option<&str>,
    ) -> Result<(), String> {
        Err(not_implemented("issue close"))
    }

    fn pr_is_merged(&self, _repo: &str, _pr: u64) -> Result<bool, String> {
        Err(not_implemented("pr is-merged"))
    }

    fn pr_merge_summary(&self, _repo: &str, _pr: u64) -> Result<PrMergeSummary, String> {
        Err(not_implemented("pr merge-summary"))
    }

    fn pr_comments(&self, _repo: &str, _pr: u64) -> Result<Vec<Value>, String> {
        Err(not_implemented("pr comments"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn every_method_returns_provider_not_implemented() {
        let adapter = ForgeCliAdapter::new(false);
        let body_file = PathBuf::from("/dev/null");

        assert!(
            adapter
                .issue_body("r", 1)
                .unwrap_err()
                .contains("provider_not_implemented")
        );
        assert!(
            adapter
                .issue_evidence("r", 1)
                .unwrap_err()
                .contains("provider_not_implemented")
        );
        assert!(
            adapter
                .create_issue("r", "t", &body_file, &[])
                .unwrap_err()
                .contains("provider_not_implemented")
        );
        assert!(
            adapter
                .edit_issue_body("r", 1, &body_file)
                .unwrap_err()
                .contains("provider_not_implemented")
        );
        assert!(
            adapter
                .comment_issue("r", 1, &body_file)
                .unwrap_err()
                .contains("provider_not_implemented")
        );
        assert!(
            adapter
                .edit_issue_labels("r", 1, &[], &[])
                .unwrap_err()
                .contains("provider_not_implemented")
        );
        assert!(
            adapter
                .close_issue("r", 1, CloseReason::Completed, None)
                .unwrap_err()
                .contains("provider_not_implemented")
        );
        assert!(
            adapter
                .pr_is_merged("r", 1)
                .unwrap_err()
                .contains("provider_not_implemented")
        );
        assert!(
            adapter
                .pr_merge_summary("r", 1)
                .unwrap_err()
                .contains("provider_not_implemented")
        );
        assert!(
            adapter
                .pr_comments("r", 1)
                .unwrap_err()
                .contains("provider_not_implemented")
        );
    }
}
