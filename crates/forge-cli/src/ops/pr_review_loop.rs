//! Durable provider-visible review-loop ledger operations.

use std::ffi::OsString;
use std::fs;

use nils_common::cli_contract::{OutputFormat, schema_version_for};
use serde::Serialize;

use crate::backend::{BackendCall, BackendProgram, BackendRunner};
use crate::cli::{
    BINARY, GlobalFlags, PrReviewLoopExtendArgs, PrReviewLoopInspectArgs, PrReviewLoopObserveArgs,
};
use crate::envelope::emit_success;
use crate::error::ForgeError;
use crate::ops::pr_comments::github_repo_slug_from_url;
use crate::ops::{pr_review, pr_view, review_state};
use crate::provider::{Provider, ProviderContext, detect, git_remote_url};
use crate::rate_limit::default_runner;
use crate::validations::RuleVerdict;

const SCHEMA_INSPECT: &str = "pr.review-loop.inspect";
const SCHEMA_OBSERVE: &str = "pr.review-loop.observe";
const SCHEMA_EXTEND: &str = "pr.review-loop.extend";
const SCHEMA_VERSION: u32 = 1;
const EXTENSION_MARKER_PREFIX: &str = "<!-- forge-cli:review-loop-extension:v1 ";

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PrReviewLoopPayload {
    pub provider: &'static str,
    pub number: u64,
    pub url: String,
    pub state_tip_digest: Option<String>,
    pub generation: Option<u64>,
    pub state: Option<review_state::ReviewLoopState>,
    pub appended: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ReviewLoopMergeGate {
    pub state_tip_digest: String,
    pub head_sha: String,
    pub round: u32,
    pub no_progress_rounds: u32,
    pub open_blocking_findings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct ReviewLoopDryRunPayload {
    provider: &'static str,
    plan: Vec<String>,
}

/// `observe --dry-run`'s envelope: the same `plan` as before, plus the verdict
/// of every read-only rule the real call enforces. `preflight` mirrors
/// `pr deliver --dry-run`'s `local_preflight[]` element shape; the name differs
/// because these rules include provider reads, never provider writes.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct ReviewLoopObserveDryRunPayload {
    provider: &'static str,
    plan: Vec<String>,
    preflight_ok: bool,
    /// Whether the real run would append a new generation. True for an accepted
    /// transition that changes state, and also for an extendable budget error,
    /// which appends a durable hard-stop receipt before failing. False when the
    /// chain is already current. `None` when the transition was not evaluated.
    #[serde(skip_serializing_if = "Option::is_none")]
    would_append: Option<bool>,
    preflight: Vec<RuleVerdict>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ApprovalComment {
    url: String,
    issue_url: String,
    author: String,
    association: String,
    created_at: String,
    body: String,
}

pub fn run_inspect(
    global: &GlobalFlags,
    args: PrReviewLoopInspectArgs,
    format: OutputFormat,
) -> Result<i32, ForgeError> {
    let runner = default_runner();
    run_inspect_with(&runner, global, args, format, git_remote_url)
}

pub fn run_inspect_with<R: BackendRunner, F: Fn(&str) -> Option<String>>(
    runner: &R,
    global: &GlobalFlags,
    args: PrReviewLoopInspectArgs,
    format: OutputFormat,
    remote_url_lookup: F,
) -> Result<i32, ForgeError> {
    let ctx = resolve_context(global, &remote_url_lookup)?;
    if global.dry_run {
        return emit_dry_run(
            &ctx,
            "read and validate the complete review-state chain",
            SCHEMA_INSPECT,
            format,
        );
    }
    let (view, repository) = resolve_pr(runner, &ctx, args.id)?;
    let state_view =
        pr_review::read_review_loop_state_view(runner, &ctx, &repository, view.number)?;
    emit_state(
        &ctx,
        view.number,
        view.url,
        state_view.chain,
        false,
        SCHEMA_INSPECT,
        format,
    )
}

pub fn run_observe(
    global: &GlobalFlags,
    args: PrReviewLoopObserveArgs,
    format: OutputFormat,
) -> Result<i32, ForgeError> {
    let runner = default_runner();
    run_observe_with(&runner, global, args, format, git_remote_url)
}

pub fn run_observe_with<R: BackendRunner, F: Fn(&str) -> Option<String>>(
    runner: &R,
    global: &GlobalFlags,
    args: PrReviewLoopObserveArgs,
    format: OutputFormat,
    remote_url_lookup: F,
) -> Result<i32, ForgeError> {
    let ctx = resolve_context(global, &remote_url_lookup)?;
    if global.dry_run {
        return emit_observe_dry_run(runner, &ctx, &args, format);
    }
    let observations = read_observations(&args.findings_file)?;
    let (view, repository) = resolve_pr(runner, &ctx, args.id)?;
    ensure_expected_head(view.head_sha.as_deref(), &args.expected_head)?;
    let state_view =
        pr_review::read_review_loop_state_view(runner, &ctx, &repository, view.number)?;
    ensure_expected_tip(
        state_view.chain.tip_digest.as_deref(),
        args.expected_state.as_deref(),
    )?;
    let previous = review_state::latest_review_loop_state(&state_view.chain);
    let transition =
        match review_state::observe_review_loop(previous, &args.expected_head, &observations) {
            Ok(transition) => transition,
            Err(error) if review_state::stop_budget_field(error.kind()).is_some() => {
                if let Some(stop) = previous
                    .and_then(|state| state.hard_stop.as_ref())
                    .filter(|stop| !stop.extension_applied)
                {
                    return Err(durable_hard_stop_error(
                        &error,
                        stop,
                        state_view.chain.tip_digest.as_deref(),
                    ));
                }
                let Some(previous) = previous else {
                    return Err(error);
                };
                let stopped = review_state::record_review_loop_hard_stop(
                    previous,
                    &args.expected_head,
                    &observations,
                    state_view.chain.tip_digest.as_deref().ok_or_else(|| {
                        ForgeError::validation(
                            schema_err(),
                            "review_state_conflict",
                            "a hard stop requires an existing review-loop chain tip",
                            None,
                        )
                    })?,
                    &error,
                )?;
                let stop = stopped
                    .hard_stop
                    .clone()
                    .expect("recorded hard stop exists");
                let chain = pr_review::append_review_loop_state(
                    runner,
                    &ctx,
                    &repository,
                    view.number,
                    &args.expected_head,
                    state_view.chain.tip_digest.as_deref(),
                    stopped,
                )?;
                return Err(durable_hard_stop_error(
                    &error,
                    &stop,
                    chain.tip_digest.as_deref(),
                ));
            }
            Err(error) => return Err(error),
        };

    if !transition.changed {
        return emit_state(
            &ctx,
            view.number,
            view.url,
            state_view.chain,
            false,
            SCHEMA_OBSERVE,
            format,
        );
    }
    let chain = pr_review::append_review_loop_state(
        runner,
        &ctx,
        &repository,
        view.number,
        &args.expected_head,
        state_view.chain.tip_digest.as_deref(),
        transition.state,
    )?;
    emit_state(
        &ctx,
        view.number,
        view.url,
        chain,
        true,
        SCHEMA_OBSERVE,
        format,
    )
}

pub fn run_extend(
    global: &GlobalFlags,
    args: PrReviewLoopExtendArgs,
    format: OutputFormat,
) -> Result<i32, ForgeError> {
    let runner = default_runner();
    run_extend_with(&runner, global, args, format, git_remote_url)
}

pub fn run_extend_with<R: BackendRunner, F: Fn(&str) -> Option<String>>(
    runner: &R,
    global: &GlobalFlags,
    args: PrReviewLoopExtendArgs,
    format: OutputFormat,
    remote_url_lookup: F,
) -> Result<i32, ForgeError> {
    let ctx = resolve_context(global, &remote_url_lookup)?;
    if global.dry_run {
        return emit_dry_run(
            &ctx,
            "verify one post-stop approval and append exactly one budget extension",
            SCHEMA_EXTEND,
            format,
        );
    }
    let (view, repository) = resolve_pr(runner, &ctx, args.id)?;
    ensure_expected_head(view.head_sha.as_deref(), &args.expected_head)?;
    let state_view =
        pr_review::read_review_loop_state_view(runner, &ctx, &repository, view.number)?;
    ensure_expected_tip(
        state_view.chain.tip_digest.as_deref(),
        Some(&args.expected_state),
    )?;
    let previous = review_state::latest_review_loop_state(&state_view.chain).ok_or_else(|| {
        ForgeError::validation(
            schema_err(),
            "review_extension_invalid",
            "a review-loop state must exist before extending its budget",
            None,
        )
    })?;
    let stopped_head_matches = previous
        .hard_stop
        .as_ref()
        .is_some_and(|stop| stop.attempted_head_sha == args.expected_head);
    if previous.head_sha != args.expected_head && !stopped_head_matches {
        return Err(ForgeError::validation(
            schema_err(),
            "review_state_conflict",
            "the review-loop state is bound to a different head",
            Some(format!(
                "state_head={}; expected_head={}",
                previous.head_sha, args.expected_head
            )),
        ));
    }
    let hard_stop = previous.hard_stop.as_ref().ok_or_else(|| {
        ForgeError::validation(
            schema_err(),
            "review_extension_invalid",
            "the current review-loop state has no durable hard stop to extend",
            None,
        )
    })?;
    if hard_stop.extension_applied
        || hard_stop.code != args.stop_code
        || hard_stop.budget_field != args.budget_field
        || hard_stop.increment != args.increment
        || hard_stop.proposal_digest != args.proposal_digest
        || review_state::stop_budget_field(&args.stop_code) != Some(args.budget_field.as_str())
    {
        return Err(ForgeError::validation(
            schema_err(),
            "review_extension_invalid",
            "the extension request does not match the current durable hard stop",
            Some(format!(
                "expected_code={}; expected_field={}; expected_increment={}; expected_proposal={}",
                hard_stop.code,
                hard_stop.budget_field,
                hard_stop.increment,
                hard_stop.proposal_digest
            )),
        ));
    }
    let approval = read_approval_comment(
        runner,
        &ctx,
        &repository,
        view.number,
        args.approval_comment,
    )?;
    validate_approval(
        &approval,
        state_view.tip_created_at.as_deref(),
        &args.proposal_digest,
        &args.budget_field,
        args.increment,
    )?;
    let extended = review_state::apply_review_loop_extension(
        previous,
        args.proposal_digest,
        approval.url,
        &args.stop_code,
        &args.budget_field,
        args.increment,
    )?;
    let chain = pr_review::append_review_loop_state(
        runner,
        &ctx,
        &repository,
        view.number,
        &args.expected_head,
        state_view.chain.tip_digest.as_deref(),
        extended,
    )?;
    emit_state(
        &ctx,
        view.number,
        view.url,
        chain,
        true,
        SCHEMA_EXTEND,
        format,
    )
}

pub fn ensure_merge_ready<R: BackendRunner>(
    runner: &R,
    ctx: &ProviderContext,
    pr_url: &str,
    number: u64,
    expected_head: &str,
    require_ledger: bool,
) -> Result<Option<ReviewLoopMergeGate>, ForgeError> {
    let repository = github_repo_slug_from_url(pr_url).ok_or_else(|| {
        ForgeError::software(
            schema_err(),
            "unable to derive GitHub owner/repo from pull-request URL",
            Some(format!("url={pr_url}")),
        )
    })?;
    let state_view = pr_review::read_review_loop_state_view(runner, ctx, &repository, number)?;
    let Some(state) = review_state::latest_review_loop_state(&state_view.chain) else {
        if require_ledger {
            return Err(ForgeError::validation(
                schema_err(),
                "review_state_conflict",
                "bounded review delivery requires an explicit genesis ledger observation",
                Some(format!("pr={number}; expected_head={expected_head}")),
            ));
        }
        return Ok(None);
    };
    if let Some(stop) = state.hard_stop.as_ref() {
        let error = durable_hard_stop_error(
            &ForgeError::validation(
                schema_err(),
                match stop.code.as_str() {
                    "review_round_limit_exceeded" => "review_round_limit_exceeded",
                    "review_no_progress" => "review_no_progress",
                    "review_finding_reopened" => "review_finding_reopened",
                    _ => "review_state_conflict",
                },
                "the durable review-loop hard stop blocks merge",
                None,
            ),
            stop,
            state_view.chain.tip_digest.as_deref(),
        );
        return Err(error);
    }
    if state.head_sha != expected_head {
        return Err(ForgeError::validation(
            schema_err(),
            "review_state_conflict",
            "the latest review-loop observation is not bound to the merge head",
            Some(format!(
                "merge_head={expected_head}; review_loop_head={}",
                state.head_sha
            )),
        ));
    }
    let open_blocking_findings = state
        .findings
        .iter()
        .filter(|(_, finding)| {
            finding.status == review_state::ReviewFindingStatus::Open && finding.blocking
        })
        .map(|(fingerprint, _)| fingerprint.clone())
        .collect::<Vec<_>>();
    if !open_blocking_findings.is_empty() {
        return Err(ForgeError::validation(
            schema_err(),
            "review_findings_open",
            "the durable review-loop ledger still contains blocking open findings",
            Some(format!("findings={}", open_blocking_findings.join(","))),
        ));
    }
    Ok(Some(ReviewLoopMergeGate {
        state_tip_digest: state_view.chain.tip_digest.clone().ok_or_else(|| {
            ForgeError::validation(
                schema_err(),
                "review_state_conflict",
                "review-loop state exists without a chain tip",
                None,
            )
        })?,
        head_sha: state.head_sha.clone(),
        round: state.round,
        no_progress_rounds: state.no_progress_rounds,
        open_blocking_findings,
    }))
}

pub fn recheck_merge_ready<R: BackendRunner>(
    runner: &R,
    ctx: &ProviderContext,
    pr_url: &str,
    number: u64,
    expected_head: &str,
    previous: &ReviewLoopMergeGate,
) -> Result<ReviewLoopMergeGate, ForgeError> {
    let current = ensure_merge_ready(runner, ctx, pr_url, number, expected_head, true)?
        .ok_or_else(|| {
            ForgeError::validation(
                schema_err(),
                "review_state_conflict",
                "the review-loop state disappeared before merge",
                None,
            )
        })?;
    if current.state_tip_digest != previous.state_tip_digest {
        return Err(ForgeError::validation(
            schema_err(),
            "review_state_conflict",
            "the review-loop state changed after merge gates and before provider merge",
            Some(format!(
                "expected_tip={}; provider_tip={}",
                previous.state_tip_digest, current.state_tip_digest
            )),
        ));
    }
    Ok(current)
}

fn resolve_context<F: Fn(&str) -> Option<String>>(
    global: &GlobalFlags,
    remote_url_lookup: &F,
) -> Result<ProviderContext, ForgeError> {
    let ctx = detect(
        global.provider_hint(),
        &global.remote,
        global.repo.as_deref(),
        remote_url_lookup,
    )?;
    if ctx.provider != Provider::GitHub {
        return Err(ForgeError::provider_unsupported(
            schema_err(),
            format!(
                "pr review-loop is GitHub-only in v1 (provider: {})",
                ctx.provider.as_str()
            ),
            None,
        ));
    }
    Ok(ctx)
}

fn resolve_pr<R: BackendRunner>(
    runner: &R,
    ctx: &ProviderContext,
    id: u64,
) -> Result<(pr_view::PrViewPayload, String), ForgeError> {
    let view = pr_view::compute(runner, ctx, id)?;
    let repository = github_repo_slug_from_url(&view.url).ok_or_else(|| {
        ForgeError::software(
            schema_err(),
            "unable to derive GitHub owner/repo from pull-request URL",
            Some(format!("url={}", view.url)),
        )
    })?;
    Ok((view, repository))
}

fn ensure_expected_head(provider_head: Option<&str>, expected: &str) -> Result<(), ForgeError> {
    if provider_head == Some(expected) {
        return Ok(());
    }
    Err(ForgeError::validation(
        schema_err(),
        "review_state_conflict",
        "the provider pull-request head differs from --expected-head",
        Some(format!(
            "expected_head={expected}; provider_head={}",
            provider_head.unwrap_or("<missing>")
        )),
    ))
}

fn ensure_expected_tip(provider: Option<&str>, expected: Option<&str>) -> Result<(), ForgeError> {
    if provider == expected {
        return Ok(());
    }
    Err(ForgeError::validation(
        schema_err(),
        "review_state_conflict",
        "the provider review-state tip differs from --expected-state",
        Some(format!(
            "expected_state={}; provider_state={}",
            expected.unwrap_or("<genesis>"),
            provider.unwrap_or("<genesis>")
        )),
    ))
}

fn read_observations(
    path: &str,
) -> Result<Vec<review_state::ReviewFindingObservation>, ForgeError> {
    let body = fs::read_to_string(path).map_err(|error| {
        ForgeError::validation(
            schema_err(),
            "review_findings_invalid",
            "failed to read review finding observations",
            Some(format!("path={path}; error={error}")),
        )
    })?;
    let value: serde_json::Value = serde_json::from_str(&body).map_err(|error| {
        ForgeError::validation(
            schema_err(),
            "review_findings_invalid",
            "review finding observations are not valid JSON",
            Some(error.to_string()),
        )
    })?;
    let value = value.get("data").unwrap_or(&value);
    let rows = value
        .get("findings")
        .unwrap_or(value)
        .as_array()
        .ok_or_else(|| {
            ForgeError::validation(
                schema_err(),
                "review_findings_invalid",
                "findings input must be an array or a review-specialists merge envelope",
                None,
            )
        })?;
    rows.iter().map(observation_from_value).collect()
}

fn observation_from_value(
    value: &serde_json::Value,
) -> Result<review_state::ReviewFindingObservation, ForgeError> {
    let primary = value.get("primary").unwrap_or(value);
    let fingerprint = value
        .get("lifecycle_fingerprint")
        .or_else(|| value.get("fingerprint"))
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            ForgeError::validation(
                schema_err(),
                "review_fingerprint_required",
                "each review-loop observation requires a lifecycle fingerprint",
                None,
            )
        })?
        .to_string();
    let root_cause_fingerprint = primary
        .get("root_cause_fingerprint")
        .or_else(|| value.get("root_cause_fingerprint"))
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let blocking = value
        .get("blocking")
        .and_then(|value| value.as_bool())
        .unwrap_or_else(|| {
            primary.get("severity").and_then(|value| value.as_str()) != Some("info")
        });
    let status = value
        .get("disposition")
        .or_else(|| value.get("status"))
        .and_then(|value| value.as_str())
        .map(parse_finding_status)
        .transpose()?
        .unwrap_or(review_state::ReviewFindingStatus::Open);
    let threads = value
        .get("threads")
        .and_then(|value| value.as_array())
        .map(|threads| {
            threads
                .iter()
                .filter_map(|thread| thread.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    Ok(review_state::ReviewFindingObservation {
        fingerprint,
        root_cause_fingerprint,
        blocking,
        status,
        threads,
    })
}

fn parse_finding_status(value: &str) -> Result<review_state::ReviewFindingStatus, ForgeError> {
    match value {
        "open" => Ok(review_state::ReviewFindingStatus::Open),
        "fixed" => Ok(review_state::ReviewFindingStatus::Fixed),
        "accepted" => Ok(review_state::ReviewFindingStatus::Accepted),
        "preference" => Ok(review_state::ReviewFindingStatus::Preference),
        "follow-up" => Ok(review_state::ReviewFindingStatus::FollowUp),
        _ => Err(ForgeError::validation(
            schema_err(),
            "review_findings_invalid",
            "review finding disposition is not supported",
            Some(format!("status={value}")),
        )),
    }
}

fn durable_hard_stop_error(
    error: &ForgeError,
    stop: &review_state::ReviewLoopHardStop,
    stopped_tip: Option<&str>,
) -> ForgeError {
    let code = match error.kind() {
        "review_round_limit_exceeded" => "review_round_limit_exceeded",
        "review_no_progress" => "review_no_progress",
        "review_finding_reopened" => "review_finding_reopened",
        _ => "review_state_conflict",
    };
    ForgeError::validation(
        schema_err(),
        code,
        error.message(),
        Some(format!(
            "{}; stopped_state={}; attempted_head={}; observation_digest={}; proposal_digest={}; budget_field={}; increment={}; approval_marker={}",
            error.detail().unwrap_or(""),
            stopped_tip.unwrap_or("<missing>"),
            stop.attempted_head_sha,
            stop.observation_digest,
            stop.proposal_digest,
            stop.budget_field,
            stop.increment,
            extension_approval_marker(&stop.proposal_digest, &stop.budget_field, stop.increment)
        )),
    )
}

fn extension_approval_marker(proposal: &str, budget_field: &str, increment: u32) -> String {
    format!(
        "{EXTENSION_MARKER_PREFIX}proposal={proposal} field={budget_field} increment={increment} -->"
    )
}

fn read_approval_comment<R: BackendRunner>(
    runner: &R,
    ctx: &ProviderContext,
    repository: &str,
    pr_number: u64,
    comment_id: u64,
) -> Result<ApprovalComment, ForgeError> {
    let mut argv = vec![OsString::from("api")];
    ctx.push_github_api_hostname(&mut argv);
    argv.push(OsString::from(format!(
        "repos/{repository}/issues/comments/{comment_id}"
    )));
    let output = runner.run(&BackendCall::new(BackendProgram::Gh, argv))?;
    let value: serde_json::Value = serde_json::from_str(output.stdout.trim()).map_err(|error| {
        ForgeError::software(
            schema_err(),
            "provider approval comment response is invalid JSON",
            Some(error.to_string()),
        )
    })?;
    let required = |pointer: &str| {
        value
            .pointer(pointer)
            .and_then(|value| value.as_str())
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .ok_or_else(|| {
                ForgeError::validation(
                    schema_err(),
                    "review_extension_approval_invalid",
                    "provider approval comment is missing required metadata",
                    Some(format!("field={pointer}")),
                )
            })
    };
    let approval = ApprovalComment {
        url: required("/html_url")?,
        issue_url: required("/issue_url")?,
        author: required("/user/login")?,
        association: required("/author_association")?,
        created_at: required("/created_at")?,
        body: required("/body")?,
    };
    ensure_approval_targets_pr(&approval, repository, pr_number)?;
    Ok(approval)
}

fn ensure_approval_targets_pr(
    approval: &ApprovalComment,
    repository: &str,
    pr_number: u64,
) -> Result<(), ForgeError> {
    let expected_suffix = format!("/repos/{repository}/issues/{pr_number}");
    if !approval.issue_url.ends_with(&expected_suffix) {
        return Err(ForgeError::validation(
            schema_err(),
            "review_extension_approval_invalid",
            "the approval comment does not belong to the target pull request",
            Some(format!(
                "expected_issue_suffix={expected_suffix}; provider_issue_url={}",
                approval.issue_url
            )),
        ));
    }
    Ok(())
}

fn validate_approval(
    approval: &ApprovalComment,
    state_created_at: Option<&str>,
    proposal: &str,
    budget_field: &str,
    increment: u32,
) -> Result<(), ForgeError> {
    if !matches!(
        approval.association.as_str(),
        "OWNER" | "MEMBER" | "COLLABORATOR"
    ) {
        return Err(ForgeError::validation(
            schema_err(),
            "review_extension_approval_invalid",
            "the approval comment author is not a maintainer or collaborator",
            Some(format!(
                "author={}; association={}",
                approval.author, approval.association
            )),
        ));
    }
    let state_created_at = state_created_at.ok_or_else(|| {
        ForgeError::validation(
            schema_err(),
            "review_extension_approval_invalid",
            "the current state tip has no provider creation timestamp",
            None,
        )
    })?;
    let state_time = state_created_at
        .parse::<jiff::Timestamp>()
        .map_err(|error| {
            ForgeError::validation(
                schema_err(),
                "review_extension_approval_invalid",
                "the current state timestamp is invalid",
                Some(error.to_string()),
            )
        })?;
    let approval_time = approval
        .created_at
        .parse::<jiff::Timestamp>()
        .map_err(|error| {
            ForgeError::validation(
                schema_err(),
                "review_extension_approval_invalid",
                "the approval timestamp is invalid",
                Some(error.to_string()),
            )
        })?;
    if approval_time <= state_time {
        return Err(ForgeError::validation(
            schema_err(),
            "review_extension_approval_invalid",
            "the approval comment must be authored after the hard-stop state tip",
            Some(format!(
                "state_created_at={state_created_at}; approval_created_at={}",
                approval.created_at
            )),
        ));
    }
    let marker = extension_approval_marker(proposal, budget_field, increment);
    if !approval.body.lines().any(|line| line.trim() == marker) {
        return Err(ForgeError::validation(
            schema_err(),
            "review_extension_approval_invalid",
            "the approval comment does not contain the exact extension marker",
            Some(format!("required_marker={marker}")),
        ));
    }
    Ok(())
}

fn emit_state(
    ctx: &ProviderContext,
    number: u64,
    url: String,
    chain: review_state::ReviewStateChain,
    appended: bool,
    schema: &str,
    format: OutputFormat,
) -> Result<i32, ForgeError> {
    let generation = chain.records.last().map(|record| record.generation);
    let state = review_state::latest_review_loop_state(&chain).cloned();
    Ok(emit_success(
        schema_version_for(BINARY, schema, SCHEMA_VERSION),
        PrReviewLoopPayload {
            provider: ctx.provider.as_str(),
            number,
            url,
            state_tip_digest: chain.tip_digest,
            generation,
            state,
            appended,
        },
        format,
        |payload| {
            println!(
                "review loop #{number}: round={round} generation={generation} appended={appended}",
                number = payload.number,
                round = payload.state.as_ref().map(|state| state.round).unwrap_or(0),
                generation = payload.generation.unwrap_or(0),
                appended = payload.appended,
            );
        },
    ))
}

/// Run every read-only check the real `observe` would run, in order, without
/// short-circuiting and without appending anything.
///
/// Non-short-circuiting matters twice over. It reports every reason a real run
/// would be rejected in one pass instead of one per attempt, and it keeps the
/// local payload verdict available when the provider cannot be reached — which
/// is what makes `--dry-run` usable as a schema check. Discovering the
/// observation schema previously required a live `observe`, and a live
/// `observe` appends durable, provider-visible state on success.
fn emit_observe_dry_run<R: BackendRunner>(
    runner: &R,
    ctx: &ProviderContext,
    args: &PrReviewLoopObserveArgs,
    format: OutputFormat,
) -> Result<i32, ForgeError> {
    let mut verdicts: Vec<RuleVerdict> = Vec::with_capacity(5);

    // Local first, so it survives an unreachable provider.
    let observations = match read_observations(&args.findings_file) {
        Ok(observations) => {
            verdicts.push(RuleVerdict::from_result("findings_file", Ok(())));
            Some(observations)
        }
        Err(error) => {
            verdicts.push(RuleVerdict::from_result("findings_file", Err(error)));
            None
        }
    };

    let mut would_append = None;
    match resolve_pr(runner, ctx, args.id) {
        Ok((view, repository)) => {
            verdicts.push(RuleVerdict::from_result("provider_pull_request", Ok(())));
            verdicts.push(RuleVerdict::from_result(
                "expected_head",
                ensure_expected_head(view.head_sha.as_deref(), &args.expected_head),
            ));
            match pr_review::read_review_loop_state_view(runner, ctx, &repository, view.number) {
                Ok(state_view) => {
                    verdicts.push(RuleVerdict::from_result("review_state_chain", Ok(())));
                    verdicts.push(RuleVerdict::from_result(
                        "expected_state_tip",
                        ensure_expected_tip(
                            state_view.chain.tip_digest.as_deref(),
                            args.expected_state.as_deref(),
                        ),
                    ));
                    match &observations {
                        Some(observations) => {
                            let previous =
                                review_state::latest_review_loop_state(&state_view.chain);
                            let transition = review_state::observe_review_loop(
                                previous,
                                &args.expected_head,
                                observations,
                            );
                            match transition {
                                Ok(transition) => {
                                    would_append = Some(transition.changed);
                                    verdicts.push(RuleVerdict::from_result(
                                        "observation_transition",
                                        Ok(()),
                                    ));
                                }
                                Err(error) => {
                                    // An extendable budget error is the one
                                    // failure the real run still writes for: it
                                    // appends a durable hard-stop receipt so a
                                    // restart returns the same stop. Predicting
                                    // "this fails" without that would understate
                                    // what the real call does.
                                    if review_state::stop_budget_field(error.kind()).is_some() {
                                        would_append = Some(true);
                                    }
                                    verdicts.push(RuleVerdict::from_result(
                                        "observation_transition",
                                        Err(error),
                                    ));
                                }
                            }
                        }
                        None => verdicts.push(RuleVerdict::not_evaluated(
                            "observation_transition",
                            "the findings file could not be read",
                        )),
                    }
                }
                Err(error) => {
                    verdicts.push(RuleVerdict::from_result("review_state_chain", Err(error)));
                    verdicts.push(RuleVerdict::not_evaluated(
                        "expected_state_tip",
                        "the review-state chain could not be read",
                    ));
                    verdicts.push(RuleVerdict::not_evaluated(
                        "observation_transition",
                        "the review-state chain could not be read",
                    ));
                }
            }
        }
        Err(error) => {
            verdicts.push(RuleVerdict::from_result(
                "provider_pull_request",
                Err(error),
            ));
            for rule in ["expected_head", "review_state_chain", "expected_state_tip"] {
                verdicts.push(RuleVerdict::not_evaluated(
                    rule,
                    "the provider pull request could not be read",
                ));
            }
            verdicts.push(RuleVerdict::not_evaluated(
                "observation_transition",
                "the provider pull request could not be read",
            ));
        }
    }

    let preflight_ok = verdicts.iter().all(|verdict| verdict.ok);
    Ok(emit_success(
        schema_version_for(BINARY, SCHEMA_OBSERVE, SCHEMA_VERSION),
        ReviewLoopObserveDryRunPayload {
            provider: ctx.provider.as_str(),
            plan: vec![
                "read the chain, evaluate one observation, and append with tip/head CAS"
                    .to_string(),
            ],
            preflight_ok,
            would_append,
            preflight: verdicts,
        },
        format,
        |payload| {
            println!(
                "would {plan} (preflight_ok={ok})",
                plan = payload.plan.join(", then "),
                ok = payload.preflight_ok,
            );
            for verdict in &payload.preflight {
                let status = if verdict.ok { "ok" } else { "FAIL" };
                let detail = verdict.message.as_deref().unwrap_or("");
                println!("  {status} {rule} {detail}", rule = verdict.rule);
            }
        },
    ))
}

fn emit_dry_run(
    ctx: &ProviderContext,
    step: &str,
    schema: &str,
    format: OutputFormat,
) -> Result<i32, ForgeError> {
    Ok(emit_success(
        schema_version_for(BINARY, schema, SCHEMA_VERSION),
        ReviewLoopDryRunPayload {
            provider: ctx.provider.as_str(),
            plan: vec![step.to_string()],
        },
        format,
        |payload| println!("would {}", payload.plan.join(", then ")),
    ))
}

fn schema_err() -> String {
    schema_version_for(BINARY, "error", 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delivery_merge_envelope_maps_to_lifecycle_observations() {
        let value = serde_json::json!({
            "lifecycle_fingerprint": "correctness:review-loop:shared-root",
            "primary": {
                "severity": "high",
                "root_cause_fingerprint": "correctness:review-loop:shared-root"
            },
            "threads": ["PRRT_1"]
        });
        let observation = observation_from_value(&value).expect("observation");
        assert_eq!(
            observation.fingerprint,
            "correctness:review-loop:shared-root"
        );
        assert!(observation.blocking);
        assert_eq!(observation.threads, vec!["PRRT_1"]);
    }

    #[test]
    fn approval_requires_authority_time_and_exact_marker() {
        let proposal = "sha256:proposal";
        let approval = ApprovalComment {
            url: "https://github.com/acme/widgets/pull/7#issuecomment-9".into(),
            issue_url: "https://api.github.com/repos/acme/widgets/issues/7".into(),
            author: "maintainer".into(),
            association: "MEMBER".into(),
            created_at: "2026-07-20T12:00:01Z".into(),
            body: extension_approval_marker(proposal, "max_repair_rounds", 1),
        };
        validate_approval(
            &approval,
            Some("2026-07-20T12:00:00Z"),
            proposal,
            "max_repair_rounds",
            1,
        )
        .expect("valid approval");

        let mut stale = approval.clone();
        stale.created_at = "2026-07-20T11:59:59Z".into();
        assert_eq!(
            validate_approval(
                &stale,
                Some("2026-07-20T12:00:00Z"),
                proposal,
                "max_repair_rounds",
                1,
            )
            .expect_err("stale approval")
            .kind(),
            "review_extension_approval_invalid"
        );

        let mut wrong_pr = approval;
        wrong_pr.issue_url = "https://api.github.com/repos/acme/widgets/issues/8".into();
        assert_eq!(
            ensure_approval_targets_pr(&wrong_pr, "acme/widgets", 7)
                .expect_err("approval comment must belong to the target PR")
                .kind(),
            "review_extension_approval_invalid"
        );
    }
}
