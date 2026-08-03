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
//! | Outcome            | Variant                                       | Exit      |
//! | ------------------ | --------------------------------------------- | --------- |
//! | All required pass  | `Ok(PrChecksPayload)`                         | n/a       |
//! | Any required fail  | `Err(checks_failed)` → RuntimeFailure         | RUNTIME 1 |
//! | Else any pending   | `Err(checks_pending)` → Validation            | DATA 65   |
//! | Else nothing ran   | `Err(checks_not_registered)` → Validation     | DATA 65   |
//!
//! Failure dominates pending — if both are present the gate reports
//! `checks_failed` because the spec terminal-state mapping puts failure ahead
//! of pending.
//!
//! # Absence is not success
//!
//! "All required checks passed" is vacuously true over an empty set, so a
//! snapshot with nothing failing, nothing pending, **and nothing passing** used
//! to reach `Ok`. That let a PR satisfy the gate having run no CI at all, which
//! is not hypothetical: `gh pr checks --required` exits non-zero with "no
//! required checks reported" during the provider's check-registration window
//! after a force-with-lease, and [`pr_checks`] deliberately normalizes that into
//! an empty successful snapshot so the read surface can report "nothing is
//! failing" without erroring.
//!
//! Reporting and gating want opposite defaults there. The read surface is right
//! to say "no checks are failing"; the gate must not accept that as proof the
//! head was checked, because the gate exists to establish exactly that. So the
//! empty gating set fails closed here as `checks_not_registered`, while
//! [`pr_checks`] keeps reporting it as `success` — and a repository that
//! genuinely configures no checks opts out per invocation with
//! `--allow-no-checks`.
//!
//! The neighbouring REST fallback already reached this conclusion: when
//! requiredness is unknown and the row set is empty it *synthesizes a pending
//! row* rather than returning an empty pass. This is the same rule applied to
//! the path that reports emptiness honestly.

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

/// Whether the caller requires the head to have been checked at all.
///
/// Deliberately not a bare `bool`: at the call site `CheckPresence::Optional`
/// says which authority is being granted, where `true` would not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckPresence {
    /// The gating set must be non-empty — absence of checks blocks the merge.
    Required,
    /// The repository genuinely configures no checks; an empty gating set is
    /// acceptable. Failing and pending checks are still reported.
    Optional,
}

impl CheckPresence {
    /// Map the `--allow-no-checks` flag onto the presence requirement.
    pub fn from_allow_no_checks(allow_no_checks: bool) -> Self {
        if allow_no_checks {
            Self::Optional
        } else {
            Self::Required
        }
    }
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
    presence: CheckPresence,
) -> Result<PrChecksPayload, ForgeError> {
    let args = PrChecksArgs {
        id: pr_id.to_string(),
        required_only: true,
    };
    let snapshot = pr_checks::snapshot(runner, global, ctx, &args)?;
    classify(snapshot, presence)
}

/// True when the provider reported nothing at all for this head.
///
/// The predicate is deliberately narrower than "no *required* checks". A
/// repository can run CI without branch protection, so a snapshot with visible
/// rows but nothing marked required means the head *was* checked — `pr deliver`
/// re-gates those visible rows through `should_gate_visible_checks`, and
/// refusing them here would block every such repository. What must not pass is
/// the case with no gating set *and* no rows to fall back to: then nothing ran,
/// and nothing can be re-gated.
pub(crate) fn nothing_was_checked(snapshot: &PrChecksPayload) -> bool {
    snapshot.required_count == 0 && snapshot.checks.is_empty()
}

fn classify(
    snapshot: PrChecksPayload,
    presence: CheckPresence,
) -> Result<PrChecksPayload, ForgeError> {
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
    // Checked last, so a snapshot that also carries a failure or a pending row
    // still reports the more actionable of the two first.
    if presence == CheckPresence::Required && nothing_was_checked(&snapshot) {
        return Err(ForgeError::validation(
            schema(),
            "checks_not_registered",
            "no required checks are registered for this head, so nothing proves it was checked; \
             wait for the provider to register them, or pass --allow-no-checks if this repository \
             genuinely configures none",
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
            warnings: Vec::new(),
        }
    }

    /// What `gh pr checks --required` yields when the provider reports "no
    /// required checks reported": a successful, entirely empty snapshot.
    fn empty_snapshot() -> PrChecksPayload {
        PrChecksPayload {
            provider: "github",
            state: "success",
            required_count: 0,
            success_count: 0,
            failed: vec![],
            pending: vec![],
            checks: vec![],
            duration_ms: None,
            warnings: Vec::new(),
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
        let result = classify(snap.clone(), CheckPresence::Required).expect("must pass");
        assert_eq!(result.state, "success");
        assert_eq!(result.success_count, 2);
    }

    #[test]
    fn pending_only_yields_checks_pending_validation_with_data_65() {
        let err = classify(with_pending("integration"), CheckPresence::Required)
            .expect_err("must fail with pending");
        assert_eq!(err.kind(), "checks_pending");
        assert_eq!(err.exit_code(), exit::DATA);
    }

    #[test]
    fn failure_yields_checks_failed_runtime_with_exit_1() {
        let err = classify(with_failure("test"), CheckPresence::Required)
            .expect_err("must fail with failure");
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
        let err = classify(snap, CheckPresence::Required).expect_err("must fail with failure");
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
        let err = classify(snap, CheckPresence::Required).expect_err("must fail");
        let rendered = format!("{err}");
        assert!(rendered.contains("test"), "got {rendered}");
        assert!(rendered.contains("lint"), "got {rendered}");
        assert!(rendered.starts_with("2 required check(s) failed"));
    }

    /// The gate's whole purpose is proving the head was checked, and an empty
    /// snapshot proves the opposite. "All required checks passed" is vacuously
    /// true over an empty set, which is how a PR reached a green gate having
    /// run no CI at all — the provider's check-registration window after a
    /// force-with-lease is exactly when that happens.
    #[test]
    fn an_empty_snapshot_cannot_satisfy_the_gate() {
        let err = classify(empty_snapshot(), CheckPresence::Required)
            .expect_err("an unchecked head must not satisfy the gate");
        assert_eq!(err.kind(), "checks_not_registered");
        assert_eq!(err.exit_code(), exit::DATA);
    }

    /// CI without branch protection is not an unchecked head.
    ///
    /// A repository can run checks while marking none of them required, which
    /// yields `required_count == 0` alongside visible passing rows. `pr deliver`
    /// re-gates exactly those rows, so refusing here would block every such
    /// repository — a far bigger blast radius than the defect being fixed. The
    /// refusal is scoped to "no gating set *and* nothing to fall back to".
    #[test]
    fn visible_checks_with_no_required_ones_are_not_an_unchecked_head() {
        let mut snap = pass_snapshot();
        // The rows stay visible; branch protection just does not gate on them.
        snap.required_count = 0;
        snap.success_count = 0;
        for check in &mut snap.checks {
            check.required = false;
        }

        let result = classify(snap, CheckPresence::Required)
            .expect("visible checks mean the head was checked");
        assert_eq!(result.checks.len(), 2);
    }

    /// Pins a gap this rule does **not** close, so nobody reads the rule as
    /// wider than it is.
    ///
    /// With `required_only`, a non-required row is not in `failed`, so a head
    /// whose only visible check FAILED still reaches `Ok` here. `pr deliver`
    /// catches it by re-gating the visible set — on GitHub only — but
    /// standalone `pr merge` does not, on any provider. That predates this
    /// change and is out of its scope; the test exists so the behaviour is
    /// owned and a future fix has something to flip.
    #[test]
    fn a_failing_visible_row_with_no_required_checks_still_passes_the_gate() {
        let mut snap = pass_snapshot();
        snap.required_count = 0;
        snap.success_count = 0;
        for check in &mut snap.checks {
            check.required = false;
        }
        snap.checks[0].state = "failure";

        let result = classify(snap, CheckPresence::Required)
            .expect("documented current behaviour, not an endorsement");
        assert_eq!(result.checks[0].state, "failure");
        assert!(
            result.failed.is_empty(),
            "a non-required failure never enters the gating set"
        );
    }

    /// A repository that genuinely configures no checks must still be able to
    /// merge, so the refusal is opt-out-able — explicitly, per invocation.
    #[test]
    fn an_empty_snapshot_passes_when_the_caller_allows_no_checks() {
        let snap = classify(empty_snapshot(), CheckPresence::Optional)
            .expect("an explicit allowance must let an unchecked head through");
        assert_eq!(snap.required_count, 0);
    }

    /// The allowance is scoped to absence only. It must not become a blanket
    /// "ignore the checks" switch, because that is a far larger authority than
    /// "this repo has no checks".
    #[test]
    fn allowing_no_checks_still_reports_a_failing_check() {
        let err = classify(with_failure("test"), CheckPresence::Optional)
            .expect_err("an allowance for absence must not excuse a failure");
        assert_eq!(err.kind(), "checks_failed");
    }

    #[test]
    fn allowing_no_checks_still_reports_a_pending_check() {
        let err = classify(with_pending("integration"), CheckPresence::Optional)
            .expect_err("an allowance for absence must not excuse a pending check");
        assert_eq!(err.kind(), "checks_pending");
    }

    /// The message has to say what to do about it: a caller who hits this needs
    /// to know both that nothing ran and which flag states otherwise.
    #[test]
    fn not_registered_message_names_the_head_state_and_the_opt_out() {
        let err = classify(empty_snapshot(), CheckPresence::Required).expect_err("must fail");
        let rendered = format!("{err}");
        assert!(
            rendered.contains("no required checks"),
            "must say nothing was registered: {rendered}"
        );
        assert!(
            rendered.contains("--allow-no-checks"),
            "must name the opt-out: {rendered}"
        );
    }

    #[test]
    fn schema_literal_matches_pr_checks_v1() {
        // Failure envelopes from the gate share the pr.checks.v1 schema so
        // upstream consumers parse one shape end-to-end across the merge flow.
        assert_eq!(schema(), "cli.forge-cli.pr.checks.v1");
    }
}
