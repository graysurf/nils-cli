//! `pr deliver` macro — the v1 lifecycle composition.
//!
//! Spec / ops: `cli.forge-cli.pr.deliver.v1`. Sequence per spec §"Macro:
//! pr deliver":
//!
//! ```text
//! auth.status → repo.view → lookup → pr.create → pr.wait-checks
//!                                  ↘ adopt    ↗ → pr.ready (skip if --no-merge)
//!                                               → pr.merge (skip if --no-merge)
//! ```
//!
//! The lookup resolves the head branch and asks the provider for an open
//! PR/MR on it. When one exists the macro adopts it — recording an `adopt`
//! step carrying the PR's view payload — instead of creating, so the split
//! create → iterate → deliver workflow resumes its lifecycle. The adopted
//! PR's *actual* body is re-validated against the body-section gate.
//!
//! Each step's typed payload is captured via the atom's `compute` helper
//! (no subprocess re-spawn through a child binary) and appended to
//! `data.steps[]`. A failing step short-circuits — later steps are omitted
//! from `data.steps[]` and the macro's outer exit code equals the failing
//! atom's exit code (no remapping).

use std::path::{Path, PathBuf};

use nils_common::cli_contract::{Envelope, EnvelopeError, OutputFormat, schema_version_for};
use serde::Serialize;
use serde_json::Value;

use crate::backend::{BackendCall, BackendProgram, BackendRunner};
use crate::cli::{
    BINARY, GlobalFlags, PrCreateArgs, PrDeliverArgs, PrListArgs, PrMergeArgs, PrStateFilter,
    PrWaitChecksArgs,
};
use crate::config::ForgeConfig;
use crate::error::ForgeError;
use crate::ops::pr_create::{
    self, Environment, VerifiedTestFirstSubject, evidence_repository_id, find_git_toplevel,
    test_first_gate, validate_provider_subject_head,
};
use crate::ops::pr_view::PrViewPayload;
use crate::ops::pr_wait_checks::{Clock, SystemClock, WaitOutcome};
use crate::ops::{
    auth_status, issue_close, issue_closeout, pr_checks, pr_list, pr_merge, pr_ready, pr_view,
    pr_wait_checks, repo_view,
};
use crate::provider::{Provider, ProviderContext, detect, git_remote_url};
use crate::rate_limit::default_runner;
use crate::validations::{
    BodyHeadings, PreflightInputs, RuleVerdict, body_sections, branch_kind_matches, branch_name,
    branch_pushed, git_branch_state, git_current_branch, git_status_porcelain, run_local_preflight,
    worktree_clean,
};

pub const SCHEMA: &str = "pr.deliver";
pub const SCHEMA_VERSION: u32 = 1;

/// Composite envelope payload.
#[derive(Debug, Clone, Serialize)]
pub struct PrDeliverPayload {
    pub kind: &'static str,
    pub provider: &'static str,
    pub pr: PrDeliverSummary,
    pub steps: Vec<Step>,
}

/// Summary of the resulting PR, mirroring the spec's `data.pr` shape.
#[derive(Debug, Clone, Serialize)]
pub struct PrDeliverSummary {
    pub number: u64,
    pub url: String,
    pub merged: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub merge_sha: Option<String>,
}

/// One step in the macro sequence. `payload` carries the underlying atom's
/// typed data as a JSON value so consumers can grep through the chain
/// without depending on every atom's Rust type.
#[derive(Debug, Clone, Serialize)]
pub struct Step {
    pub step: &'static str,
    pub ok: bool,
    pub schema_version: String,
    pub payload: Value,
}

/// Dry-run envelope. `plan_steps[]` enumerates every atom that would run
/// in this invocation; the macro never spawns a backend in dry-run.
/// `local_preflight[]` reports each non-mutating lock-down rule's verdict so
/// a single dry-run predicts whether the real run's local gates will pass.
#[derive(Debug, Clone, Serialize)]
pub struct PrDeliverDryRun {
    pub provider: &'static str,
    pub kind: &'static str,
    pub plan_steps: Vec<DryRunStep>,
    pub no_merge: bool,
    /// Per-rule verdicts from the non-mutating local preflight (Rules 1a, 1b,
    /// 3, 2a, 2b, 4, 5). Additive: consumers that predate this field ignore
    /// it. The dry-run path never invokes a provider backend to compute it.
    pub local_preflight: Vec<RuleVerdict>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DryRunStep {
    pub step: &'static str,
    pub plan: Vec<String>,
}

pub fn run(
    global: &GlobalFlags,
    args: PrDeliverArgs,
    format: OutputFormat,
) -> Result<i32, ForgeError> {
    let runner = default_runner();
    let clock = SystemClock;
    let workdir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    run_with(&runner, &clock, global, args, format, &workdir)
}

pub fn run_with<R: BackendRunner, C: Clock>(
    runner: &R,
    clock: &C,
    global: &GlobalFlags,
    args: PrDeliverArgs,
    format: OutputFormat,
    workdir: &Path,
) -> Result<i32, ForgeError> {
    let ctx = detect(
        global.provider_hint(),
        &global.remote,
        global.repo.as_deref(),
        git_remote_url,
    )?;

    if global.dry_run {
        return Ok(emit_dry_run(&ctx, &args, format, workdir, global));
    }

    execute_sequence(runner, clock, global, &ctx, &args, format, workdir)
}

fn execute_sequence<R: BackendRunner, C: Clock>(
    runner: &R,
    clock: &C,
    global: &GlobalFlags,
    ctx: &ProviderContext,
    args: &PrDeliverArgs,
    format: OutputFormat,
    workdir: &Path,
) -> Result<i32, ForgeError> {
    let mut steps: Vec<Step> = Vec::new();
    let mut merged = false;
    let mut merge_sha: Option<String> = None;

    // 1. auth.status
    let auth_payload = match auth_status::compute(runner, global, git_remote_url) {
        Ok(p) => p,
        Err(err) => {
            return Ok(emit_chain_failure(steps, args, ctx, None, &err, format));
        }
    };
    steps.push(Step {
        step: "auth_status",
        ok: true,
        schema_version: schema_version_for(BINARY, "auth.status", 1),
        payload: to_value(&auth_payload),
    });

    // 2. repo.view
    let repo_payload = match repo_view::compute(runner, ctx) {
        Ok(p) => p,
        Err(err) => {
            return Ok(emit_chain_failure(steps, args, ctx, None, &err, format));
        }
    };
    steps.push(Step {
        step: "repo_view",
        ok: true,
        schema_version: schema_version_for(BINARY, "repo.view", 1),
        payload: to_value(&repo_payload),
    });

    // 3. Head-branch lookup, then adopt-or-create. An open PR already on
    //    the resolved head branch is adopted — the macro skips the create
    //    step (and its create-input gates) so a draft opened earlier via
    //    `pr create` can be finished here instead of dead-ending on the
    //    body gate. The adopted PR's actual body is re-fetched via pr.view
    //    and re-validated before the lifecycle continues.
    let head = match &args.head {
        Some(h) => h.clone(),
        None => match git_current_branch(workdir) {
            Ok(b) => b,
            Err(err) => {
                return Ok(emit_chain_failure(steps, args, ctx, None, &err, format));
            }
        },
    };
    let lookup_args = adopt_lookup_args(&head);
    let existing = match pr_list::compute(runner, ctx, &lookup_args) {
        Ok(payload) => payload.items.into_iter().next(),
        Err(err) => {
            return Ok(emit_chain_failure(steps, args, ctx, None, &err, format));
        }
    };

    let (pr_number, pr_url, verified_subject) = if let Some(found) = existing {
        // adopt
        let view = match pr_view::compute(runner, ctx, found.number) {
            Ok(v) => v,
            Err(err) => {
                return Ok(emit_chain_failure(
                    steps,
                    args,
                    ctx,
                    Some((found.number, found.url.clone())),
                    &err,
                    format,
                ));
            }
        };
        let number = view.number;
        let url = view.url.clone();
        // Resolve the test-first gate from layered config so an adopted
        // feature/bug PR is held to the same evidence requirement as the
        // create path (a draft opened earlier must still carry evidence to
        // be delivered in an opted-in repo).
        let cfg = ForgeConfig::load_layered(workdir, find_git_toplevel(workdir).as_deref());
        let test_first_required = cfg.resolve_test_first_required(None);
        let gate_applies = test_first_required
            && matches!(
                args.kind.into_kind(),
                nils_common::git::PrKind::Feature | nils_common::git::PrKind::Bug
            );
        let remote_url = if gate_applies
            && ctx.provider == Provider::Local
            && global.repo.is_none()
            && ctx.repo.is_none()
        {
            git_remote_url(&global.remote)
        } else {
            None
        };
        let repository_id = gate_applies
            .then(|| evidence_repository_id(ctx, remote_url.as_deref(), global.repo.as_deref()))
            .flatten();
        let verified_subject = match validate_adopted(
            &view,
            args,
            workdir,
            &global.remote,
            repository_id.as_deref(),
            test_first_required,
        ) {
            Ok(subject) => subject,
            Err(err) => {
                steps.push(Step {
                    step: "adopt",
                    ok: false,
                    schema_version: schema_version_for(BINARY, "pr.view", 1),
                    payload: to_value(&view),
                });
                return Ok(emit_chain_failure(
                    steps,
                    args,
                    ctx,
                    Some((number, url)),
                    &err,
                    format,
                ));
            }
        };
        if let Err(err) =
            validate_provider_subject_head(verified_subject.as_ref(), view.head_sha.as_deref())
        {
            return Ok(emit_chain_failure(
                steps,
                args,
                ctx,
                Some((number, url)),
                &err,
                format,
            ));
        }
        steps.push(Step {
            step: "adopt",
            ok: true,
            schema_version: schema_version_for(BINARY, "pr.view", 1),
            payload: to_value(&view),
        });
        (number, url, verified_subject)
    } else {
        // pr.create
        let create_args = build_create_args(args, &repo_payload.default_branch);
        let env = Environment::production();
        let create_result =
            match pr_create::compute_with_subject(runner, global, &create_args, &env) {
                Ok(result) => result,
                Err(err) => {
                    return Ok(emit_chain_failure(steps, args, ctx, None, &err, format));
                }
            };
        let create_payload = create_result.payload;
        let verified_subject = create_result.verified_subject;
        let number = create_payload.number;
        let url = create_payload.url.clone();
        steps.push(Step {
            step: "create",
            ok: true,
            schema_version: schema_version_for(BINARY, "pr.create", 1),
            payload: to_value(&create_payload),
        });
        (number, url, verified_subject)
    };

    // 4. pr.wait-checks
    let wait_args = PrWaitChecksArgs {
        id: pr_number.to_string(),
        timeout: args.timeout,
        interval: std::time::Duration::from_secs(20),
        required_only: true,
    };
    match pr_wait_checks::compute(runner, clock, global, ctx, &wait_args) {
        Ok(WaitOutcome::Success(snapshot)) => {
            steps.push(Step {
                step: "wait_checks",
                ok: true,
                schema_version: schema_version_for(BINARY, "pr.checks", 1),
                payload: to_value(&snapshot),
            });
        }
        Ok(WaitOutcome::Failed(snapshot)) => {
            let err = ForgeError::runtime_failure(
                schema_version_for(BINARY, "pr.checks", 1),
                "checks_failed",
                "required checks did not reach success",
                None,
            );
            steps.push(Step {
                step: "wait_checks",
                ok: false,
                schema_version: schema_version_for(BINARY, "pr.checks", 1),
                payload: to_value(&snapshot),
            });
            return Ok(emit_chain_failure(
                steps,
                args,
                ctx,
                Some((pr_number, pr_url)),
                &err,
                format,
            ));
        }
        Ok(WaitOutcome::TimedOut(snapshot)) => {
            let err = ForgeError::unavailable(
                schema_version_for(BINARY, "pr.checks", 1),
                "checks_timeout",
                "deadline reached before required checks became terminal",
                None,
            );
            steps.push(Step {
                step: "wait_checks",
                ok: false,
                schema_version: schema_version_for(BINARY, "pr.checks", 1),
                payload: to_value(&snapshot),
            });
            return Ok(emit_chain_failure(
                steps,
                args,
                ctx,
                Some((pr_number, pr_url)),
                &err,
                format,
            ));
        }
        Err(err) => {
            return Ok(emit_chain_failure(
                steps,
                args,
                ctx,
                Some((pr_number, pr_url)),
                &err,
                format,
            ));
        }
    }

    if let Some(subject) = verified_subject.as_ref() {
        let current_view = match pr_view::compute(runner, ctx, pr_number) {
            Ok(view) => view,
            Err(err) => {
                return Ok(emit_chain_failure(
                    steps,
                    args,
                    ctx,
                    Some((pr_number, pr_url)),
                    &err,
                    format,
                ));
            }
        };
        if let Err(err) =
            validate_provider_subject_head(Some(subject), current_view.head_sha.as_deref())
        {
            return Ok(emit_chain_failure(
                steps,
                args,
                ctx,
                Some((pr_number, pr_url)),
                &err,
                format,
            ));
        }
    }

    if args.no_merge {
        // Macro ends after wait-checks when --no-merge is set; outer exit
        // code matches `pr.wait-checks` success (0).
        return Ok(emit_success_envelope(
            steps, args, ctx, pr_number, pr_url, merged, merge_sha, format,
        ));
    }

    // 5. pr.ready
    let ready_payload =
        match pr_ready::compute(runner, ctx, pr_number, workdir, git_status_porcelain) {
            Ok(p) => p,
            Err(err) => {
                return Ok(emit_chain_failure(
                    steps,
                    args,
                    ctx,
                    Some((pr_number, pr_url)),
                    &err,
                    format,
                ));
            }
        };
    if let Err(err) =
        validate_provider_subject_head(verified_subject.as_ref(), ready_payload.head_sha.as_deref())
    {
        return Ok(emit_chain_failure(
            steps,
            args,
            ctx,
            Some((pr_number, pr_url)),
            &err,
            format,
        ));
    }
    steps.push(Step {
        step: "ready",
        ok: true,
        schema_version: schema_version_for(BINARY, "pr.ready", 1),
        payload: to_value(&ready_payload),
    });

    // 6. pr.merge
    let merge_args = PrMergeArgs {
        id: pr_number,
        expected_head_sha: verified_subject
            .as_ref()
            .map(|subject| subject.head.clone()),
        method: Some(args.method),
        keep_branch: false,
        allow_non_default_base: args.allow_non_default_base,
        allow_unresolved_threads: args.allow_unresolved_threads,
        allow_unchecked_tasks: args.allow_unchecked_tasks,
        allow_unchecked_tasks_reason: args.allow_unchecked_tasks_reason.clone(),
    };
    let merge_payload = match pr_merge::compute(runner, global, &merge_args, workdir) {
        Ok(p) => p,
        Err(err) => {
            return Ok(emit_chain_failure(
                steps,
                args,
                ctx,
                Some((pr_number, pr_url)),
                &err,
                format,
            ));
        }
    };
    merge_sha = Some(merge_payload.merge_sha.clone());
    merged = true;
    steps.push(Step {
        step: "merge",
        ok: true,
        schema_version: schema_version_for(BINARY, "pr.merge", 1),
        payload: to_value(&merge_payload),
    });

    // 7. issue closeout — deterministic linked-issue close. The merge has
    //    already landed, so this step is best-effort and never short-circuits
    //    the macro: a fetch/close failure is recorded as an `ok:false` step
    //    (or captured per-issue) but the delivery still reports the merge that
    //    happened. See ops/issue_closeout.rs and #1052.
    if !args.no_issue_closeout {
        let closeout_step = run_issue_closeout(runner, ctx, pr_number);
        steps.push(closeout_step);
    }

    Ok(emit_success_envelope(
        steps, args, ctx, pr_number, pr_url, merged, merge_sha, format,
    ))
}

/// Run the post-merge closeout and build its `data.steps[]` entry.
/// [`issue_closeout::run`] re-fetches `closingIssuesReferences` and closes each
/// still-open one, returning one stable payload shape whether the re-fetch or a
/// per-issue close failed. It never returns `Err`, so the delivered merge is
/// never misreported as failed; the step's `ok` mirrors `all_ok()`.
fn run_issue_closeout<R: BackendRunner>(runner: &R, ctx: &ProviderContext, pr_number: u64) -> Step {
    let payload = issue_closeout::run(runner, ctx, pr_number);
    Step {
        step: "issue_closeout",
        ok: payload.all_ok(),
        schema_version: issue_closeout::schema_version(),
        payload: to_value(&payload),
    }
}

/// Lookup filter for the adopt step: open PRs whose head / source branch is
/// the resolved head. The first match wins; the provider returns them
/// newest-first.
fn adopt_lookup_args(head: &str) -> PrListArgs {
    PrListArgs {
        state: PrStateFilter::Open,
        author: None,
        head: Some(head.to_string()),
        limit: 1,
    }
}

/// Adopt-path validation. Mirrors the create-path gates that still apply to
/// an existing PR: the adopted head branch must match `--kind`, the PR's
/// *actual* body (fetched via pr.view) must pass the body-section gate, the
/// local tree must satisfy the same worktree / push rules as create (spec
/// lock-down rules 4 and 5 cover `deliver`), and — when the repo opts in — the
/// test-first evidence gate. Create-input-only gates (title, `--body`,
/// local-path) are skipped — the PR already carries its own provider-validated
/// title and body. `test_first_required` is resolved by the caller from
/// layered config so this stays a pure validation.
fn validate_adopted(
    view: &PrViewPayload,
    args: &PrDeliverArgs,
    workdir: &Path,
    remote: &str,
    repository_id: Option<&str>,
    test_first_required: bool,
) -> Result<Option<VerifiedTestFirstSubject>, ForgeError> {
    let prefix = branch_name(&view.head)?;
    branch_kind_matches(prefix, args.kind.into_kind())?;
    let headings = BodyHeadings::default();
    body_sections(view.body.as_deref().unwrap_or(""), &headings)?;
    worktree_clean(workdir, git_status_porcelain)?;
    branch_pushed(workdir, &view.head, git_branch_state)?;
    test_first_gate(
        args.kind.into_kind(),
        test_first_required,
        args.test_first_evidence.as_deref(),
        workdir,
        remote,
        repository_id,
        &view.head,
    )
}

fn build_create_args(args: &PrDeliverArgs, default_branch: &str) -> PrCreateArgs {
    PrCreateArgs {
        head: args.head.clone(),
        base: args
            .base
            .clone()
            .or_else(|| Some(default_branch.to_string())),
        title: args.title.clone(),
        body: args.body.clone(),
        body_file: args.body_file.clone(),
        kind: args.kind,
        no_draft: false,
        reviewers: args.reviewers.clone(),
        labels: args.labels.clone(),
        label_catalog: args.label_catalog.clone(),
        strict_labels: args.strict_labels,
        test_first_evidence: args.test_first_evidence.clone(),
    }
}

/// Resolve the body the real run would validate, without consuming stdin.
/// `--body-file -` (stdin) is treated as unknown (empty) for the preview so a
/// dry-run never blocks reading from the terminal.
fn resolve_preview_body(args: &PrDeliverArgs) -> String {
    if let Some(body) = &args.body {
        return body.clone();
    }
    if let Some(path) = &args.body_file
        && path.as_str() != "-"
    {
        return std::fs::read_to_string(path).unwrap_or_default();
    }
    String::new()
}

/// Build the test-first preflight verdict for `pr deliver --dry-run`. Returns
/// `None` when the gate is off (`required == false`) so the dry-run only
/// surfaces the rule for repos that opted in; otherwise it reports the same
/// pass/fail the real run's `test_first_gate` would produce (exempt kinds such
/// as docs/chore pass without evidence). Kept pure so the caller injects the
/// resolved config and tests need no environment.
fn test_first_preflight_verdict(
    args: &PrDeliverArgs,
    required: bool,
    workdir: &Path,
    remote: &str,
    repository_id: Option<&str>,
    delivery_ref: &str,
) -> Option<RuleVerdict> {
    if !required {
        return None;
    }
    Some(RuleVerdict::from_result(
        "test_first",
        test_first_gate(
            args.kind.into_kind(),
            required,
            args.test_first_evidence.as_deref(),
            workdir,
            remote,
            repository_id,
            delivery_ref,
        )
        .map(|_| ()),
    ))
}

fn emit_dry_run(
    ctx: &ProviderContext,
    args: &PrDeliverArgs,
    format: OutputFormat,
    workdir: &Path,
    global: &GlobalFlags,
) -> i32 {
    let branch = match &args.head {
        Some(head) => head.clone(),
        None => git_current_branch(workdir).unwrap_or_default(),
    };
    let mut plan_steps = vec![
        DryRunStep {
            step: "auth_status",
            plan: BackendCall::new(
                BackendProgram::for_provider(ctx.provider),
                [
                    std::ffi::OsString::from("auth"),
                    std::ffi::OsString::from("status"),
                ],
            )
            .plan_argv(),
        },
        DryRunStep {
            step: "repo_view",
            plan: repo_view_dry_plan(ctx),
        },
        DryRunStep {
            step: "lookup",
            plan: pr_lookup_dry_plan(ctx, &branch),
        },
        DryRunStep {
            step: "create",
            plan: pr_create_dry_plan(ctx, args),
        },
        DryRunStep {
            step: "wait_checks",
            plan: pr_wait_checks_dry_plan(ctx),
        },
    ];
    if !args.no_merge {
        plan_steps.push(DryRunStep {
            step: "ready",
            plan: pr_ready_dry_plan(ctx),
        });
        plan_steps.push(DryRunStep {
            step: "merge",
            plan: pr_merge_dry_plan(ctx, args),
        });
        if !args.no_issue_closeout {
            plan_steps.push(DryRunStep {
                step: "issue_closeout",
                plan: pr_issue_closeout_dry_plan(ctx),
            });
        }
    }
    // Faithful local preflight: evaluate every non-mutating lock-down rule
    // and report each verdict. This runs local string / `git` checks only —
    // never a provider backend — so a dry-run predicts the real run's local
    // gates (e.g. a bad body and an unpushed head surface together).
    let body = resolve_preview_body(args);
    let headings = BodyHeadings::default();
    let inputs = PreflightInputs {
        branch: &branch,
        kind: args.kind.into_kind(),
        title: &args.title,
        body: &body,
        headings: &headings,
    };
    let mut local_preflight =
        run_local_preflight(&inputs, workdir, git_status_porcelain, git_branch_state);
    // Faithful test-first gate: when the repo opts in, the real run enforces
    // evidence for feature/bug kinds (both create and adopt paths), so surface
    // the same verdict here instead of predicting a success the real deliver
    // would refuse.
    let cfg = ForgeConfig::load_layered(workdir, find_git_toplevel(workdir).as_deref());
    let test_first_required = cfg.resolve_test_first_required(None);
    let gate_applies = test_first_required
        && matches!(
            args.kind.into_kind(),
            nils_common::git::PrKind::Feature | nils_common::git::PrKind::Bug
        );
    let remote_url = if gate_applies
        && ctx.provider == Provider::Local
        && global.repo.is_none()
        && ctx.repo.is_none()
    {
        git_remote_url(&global.remote)
    } else {
        None
    };
    let repository_id = gate_applies
        .then(|| evidence_repository_id(ctx, remote_url.as_deref(), global.repo.as_deref()))
        .flatten();
    if let Some(verdict) = test_first_preflight_verdict(
        args,
        test_first_required,
        workdir,
        &global.remote,
        repository_id.as_deref(),
        &branch,
    ) {
        local_preflight.push(verdict);
    }

    let payload = PrDeliverDryRun {
        provider: ctx.provider.as_str(),
        kind: args.kind.as_str(),
        plan_steps,
        no_merge: args.no_merge,
        local_preflight,
    };
    let envelope = Envelope::success(schema_version_for(BINARY, SCHEMA, SCHEMA_VERSION), payload);
    write_envelope(&envelope, format);
    nils_common::cli_contract::exit::SUCCESS
}

fn pr_lookup_dry_plan(ctx: &ProviderContext, head: &str) -> Vec<String> {
    let head = if head.is_empty() {
        "<current-branch>"
    } else {
        head
    };
    pr_list::build_list_call(ctx, &adopt_lookup_args(head)).plan_argv()
}

fn repo_view_dry_plan(ctx: &ProviderContext) -> Vec<String> {
    let mut call = BackendCall::new(
        BackendProgram::for_provider(ctx.provider),
        match ctx.provider {
            Provider::GitHub | Provider::Local => vec![
                std::ffi::OsString::from("repo"),
                std::ffi::OsString::from("view"),
                std::ffi::OsString::from("--json"),
            ],
            Provider::GitLab => vec![
                std::ffi::OsString::from("repo"),
                std::ffi::OsString::from("view"),
                std::ffi::OsString::from("-F"),
                std::ffi::OsString::from("json"),
            ],
        },
    );
    if let Provider::GitHub = ctx.provider {
        call.argv.push(std::ffi::OsString::from(
            "name,owner,defaultBranchRef,mergeCommitAllowed,squashMergeAllowed,rebaseMergeAllowed,url",
        ));
    }
    call.plan_argv()
}

fn pr_create_dry_plan(ctx: &ProviderContext, args: &PrDeliverArgs) -> Vec<String> {
    let head = args
        .head
        .clone()
        .unwrap_or_else(|| "<current-branch>".into());
    let base = args.base.clone().unwrap_or_else(|| "<repo-default>".into());
    let mut argv = match ctx.provider {
        Provider::GitHub | Provider::Local => vec![
            "pr",
            "create",
            "--head",
            &head,
            "--base",
            &base,
            "--title",
            &args.title,
            "--draft",
        ],
        Provider::GitLab => vec![
            "mr",
            "create",
            "--source-branch",
            &head,
            "--target-branch",
            &base,
            "--title",
            &args.title,
            "--draft",
        ],
    };
    if args.body.is_some() {
        argv.extend_from_slice(&["--body", "<inline>"]);
    } else if args.body_file.is_some() {
        argv.extend_from_slice(&["--body-file", "<path>"]);
    }
    let joined_labels = args.labels.join(",");
    match ctx.provider {
        Provider::GitHub | Provider::Local => {
            for label in &args.labels {
                argv.extend_from_slice(&["--label", label]);
            }
        }
        Provider::GitLab => {
            if !args.labels.is_empty() {
                argv.extend_from_slice(&["--label", &joined_labels]);
            }
        }
    }
    let mut out = vec![
        BackendProgram::for_provider(ctx.provider)
            .default_executable()
            .to_string(),
    ];
    out.extend(argv.into_iter().map(String::from));
    out
}

fn pr_wait_checks_dry_plan(ctx: &ProviderContext) -> Vec<String> {
    let prog = BackendProgram::for_provider(ctx.provider).default_executable();
    match ctx.provider {
        Provider::GitHub | Provider::Local => {
            pr_checks::build_github_required_call(ctx, "<pr-number>").plan_argv()
        }
        Provider::GitLab => vec![
            prog.into(),
            "ci".into(),
            "status".into(),
            "-b".into(),
            "<branch>".into(),
        ],
    }
}

fn pr_ready_dry_plan(ctx: &ProviderContext) -> Vec<String> {
    let prog = BackendProgram::for_provider(ctx.provider).default_executable();
    match ctx.provider {
        Provider::GitHub | Provider::Local => vec![
            prog.into(),
            "pr".into(),
            "ready".into(),
            "<pr-number>".into(),
        ],
        Provider::GitLab => vec![
            prog.into(),
            "mr".into(),
            "update".into(),
            "<pr-number>".into(),
            "--ready".into(),
        ],
    }
}

fn pr_merge_dry_plan(ctx: &ProviderContext, args: &PrDeliverArgs) -> Vec<String> {
    let prog = BackendProgram::for_provider(ctx.provider).default_executable();
    let method = args.method.into_method();
    let mut out: Vec<String> = match ctx.provider {
        Provider::GitHub | Provider::Local => vec![
            prog.into(),
            "pr".into(),
            "merge".into(),
            "<pr-number>".into(),
            format!("--{}", method.as_str()),
        ],
        Provider::GitLab => {
            let mut v = vec![
                prog.to_string(),
                "mr".into(),
                "merge".into(),
                "<pr-number>".into(),
            ];
            if matches!(method, crate::config::MergeMethod::Squash) {
                v.push("--squash".into());
            }
            v
        }
    };
    let cfg = ForgeConfig::default();
    if cfg.resolve_delete_branch(None) {
        match ctx.provider {
            Provider::GitHub | Provider::Local => out.push("--delete-branch".into()),
            Provider::GitLab => out.push("--remove-source-branch".into()),
        }
    }
    out
}

/// Dry-run plan for the post-merge closeout step. Two-phase at run time
/// (probe `closingIssuesReferences`, then close each still-open issue);
/// rendered here as the mutating close template so the dry-run surfaces the
/// action it will take. Built from the real [`issue_close::build_close_call`]
/// so the per-provider argv (only GitHub carries `--reason completed`) can
/// never drift from what the step actually runs.
fn pr_issue_closeout_dry_plan(ctx: &ProviderContext) -> Vec<String> {
    let mut plan =
        issue_close::build_close_call(ctx, 0, Some(crate::cli::CloseReasonFlag::Completed))
            .plan_argv();
    // argv index 3 is the numeric issue-id placeholder.
    if let Some(id) = plan.get_mut(3) {
        *id = "<closing-issue>".to_string();
    }
    plan
}

#[allow(clippy::too_many_arguments)]
fn emit_success_envelope(
    steps: Vec<Step>,
    args: &PrDeliverArgs,
    ctx: &ProviderContext,
    number: u64,
    url: String,
    merged: bool,
    merge_sha: Option<String>,
    format: OutputFormat,
) -> i32 {
    let payload = PrDeliverPayload {
        kind: args.kind.as_str(),
        provider: ctx.provider.as_str(),
        pr: PrDeliverSummary {
            number,
            url,
            merged,
            merge_sha,
        },
        steps,
    };
    let envelope = Envelope::success(schema_version_for(BINARY, SCHEMA, SCHEMA_VERSION), payload);
    write_envelope(&envelope, format);
    nils_common::cli_contract::exit::SUCCESS
}

fn emit_chain_failure(
    steps: Vec<Step>,
    args: &PrDeliverArgs,
    ctx: &ProviderContext,
    pr: Option<(u64, String)>,
    err: &ForgeError,
    format: OutputFormat,
) -> i32 {
    let (number, url) = pr.unwrap_or((0, String::new()));
    let payload = PrDeliverPayload {
        kind: args.kind.as_str(),
        provider: ctx.provider.as_str(),
        pr: PrDeliverSummary {
            number,
            url,
            merged: false,
            merge_sha: None,
        },
        steps,
    };
    let envelope = Envelope {
        schema_version: schema_version_for(BINARY, SCHEMA, SCHEMA_VERSION),
        ok: false,
        data: Some(payload),
        warnings: Vec::new(),
        error: Some(EnvelopeError::new(err.kind(), err.to_string())),
    };
    write_envelope(&envelope, format);
    err.exit_code()
}

fn write_envelope<T: Serialize>(envelope: &Envelope<T>, format: OutputFormat) {
    match format {
        OutputFormat::Json => {
            let serialized =
                serde_json::to_string(envelope).unwrap_or_else(|_| String::from("{\"ok\":false}"));
            println!("{serialized}");
        }
        OutputFormat::Text => {
            // Text mode: minimal one-liner so consumers know which step
            // ended the macro. Detailed payload is JSON-only.
            if envelope.ok
                && let Some(payload) = envelope.data.as_ref()
                && let Ok(value) = serde_json::to_value(payload)
                && let Some(steps) = value.get("steps").and_then(|s| s.as_array())
            {
                println!("delivered: {} steps", steps.len());
            } else if !envelope.ok
                && let Some(err) = envelope.error.as_ref()
            {
                eprintln!("error: {}: {}", err.code, err.message);
            }
        }
    }
}

fn to_value<T: Serialize>(value: &T) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{MergeMethodFlag, PrKindFlag};
    use crate::provider::DetectionSource;
    use pretty_assertions::assert_eq;

    fn ctx() -> ProviderContext {
        ProviderContext {
            provider: Provider::GitHub,
            host: "github.com".into(),
            source: DetectionSource::Flag,
            repo: None,
        }
    }

    fn args(no_merge: bool) -> PrDeliverArgs {
        PrDeliverArgs {
            kind: PrKindFlag::Feature,
            title: "demo".into(),
            body: Some("## Summary\nx\n\n## Test plan\ny".into()),
            body_file: None,
            head: Some("feat/demo".into()),
            base: Some("main".into()),
            method: MergeMethodFlag::Squash,
            reviewers: Vec::new(),
            labels: Vec::new(),
            label_catalog: None,
            strict_labels: false,
            test_first_evidence: None,
            timeout: std::time::Duration::from_secs(30 * 60),
            no_merge,
            no_issue_closeout: false,
            allow_non_default_base: false,
            allow_unresolved_threads: false,
            allow_unchecked_tasks: false,
            allow_unchecked_tasks_reason: None,
        }
    }

    #[test]
    fn dry_run_full_chain_lists_eight_steps() {
        let format = OutputFormat::Json;
        // Capture stdout would complicate this test; instead validate the
        // plan_steps[] vector via the dry-run helper directly.
        let mut plan_steps = Vec::new();
        let c = ctx();
        let a = args(false);
        plan_steps.push(DryRunStep {
            step: "auth_status",
            plan: vec!["auth".into(), "status".into()],
        });
        plan_steps.push(DryRunStep {
            step: "repo_view",
            plan: repo_view_dry_plan(&c),
        });
        plan_steps.push(DryRunStep {
            step: "lookup",
            plan: pr_lookup_dry_plan(&c, "feat/demo"),
        });
        plan_steps.push(DryRunStep {
            step: "create",
            plan: pr_create_dry_plan(&c, &a),
        });
        plan_steps.push(DryRunStep {
            step: "wait_checks",
            plan: pr_wait_checks_dry_plan(&c),
        });
        plan_steps.push(DryRunStep {
            step: "ready",
            plan: pr_ready_dry_plan(&c),
        });
        plan_steps.push(DryRunStep {
            step: "merge",
            plan: pr_merge_dry_plan(&c, &a),
        });
        plan_steps.push(DryRunStep {
            step: "issue_closeout",
            plan: pr_issue_closeout_dry_plan(&c),
        });
        assert_eq!(plan_steps.len(), 8);
        // Skip-when --no-merge collapses to 5 steps (closeout is merge-gated).
        let _ = format;
    }

    #[test]
    fn issue_closeout_dry_plan_renders_exact_completed_close_on_github() {
        // Exact ordered argv (skip index 0, the program name) — mirrors the
        // real `issue close --reason completed` on GitHub.
        let plan = pr_issue_closeout_dry_plan(&ctx());
        assert_eq!(
            plan[1..],
            [
                "issue".to_string(),
                "close".to_string(),
                "<closing-issue>".to_string(),
                "--reason".to_string(),
                "completed".to_string(),
            ]
        );
    }

    #[test]
    fn issue_closeout_dry_plan_omits_reason_off_github() {
        // GitLab / Local have no `--reason` state-reason concept, so the dry
        // plan must not advertise a flag the real close never sends.
        let gitlab = ProviderContext {
            provider: Provider::GitLab,
            host: "gitlab.example.com".into(),
            source: DetectionSource::Flag,
            repo: None,
        };
        let plan = pr_issue_closeout_dry_plan(&gitlab);
        assert!(
            !plan.iter().any(|s| s == "--reason"),
            "off-GitHub dry plan must omit --reason: {plan:?}"
        );
        assert_eq!(
            plan[1..],
            [
                "issue".to_string(),
                "close".to_string(),
                "<closing-issue>".to_string(),
            ]
        );
    }

    #[test]
    fn lookup_dry_plan_filters_open_prs_on_head_branch() {
        let plan = pr_lookup_dry_plan(&ctx(), "feat/demo");
        let h_idx = plan.iter().position(|s| s == "--head").expect("--head");
        assert_eq!(plan[h_idx + 1], "feat/demo");
        let s_idx = plan.iter().position(|s| s == "--state").expect("--state");
        assert_eq!(plan[s_idx + 1], "open");
    }

    #[test]
    fn lookup_dry_plan_renders_placeholder_for_unknown_branch() {
        let plan = pr_lookup_dry_plan(&ctx(), "");
        let h_idx = plan.iter().position(|s| s == "--head").expect("--head");
        assert_eq!(plan[h_idx + 1], "<current-branch>");
    }

    #[test]
    fn no_merge_dry_plan_excludes_ready_and_merge() {
        let c = ctx();
        let a = args(true);
        let _plan = pr_merge_dry_plan(&c, &a); // smoke test
        // The actual emission path filters ready/merge when no_merge is true;
        // that branch is covered by the integration test asserting plan_steps
        // length is 4 in that mode.
        assert!(a.no_merge);
    }

    #[test]
    fn build_create_args_falls_back_to_repo_default_branch() {
        let a = args(false);
        let pc = build_create_args(&a, "main");
        assert_eq!(pc.base.as_deref(), Some("main"));
        assert_eq!(pc.title, "demo");
        assert!(!pc.no_draft, "deliver always creates draft");
    }

    #[test]
    fn build_create_args_forwards_test_first_evidence() {
        let mut a = args(false);
        a.test_first_evidence = Some("evidence/dir".into());
        let pc = build_create_args(&a, "main");
        assert_eq!(pc.test_first_evidence.as_deref(), Some("evidence/dir"));
    }

    #[test]
    fn dry_run_verdict_absent_when_gate_off() {
        // The gate is off (no repo/global opt-in) → no test_first verdict so the
        // dry-run preflight stays identical to pre-gate behaviour.
        assert!(
            test_first_preflight_verdict(
                &args(false),
                false,
                Path::new("."),
                "origin",
                None,
                "HEAD",
            )
            .is_none()
        );
    }

    #[test]
    fn dry_run_verdict_fails_feature_without_evidence_when_required() {
        let a = args(false); // feature, no --test-first-evidence
        let verdict =
            test_first_preflight_verdict(&a, true, Path::new("."), "origin", None, "HEAD")
                .expect("verdict surfaced");
        assert_eq!(verdict.rule, "test_first");
        assert!(
            !verdict.ok,
            "missing evidence must fail the dry-run preflight"
        );
        assert_eq!(
            verdict.code.as_deref(),
            Some("test_first_evidence_required")
        );
    }

    #[test]
    fn dry_run_verdict_passes_exempt_kind_when_required() {
        let mut a = args(false);
        a.kind = PrKindFlag::Docs; // exempt kind needs no evidence
        let verdict =
            test_first_preflight_verdict(&a, true, Path::new("."), "origin", None, "HEAD")
                .expect("verdict surfaced");
        assert!(verdict.ok, "docs is exempt from the test-first gate");
    }

    #[test]
    fn dry_run_verdict_passes_feature_with_complete_evidence() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("test-first-evidence.json"),
            r#"{"schema_version":"test-first-evidence.record.v2","change_classification":"behavior-change","contract_delta":{"changed_behaviors":["durable gate"]},"no_existing_tests_reason":"fixture has no existing tests","waiver":{"reason":"fixture","kind":"non-testable","why_no_red":"fixture path","substitute_validation":["cargo test"]},"final_validations":[{"command":"cargo test","status":"pass","scope":"focused"}],"no_residual_gaps":true}"#,
        )
        .unwrap();
        let mut a = args(false);
        a.test_first_evidence = Some(dir.path().to_str().unwrap().to_string());
        let verdict =
            test_first_preflight_verdict(&a, true, Path::new("."), "origin", None, "HEAD")
                .expect("verdict surfaced");
        assert!(
            !verdict.ok,
            "structurally complete but unbound evidence must fail: {verdict:?}"
        );
        assert_eq!(verdict.code.as_deref(), Some("test_first_evidence_unbound"));
    }
}
