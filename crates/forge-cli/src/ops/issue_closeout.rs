//! `issue closeout` — deterministic post-merge linked-issue close.
//!
//! Not a standalone CLI atom: this is the helper behind the `pr deliver`
//! macro's `issue_closeout` step (envelope schema `issue.closeout`). See
//! `crates/forge-cli/docs/specs/forge-cli-ops-v1.yaml` and
//! sympoies/nils-cli#1052.
//!
//! Motivation (#1052): GitHub auto-closes an issue referenced by a
//! `Closes/Fixes #N` closing keyword when the PR merges, but that auto-close
//! is asynchronous and lags the merge — often by more than the few seconds a
//! delivery flow needs to check the issue right after merge. That produced a
//! recurring false alarm ("issue still OPEN after merge") that the flow then
//! papered over with a manual close. This helper makes the outcome
//! deterministic: for each still-open closing-keyword issue it issues one
//! explicit, idempotent close (`--reason completed`), so delivery never
//! depends on GitHub's auto-close timing. It is a determinism layer over
//! GitHub's eventually-consistent auto-close, not a fix for a GitHub defect —
//! the `closingIssuesReferences` link itself is established correctly.
//!
//! Scope: issues referenced with a non-closing `Refs #N` never appear in
//! `closingIssuesReferences`, so plan-tracking / dispatch flows (which use
//! `Refs #N` + explicit `plan-issue record close`) are untouched. GitLab is a
//! no-op today because `glab mr view` does not expose the closes-issues
//! connection (`pr_view` yields an empty ref list there).

use nils_common::cli_contract::schema_version_for;
use serde::Serialize;

use crate::backend::BackendRunner;
use crate::cli::{BINARY, CloseReasonFlag};
use crate::error::ForgeError;
use crate::ops::pr_view::ClosingIssueRef;
use crate::ops::{issue_close, issue_view};
use crate::provider::ProviderContext;

pub const SCHEMA: &str = "issue.closeout";
pub const SCHEMA_VERSION: u32 = 1;

/// What happened to one referenced issue during closeout.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CloseoutOutcome {
    pub number: u64,
    pub url: String,
    /// - `closed` — the issue was open and this step closed it.
    /// - `already_closed` — the issue was already closed (GitHub auto-close
    ///   won the race, or it was closed manually); left untouched.
    /// - `error` — the state check or the close call failed; `error` carries
    ///   why.
    pub action: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Payload for the `pr deliver` `issue_closeout` step (schema `issue.closeout`).
/// One stable shape for both the per-issue result and the pre-flight
/// (fetch-the-references) failure: `issues` is always present (possibly empty)
/// and `error` carries a step-level failure such as a post-merge `pr view`
/// that could not be read.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct IssueCloseoutPayload {
    pub provider: &'static str,
    pub issues: Vec<CloseoutOutcome>,
    /// Step-level error (e.g. the post-merge `pr view` re-fetch failed) that
    /// prevented per-issue processing. Per-issue failures live in
    /// `issues[].error` instead.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl IssueCloseoutPayload {
    /// True when the step had no step-level error and no referenced issue
    /// ended in the `error` state. Callers use this for the delivery step's
    /// `ok` flag.
    pub fn all_ok(&self) -> bool {
        self.error.is_none() && self.issues.iter().all(|o| o.action != "error")
    }
}

/// Fully-qualified schema literal for the `issue_closeout` step envelope entry.
pub fn schema_version() -> String {
    schema_version_for(BINARY, SCHEMA, SCHEMA_VERSION)
}

/// Post-merge entry point for the `pr deliver` macro: re-fetch the PR to read
/// its `closingIssuesReferences`, then close every still-open one.
///
/// Best-effort and infallible: a failed re-fetch is captured in the payload's
/// step-level `error` (empty `issues`), and per-issue failures in
/// `issues[].error` — never a returned `Err` — because the merge has already
/// landed and must not be reported as failed. Callers gate on
/// [`IssueCloseoutPayload::all_ok`] for the step's `ok` flag.
pub(crate) fn run<R: BackendRunner>(
    runner: &R,
    ctx: &ProviderContext,
    pr_number: u64,
) -> IssueCloseoutPayload {
    match crate::ops::pr_view::compute(runner, ctx, pr_number) {
        Ok(view) => compute(runner, ctx, &view.closing_issue_refs),
        Err(err) => IssueCloseoutPayload {
            provider: ctx.provider.as_str(),
            issues: Vec::new(),
            error: Some(err.to_string()),
        },
    }
}

/// Close every still-open closing-keyword issue in `refs`. See [`run`] for the
/// full post-merge flow; this is the per-issue engine, kept separate so it is
/// trivially testable with a fixed ref list.
pub(crate) fn compute<R: BackendRunner>(
    runner: &R,
    ctx: &ProviderContext,
    refs: &[ClosingIssueRef],
) -> IssueCloseoutPayload {
    let issues = refs.iter().map(|r| close_one(runner, ctx, r)).collect();
    IssueCloseoutPayload {
        provider: ctx.provider.as_str(),
        issues,
        error: None,
    }
}

fn close_one<R: BackendRunner>(
    runner: &R,
    ctx: &ProviderContext,
    r: &ClosingIssueRef,
) -> CloseoutOutcome {
    // Closes by `r.number` against the PR's own repo context. GitHub closing
    // keywords only ever link same-repo issues, so `closingIssuesReferences`
    // is same-repo and `r.url` (which encodes the owning repo) is not needed
    // to target the right issue.
    //
    // 1. Check current state — skip the close when GitHub's async auto-close
    //    (or a manual close) already landed. This also keeps the step from
    //    re-closing an issue and preserves a truthful `already_closed` signal.
    //    The probe narrows, but cannot fully eliminate, the race where GitHub
    //    auto-closes between the probe and our close; if `gh issue close` ever
    //    errors on an already-closed issue in that window the outcome is
    //    recorded as `error` (best-effort — the end state is still closed).
    let state = match fetch_state(runner, ctx, r.number) {
        Ok(state) => state,
        Err(err) => return outcome(r, "error", Some(err.to_string())),
    };
    if state != "open" {
        return outcome(r, "already_closed", None);
    }
    // 2. Still open — close it explicitly and idempotently.
    let call = issue_close::build_close_call(ctx, r.number, Some(CloseReasonFlag::Completed));
    match runner.run(&call) {
        Ok(_) => outcome(r, "closed", None),
        Err(err) => outcome(r, "error", Some(err.to_string())),
    }
}

fn outcome(r: &ClosingIssueRef, action: &'static str, error: Option<String>) -> CloseoutOutcome {
    CloseoutOutcome {
        number: r.number,
        url: r.url.clone(),
        action,
        error,
    }
}

fn fetch_state<R: BackendRunner>(
    runner: &R,
    ctx: &ProviderContext,
    number: u64,
) -> Result<&'static str, ForgeError> {
    let output = runner.run(&issue_view::build_view_call(ctx, number))?;
    let view = issue_view::parse_view_output(ctx, &output)?;
    Ok(view.state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{BackendCall, BackendSuccess};
    use crate::provider::{DetectionSource, Provider};
    use std::cell::RefCell;

    fn ctx() -> ProviderContext {
        ProviderContext {
            provider: Provider::GitHub,
            host: "github.com".into(),
            source: DetectionSource::Flag,
            repo: None,
        }
    }

    fn issue_view_json(number: u64, state: &str) -> String {
        format!(
            r#"{{"number":{number},"url":"https://x/issues/{number}","state":"{state}","title":"t","labels":[],"assignees":[],"body":""}}"#
        )
    }

    /// Scripted runner: answers `issue view` with a fixed state, records every
    /// `issue close` id plus its full argv, and can fail a chosen call.
    struct MockRunner {
        state: String,
        close_ids: RefCell<Vec<String>>,
        close_argvs: RefCell<Vec<Vec<String>>>,
        /// Closing refs the mock's `pr view` reports (drives `run`).
        pr_view_refs: Vec<u64>,
        fail_pr_view: bool,
        fail_view: bool,
        fail_close: bool,
    }

    impl MockRunner {
        fn with_state(state: &str) -> Self {
            Self {
                state: state.into(),
                close_ids: RefCell::new(Vec::new()),
                close_argvs: RefCell::new(Vec::new()),
                pr_view_refs: Vec::new(),
                fail_pr_view: false,
                fail_view: false,
                fail_close: false,
            }
        }
    }

    impl BackendRunner for MockRunner {
        fn run(&self, call: &BackendCall) -> Result<BackendSuccess, ForgeError> {
            let plan = call.plan_argv();
            let sub = format!(
                "{} {}",
                plan.get(1).cloned().unwrap_or_default(),
                plan.get(2).cloned().unwrap_or_default()
            );
            if sub == "pr view" {
                if self.fail_pr_view {
                    return Err(ForgeError::software("e", "pr view boom", None));
                }
                let refs: Vec<String> = self
                    .pr_view_refs
                    .iter()
                    .map(|n| format!(r#"{{"number":{n},"url":"https://x/issues/{n}"}}"#))
                    .collect();
                let stdout = format!(
                    r#"{{"number":123,"url":"https://x/pull/123","state":"MERGED","isDraft":false,"title":"t","headRefName":"feat/x","baseRefName":"main","mergeable":"UNKNOWN","mergedAt":"2026-01-01T00:00:00Z","labels":[],"closingIssuesReferences":[{}]}}"#,
                    refs.join(",")
                );
                return Ok(BackendSuccess {
                    stdout,
                    stderr: String::new(),
                });
            }
            if sub == "issue view" {
                if self.fail_view {
                    return Err(ForgeError::software("e", "view boom", None));
                }
                let number: u64 = plan.get(3).and_then(|s| s.parse().ok()).unwrap_or(0);
                return Ok(BackendSuccess {
                    stdout: issue_view_json(number, &self.state),
                    stderr: String::new(),
                });
            }
            if sub == "issue close" {
                if self.fail_close {
                    return Err(ForgeError::software("e", "close boom", None));
                }
                // argv: [gh, issue, close, <id>, --reason, completed]
                let id = plan.get(3).cloned().unwrap_or_default();
                self.close_ids.borrow_mut().push(id);
                self.close_argvs.borrow_mut().push(plan.clone());
                return Ok(BackendSuccess {
                    stdout: String::new(),
                    stderr: String::new(),
                });
            }
            Err(ForgeError::software(
                "e",
                format!("unexpected call: {plan:?}"),
                None,
            ))
        }
    }

    fn refs(nums: &[u64]) -> Vec<ClosingIssueRef> {
        nums.iter()
            .map(|n| ClosingIssueRef {
                number: *n,
                url: format!("https://x/issues/{n}"),
            })
            .collect()
    }

    #[test]
    fn empty_refs_yield_empty_ok_payload() {
        let runner = MockRunner::with_state("open");
        let payload = compute(&runner, &ctx(), &[]);
        assert!(payload.issues.is_empty());
        assert!(payload.all_ok());
        assert!(runner.close_ids.borrow().is_empty());
    }

    #[test]
    fn open_issue_is_closed_with_completed_reason() {
        let runner = MockRunner::with_state("open");
        let payload = compute(&runner, &ctx(), &refs(&[42]));
        assert_eq!(payload.issues.len(), 1);
        assert_eq!(payload.issues[0].number, 42);
        assert_eq!(payload.issues[0].action, "closed");
        assert!(payload.all_ok());
        // The backend close ran for the still-open issue with the exact
        // `--reason completed` argv — the auto-close-equivalent state reason.
        assert_eq!(runner.close_ids.borrow().as_slice(), ["42".to_string()]);
        assert_eq!(
            runner.close_argvs.borrow()[0][1..],
            [
                "issue".to_string(),
                "close".to_string(),
                "42".to_string(),
                "--reason".to_string(),
                "completed".to_string(),
            ]
        );
    }

    #[test]
    fn already_closed_issue_is_left_untouched() {
        let runner = MockRunner::with_state("closed");
        let payload = compute(&runner, &ctx(), &refs(&[42]));
        assert_eq!(payload.issues[0].action, "already_closed");
        assert!(payload.all_ok());
        assert!(
            runner.close_ids.borrow().is_empty(),
            "must not re-close an already-closed issue"
        );
    }

    #[test]
    fn view_failure_is_captured_as_error_not_panic() {
        let mut runner = MockRunner::with_state("open");
        runner.fail_view = true;
        let payload = compute(&runner, &ctx(), &refs(&[42]));
        assert_eq!(payload.issues[0].action, "error");
        assert!(payload.issues[0].error.is_some());
        assert!(!payload.all_ok());
    }

    #[test]
    fn close_failure_is_captured_as_error() {
        let mut runner = MockRunner::with_state("open");
        runner.fail_close = true;
        let payload = compute(&runner, &ctx(), &refs(&[42]));
        assert_eq!(payload.issues[0].action, "error");
        assert!(!payload.all_ok());
    }

    #[test]
    fn multiple_refs_each_get_an_outcome() {
        let runner = MockRunner::with_state("open");
        let payload = compute(&runner, &ctx(), &refs(&[7, 8, 9]));
        assert_eq!(payload.issues.len(), 3);
        assert!(payload.issues.iter().all(|o| o.action == "closed"));
        assert_eq!(
            runner.close_ids.borrow().as_slice(),
            ["7".to_string(), "8".to_string(), "9".to_string()]
        );
    }

    #[test]
    fn run_reads_pr_view_refs_and_closes_them() {
        let mut runner = MockRunner::with_state("open");
        runner.pr_view_refs = vec![42];
        let payload = run(&runner, &ctx(), 123);
        assert!(payload.error.is_none());
        assert_eq!(payload.issues.len(), 1);
        assert_eq!(payload.issues[0].number, 42);
        assert_eq!(payload.issues[0].action, "closed");
        assert!(payload.all_ok());
    }

    #[test]
    fn run_captures_pr_view_fetch_failure_as_step_error() {
        // The post-merge PR re-fetch fails: `run` must yield one stable shape
        // (provider + empty issues) with a step-level `error` and all_ok=false,
        // never a returned Err — the merge has already landed.
        let mut runner = MockRunner::with_state("open");
        runner.fail_pr_view = true;
        let payload = run(&runner, &ctx(), 123);
        assert!(payload.issues.is_empty());
        assert!(payload.error.is_some());
        assert!(!payload.all_ok());
        assert!(
            runner.close_ids.borrow().is_empty(),
            "no close attempted when the reference fetch failed"
        );
    }
}
