//! `pr merge` atom — the heaviest single atom in the v1 surface.
//!
//! Spec / ops: `cli.forge-cli.pr.merge.v1`. Layers nine lock-down policy
//! rules on top of a backend invocation:
//!
//! | Rule                                | Triggered when                                              | Error kind                | Exit       |
//! | ----------------------------------- | ----------------------------------------------------------- | ------------------------- | ---------- |
//! | 4 — worktree_clean                  | local repo has uncommitted changes                          | `dirty_worktree`          | DATA 65    |
//! | 6 — default_branch_protected        | PR base ≠ repo default branch, `--allow-non-default-base=0` | `default_branch_protected`| DATA 65    |
//! | 7 — draft_merge_refused             | `pr view` returns `draft=true`                              | `draft_merge_refused`     | DATA 65    |
//! | 8 — required_checks_green (TTL=0)   | fresh `pr.checks --required-only` not all green             | `checks_pending`/`failed` | DATA / RT  |
//! | 9 — merge_method_supported          | resolved method not in `repo.view.merge_methods_allowed`    | `merge_method_unsupported`| DATA 65    |
//! | 10 — keep_branch_conflict           | `--keep-branch` set while `[merge].delete_branch=true`      | `keep_branch_conflict`    | DATA 65    |
//! | 12 — review_convergence             | enabled native-review policy has not converged              | review-specific kind       | DATA / UNAV |
//! | 13 — review_threads_resolved        | unresolved review threads, `--allow-unresolved-threads=0`   | `unresolved_review_threads`| DATA 65   |
//! | 14 — tasklist_complete              | unchecked task-list items, `--allow-unchecked-tasks=0`      | `unchecked_task_items`    | DATA 65    |
//!
//! Backend argv (per ops YAML):
//! - GitHub: `gh pr merge <id> --{method} [--delete-branch]`
//! - GitLab: `glab mr merge <id> [--squash] [--remove-source-branch]`
//!
//! Post-merge: re-fetches `pr.view` with `mergeCommit` / `merge_commit_sha`
//! included so the envelope can surface `data.merge_sha`.

use std::ffi::OsString;
use std::path::PathBuf;

use nils_common::cli_contract::{OutputFormat, schema_version_for};
use serde::Serialize;

use crate::backend::{BackendCall, BackendProgram, BackendRunner, BackendSuccess, DryRunPayload};
use crate::cli::{BINARY, GlobalFlags, PrMergeArgs};
use crate::config::{ForgeConfig, MergeMethod, ReviewConvergencePolicy};
use crate::envelope::emit_success;
use crate::error::ForgeError;
use crate::ops::gitlab_api;
use crate::ops::pr_review_threads;
use crate::ops::pr_tasks;
use crate::ops::pr_view;
use crate::ops::pr_wait_checks::{Clock, SystemClock};
use crate::ops::repo_view::{self, RepoViewPayload};
use crate::ops::required_check_gate::ensure_required_checks_green;
use crate::ops::review_convergence::{self, ReviewConvergenceSnapshot};
use crate::provider::{Provider, ProviderContext, detect, git_remote_url};
use crate::rate_limit::default_runner;
use crate::validations::{git_status_porcelain, worktree_clean};

pub const SCHEMA: &str = "pr.merge";
pub const SCHEMA_VERSION: u32 = 1;

/// Envelope payload for `cli.forge-cli.pr.merge.v1`.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PrMergePayload {
    pub provider: &'static str,
    pub number: u64,
    pub url: String,
    pub merge_sha: String,
    pub method: &'static str,
    pub deleted_branch: bool,
    pub base: String,
    pub head: String,
    /// Recorded `--allow-unchecked-tasks-reason` when the task-list gate
    /// (rule 14) was explicitly bypassed; absent otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unchecked_tasks_override_reason: Option<String>,
    /// Recorded `--allow-unresolved-threads-reason` when the unresolved-threads
    /// gate (rule 13) was explicitly bypassed; absent otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unresolved_threads_override_reason: Option<String>,
    /// Outdated unresolved review threads mechanically dispositioned `stale`
    /// at rule 13 (the anchored diff hunk changed) so they no longer block;
    /// recorded for auditability. Empty/absent when none were dispositioned.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub stale_thread_dispositions: Vec<pr_review_threads::StaleThreadDisposition>,
    /// Present only when the default-off review convergence policy resolves
    /// enabled for this invocation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review_convergence: Option<ReviewConvergenceSnapshot>,
}

pub fn run(
    global: &GlobalFlags,
    args: PrMergeArgs,
    format: OutputFormat,
) -> Result<i32, ForgeError> {
    let runner = default_runner();
    let workdir = std::env::current_dir().map_err(|e| {
        ForgeError::software(
            schema_err(),
            "could not resolve current dir",
            Some(e.to_string()),
        )
    })?;
    run_with(&runner, global, &args, format, &workdir, git_remote_url)
}

pub fn run_with<R: BackendRunner, F: Fn(&str) -> Option<String>>(
    runner: &R,
    global: &GlobalFlags,
    args: &PrMergeArgs,
    format: OutputFormat,
    workdir: &std::path::Path,
    remote_url_lookup: F,
) -> Result<i32, ForgeError> {
    let ctx = detect(
        global.provider_hint(),
        &global.remote,
        global.repo.as_deref(),
        remote_url_lookup,
    )?;

    // Load layered config (global ~/.config/forge-cli + per-repo
    // .forge-cli.toml) so the method default + delete_branch default flow from
    // either layer when no explicit flag is set. `repo_delete_branch` keeps the
    // repo-layer `delete_branch` value alone for the rule-10 conflict check.
    let (cfg, repo_delete_branch) = load_merge_config(workdir);
    let method = cfg.resolve_merge_method(args.method.map(|m| m.into_method()));
    let cfg_delete = cfg.resolve_delete_branch(None);
    // --keep-branch flips the implicit-true default off; explicit conflict
    // (rule 10) fires only when the user paired --keep-branch with an explicit
    // [merge].delete_branch = true config in the same repo.
    enforce_keep_branch_conflict(args.keep_branch, repo_delete_branch)?;
    let delete_branch = if args.keep_branch { false } else { cfg_delete };
    let policy = resolve_review_convergence_policy(&cfg, args.review_convergence)?;
    ensure_review_convergence_provider(ctx.provider, &policy)?;

    if global.dry_run {
        let call = build_dry_run_merge_call(&ctx, args.id, method, delete_branch);
        let payload = DryRunPayload::new(ctx.provider, &call).with_review_convergence(&policy);
        return Ok(emit_success(
            schema_version_for(BINARY, SCHEMA, SCHEMA_VERSION),
            payload,
            format,
            |p| println!("would run: {plan}", plan = p.plan.join(" ")),
        ));
    }

    let clock = SystemClock;
    let payload = run_lockdown_chain(
        runner,
        &clock,
        global,
        &ctx,
        args,
        workdir,
        ResolvedMergeSettings {
            method,
            delete_branch,
            review_policy: &policy,
        },
    )?;
    Ok(emit_success(
        schema_version_for(BINARY, SCHEMA, SCHEMA_VERSION),
        payload,
        format,
        render_text,
    ))
}

/// Macro-facing entry point: run the entire lock-down chain + merge +
/// post-merge fetch and return the typed payload without emitting an
/// envelope. The macro is responsible for surfacing the result through the
/// composite `data.steps[]` envelope.
pub fn compute<R: BackendRunner>(
    runner: &R,
    global: &GlobalFlags,
    args: &PrMergeArgs,
    workdir: &std::path::Path,
) -> Result<PrMergePayload, ForgeError> {
    let clock = SystemClock;
    compute_with_clock(runner, &clock, global, args, workdir)
}

/// Clock-injected merge entry point used by `pr deliver` so its test clock
/// also drives the review quiet window deterministically.
pub fn compute_with_clock<R: BackendRunner, C: Clock>(
    runner: &R,
    clock: &C,
    global: &GlobalFlags,
    args: &PrMergeArgs,
    workdir: &std::path::Path,
) -> Result<PrMergePayload, ForgeError> {
    let ctx = detect(
        global.provider_hint(),
        &global.remote,
        global.repo.as_deref(),
        git_remote_url,
    )?;
    let (cfg, repo_delete_branch) = load_merge_config(workdir);
    let method = cfg.resolve_merge_method(args.method.map(|m| m.into_method()));
    let cfg_delete = cfg.resolve_delete_branch(None);
    enforce_keep_branch_conflict(args.keep_branch, repo_delete_branch)?;
    let delete_branch = if args.keep_branch { false } else { cfg_delete };
    let policy = resolve_review_convergence_policy(&cfg, args.review_convergence)?;
    ensure_review_convergence_provider(ctx.provider, &policy)?;
    run_lockdown_chain(
        runner,
        clock,
        global,
        &ctx,
        args,
        workdir,
        ResolvedMergeSettings {
            method,
            delete_branch,
            review_policy: &policy,
        },
    )
}

/// Load the layered merge config and, alongside the merged view, surface the
/// repo-layer `[merge].delete_branch` value on its own.
///
/// Rule 10 (`keep_branch_conflict`) must check only an explicit *repo*
/// `.forge-cli.toml` opt-in: an explicit `--keep-branch` outranks a global
/// default in the precedence chain (`explicit flag > repo > global > spec
/// default`), so a global `delete_branch = true` must not collide with it.
/// The merged `cfg` is still used for every other resolution (method, the
/// effective `delete_branch` default when `--keep-branch` is absent).
fn load_merge_config(workdir: &std::path::Path) -> (ForgeConfig, Option<bool>) {
    let repo = ForgeConfig::load_from(workdir, find_git_toplevel(workdir).as_deref());
    let repo_delete_branch = repo.merge_delete_branch;
    (
        ForgeConfig::load_global().overlaid_by(repo),
        repo_delete_branch,
    )
}

pub(crate) fn resolve_review_convergence_for_workdir(
    workdir: &std::path::Path,
    explicit_required: Option<bool>,
    provider: Provider,
) -> Result<crate::config::ReviewConvergencePolicy, ForgeError> {
    let (cfg, _) = load_merge_config(workdir);
    let policy = resolve_review_convergence_policy(&cfg, explicit_required)?;
    ensure_review_convergence_provider(provider, &policy)?;
    Ok(policy)
}

fn resolve_review_convergence_policy(
    cfg: &ForgeConfig,
    explicit_required: Option<bool>,
) -> Result<crate::config::ReviewConvergencePolicy, ForgeError> {
    let policy = cfg.resolve_review_convergence(explicit_required);
    ensure_review_convergence_config_valid(cfg, &policy)?;
    Ok(policy)
}

fn ensure_review_convergence_config_valid(
    cfg: &ForgeConfig,
    policy: &crate::config::ReviewConvergencePolicy,
) -> Result<(), ForgeError> {
    let invalid = cfg.invalid_review_convergence_warnings();
    if !policy.require || invalid.is_empty() {
        return Ok(());
    }
    Err(ForgeError::validation(
        schema_err(),
        "invalid_review_convergence_config",
        "enabled review convergence has invalid configuration",
        Some(invalid.join(",")),
    ))
}

fn ensure_review_convergence_provider(
    provider: Provider,
    policy: &ReviewConvergencePolicy,
) -> Result<(), ForgeError> {
    if !policy.require || matches!(provider, Provider::GitHub) {
        return Ok(());
    }
    Err(ForgeError::provider_unsupported(
        schema_err(),
        format!(
            "review convergence is GitHub-only in v1 (provider: {})",
            provider.as_str()
        ),
        None,
    ))
}

struct ResolvedMergeSettings<'a> {
    method: MergeMethod,
    delete_branch: bool,
    review_policy: &'a crate::config::ReviewConvergencePolicy,
}

fn run_lockdown_chain<R: BackendRunner, C: Clock>(
    runner: &R,
    clock: &C,
    global: &GlobalFlags,
    ctx: &ProviderContext,
    args: &PrMergeArgs,
    workdir: &std::path::Path,
    settings: ResolvedMergeSettings<'_>,
) -> Result<PrMergePayload, ForgeError> {
    // Rule 4 — clean worktree.
    worktree_clean(workdir, git_status_porcelain)?;

    // Rule 7 + base discovery — fetch pr.view once and reuse.
    let pr = fetch_pr_view(runner, ctx, args.id)?;
    if let Some(expected) = args.expected_head_sha.as_deref()
        && pr.head_sha.as_deref() != Some(expected)
    {
        return Err(ForgeError::validation(
            schema_err(),
            "test_first_evidence_provider_head_mismatch",
            "the provider PR/MR head changed after subject verification",
            Some(format!(
                "attested_head={expected} provider_head={}",
                pr.head_sha.as_deref().unwrap_or("<missing>")
            )),
        ));
    }
    let convergence_head = if settings.review_policy.require {
        Some(
            pr.head_sha
                .as_deref()
                .filter(|head| !head.is_empty())
                .ok_or_else(|| {
                    ForgeError::validation(
                        schema_err(),
                        "review_convergence_head_missing",
                        "enabled review convergence requires a non-empty initial provider head",
                        Some(format!("pr={}; url={}", pr.number, pr.url)),
                    )
                })?,
        )
    } else {
        pr.head_sha.as_deref()
    };
    if pr.draft {
        return Err(ForgeError::validation(
            schema_err(),
            "draft_merge_refused",
            "PR is still a draft; mark ready before merging",
            None,
        ));
    }

    // Rule 6 — default branch protection (unless explicitly overridden).
    let repo = fetch_repo_view(runner, ctx)?;
    if !args.allow_non_default_base && pr.base != repo.default_branch {
        return Err(ForgeError::validation(
            schema_err(),
            "default_branch_protected",
            format!(
                "PR base {base:?} differs from repo default {default:?}; pass --allow-non-default-base to override",
                base = pr.base,
                default = repo.default_branch,
            ),
            None,
        ));
    }

    // Rule 9 — method must be in the repo's allowed list.
    enforce_method_supported(settings.method, &repo)?;

    // Rule 8 — TTL-zero required-check re-check.
    ensure_required_checks_green(runner, global, ctx, &args.id.to_string())?;

    // Rule 12 — optional native-review convergence. The first v1 mode is
    // absence-tolerant `observed`: configured bots do not become required, but
    // any current-head activity that already exists must settle for the quiet
    // window and native CHANGES_REQUESTED blocks mechanically.
    let mut review_snapshot = if settings.review_policy.require {
        Some(review_convergence::converge(
            runner,
            clock,
            ctx,
            args.id,
            &pr.url,
            convergence_head,
            settings.review_policy,
        )?)
    } else {
        None
    };

    // Rule 13 — unresolved review threads. Outdated threads (their anchored
    // diff hunk changed) are mechanically dispositioned `stale` and recorded
    // rather than blocking; only non-outdated unresolved threads block. One
    // thread read serves the gate, the recorded stale dispositions, and (when
    // convergence is enabled) the structured convergence snapshot.
    // Read threads only when the gate or the convergence snapshot consumes the
    // result. When the bypass is set and convergence is off, skip the read
    // entirely so a transient thread-read failure cannot block a merge the
    // caller explicitly bypassed the thread gate for (preserving the
    // pre-change escape-hatch contract).
    let mut stale_thread_dispositions = Vec::new();
    if review_snapshot.is_some() || !args.allow_unresolved_threads {
        let thread_payload = pr_review_threads::compute_for_pr(runner, ctx, &pr.url, args.id)?;
        if let Some(snapshot) = review_snapshot.as_mut() {
            snapshot.unresolved_threads = thread_payload.unresolved;
        }
        stale_thread_dispositions = pr_review_threads::stale_dispositions(&thread_payload);
        if !args.allow_unresolved_threads {
            pr_review_threads::ensure_payload_resolved(&thread_payload)?;
        }
    }

    // Rule 14 — unchecked task-list items in the description block the
    // merge. The description is the delivery contract: every `- [ ]` must be
    // checked off or rewritten as dispositioned before merge, unless
    // explicitly bypassed with a recorded reason.
    if !args.allow_unchecked_tasks {
        pr_tasks::ensure_tasklist_complete(&pr.body)?;
    }

    // Close the thread/task TOCTOU window with one final complete native-review
    // read. Provider-side branch protection remains the atomic enforcement
    // layer for activity racing this final read and the merge mutation.
    if let Some(previous) = review_snapshot.as_ref() {
        let unresolved_threads = previous.unresolved_threads;
        let mut final_snapshot = review_convergence::recheck_before_merge(
            runner,
            ctx,
            args.id,
            &pr.url,
            convergence_head,
            settings.review_policy,
            previous,
        )?;
        final_snapshot.unresolved_threads = unresolved_threads;
        review_snapshot = Some(final_snapshot);
    }

    // All gates clear — invoke the backend.
    let merge_call =
        build_live_merge_call(ctx, args.id, &pr, settings.method, settings.delete_branch)?;
    invoke_merge_with_idempotency_check(runner, ctx, args.id, &merge_call, pr.head_sha.as_deref())?;

    // Post-merge re-fetch for merge_sha.
    let merge_sha = fetch_merge_sha(runner, ctx, args.id)?;

    Ok(PrMergePayload {
        provider: ctx.provider.as_str(),
        number: pr.number,
        url: pr.url,
        merge_sha,
        method: settings.method.as_str(),
        deleted_branch: settings.delete_branch,
        base: pr.base,
        head: pr.head,
        unchecked_tasks_override_reason: if args.allow_unchecked_tasks {
            args.allow_unchecked_tasks_reason.clone()
        } else {
            None
        },
        unresolved_threads_override_reason: if args.allow_unresolved_threads {
            args.allow_unresolved_threads_reason.clone()
        } else {
            None
        },
        stale_thread_dispositions,
        review_convergence: review_snapshot,
    })
}

/// Build the backend merge invocation.
pub fn build_merge_call(
    ctx: &ProviderContext,
    id: u64,
    method: MergeMethod,
    delete_branch: bool,
) -> BackendCall {
    let program = BackendProgram::for_provider(ctx.provider);
    let id_str = id.to_string();
    let mut argv: Vec<OsString> = match ctx.provider {
        Provider::GitHub | Provider::Local => {
            let mut v = vec![
                OsString::from("pr"),
                OsString::from("merge"),
                OsString::from(id_str),
                OsString::from(format!("--{}", method.as_str())),
            ];
            if delete_branch {
                v.push(OsString::from("--delete-branch"));
            }
            v
        }
        Provider::GitLab => {
            let mut v = vec![
                OsString::from("mr"),
                OsString::from("merge"),
                OsString::from(id_str),
            ];
            if matches!(method, MergeMethod::Squash) {
                v.push(OsString::from("--squash"));
            }
            // glab does not have a dedicated `--rebase` boolean on the merge
            // command in the pinned minor; rebase is achieved via repo-level
            // merge method setup. We still surface `merge_method_unsupported`
            // for unsupported choices at validation time (rule 9), so the
            // argv here only diverges between squash and the default merge.
            if delete_branch {
                v.push(OsString::from("--remove-source-branch"));
            }
            v
        }
    };
    ctx.push_repo_override(&mut argv);
    BackendCall::new(program, argv)
}

pub(crate) fn build_dry_run_merge_call(
    ctx: &ProviderContext,
    id: u64,
    method: MergeMethod,
    delete_branch: bool,
) -> BackendCall {
    if matches!(ctx.provider, Provider::GitLab)
        && let Some(project) = gitlab_api::project_path_from_ctx(ctx)
    {
        return build_gitlab_api_merge_call(&ctx.host, project, id, method, delete_branch, None);
    }
    build_merge_call(ctx, id, method, delete_branch)
}

fn build_live_merge_call(
    ctx: &ProviderContext,
    id: u64,
    pr: &PrView,
    method: MergeMethod,
    delete_branch: bool,
) -> Result<BackendCall, ForgeError> {
    if !matches!(ctx.provider, Provider::GitLab) {
        let mut call = build_merge_call(ctx, id, method, delete_branch);
        if let Some(sha) = pr.head_sha.as_deref().filter(|sha| !sha.is_empty()) {
            call.argv.push(OsString::from("--match-head-commit"));
            call.argv.push(OsString::from(sha));
        }
        return Ok(call);
    }
    let host = gitlab_api::host_from_url(&pr.url).unwrap_or_else(|| ctx.host.clone());
    let project = gitlab_api::project_path_from_mr_url(&pr.url)
        .or_else(|| gitlab_api::project_path_from_ctx(ctx).map(str::to_string))
        .ok_or_else(|| {
            ForgeError::software(
                schema_err(),
                "unable to derive GitLab project path for merge API",
                Some(format!("url={}", pr.url)),
            )
        })?;
    Ok(build_gitlab_api_merge_call(
        &host,
        &project,
        id,
        method,
        delete_branch,
        pr.head_sha.as_deref(),
    ))
}

fn build_gitlab_api_merge_call(
    host: &str,
    project: &str,
    id: u64,
    method: MergeMethod,
    delete_branch: bool,
    head_sha: Option<&str>,
) -> BackendCall {
    let encoded_project = gitlab_api::encode_project_path(project);
    let path = format!("projects/{encoded_project}/merge_requests/{id}/merge");
    let mut fields = vec![
        ("squash", matches!(method, MergeMethod::Squash).to_string()),
        ("should_remove_source_branch", delete_branch.to_string()),
    ];
    if let Some(sha) = head_sha.filter(|sha| !sha.is_empty()) {
        fields.push(("sha", sha.to_string()));
    }
    gitlab_api::api_call_with_method_fields(host, "PUT", path, &fields)
}

fn fetch_pr_view<R: BackendRunner>(
    runner: &R,
    ctx: &ProviderContext,
    id: u64,
) -> Result<PrView, ForgeError> {
    let call = pr_view_call(ctx, id);
    let output = runner.run(&call)?;
    let payload = pr_view::parse_view_output(ctx, &output)?;
    let head_sha = payload.head_sha.clone();
    let body = extract_body(ctx, &output);
    Ok(PrView {
        number: payload.number,
        url: payload.url,
        draft: payload.draft,
        base: payload.base,
        head: payload.head,
        state: payload.state.to_string(),
        head_sha,
        body,
    })
}

fn pr_view_call(ctx: &ProviderContext, id: u64) -> BackendCall {
    let program = BackendProgram::for_provider(ctx.provider);
    let id_str = id.to_string();
    let mut argv: Vec<OsString> = match ctx.provider {
        Provider::GitHub | Provider::Local => vec![
            OsString::from("pr"),
            OsString::from("view"),
            OsString::from(id_str),
            OsString::from("--json"),
            // Diverges from `pr_view::GH_JSON_FIELDS` on purpose: the merge
            // chain needs `body` for the rule-14 task-list gate and fetches
            // `mergeCommit` separately post-merge via `merge_sha_call`.
            OsString::from(pr_view::GH_JSON_FIELDS),
        ],
        Provider::GitLab => vec![
            OsString::from("mr"),
            OsString::from("view"),
            OsString::from(id_str),
            OsString::from("-F"),
            OsString::from("json"),
        ],
    };
    ctx.push_repo_override(&mut argv);
    BackendCall::new(program, argv)
}

fn fetch_repo_view<R: BackendRunner>(
    runner: &R,
    ctx: &ProviderContext,
) -> Result<RepoViewPayload, ForgeError> {
    let call = repo_view::build_call_for_default_branch(ctx);
    let output = runner.run(&call)?;
    repo_view::parse_backend_output(ctx, &output)
}

/// Run the backend merge call, then verify the PR state if the backend
/// exits non-zero. `gh` sometimes returns exit 1 after the actual merge
/// API call succeeds — typically when a post-merge branch cleanup races
/// the repo's `delete_branch_on_merge` setting, or when `gh` treats a
/// non-fatal stderr warning as a failure. Treat the call as success only
/// when GitHub / GitLab actually reports the PR as merged; otherwise
/// propagate the original [`ForgeError::BackendError`] so a real merge
/// failure stays loud.
fn invoke_merge_with_idempotency_check<R: BackendRunner>(
    runner: &R,
    ctx: &ProviderContext,
    pr_id: u64,
    merge_call: &BackendCall,
    expected_head_sha: Option<&str>,
) -> Result<(), ForgeError> {
    match runner.run(merge_call) {
        Ok(_) => Ok(()),
        Err(err) => {
            if !matches!(err, ForgeError::BackendError { .. }) {
                // Software / validation / unavailable errors short-circuit
                // without consulting the live PR state.
                return Err(err);
            }
            match fetch_pr_view(runner, ctx, pr_id) {
                Ok(post)
                    if post.state.eq_ignore_ascii_case("merged")
                        && expected_head_sha
                            .is_none_or(|expected| post.head_sha.as_deref() == Some(expected)) =>
                {
                    Ok(())
                }
                Ok(post) if post.state.eq_ignore_ascii_case("merged") => {
                    Err(ForgeError::validation(
                        schema_err(),
                        "test_first_evidence_provider_head_mismatch",
                        "the provider merged a different head after the guarded merge failed",
                        Some(format!(
                            "expected_head={} provider_head={}",
                            expected_head_sha.unwrap_or("<missing>"),
                            post.head_sha.as_deref().unwrap_or("<missing>")
                        )),
                    ))
                }
                _ => Err(err),
            }
        }
    }
}

fn fetch_merge_sha<R: BackendRunner>(
    runner: &R,
    ctx: &ProviderContext,
    id: u64,
) -> Result<String, ForgeError> {
    let call = merge_sha_call(ctx, id);
    let output = runner.run(&call)?;
    extract_merge_sha(ctx, &output)
}

fn merge_sha_call(ctx: &ProviderContext, id: u64) -> BackendCall {
    let program = BackendProgram::for_provider(ctx.provider);
    let id_str = id.to_string();
    let mut argv: Vec<OsString> = match ctx.provider {
        Provider::GitHub | Provider::Local => vec![
            OsString::from("pr"),
            OsString::from("view"),
            OsString::from(id_str),
            OsString::from("--json"),
            OsString::from("mergeCommit"),
        ],
        Provider::GitLab => vec![
            OsString::from("mr"),
            OsString::from("view"),
            OsString::from(id_str),
            OsString::from("-F"),
            OsString::from("json"),
        ],
    };
    ctx.push_repo_override(&mut argv);
    BackendCall::new(program, argv)
}

fn extract_merge_sha(ctx: &ProviderContext, output: &BackendSuccess) -> Result<String, ForgeError> {
    let value: serde_json::Value = serde_json::from_str(output.stdout.trim()).map_err(|e| {
        ForgeError::software(
            schema_err(),
            "post-merge view returned invalid JSON",
            Some(e.to_string()),
        )
    })?;
    let sha = match ctx.provider {
        Provider::GitHub | Provider::Local => value
            .get("mergeCommit")
            .and_then(|v| v.get("oid"))
            .and_then(|v| v.as_str())
            .map(str::to_string),
        Provider::GitLab => value
            .get("merge_commit_sha")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .filter(|s| !s.is_empty()),
    };
    sha.ok_or_else(|| {
        ForgeError::software(
            schema_err(),
            "merge succeeded but merge_sha was missing from post-merge view",
            Some(format!("stdout={:?}", output.stdout)),
        )
    })
}

/// Pull the PR/MR description out of the raw view output for the rule-14
/// task-list gate: `body` on GitHub, `description` on GitLab. Providers
/// without a body model (local) yield the empty string, which passes the
/// gate trivially.
fn extract_body(ctx: &ProviderContext, output: &BackendSuccess) -> String {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(output.stdout.trim()) else {
        return String::new();
    };
    let key = match ctx.provider {
        Provider::GitHub | Provider::Local => "body",
        Provider::GitLab => "description",
    };
    value
        .get(key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

fn enforce_keep_branch_conflict(
    keep_branch: bool,
    repo_delete_branch: Option<bool>,
) -> Result<(), ForgeError> {
    // Conflict surfaces when the user asks to keep the source branch AND the
    // *repo* `.forge-cli.toml` explicitly opts into branch deletion. The
    // implicit default (`delete_branch=true` when nothing is set) and a
    // lower-precedence global default are both silently overridden by
    // `--keep-branch`; only an explicit same-repo collision is an error.
    if keep_branch && matches!(repo_delete_branch, Some(true)) {
        return Err(ForgeError::validation(
            schema_err(),
            "keep_branch_conflict",
            "--keep-branch conflicts with [merge].delete_branch = true in .forge-cli.toml",
            None,
        ));
    }
    Ok(())
}

fn enforce_method_supported(method: MergeMethod, repo: &RepoViewPayload) -> Result<(), ForgeError> {
    if repo.merge_methods_allowed.is_empty() {
        // repo view returned no info — pass through; the backend will fail
        // loudly if the method really is unsupported.
        return Ok(());
    }
    let wanted = method.as_str();
    if repo.merge_methods_allowed.contains(&wanted) {
        Ok(())
    } else {
        Err(ForgeError::validation(
            schema_err(),
            "merge_method_unsupported",
            format!(
                "merge method {wanted:?} is not enabled on the repo (allowed: {allowed:?})",
                allowed = repo.merge_methods_allowed,
            ),
            None,
        ))
    }
}

fn find_git_toplevel(start: &std::path::Path) -> Option<PathBuf> {
    let mut cursor = Some(start.to_path_buf());
    while let Some(dir) = cursor {
        if dir.join(".git").exists() {
            return Some(dir);
        }
        cursor = dir.parent().map(std::path::Path::to_path_buf);
    }
    None
}

fn schema_err() -> String {
    schema_version_for(BINARY, "error", 1)
}

fn render_text(payload: &PrMergePayload) {
    println!(
        "merged #{number} via {method} → {sha}{deleted}\n  {url}",
        number = payload.number,
        method = payload.method,
        sha = payload.merge_sha,
        deleted = if payload.deleted_branch {
            " (branch deleted)"
        } else {
            ""
        },
        url = payload.url,
    );
}

/// Minimal post-pr.view projection used internally to wire base/draft into the
/// lock-down chain without re-deriving every field.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PrView {
    number: u64,
    url: String,
    draft: bool,
    base: String,
    head: String,
    state: String,
    head_sha: Option<String>,
    /// PR/MR description used by the rule-14 task-list gate; empty when the
    /// provider has no body model.
    body: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::DetectionSource;
    use pretty_assertions::assert_eq;

    fn ctx(provider: Provider) -> ProviderContext {
        ProviderContext {
            provider,
            host: "github.com".into(),
            source: DetectionSource::Flag,
            repo: None,
        }
    }

    fn repo_with(default: &str, methods: Vec<&'static str>) -> RepoViewPayload {
        RepoViewPayload {
            provider: "github",
            owner: "acme".into(),
            name: "widgets".into(),
            url: "https://github.com/acme/widgets".into(),
            default_branch: default.into(),
            merge_methods_allowed: methods,
        }
    }

    #[test]
    fn merge_call_gh_includes_method_flag_and_delete_branch() {
        let call = build_merge_call(&ctx(Provider::GitHub), 7, MergeMethod::Squash, true);
        let plan = call.plan_argv();
        assert_eq!(plan[1..3], ["pr".to_string(), "merge".to_string()]);
        assert!(plan.iter().any(|s| s == "--squash"));
        assert!(plan.iter().any(|s| s == "--delete-branch"));
    }

    #[test]
    fn merge_call_gh_drops_delete_branch_when_disabled() {
        let call = build_merge_call(&ctx(Provider::GitHub), 7, MergeMethod::Merge, false);
        let plan = call.plan_argv();
        assert!(plan.iter().any(|s| s == "--merge"));
        assert!(!plan.iter().any(|s| s == "--delete-branch"));
    }

    #[test]
    fn live_github_merge_uses_provider_head_as_compare_and_swap() {
        let pr = PrView {
            number: 7,
            url: "https://github.com/acme/widgets/pull/7".into(),
            draft: false,
            base: "main".into(),
            head: "feat/subject".into(),
            state: "open".into(),
            head_sha: Some("abc123".into()),
            body: String::new(),
        };
        let call =
            build_live_merge_call(&ctx(Provider::GitHub), 7, &pr, MergeMethod::Squash, false)
                .expect("merge call");
        let plan = call.plan_argv();
        let index = plan
            .iter()
            .position(|item| item == "--match-head-commit")
            .expect("CAS flag");
        assert_eq!(plan[index + 1], "abc123");
    }

    #[test]
    fn merge_call_glab_includes_squash_and_remove_branch() {
        let call = build_merge_call(&ctx(Provider::GitLab), 9, MergeMethod::Squash, true);
        let plan = call.plan_argv();
        assert_eq!(plan[1..3], ["mr".to_string(), "merge".to_string()]);
        assert!(plan.iter().any(|s| s == "--squash"));
        assert!(plan.iter().any(|s| s == "--remove-source-branch"));
    }

    #[test]
    fn gitlab_api_merge_call_uses_put_endpoint_and_head_sha() {
        let call = build_gitlab_api_merge_call(
            "gitlab.example.com",
            "group/sub/project",
            9,
            MergeMethod::Squash,
            true,
            Some("abc123"),
        );
        let plan = call.plan_argv();
        assert!(plan.iter().any(|s| s == "--method"));
        assert!(plan.iter().any(|s| s == "PUT"));
        assert!(
            plan.iter()
                .any(|s| s == "projects/group%2Fsub%2Fproject/merge_requests/9/merge")
        );
        assert!(plan.iter().any(|s| s == "squash=true"));
        assert!(plan.iter().any(|s| s == "should_remove_source_branch=true"));
        assert!(plan.iter().any(|s| s == "sha=abc123"));
    }

    #[test]
    fn merge_call_glab_merge_method_omits_squash_flag() {
        let call = build_merge_call(&ctx(Provider::GitLab), 9, MergeMethod::Merge, false);
        let plan = call.plan_argv();
        assert!(!plan.iter().any(|s| s == "--squash"));
        assert!(!plan.iter().any(|s| s == "--remove-source-branch"));
    }

    #[test]
    fn keep_branch_conflict_fires_only_on_explicit_config_collision() {
        // Implicit default (None) — no conflict.
        assert!(enforce_keep_branch_conflict(true, None).is_ok());
        // Explicit repo delete_branch = false — no conflict.
        assert!(enforce_keep_branch_conflict(true, Some(false)).is_ok());
        // Explicit repo delete_branch = true — conflict.
        let err = enforce_keep_branch_conflict(true, Some(true)).expect_err("must fail");
        assert_eq!(err.kind(), "keep_branch_conflict");
        assert_eq!(err.exit_code(), 65);
        // Without --keep-branch, no collision regardless of config.
        assert!(enforce_keep_branch_conflict(false, Some(true)).is_ok());
    }

    #[test]
    fn keep_branch_ignores_global_layer_delete_branch() {
        // Regression for the layered-config edge case: a *global*
        // `[merge] delete_branch = true` with no repo override must not collide
        // with an explicit `--keep-branch` (explicit flag > repo > global).
        let global = ForgeConfig {
            merge_delete_branch: Some(true),
            ..ForgeConfig::default()
        };
        let repo = ForgeConfig::default();
        let merged = global.overlaid_by(repo.clone());

        // The merged view carries the global default (so passing it to the
        // conflict check — the old bug — would wrongly error)...
        assert_eq!(merged.merge_delete_branch, Some(true));
        // ...but the conflict check sees only the repo layer, so no conflict.
        assert!(enforce_keep_branch_conflict(true, repo.merge_delete_branch).is_ok());
        // The merged default still drives the actual deletion when --keep-branch
        // is absent.
        assert!(merged.resolve_delete_branch(None));
    }

    #[test]
    fn method_supported_passes_through_when_repo_methods_unknown() {
        let repo = repo_with("main", vec![]);
        assert!(enforce_method_supported(MergeMethod::Rebase, &repo).is_ok());
    }

    #[test]
    fn method_supported_accepts_listed_method() {
        let repo = repo_with("main", vec!["squash", "merge"]);
        assert!(enforce_method_supported(MergeMethod::Squash, &repo).is_ok());
    }

    #[test]
    fn method_supported_rejects_unlisted_method_with_data_65() {
        let repo = repo_with("main", vec!["merge"]);
        let err = enforce_method_supported(MergeMethod::Rebase, &repo).expect_err("must fail");
        assert_eq!(err.kind(), "merge_method_unsupported");
        assert_eq!(err.exit_code(), 65);
    }

    #[test]
    fn extract_merge_sha_github_pulls_oid() {
        let output = BackendSuccess {
            stdout: r#"{"mergeCommit":{"oid":"abc123"}}"#.into(),
            stderr: String::new(),
        };
        assert_eq!(
            extract_merge_sha(&ctx(Provider::GitHub), &output).unwrap(),
            "abc123"
        );
    }

    #[test]
    fn extract_merge_sha_gitlab_pulls_merge_commit_sha() {
        let output = BackendSuccess {
            stdout: r#"{"merge_commit_sha":"def456"}"#.into(),
            stderr: String::new(),
        };
        assert_eq!(
            extract_merge_sha(&ctx(Provider::GitLab), &output).unwrap(),
            "def456"
        );
    }

    #[test]
    fn extract_merge_sha_errors_software_when_missing() {
        let output = BackendSuccess {
            stdout: r#"{}"#.into(),
            stderr: String::new(),
        };
        let err = extract_merge_sha(&ctx(Provider::GitHub), &output).expect_err("must fail");
        assert_eq!(err.kind(), "software_error");
    }

    #[test]
    fn extract_body_github_reads_body_field() {
        let output = BackendSuccess {
            stdout: r#"{"number":7,"body":"- [ ] item"}"#.into(),
            stderr: String::new(),
        };
        assert_eq!(extract_body(&ctx(Provider::GitHub), &output), "- [ ] item");
    }

    #[test]
    fn extract_body_gitlab_reads_description_field() {
        let output = BackendSuccess {
            stdout: r#"{"iid":9,"description":"- [x] item"}"#.into(),
            stderr: String::new(),
        };
        assert_eq!(extract_body(&ctx(Provider::GitLab), &output), "- [x] item");
    }

    #[test]
    fn extract_body_defaults_empty_on_missing_or_invalid() {
        let missing = BackendSuccess {
            stdout: r#"{"number":7}"#.into(),
            stderr: String::new(),
        };
        assert_eq!(extract_body(&ctx(Provider::GitHub), &missing), "");
        let invalid = BackendSuccess {
            stdout: "not json".into(),
            stderr: String::new(),
        };
        assert_eq!(extract_body(&ctx(Provider::GitHub), &invalid), "");
    }

    #[test]
    fn extract_merge_sha_errors_on_empty_gitlab_sha() {
        let output = BackendSuccess {
            stdout: r#"{"merge_commit_sha":""}"#.into(),
            stderr: String::new(),
        };
        let err = extract_merge_sha(&ctx(Provider::GitLab), &output).expect_err("must fail");
        assert_eq!(err.kind(), "software_error");
    }
}
