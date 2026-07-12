//! Provider-adapter trait and shared value types for plan-issue.
//!
//! plan-issue performs every provider mutation through one
//! [`ProviderAdapter`] trait. Adapter selection is provider-driven by
//! [`crate::provider::select_adapter`]; all providers route through
//! [`crate::forge_cli_adapter::ForgeCliAdapter`] (a `forge-cli` subprocess
//! wrapper). The trait and its value types live here — independent of any one
//! backend — so the abstraction does not depend on a concrete adapter module.

use std::path::Path;

use serde_json::Value;

use crate::commands::plan::CloseReason;

pub trait ProviderAdapter {
    fn issue_body(&self, repo: &str, issue: u64) -> Result<String, String>;

    /// Fetch the issue body plus comments JSON, suitable for `audit_record`
    /// fixture parsing. Returns `(body, comments_json)` where
    /// `comments_json` is the raw JSON string from the provider's
    /// issue-view-with-comments call.
    fn issue_evidence(&self, repo: &str, issue: u64) -> Result<(String, String), String>;

    /// Read the provider-confirmed labels currently attached to an issue.
    fn issue_labels(&self, repo: &str, issue: u64) -> Result<Vec<String>, String>;

    /// Read the repository label catalog used to preflight requested label
    /// additions before an irreversible closeout mutation.
    fn repository_labels(&self, repo: &str) -> Result<Vec<String>, String>;

    /// Enumerate open tracker issues to consider for `record open` resume,
    /// scoped by `labels` (AND semantics; an empty slice lists every open
    /// issue). Returns the issue numbers; the caller reads each one's
    /// lifecycle evidence to match the bundle's source snapshot identity.
    fn list_open_tracker_issues(&self, repo: &str, labels: &[String]) -> Result<Vec<u64>, String>;

    fn create_issue(
        &self,
        repo: &str,
        title: &str,
        body_file: &Path,
        labels: &[String],
    ) -> Result<(u64, String), String>;

    fn edit_issue_body(&self, repo: &str, issue: u64, body_file: &Path) -> Result<(), String>;

    /// Post an issue comment. Returns the URL of the created comment.
    fn comment_issue(&self, repo: &str, issue: u64, body_file: &Path) -> Result<String, String>;

    fn edit_issue_labels(
        &self,
        repo: &str,
        issue: u64,
        add_labels: &[String],
        remove_labels: &[String],
    ) -> Result<(), String>;

    fn close_issue(
        &self,
        repo: &str,
        issue: u64,
        reason: CloseReason,
        close_comment: Option<&str>,
    ) -> Result<(), String>;

    fn pr_is_merged(&self, repo: &str, pr: u64) -> Result<bool, String>;

    /// Provider-verified PR summary used by `record close` strict gating.
    /// Returns merge state, optional merge commit SHA, and rolled-up check
    /// status when available.
    fn pr_merge_summary(&self, repo: &str, pr: u64) -> Result<PrMergeSummary, String>;

    /// List the PR's issue-style comments. Returns an array of objects with
    /// at least `body` and `html_url` keys. Used by `resolve-approval`.
    fn pr_comments(&self, repo: &str, pr: u64) -> Result<Vec<Value>, String>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrMergeSummary {
    /// Raw `state` field from the PR view (`MERGED`, `OPEN`, `CLOSED`).
    pub state: String,
    pub merged: bool,
    pub merge_sha: Option<String>,
    /// Rolled-up status check state when known
    /// (`success`, `failure`, `pending`, `error`, ...).
    pub checks: Option<String>,
    /// Required-check rollup state when the adapter can resolve the
    /// required/non-required distinction
    /// (`success`, `failure`, `pending`, ...). `None` means the
    /// adapter could not classify (e.g. GitLab today, or a degraded
    /// call); the close gate falls back to `checks` in that case.
    pub required_state: Option<String>,
    /// Number of required checks reported by the provider. `None` when
    /// classification is unavailable; `Some(0)` means zero required
    /// checks were declared.
    pub required_count: Option<u32>,
    /// Names of non-required checks that ended in a failure-class
    /// state. Used as informational evidence in the closeout comment;
    /// the close gate never blocks on this alone.
    pub non_required_failures: Vec<String>,
}
