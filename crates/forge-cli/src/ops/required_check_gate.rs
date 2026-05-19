//! Required-check gate used by `pr merge` immediately before the backend
//! invocation. Performs a fresh TTL-zero snapshot through
//! [`pr_checks::snapshot`] and refuses to proceed when any required check is
//! failing or pending.
//!
//! Spec: `crates/forge-cli/docs/specs/forge-cli-spec-v1.md` §"Lock-down policy"
//! rule 8 (`required_checks_green`). Mirrors the
//! `github-pr-required-check-gating` operation record: even when an upstream
//! `pr wait-checks` finished < 1s ago, the gate re-fetches from the provider
//! so stale "all green" answers cannot leak through into a destructive merge.
//!
//! Error surface:
//!
//! | Outcome           | Variant                                 | Exit         |
//! | ----------------- | --------------------------------------- | ------------ |
//! | All required pass | `Ok(PrChecksPayload)`                   | n/a          |
//! | Any required fail | `Err(checks_failed)` → RuntimeFailure   | RUNTIME 1    |
//! | Else any pending  | `Err(checks_pending)` → Validation      | DATA 65      |
//!
//! Failure dominates pending — if both are present the gate reports
//! `checks_failed` because the spec terminal-state mapping puts failure ahead
//! of pending.

use crate::backend::BackendRunner;
use crate::cli::{BINARY, GlobalFlags, PrChecksArgs};
use crate::error::ForgeError;
use crate::ops::pr_checks::{self, PrChecksPayload};
use crate::provider::ProviderContext;
use nils_common::cli_contract::schema_version_for;

/// Schema literal used by the gate's failure envelopes. Re-uses the
/// `pr.checks.v1` schema so failure envelopes carry the same shape that
/// upstream consumers already parse — the only thing that changes is `ok` and
/// the `error.kind` discriminator.
fn schema() -> String {
    schema_version_for(BINARY, pr_checks::SCHEMA, pr_checks::SCHEMA_VERSION)
}

/// Fetch the current required-check snapshot and assert all required checks
/// are passing.
///
/// On success returns the populated snapshot so callers (e.g. `pr merge`) can
/// surface counts under `data.checks_snapshot` if they choose. On failure the
/// returned [`ForgeError`] already carries the discriminator + exit code; the
/// caller just forwards it to the envelope emitter.
pub fn ensure_required_checks_green<R: BackendRunner>(
    runner: &R,
    global: &GlobalFlags,
    ctx: &ProviderContext,
    pr_id: &str,
) -> Result<PrChecksPayload, ForgeError> {
    let args = PrChecksArgs {
        id: pr_id.to_string(),
        required_only: true,
    };
    let snapshot = pr_checks::snapshot(runner, global, ctx, &args)?;
    classify(snapshot)
}

fn classify(snapshot: PrChecksPayload) -> Result<PrChecksPayload, ForgeError> {
    if !snapshot.failed.is_empty() {
        let names = snapshot
            .failed
            .iter()
            .map(|f| f.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(ForgeError::runtime_failure(
            schema(),
            "checks_failed",
            format!(
                "{} required check(s) failed: {}",
                snapshot.failed.len(),
                names
            ),
            None,
        ));
    }
    if !snapshot.pending.is_empty() {
        let names = snapshot
            .pending
            .iter()
            .map(|p| p.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(ForgeError::validation(
            schema(),
            "checks_pending",
            format!(
                "{} required check(s) still pending: {}",
                snapshot.pending.len(),
                names
            ),
            None,
        ));
    }
    Ok(snapshot)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::pr_checks::{CheckItem, FailedCheck, PendingCheck};
    use nils_common::cli_contract::exit;
    use pretty_assertions::assert_eq;

    fn pass_snapshot() -> PrChecksPayload {
        PrChecksPayload {
            provider: "github",
            state: "success",
            required_count: 2,
            success_count: 2,
            failed: vec![],
            pending: vec![],
            checks: vec![
                CheckItem {
                    name: "test".into(),
                    state: "success",
                    url: None,
                    conclusion: Some("success".into()),
                    workflow: None,
                    required: true,
                    started_at: None,
                    completed_at: None,
                },
                CheckItem {
                    name: "lint".into(),
                    state: "success",
                    url: None,
                    conclusion: Some("success".into()),
                    workflow: None,
                    required: true,
                    started_at: None,
                    completed_at: None,
                },
            ],
            duration_ms: None,
        }
    }

    fn with_failure(name: &str) -> PrChecksPayload {
        let mut snap = pass_snapshot();
        snap.state = "failure";
        snap.success_count = 1;
        snap.failed.push(FailedCheck {
            name: name.into(),
            url: None,
            conclusion: Some("failure".into()),
        });
        snap
    }

    fn with_pending(name: &str) -> PrChecksPayload {
        let mut snap = pass_snapshot();
        snap.state = "pending";
        snap.success_count = 1;
        snap.pending.push(PendingCheck {
            name: name.into(),
            url: None,
        });
        snap
    }

    #[test]
    fn all_green_returns_payload_untouched() {
        let snap = pass_snapshot();
        let result = classify(snap.clone()).expect("must pass");
        assert_eq!(result.state, "success");
        assert_eq!(result.success_count, 2);
    }

    #[test]
    fn pending_only_yields_checks_pending_validation_with_data_65() {
        let err = classify(with_pending("integration")).expect_err("must fail with pending");
        assert_eq!(err.kind(), "checks_pending");
        assert_eq!(err.exit_code(), exit::DATA);
    }

    #[test]
    fn failure_yields_checks_failed_runtime_with_exit_1() {
        let err = classify(with_failure("test")).expect_err("must fail with failure");
        assert_eq!(err.kind(), "checks_failed");
        assert_eq!(err.exit_code(), exit::RUNTIME);
    }

    #[test]
    fn failure_dominates_pending_when_both_present() {
        // Per spec terminal-state mapping (failure > pending), checks_failed
        // wins so the merge atom surfaces the most actionable error first.
        let mut snap = with_failure("compile");
        snap.pending.push(PendingCheck {
            name: "integration".into(),
            url: None,
        });
        let err = classify(snap).expect_err("must fail with failure");
        assert_eq!(err.kind(), "checks_failed");
    }

    #[test]
    fn error_message_lists_offending_check_names() {
        let snap = {
            let mut s = pass_snapshot();
            s.state = "failure";
            s.success_count = 0;
            s.failed.push(FailedCheck {
                name: "test".into(),
                url: None,
                conclusion: Some("failure".into()),
            });
            s.failed.push(FailedCheck {
                name: "lint".into(),
                url: None,
                conclusion: Some("failure".into()),
            });
            s
        };
        let err = classify(snap).expect_err("must fail");
        let rendered = format!("{err}");
        assert!(rendered.contains("test"), "got {rendered}");
        assert!(rendered.contains("lint"), "got {rendered}");
        assert!(rendered.starts_with("2 required check(s) failed"));
    }

    #[test]
    fn schema_literal_matches_pr_checks_v1() {
        // Failure envelopes from the gate share the pr.checks.v1 schema so
        // upstream consumers parse one shape end-to-end across the merge flow.
        assert_eq!(schema(), "cli.forge-cli.pr.checks.v1");
    }
}
