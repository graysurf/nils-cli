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
use crate::ops::{pr_comment, pr_review, pr_view, review_state};
use crate::provider::{Provider, ProviderContext, detect, git_remote_url};
use crate::rate_limit::default_runner;
use crate::validations::{RuleVerdict, no_escaped_control_markdown, no_local_path};

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
    /// Whether a supplied `--body` / `--body-file` delivery outcome was posted.
    /// Only `observe` can set this, and only together with an append: the outcome
    /// shares the appended comment, so `appended: false` always means the outcome
    /// was not posted (the ledger was already current).
    pub outcome_posted: bool,
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
    /// The exact provider comment the live append would post. Present only when
    /// the transition was evaluated and would append; absent when the chain is
    /// already current, because then nothing is written at all.
    #[serde(skip_serializing_if = "Option::is_none")]
    planned_comment: Option<PlannedStateComment>,
    preflight: Vec<RuleVerdict>,
}

/// The rendered shape of one planned ledger comment.
///
/// The rendered body is reported by size and visible label rather than verbatim:
/// a combined outcome body can approach the provider's 64 KiB comment limit, and
/// the caller already owns the outcome bytes it supplied.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct PlannedStateComment {
    /// The visible metadata line rendered above the machine marker.
    visible_metadata: String,
    /// Whether the supplied delivery outcome shares this one comment.
    includes_outcome_body: bool,
    /// Complete rendered body size, the value checked against the safe limit.
    bytes: usize,
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
    // Read and validate the outcome body before the first provider call, so an
    // unreadable path or a non-portable body cannot fail between the ledger read
    // and the append.
    let outcome_body = read_outcome_body(&args)?;
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
                // A hard stop is a failure receipt, never a delivery outcome, so
                // the outcome body is deliberately not attached here.
                let chain = pr_review::append_review_loop_state(
                    runner,
                    &ctx,
                    pr_review::ReviewLoopAppend {
                        repository: &repository,
                        number: view.number,
                        expected_head: &args.expected_head,
                        expected_tip: state_view.chain.tip_digest.as_deref(),
                        state: stopped,
                        visible_outcome: None,
                    },
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
        // The ledger is already current, so there is nothing to append and no
        // comment to carry an outcome. Posting the outcome on its own here would
        // reintroduce exactly the duplicate an identical retry must not create.
        return emit_state(
            &ctx,
            view.number,
            view.url,
            state_view.chain,
            false,
            false,
            SCHEMA_OBSERVE,
            format,
        );
    }
    let chain = pr_review::append_review_loop_state(
        runner,
        &ctx,
        pr_review::ReviewLoopAppend {
            repository: &repository,
            number: view.number,
            expected_head: &args.expected_head,
            expected_tip: state_view.chain.tip_digest.as_deref(),
            state: transition.state,
            visible_outcome: outcome_body.as_deref(),
        },
    )?;
    emit_state(
        &ctx,
        view.number,
        view.url,
        chain,
        true,
        outcome_body.is_some(),
        SCHEMA_OBSERVE,
        format,
    )
}

/// Reads and validates the optional visible delivery outcome body.
///
/// Returns `None` when neither flag was supplied. When one was, the body must be
/// present and portable: the same no-local-path and escaped-control rules the
/// review comment surfaces enforce apply, because this text becomes a permanent
/// provider-visible comment.
fn read_outcome_body(args: &PrReviewLoopObserveArgs) -> Result<Option<String>, ForgeError> {
    if args.body.is_none() && args.body_file.is_none() {
        return Ok(None);
    }
    let body = pr_comment::read_body_with_file_flag(
        args.body.as_deref(),
        args.body_file.as_deref(),
        "--body-file",
    )?;
    if body.trim().is_empty() {
        return Err(ForgeError::validation(
            schema_err(),
            "review_state_comment_invalid",
            "the review-loop outcome body is empty (supply --body or --body-file)",
            None,
        ));
    }
    no_local_path(&body, "review-loop outcome body")?;
    no_escaped_control_markdown(&body)?;
    Ok(Some(body))
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
        pr_review::ReviewLoopAppend {
            repository: &repository,
            number: view.number,
            expected_head: &args.expected_head,
            expected_tip: state_view.chain.tip_digest.as_deref(),
            state: extended,
            visible_outcome: None,
        },
    )?;
    emit_state(
        &ctx,
        view.number,
        view.url,
        chain,
        true,
        false,
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

#[allow(clippy::too_many_arguments)]
fn emit_state(
    ctx: &ProviderContext,
    number: u64,
    url: String,
    chain: review_state::ReviewStateChain,
    appended: bool,
    outcome_posted: bool,
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
            outcome_posted,
        },
        format,
        |payload| {
            println!(
                "review loop #{number}: round={round} generation={generation} appended={appended} outcome_posted={outcome_posted}",
                number = payload.number,
                round = payload.state.as_ref().map(|state| state.round).unwrap_or(0),
                generation = payload.generation.unwrap_or(0),
                appended = payload.appended,
                outcome_posted = payload.outcome_posted,
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
    let mut verdicts: Vec<RuleVerdict> = Vec::with_capacity(7);

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
    let outcome_body = match read_outcome_body(args) {
        Ok(body) => {
            if args.body.is_some() || args.body_file.is_some() {
                verdicts.push(RuleVerdict::from_result("outcome_body", Ok(())));
            }
            body
        }
        Err(error) => {
            verdicts.push(RuleVerdict::from_result("outcome_body", Err(error)));
            None
        }
    };

    let mut would_append = None;
    let mut planned_comment = None;
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
                            // `state_comment_body` is reported only when a write
                            // is actually planned. A current chain writes
                            // nothing, so there is no body to size-check and an
                            // unconditional verdict would wrongly fail the
                            // preflight of a legitimate no-op observation.
                            match transition {
                                Ok(transition) => {
                                    would_append = Some(transition.changed);
                                    verdicts.push(RuleVerdict::from_result(
                                        "observation_transition",
                                        Ok(()),
                                    ));
                                    if transition.changed {
                                        match plan_state_comment(
                                            &repository,
                                            view.number,
                                            &args.expected_head,
                                            &state_view.chain,
                                            transition.state,
                                            outcome_body.as_deref(),
                                        ) {
                                            Ok(planned) => {
                                                planned_comment = Some(planned);
                                                verdicts.push(RuleVerdict::from_result(
                                                    "state_comment_body",
                                                    Ok(()),
                                                ));
                                            }
                                            Err(error) => verdicts.push(RuleVerdict::from_result(
                                                "state_comment_body",
                                                Err(error),
                                            )),
                                        }
                                    }
                                }
                                Err(error) => {
                                    // An extendable budget error is the one
                                    // failure the real run still writes for: it
                                    // appends a durable hard-stop receipt so a
                                    // restart returns the same stop. Predicting
                                    // "this fails" without that would understate
                                    // what the real call does. That receipt never
                                    // carries the outcome body.
                                    if review_state::stop_budget_field(error.kind()).is_some() {
                                        would_append = Some(true);
                                        planned_comment = plan_hard_stop_comment(
                                            &repository,
                                            view.number,
                                            &args.expected_head,
                                            &state_view.chain,
                                            previous,
                                            observations,
                                            &error,
                                        );
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
    let combined = args.body.is_some() || args.body_file.is_some();
    Ok(emit_success(
        schema_version_for(BINARY, SCHEMA_OBSERVE, SCHEMA_VERSION),
        ReviewLoopObserveDryRunPayload {
            provider: ctx.provider.as_str(),
            plan: vec![
                match combined {
                    true => "read the chain, evaluate one observation, and append the ledger record together with the delivery outcome in one comment using tip/head CAS".to_string(),
                    false => "read the chain, evaluate one observation, and append with tip/head CAS".to_string(),
                },
            ],
            preflight_ok,
            would_append,
            planned_comment,
            preflight: verdicts,
        },
        format,
        |payload| {
            println!(
                "would {plan} (preflight_ok={ok})",
                plan = payload.plan.join(", then "),
                ok = payload.preflight_ok,
            );
            if let Some(planned) = payload.planned_comment.as_ref() {
                println!(
                    "  comment {bytes} bytes, outcome_body={outcome}: {metadata}",
                    bytes = planned.bytes,
                    outcome = planned.includes_outcome_body,
                    metadata = planned.visible_metadata,
                );
            }
            for verdict in &payload.preflight {
                let status = if verdict.ok { "ok" } else { "FAIL" };
                let detail = verdict.message.as_deref().unwrap_or("");
                println!("  {status} {rule} {detail}", rule = verdict.rule);
            }
        },
    ))
}

/// Renders the exact comment a live append would post, without posting it.
fn plan_state_comment(
    repository: &str,
    number: u64,
    expected_head: &str,
    chain: &review_state::ReviewStateChain,
    state: review_state::ReviewLoopState,
    outcome_body: Option<&str>,
) -> Result<PlannedStateComment, ForgeError> {
    let record = review_state::ReviewStateRecord::new(
        repository,
        number,
        expected_head,
        chain.records.len() as u64,
        chain.tip_digest.clone(),
        review_state::ReviewStatePayload::ReviewLoop { state },
    )?;
    let body = review_state::render_state_comment_body(&record, outcome_body)?;
    Ok(PlannedStateComment {
        visible_metadata: review_state::state_comment_visible_metadata(&record),
        includes_outcome_body: outcome_body.is_some(),
        bytes: body.len(),
    })
}

/// Renders the durable hard-stop receipt an extendable budget failure would
/// append. Returns `None` when the receipt itself cannot be built, because the
/// reported `observation_transition` failure is already the actionable verdict.
fn plan_hard_stop_comment(
    repository: &str,
    number: u64,
    expected_head: &str,
    chain: &review_state::ReviewStateChain,
    previous: Option<&review_state::ReviewLoopState>,
    observations: &[review_state::ReviewFindingObservation],
    error: &ForgeError,
) -> Option<PlannedStateComment> {
    let stopped = review_state::record_review_loop_hard_stop(
        previous?,
        expected_head,
        observations,
        chain.tip_digest.as_deref()?,
        error,
    )
    .ok()?;
    plan_state_comment(repository, number, expected_head, chain, stopped, None).ok()
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

    use crate::backend::BackendSuccess;

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

    // ---------------------------------------------------------------------
    // Command-level coverage: inspect / observe / extend drive the durable
    // provider chain through a stateful fake `gh`, so an appended state is
    // visible to the very next read exactly as it would be on the provider.
    // ---------------------------------------------------------------------

    const REPO: &str = "acme/widgets";
    const PR: u64 = 7;
    const HEAD: &str = "head-7";
    const NEXT_HEAD: &str = "head-8";
    const VIEWER: &str = "forge-bot";
    const TIP_CREATED_AT: &str = "2026-07-20T12:00:00Z";

    fn view_json(head_sha: &str) -> String {
        serde_json::json!({
            "number": PR,
            "url": "https://github.com/acme/widgets/pull/7",
            "state": "OPEN",
            "isDraft": false,
            "title": "feat: sample",
            "headRefName": "feat/sample",
            "headRefOid": head_sha,
            "baseRefName": "main",
            "mergeable": "MERGEABLE",
            "mergedAt": null,
            "labels": []
        })
        .to_string()
    }

    fn comment_node(body: &str, created_at: &str) -> serde_json::Value {
        serde_json::json!({
            "author": {"login": VIEWER},
            "authorAssociation": "MEMBER",
            "body": body,
            "createdAt": created_at
        })
    }

    /// Stateful `gh` double: a posted review-state comment becomes part of the
    /// next comment page, so append + read-back verification runs for real.
    struct FakeGitHub {
        view: String,
        comments: std::cell::RefCell<Vec<serde_json::Value>>,
        approval: Option<String>,
        calls: std::cell::RefCell<Vec<Vec<String>>>,
    }

    impl FakeGitHub {
        fn new(head_sha: &str) -> Self {
            Self {
                view: view_json(head_sha),
                comments: std::cell::RefCell::new(Vec::new()),
                approval: None,
                calls: std::cell::RefCell::new(Vec::new()),
            }
        }

        fn with_state(self, state: &review_state::ReviewLoopState, head: &str) -> Self {
            let record = review_state::ReviewStateRecord::new(
                REPO,
                PR,
                head,
                0,
                None,
                review_state::ReviewStatePayload::ReviewLoop {
                    state: state.clone(),
                },
            )
            .expect("seed record");
            let marker = record.marker().expect("seed marker");
            self.comments
                .borrow_mut()
                .push(comment_node(&marker, TIP_CREATED_AT));
            self
        }

        fn with_approval(mut self, approval: serde_json::Value) -> Self {
            self.approval = Some(approval.to_string());
            self
        }

        fn tip_digest(&self) -> Option<String> {
            let bodies: Vec<String> = self
                .comments
                .borrow()
                .iter()
                .map(|node| node["body"].as_str().unwrap_or_default().to_string())
                .collect();
            review_state::parse_chain(bodies.iter().map(String::as_str), REPO, PR)
                .expect("seeded chain")
                .tip_digest
        }

        fn calls(&self) -> Vec<Vec<String>> {
            self.calls.borrow().clone()
        }

        fn appended_bodies(&self) -> Vec<String> {
            self.calls()
                .iter()
                .filter(|argv| argv.iter().any(|arg| arg == "--method"))
                .filter_map(|argv| {
                    argv.iter()
                        .find_map(|arg| arg.strip_prefix("body="))
                        .map(str::to_string)
                })
                .collect()
        }
    }

    impl BackendRunner for FakeGitHub {
        fn run(&self, call: &BackendCall) -> Result<BackendSuccess, ForgeError> {
            let argv: Vec<String> = call
                .argv
                .iter()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect();
            self.calls.borrow_mut().push(argv.clone());

            if argv.first().map(String::as_str) == Some("pr") {
                return Ok(BackendSuccess {
                    stdout: self.view.clone(),
                    stderr: String::new(),
                });
            }
            if argv.get(1).map(String::as_str) == Some("graphql") {
                let page = serde_json::json!({
                    "data": {
                        "viewer": {"login": VIEWER},
                        "repository": {"pullRequest": {"comments": {
                            "nodes": *self.comments.borrow(),
                            "pageInfo": {"hasNextPage": false, "endCursor": "cursor-end"}
                        }}}
                    }
                });
                return Ok(BackendSuccess {
                    stdout: page.to_string(),
                    stderr: String::new(),
                });
            }
            if argv.iter().any(|arg| arg == "--method") {
                let body = argv
                    .iter()
                    .find_map(|arg| arg.strip_prefix("body="))
                    .unwrap_or_default()
                    .to_string();
                self.comments
                    .borrow_mut()
                    .push(comment_node(&body, "2026-07-20T12:00:09Z"));
                return Ok(BackendSuccess {
                    stdout: "https://github.com/acme/widgets/pull/7#issuecomment-2".to_string(),
                    stderr: String::new(),
                });
            }
            match self.approval.as_ref() {
                Some(body) => Ok(BackendSuccess {
                    stdout: body.clone(),
                    stderr: String::new(),
                }),
                None => Err(ForgeError::software(
                    schema_err(),
                    "unexpected backend call",
                    Some(argv.join(" ")),
                )),
            }
        }

        fn run_raw(&self, call: &BackendCall) -> Result<crate::backend::BackendOutput, ForgeError> {
            self.run(call).map(|success| crate::backend::BackendOutput {
                stdout: success.stdout,
                stderr: success.stderr,
                status_success: true,
                exit_code: 0,
            })
        }
    }

    fn flags(dry_run: bool) -> GlobalFlags {
        GlobalFlags {
            format: None,
            remote: "origin".to_string(),
            provider: None,
            host: None,
            repo: Some(REPO.to_string()),
            store_root: None,
            dry_run,
        }
    }

    fn github_remote(_: &str) -> Option<String> {
        Some("https://github.com/acme/widgets.git".to_string())
    }

    fn gitlab_remote(_: &str) -> Option<String> {
        Some("https://gitlab.com/acme/widgets.git".to_string())
    }

    fn github_ctx() -> ProviderContext {
        ProviderContext {
            provider: Provider::GitHub,
            host: "github.com".to_string(),
            source: crate::provider::DetectionSource::Flag,
            repo: Some(REPO.to_string()),
        }
    }

    fn open_finding(fingerprint: &str) -> review_state::ReviewFindingObservation {
        review_state::ReviewFindingObservation {
            fingerprint: fingerprint.to_string(),
            root_cause_fingerprint: None,
            blocking: true,
            status: review_state::ReviewFindingStatus::Open,
            threads: Vec::new(),
        }
    }

    fn genesis_state(
        head: &str,
        observations: &[review_state::ReviewFindingObservation],
    ) -> review_state::ReviewLoopState {
        review_state::observe_review_loop(None, head, observations)
            .expect("genesis transition")
            .state
    }

    fn findings_file(dir: &tempfile::TempDir, body: &str) -> String {
        let path = dir.path().join("findings.json");
        fs::write(&path, body).expect("write findings");
        path.to_string_lossy().into_owned()
    }

    fn observe_args(
        findings: &str,
        expected_head: &str,
        expected_state: Option<&str>,
    ) -> PrReviewLoopObserveArgs {
        PrReviewLoopObserveArgs {
            id: PR,
            expected_head: expected_head.to_string(),
            findings_file: findings.to_string(),
            expected_state: expected_state.map(str::to_string),
            body: None,
            body_file: None,
        }
    }

    fn observe_args_with_body(
        findings: &str,
        expected_head: &str,
        expected_state: Option<&str>,
        body: &str,
    ) -> PrReviewLoopObserveArgs {
        PrReviewLoopObserveArgs {
            body: Some(body.to_string()),
            ..observe_args(findings, expected_head, expected_state)
        }
    }

    fn extend_args(
        expected_head: &str,
        expected_state: &str,
        proposal: &str,
    ) -> PrReviewLoopExtendArgs {
        PrReviewLoopExtendArgs {
            id: PR,
            expected_head: expected_head.to_string(),
            expected_state: expected_state.to_string(),
            stop_code: "review_round_limit_exceeded".to_string(),
            budget_field: "max_repair_rounds".to_string(),
            increment: 1,
            proposal_digest: proposal.to_string(),
            approval_comment: 99,
        }
    }

    fn stopped_state(head: &str, proposal: &str) -> review_state::ReviewLoopState {
        review_state::ReviewLoopState {
            head_sha: head.to_string(),
            round: 1,
            no_progress_rounds: 0,
            budget: review_state::ReviewLoopBudget::default(),
            findings: std::collections::BTreeMap::new(),
            extensions: Vec::new(),
            hard_stop: Some(review_state::ReviewLoopHardStop {
                code: "review_round_limit_exceeded".to_string(),
                budget_field: "max_repair_rounds".to_string(),
                increment: 1,
                proposal_digest: proposal.to_string(),
                attempted_head_sha: head.to_string(),
                observation_digest: "sha256:observation".to_string(),
                extension_applied: false,
            }),
        }
    }

    fn approval_payload(created_at: &str, association: &str, body: &str) -> serde_json::Value {
        serde_json::json!({
            "html_url": "https://github.com/acme/widgets/pull/7#issuecomment-99",
            "issue_url": "https://api.github.com/repos/acme/widgets/issues/7",
            "user": {"login": "maintainer"},
            "author_association": association,
            "created_at": created_at,
            "body": body
        })
    }

    // --- inspect ---------------------------------------------------------

    #[test]
    fn inspect_dry_run_plans_without_touching_the_provider() {
        let runner = FakeGitHub::new(HEAD);
        let code = run_inspect_with(
            &runner,
            &flags(true),
            PrReviewLoopInspectArgs { id: PR },
            OutputFormat::Json,
            github_remote,
        )
        .expect("dry run");

        assert_eq!(code, 0);
        assert_eq!(runner.calls(), Vec::<Vec<String>>::new());
    }

    #[test]
    fn inspect_reports_the_existing_chain_without_appending() {
        let state = genesis_state(HEAD, &[open_finding("correctness:review-loop:one")]);
        let runner = FakeGitHub::new(HEAD).with_state(&state, HEAD);

        let code = run_inspect_with(
            &runner,
            &flags(false),
            PrReviewLoopInspectArgs { id: PR },
            OutputFormat::Json,
            github_remote,
        )
        .expect("inspect");

        assert_eq!(code, 0);
        assert_eq!(runner.appended_bodies(), Vec::<String>::new());
    }

    #[test]
    fn inspect_rejects_a_non_github_provider() {
        let runner = FakeGitHub::new(HEAD);
        let error = run_inspect_with(
            &runner,
            &flags(false),
            PrReviewLoopInspectArgs { id: PR },
            OutputFormat::Json,
            gitlab_remote,
        )
        .expect_err("review-loop is GitHub-only in v1");

        assert_eq!(error.kind(), "provider_unsupported");
        assert_eq!(runner.calls(), Vec::<Vec<String>>::new());
    }

    // --- observe ---------------------------------------------------------

    #[test]
    fn observe_appends_the_genesis_observation() {
        let dir = tempfile::tempdir().unwrap();
        let findings = findings_file(
            &dir,
            r#"{"data":{"findings":[{"lifecycle_fingerprint":"correctness:review-loop:one","primary":{"severity":"high"}}]}}"#,
        );
        let runner = FakeGitHub::new(HEAD);

        let code = run_observe_with(
            &runner,
            &flags(false),
            observe_args(&findings, HEAD, None),
            OutputFormat::Json,
            github_remote,
        )
        .expect("genesis observation");

        assert_eq!(code, 0);
        assert_eq!(runner.appended_bodies().len(), 1);
        let chain_state = review_state::parse_chain(
            runner.appended_bodies().iter().map(String::as_str),
            REPO,
            PR,
        )
        .expect("appended chain");
        let state = review_state::latest_review_loop_state(&chain_state).expect("state");
        assert_eq!(state.head_sha, HEAD);
        assert_eq!(state.round, 0);
        assert!(state.findings.contains_key("correctness:review-loop:one"));
    }

    #[test]
    fn observe_dry_run_reports_an_unreadable_findings_file_without_failing() {
        let runner = FakeGitHub::new(HEAD);

        let code = run_observe_with(
            &runner,
            &flags(true),
            observe_args("/definitely/missing.json", HEAD, None),
            OutputFormat::Text,
            github_remote,
        )
        .expect("a failing preflight rule is reported, not returned as an error");

        // The dry run is a faithful read-only preflight: it reaches the
        // provider to evaluate the remaining rules even when the local
        // findings file is unusable, and it still appends nothing.
        assert_eq!(code, 0);
        assert!(
            !runner.calls().is_empty(),
            "the preflight must actually read provider state"
        );
        assert_eq!(runner.appended_bodies(), Vec::<String>::new());
    }

    #[test]
    fn observe_dry_run_predicts_an_append_without_performing_one() {
        let dir = tempfile::tempdir().unwrap();
        let findings = findings_file(&dir, r#"[{"fingerprint":"correctness:review-loop:one"}]"#);
        let runner = FakeGitHub::new(HEAD);

        let code = run_observe_with(
            &runner,
            &flags(true),
            observe_args(&findings, HEAD, None),
            OutputFormat::Json,
            github_remote,
        )
        .expect("clean preflight");

        assert_eq!(code, 0);
        assert_eq!(
            runner.appended_bodies(),
            Vec::<String>::new(),
            "a dry run must never write the durable chain"
        );
    }

    #[test]
    fn observe_is_a_no_op_when_the_observation_matches_the_state() {
        let dir = tempfile::tempdir().unwrap();
        let findings = findings_file(&dir, r#"[{"fingerprint":"correctness:review-loop:one"}]"#);
        let state = genesis_state(HEAD, &[open_finding("correctness:review-loop:one")]);
        let runner = FakeGitHub::new(HEAD).with_state(&state, HEAD);
        let tip = runner.tip_digest().expect("seeded tip");

        let code = run_observe_with(
            &runner,
            &flags(false),
            observe_args(&findings, HEAD, Some(&tip)),
            OutputFormat::Json,
            github_remote,
        )
        .expect("unchanged observation");

        assert_eq!(code, 0);
        assert_eq!(
            runner.appended_bodies(),
            Vec::<String>::new(),
            "an unchanged observation must not grow the durable chain"
        );
    }

    // --- combined delivery outcome ---------------------------------------

    #[test]
    fn observe_posts_the_delivery_outcome_and_the_ledger_in_one_comment() {
        let dir = tempfile::tempdir().unwrap();
        let findings = findings_file(&dir, "[]");
        let runner = FakeGitHub::new(HEAD);

        let code = run_observe_with(
            &runner,
            &flags(false),
            observe_args_with_body(&findings, HEAD, None, "## Delivery outcome\n\napproved"),
            OutputFormat::Json,
            github_remote,
        )
        .expect("combined observation");

        assert_eq!(code, 0);
        let bodies = runner.appended_bodies();
        assert_eq!(
            bodies.len(),
            1,
            "one comment carries both halves: {bodies:?}"
        );
        let body = &bodies[0];
        assert!(body.contains("## Delivery outcome"), "{body}");
        assert!(
            body.contains("forge-cli review ledger · generation 0"),
            "{body}"
        );
        // The ledger half is still an exact, parseable record.
        let chain = review_state::parse_chain([body.as_str()], REPO, PR).expect("appended chain");
        assert_eq!(chain.records.len(), 1);
        assert_eq!(
            review_state::latest_review_loop_state(&chain)
                .expect("state")
                .head_sha,
            HEAD
        );
    }

    #[test]
    fn an_identical_observe_retry_posts_neither_a_ledger_nor_an_outcome_comment() {
        // The outcome rides the append, so the append's deduplication is also
        // the outcome's: a retry after a lost response must not leave a second
        // outcome comment behind.
        let dir = tempfile::tempdir().unwrap();
        let findings = findings_file(&dir, r#"[{"fingerprint":"correctness:review-loop:one"}]"#);
        let state = genesis_state(HEAD, &[open_finding("correctness:review-loop:one")]);
        let runner = FakeGitHub::new(HEAD).with_state(&state, HEAD);
        let tip = runner.tip_digest().expect("seeded tip");

        let code = run_observe_with(
            &runner,
            &flags(false),
            observe_args_with_body(&findings, HEAD, Some(&tip), "## Delivery outcome"),
            OutputFormat::Json,
            github_remote,
        )
        .expect("unchanged observation");

        assert_eq!(code, 0);
        assert_eq!(
            runner.appended_bodies(),
            Vec::<String>::new(),
            "an already-current ledger writes nothing at all"
        );
    }

    #[test]
    fn observe_rejects_a_non_portable_outcome_body_before_any_provider_call() {
        let dir = tempfile::tempdir().unwrap();
        let findings = findings_file(&dir, "[]");
        let runner = FakeGitHub::new(HEAD);

        let error = run_observe_with(
            &runner,
            &flags(false),
            observe_args_with_body(
                &findings,
                HEAD,
                None,
                "outcome recorded at /home/operator/work/report.md",
            ),
            OutputFormat::Json,
            github_remote,
        )
        .expect_err("a local path must not reach a permanent provider comment");

        assert_eq!(error.kind(), "local_path_present");
        assert_eq!(
            runner.calls(),
            Vec::<Vec<String>>::new(),
            "the body is validated before the first provider read"
        );
    }

    #[test]
    fn observe_rejects_an_outcome_body_flag_with_no_content() {
        let dir = tempfile::tempdir().unwrap();
        let findings = findings_file(&dir, "[]");
        let runner = FakeGitHub::new(HEAD);

        let error = run_observe_with(
            &runner,
            &flags(false),
            observe_args_with_body(&findings, HEAD, None, "   \n  "),
            OutputFormat::Json,
            github_remote,
        )
        .expect_err("an empty outcome body is a caller error, not an omission");

        assert_eq!(error.kind(), "review_state_comment_invalid");
        assert_eq!(runner.calls(), Vec::<Vec<String>>::new());
    }

    #[test]
    fn observe_dry_run_plans_the_combined_comment_without_posting_it() {
        let dir = tempfile::tempdir().unwrap();
        let findings = findings_file(&dir, "[]");
        let runner = FakeGitHub::new(HEAD);

        let code = run_observe_with(
            &runner,
            &flags(true),
            observe_args_with_body(&findings, HEAD, None, "## Delivery outcome"),
            OutputFormat::Text,
            github_remote,
        )
        .expect("clean combined preflight");

        assert_eq!(code, 0);
        assert_eq!(
            runner.appended_bodies(),
            Vec::<String>::new(),
            "planning a combined comment must not post one"
        );
        assert!(
            !runner.calls().is_empty(),
            "the preflight still reads provider state"
        );
    }

    #[test]
    fn observe_dry_run_reports_an_unreadable_outcome_body_file_without_failing() {
        let dir = tempfile::tempdir().unwrap();
        let findings = findings_file(&dir, "[]");
        let runner = FakeGitHub::new(HEAD);
        let args = PrReviewLoopObserveArgs {
            body_file: Some("/definitely/missing-outcome.md".to_string()),
            ..observe_args(&findings, HEAD, None)
        };

        let code = run_observe_with(
            &runner,
            &flags(true),
            args,
            OutputFormat::Text,
            github_remote,
        )
        .expect("a failing preflight rule is reported, not returned as an error");

        assert_eq!(code, 0);
        assert_eq!(runner.appended_bodies(), Vec::<String>::new());
    }

    #[test]
    fn a_durable_hard_stop_receipt_is_appended_without_the_outcome_body() {
        // A hard stop is a failure receipt. Attaching the delivery outcome to it
        // would publish an approval narrative on a run that did not converge.
        const REPAIRED: &str = "head-repaired";
        let dir = tempfile::tempdir().unwrap();
        let findings = findings_file(&dir, r#"[{"fingerprint":"correctness:review-loop:one"}]"#);
        let mut state = genesis_state(HEAD, &[open_finding("correctness:review-loop:one")]);
        state.budget.max_repair_rounds = 0;
        let runner = FakeGitHub::new(REPAIRED).with_state(&state, HEAD);
        let tip = runner.tip_digest().expect("seeded tip");

        let error = run_observe_with(
            &runner,
            &flags(false),
            observe_args_with_body(&findings, REPAIRED, Some(&tip), "## Delivery outcome"),
            OutputFormat::Json,
            github_remote,
        )
        .expect_err("an exhausted round budget stops the loop");

        assert_eq!(error.kind(), "review_round_limit_exceeded");
        let bodies = runner.appended_bodies();
        assert_eq!(bodies.len(), 1, "the stop receipt is still durable");
        assert!(
            !bodies[0].contains("## Delivery outcome"),
            "a stop receipt must not carry the delivery outcome: {}",
            bodies[0]
        );
        assert!(
            bodies[0].contains("forge-cli review ledger ·"),
            "the stop receipt still carries visible metadata: {}",
            bodies[0]
        );
    }

    #[test]
    fn a_forked_ledger_history_fails_closed_before_any_append() {
        // Two sessions that each appended generation 1 from the same genesis are
        // a fork. It must be refused on read, with the supplied outcome body
        // never reaching the provider.
        let dir = tempfile::tempdir().unwrap();
        let findings = findings_file(&dir, "[]");
        let genesis = genesis_state(HEAD, &[]);
        let runner = FakeGitHub::new(HEAD).with_state(&genesis, HEAD);
        let genesis_digest = runner.tip_digest().expect("genesis tip");
        for no_progress in [0, 1] {
            let mut competing = genesis.clone();
            competing.no_progress_rounds = no_progress;
            let record = review_state::ReviewStateRecord::new(
                REPO,
                PR,
                HEAD,
                1,
                Some(genesis_digest.clone()),
                review_state::ReviewStatePayload::ReviewLoop { state: competing },
            )
            .expect("competing record");
            runner.comments.borrow_mut().push(comment_node(
                &review_state::render_state_comment_body(&record, None).expect("body"),
                TIP_CREATED_AT,
            ));
        }

        let error = run_observe_with(
            &runner,
            &flags(false),
            observe_args_with_body(
                &findings,
                HEAD,
                Some(&genesis_digest),
                "## Delivery outcome",
            ),
            OutputFormat::Json,
            github_remote,
        )
        .expect_err("a forked chain must fail closed");

        assert_eq!(error.kind(), "review_state_conflict");
        assert_eq!(runner.appended_bodies(), Vec::<String>::new());
    }

    #[test]
    fn observe_rejects_a_head_that_the_provider_does_not_report() {
        let dir = tempfile::tempdir().unwrap();
        let findings = findings_file(&dir, r#"[{"fingerprint":"correctness:review-loop:one"}]"#);
        let runner = FakeGitHub::new(HEAD);

        let error = run_observe_with(
            &runner,
            &flags(false),
            observe_args(&findings, "head-mismatch", None),
            OutputFormat::Json,
            github_remote,
        )
        .expect_err("head mismatch");

        assert_eq!(error.kind(), "review_state_conflict");
        assert!(
            error
                .detail()
                .unwrap_or_default()
                .contains("provider_head="),
            "detail should name both heads: {:?}",
            error.detail()
        );
        assert_eq!(runner.appended_bodies(), Vec::<String>::new());
    }

    #[test]
    fn observe_rejects_a_stale_expected_state_tip() {
        let dir = tempfile::tempdir().unwrap();
        let findings = findings_file(&dir, r#"[{"fingerprint":"correctness:review-loop:one"}]"#);
        let state = genesis_state(HEAD, &[open_finding("correctness:review-loop:one")]);
        let runner = FakeGitHub::new(HEAD).with_state(&state, HEAD);

        let error = run_observe_with(
            &runner,
            &flags(false),
            observe_args(&findings, HEAD, Some("sha256:stale")),
            OutputFormat::Json,
            github_remote,
        )
        .expect_err("tip mismatch");

        assert_eq!(error.kind(), "review_state_conflict");
        assert!(
            error
                .detail()
                .unwrap_or_default()
                .contains("expected_state=sha256:stale"),
            "detail should name the stale tip: {:?}",
            error.detail()
        );
    }

    #[test]
    fn observe_records_a_durable_hard_stop_before_failing_closed() {
        let dir = tempfile::tempdir().unwrap();
        let findings = findings_file(&dir, r#"[{"fingerprint":"correctness:review-loop:one"}]"#);
        let mut state = genesis_state(HEAD, &[open_finding("correctness:review-loop:one")]);
        // Exhaust the repair-round budget so advancing to a new head stops.
        state.budget.max_repair_rounds = 0;
        let runner = FakeGitHub::new(NEXT_HEAD).with_state(&state, HEAD);
        let tip = runner.tip_digest().expect("seeded tip");

        let error = run_observe_with(
            &runner,
            &flags(false),
            observe_args(&findings, NEXT_HEAD, Some(&tip)),
            OutputFormat::Json,
            github_remote,
        )
        .expect_err("budget exhausted");

        assert_eq!(error.kind(), "review_round_limit_exceeded");
        let detail = error.detail().unwrap_or_default();
        assert!(
            detail.contains("approval_marker=<!-- forge-cli:review-loop-extension:v1 "),
            "the operator needs the exact approval marker: {detail}"
        );
        assert!(
            detail.contains(&format!("attempted_head={NEXT_HEAD}")),
            "detail should name the attempted head: {detail}"
        );
        assert_eq!(
            runner.appended_bodies().len(),
            1,
            "the hard stop must be durable before the command fails"
        );
    }

    #[test]
    fn observe_replays_a_recorded_hard_stop_without_appending_again() {
        let dir = tempfile::tempdir().unwrap();
        let findings = findings_file(&dir, r#"[{"fingerprint":"correctness:review-loop:one"}]"#);
        let mut state = genesis_state(HEAD, &[open_finding("correctness:review-loop:one")]);
        state.budget.max_repair_rounds = 0;
        let seeded = FakeGitHub::new(NEXT_HEAD).with_state(&state, HEAD);
        let tip = seeded.tip_digest().expect("seeded tip");
        run_observe_with(
            &seeded,
            &flags(false),
            observe_args(&findings, NEXT_HEAD, Some(&tip)),
            OutputFormat::Json,
            github_remote,
        )
        .expect_err("first stop");
        let stopped_tip = seeded.tip_digest().expect("stopped tip");
        let appended_after_first = seeded.appended_bodies().len();

        let error = run_observe_with(
            &seeded,
            &flags(false),
            observe_args(&findings, NEXT_HEAD, Some(&stopped_tip)),
            OutputFormat::Json,
            github_remote,
        )
        .expect_err("stop is durable");

        assert_eq!(error.kind(), "review_round_limit_exceeded");
        assert_eq!(
            seeded.appended_bodies().len(),
            appended_after_first,
            "a replayed hard stop must not append a second stop record"
        );
    }

    #[test]
    fn observe_rejects_an_unreadable_findings_file() {
        let runner = FakeGitHub::new(HEAD);
        let error = run_observe_with(
            &runner,
            &flags(false),
            observe_args("/definitely/missing/findings.json", HEAD, None),
            OutputFormat::Json,
            github_remote,
        )
        .expect_err("missing findings file");

        assert_eq!(error.kind(), "review_findings_invalid");
        assert_eq!(runner.calls(), Vec::<Vec<String>>::new());
    }

    // --- extend ----------------------------------------------------------

    #[test]
    fn extend_dry_run_plans_without_touching_the_provider() {
        let runner = FakeGitHub::new(HEAD);
        let code = run_extend_with(
            &runner,
            &flags(true),
            extend_args(HEAD, "sha256:tip", "sha256:proposal"),
            OutputFormat::Text,
            github_remote,
        )
        .expect("dry run");

        assert_eq!(code, 0);
        assert_eq!(runner.calls(), Vec::<Vec<String>>::new());
    }

    #[test]
    fn extend_requires_an_existing_review_loop_state() {
        let runner = FakeGitHub::new(HEAD);
        let error = run_extend_with(
            &runner,
            &flags(false),
            extend_args(HEAD, "sha256:tip", "sha256:proposal"),
            OutputFormat::Json,
            github_remote,
        )
        .expect_err("no chain to extend");

        // A genesis chain has no tip, so the tip guard fires before the
        // "state must exist" guard.
        assert_eq!(error.kind(), "review_state_conflict");
    }

    #[test]
    fn extend_rejects_a_state_without_a_durable_hard_stop() {
        let state = genesis_state(HEAD, &[open_finding("correctness:review-loop:one")]);
        let runner = FakeGitHub::new(HEAD).with_state(&state, HEAD);
        let tip = runner.tip_digest().expect("seeded tip");

        let error = run_extend_with(
            &runner,
            &flags(false),
            extend_args(HEAD, &tip, "sha256:proposal"),
            OutputFormat::Json,
            github_remote,
        )
        .expect_err("nothing to extend");

        assert_eq!(error.kind(), "review_extension_invalid");
    }

    #[test]
    fn extend_rejects_a_request_that_does_not_match_the_hard_stop() {
        let state = stopped_state(HEAD, "sha256:proposal");
        let runner = FakeGitHub::new(HEAD).with_state(&state, HEAD);
        let tip = runner.tip_digest().expect("seeded tip");

        let error = run_extend_with(
            &runner,
            &flags(false),
            extend_args(HEAD, &tip, "sha256:other-proposal"),
            OutputFormat::Json,
            github_remote,
        )
        .expect_err("proposal digest mismatch");

        assert_eq!(error.kind(), "review_extension_invalid");
        assert!(
            error
                .detail()
                .unwrap_or_default()
                .contains("expected_proposal=sha256:proposal"),
            "detail should name the durable proposal: {:?}",
            error.detail()
        );
    }

    #[test]
    fn extend_rejects_a_state_bound_to_a_different_head() {
        let state = stopped_state(HEAD, "sha256:proposal");
        let runner = FakeGitHub::new(NEXT_HEAD).with_state(&state, HEAD);
        let tip = runner.tip_digest().expect("seeded tip");

        let error = run_extend_with(
            &runner,
            &flags(false),
            extend_args(NEXT_HEAD, &tip, "sha256:proposal"),
            OutputFormat::Json,
            github_remote,
        )
        .expect_err("head mismatch");

        assert_eq!(error.kind(), "review_state_conflict");
        assert!(
            error
                .detail()
                .unwrap_or_default()
                .contains(&format!("state_head={HEAD}")),
            "detail should name the bound head: {:?}",
            error.detail()
        );
    }

    #[test]
    fn extend_rejects_an_approval_from_a_non_maintainer() {
        let proposal = "sha256:proposal";
        let state = stopped_state(HEAD, proposal);
        let marker = extension_approval_marker(proposal, "max_repair_rounds", 1);
        let runner = FakeGitHub::new(HEAD)
            .with_state(&state, HEAD)
            .with_approval(approval_payload("2026-07-20T12:00:01Z", "NONE", &marker));
        let tip = runner.tip_digest().expect("seeded tip");

        let error = run_extend_with(
            &runner,
            &flags(false),
            extend_args(HEAD, &tip, proposal),
            OutputFormat::Json,
            github_remote,
        )
        .expect_err("outsider approval");

        assert_eq!(error.kind(), "review_extension_approval_invalid");
        assert_eq!(runner.appended_bodies(), Vec::<String>::new());
    }

    #[test]
    fn extend_rejects_an_approval_missing_required_metadata() {
        let proposal = "sha256:proposal";
        let state = stopped_state(HEAD, proposal);
        let runner = FakeGitHub::new(HEAD)
            .with_state(&state, HEAD)
            .with_approval(serde_json::json!({
                "html_url": "https://github.com/acme/widgets/pull/7#issuecomment-99",
                "issue_url": "https://api.github.com/repos/acme/widgets/issues/7",
                "user": {"login": "maintainer"},
                "author_association": "MEMBER",
                "created_at": "2026-07-20T12:00:01Z"
            }));
        let tip = runner.tip_digest().expect("seeded tip");

        let error = run_extend_with(
            &runner,
            &flags(false),
            extend_args(HEAD, &tip, proposal),
            OutputFormat::Json,
            github_remote,
        )
        .expect_err("approval body missing");

        assert_eq!(error.kind(), "review_extension_approval_invalid");
        assert!(
            error.detail().unwrap_or_default().contains("field=/body"),
            "detail should name the missing field: {:?}",
            error.detail()
        );
    }

    #[test]
    fn extend_rejects_an_approval_on_another_pull_request() {
        let proposal = "sha256:proposal";
        let state = stopped_state(HEAD, proposal);
        let marker = extension_approval_marker(proposal, "max_repair_rounds", 1);
        let mut payload = approval_payload("2026-07-20T12:00:01Z", "MEMBER", &marker);
        payload["issue_url"] =
            serde_json::json!("https://api.github.com/repos/acme/widgets/issues/8");
        let runner = FakeGitHub::new(HEAD)
            .with_state(&state, HEAD)
            .with_approval(payload);
        let tip = runner.tip_digest().expect("seeded tip");

        let error = run_extend_with(
            &runner,
            &flags(false),
            extend_args(HEAD, &tip, proposal),
            OutputFormat::Json,
            github_remote,
        )
        .expect_err("approval belongs to another PR");

        assert_eq!(error.kind(), "review_extension_approval_invalid");
    }

    #[test]
    fn extend_applies_exactly_one_budget_increment() {
        let proposal = "sha256:proposal";
        let state = stopped_state(HEAD, proposal);
        let marker = extension_approval_marker(proposal, "max_repair_rounds", 1);
        let body = format!("Approving the extension.\n\n{marker}\n");
        let runner = FakeGitHub::new(HEAD)
            .with_state(&state, HEAD)
            .with_approval(approval_payload("2026-07-20T12:00:01Z", "MEMBER", &body));
        let tip = runner.tip_digest().expect("seeded tip");

        let code = run_extend_with(
            &runner,
            &flags(false),
            extend_args(HEAD, &tip, proposal),
            OutputFormat::Json,
            github_remote,
        )
        .expect("extension applied");

        assert_eq!(code, 0);
        assert_eq!(runner.appended_bodies().len(), 1);
        let bodies: Vec<String> = runner
            .comments
            .borrow()
            .iter()
            .map(|node| node["body"].as_str().unwrap_or_default().to_string())
            .collect();
        let chain = review_state::parse_chain(bodies.iter().map(String::as_str), REPO, PR)
            .expect("extended chain");
        let extended = review_state::latest_review_loop_state(&chain).expect("state");
        assert_eq!(
            extended.budget.max_repair_rounds,
            review_state::ReviewLoopBudget::default().max_repair_rounds + 1
        );
        assert_eq!(extended.extensions.len(), 1);
        assert_eq!(extended.extensions[0].proposal_digest, proposal);
        assert!(
            extended
                .hard_stop
                .as_ref()
                .expect("stop retained")
                .extension_applied,
            "the consumed hard stop must be marked applied so it cannot be replayed"
        );
    }

    // --- merge gates -----------------------------------------------------

    #[test]
    fn merge_gate_is_optional_without_a_ledger() {
        let runner = FakeGitHub::new(HEAD);
        let gate = ensure_merge_ready(
            &runner,
            &github_ctx(),
            "https://github.com/acme/widgets/pull/7",
            PR,
            HEAD,
            false,
        )
        .expect("no ledger is allowed when not required");

        assert_eq!(gate, None);
    }

    #[test]
    fn merge_gate_requires_a_genesis_ledger_when_bounded() {
        let runner = FakeGitHub::new(HEAD);
        let error = ensure_merge_ready(
            &runner,
            &github_ctx(),
            "https://github.com/acme/widgets/pull/7",
            PR,
            HEAD,
            true,
        )
        .expect_err("bounded delivery needs a ledger");

        assert_eq!(error.kind(), "review_state_conflict");
    }

    #[test]
    fn merge_gate_rejects_a_pull_request_url_without_a_repo_slug() {
        let runner = FakeGitHub::new(HEAD);
        let error = ensure_merge_ready(&runner, &github_ctx(), "not-a-url", PR, HEAD, false)
            .expect_err("unusable URL");

        assert_eq!(error.kind(), "software_error");
    }

    #[test]
    fn merge_gate_blocks_on_a_durable_hard_stop() {
        let state = stopped_state(HEAD, "sha256:proposal");
        let runner = FakeGitHub::new(HEAD).with_state(&state, HEAD);

        let error = ensure_merge_ready(
            &runner,
            &github_ctx(),
            "https://github.com/acme/widgets/pull/7",
            PR,
            HEAD,
            true,
        )
        .expect_err("hard stop blocks merge");

        assert_eq!(error.kind(), "review_round_limit_exceeded");
    }

    #[test]
    fn merge_gate_rejects_a_ledger_bound_to_another_head() {
        let state = genesis_state(HEAD, &[open_finding("correctness:review-loop:one")]);
        let runner = FakeGitHub::new(HEAD).with_state(&state, HEAD);

        let error = ensure_merge_ready(
            &runner,
            &github_ctx(),
            "https://github.com/acme/widgets/pull/7",
            PR,
            NEXT_HEAD,
            true,
        )
        .expect_err("ledger head mismatch");

        assert_eq!(error.kind(), "review_state_conflict");
        assert!(
            error
                .detail()
                .unwrap_or_default()
                .contains("review_loop_head="),
            "detail should name both heads: {:?}",
            error.detail()
        );
    }

    #[test]
    fn merge_gate_rejects_open_blocking_findings() {
        let state = genesis_state(HEAD, &[open_finding("correctness:review-loop:one")]);
        let runner = FakeGitHub::new(HEAD).with_state(&state, HEAD);

        let error = ensure_merge_ready(
            &runner,
            &github_ctx(),
            "https://github.com/acme/widgets/pull/7",
            PR,
            HEAD,
            true,
        )
        .expect_err("blocking findings stay open");

        assert_eq!(error.kind(), "review_findings_open");
        assert!(
            error
                .detail()
                .unwrap_or_default()
                .contains("findings=correctness:review-loop:one"),
            "detail should name the open finding: {:?}",
            error.detail()
        );
    }

    #[test]
    fn merge_gate_reports_the_tip_for_a_clean_ledger() {
        let mut observation = open_finding("correctness:review-loop:one");
        observation.status = review_state::ReviewFindingStatus::Fixed;
        let state = genesis_state(HEAD, &[observation]);
        let runner = FakeGitHub::new(HEAD).with_state(&state, HEAD);
        let tip = runner.tip_digest().expect("seeded tip");

        let gate = ensure_merge_ready(
            &runner,
            &github_ctx(),
            "https://github.com/acme/widgets/pull/7",
            PR,
            HEAD,
            true,
        )
        .expect("clean ledger")
        .expect("gate present");

        assert_eq!(gate.state_tip_digest, tip);
        assert_eq!(gate.head_sha, HEAD);
        assert_eq!(gate.round, 0);
        assert_eq!(gate.no_progress_rounds, 0);
        assert_eq!(gate.open_blocking_findings, Vec::<String>::new());
    }

    #[test]
    fn merge_recheck_accepts_an_unchanged_tip() {
        let mut observation = open_finding("correctness:review-loop:one");
        observation.status = review_state::ReviewFindingStatus::Fixed;
        let state = genesis_state(HEAD, &[observation]);
        let runner = FakeGitHub::new(HEAD).with_state(&state, HEAD);
        let previous = ensure_merge_ready(
            &runner,
            &github_ctx(),
            "https://github.com/acme/widgets/pull/7",
            PR,
            HEAD,
            true,
        )
        .expect("gate")
        .expect("gate present");

        let current = recheck_merge_ready(
            &runner,
            &github_ctx(),
            "https://github.com/acme/widgets/pull/7",
            PR,
            HEAD,
            &previous,
        )
        .expect("tip unchanged");

        assert_eq!(current, previous);
    }

    #[test]
    fn merge_recheck_rejects_a_tip_that_moved_before_merge() {
        let mut observation = open_finding("correctness:review-loop:one");
        observation.status = review_state::ReviewFindingStatus::Fixed;
        let state = genesis_state(HEAD, &[observation]);
        let runner = FakeGitHub::new(HEAD).with_state(&state, HEAD);
        let previous = ReviewLoopMergeGate {
            state_tip_digest: "sha256:previous".to_string(),
            head_sha: HEAD.to_string(),
            round: 0,
            no_progress_rounds: 0,
            open_blocking_findings: Vec::new(),
        };

        let error = recheck_merge_ready(
            &runner,
            &github_ctx(),
            "https://github.com/acme/widgets/pull/7",
            PR,
            HEAD,
            &previous,
        )
        .expect_err("tip moved");

        assert_eq!(error.kind(), "review_state_conflict");
        assert!(
            error
                .detail()
                .unwrap_or_default()
                .contains("expected_tip=sha256:previous"),
            "detail should name both tips: {:?}",
            error.detail()
        );
    }

    // --- findings parsing ------------------------------------------------

    #[test]
    fn findings_input_accepts_envelope_wrapper_and_bare_array() {
        let dir = tempfile::tempdir().unwrap();
        let envelope = findings_file(
            &dir,
            r#"{"data":{"findings":[{"fingerprint":"a"},{"fingerprint":"b"}]}}"#,
        );
        assert_eq!(read_observations(&envelope).expect("envelope").len(), 2);

        let dir = tempfile::tempdir().unwrap();
        let bare = findings_file(&dir, r#"[{"fingerprint":"a"}]"#);
        assert_eq!(read_observations(&bare).expect("bare array").len(), 1);

        let dir = tempfile::tempdir().unwrap();
        let findings_key = findings_file(&dir, r#"{"findings":[{"fingerprint":"a"}]}"#);
        assert_eq!(
            read_observations(&findings_key)
                .expect("findings key")
                .len(),
            1
        );
    }

    #[test]
    fn findings_input_rejects_invalid_json_and_non_array_shapes() {
        let dir = tempfile::tempdir().unwrap();
        let broken = findings_file(&dir, "{ not json");
        assert_eq!(
            read_observations(&broken).expect_err("invalid JSON").kind(),
            "review_findings_invalid"
        );

        let dir = tempfile::tempdir().unwrap();
        let scalar = findings_file(&dir, r#"{"findings":42}"#);
        assert_eq!(
            read_observations(&scalar)
                .expect_err("findings must be an array")
                .kind(),
            "review_findings_invalid"
        );
    }

    #[test]
    fn observation_requires_a_lifecycle_fingerprint() {
        let error = observation_from_value(&serde_json::json!({"blocking": true}))
            .expect_err("fingerprint is mandatory");
        assert_eq!(error.kind(), "review_fingerprint_required");

        let empty = observation_from_value(&serde_json::json!({"fingerprint": ""}))
            .expect_err("empty fingerprint is not a fingerprint");
        assert_eq!(empty.kind(), "review_fingerprint_required");
    }

    #[test]
    fn observation_reads_disposition_severity_and_threads() {
        let info_only = observation_from_value(&serde_json::json!({
            "fingerprint": "style:review-loop:one",
            "primary": {"severity": "info"}
        }))
        .expect("observation");
        assert!(
            !info_only.blocking,
            "an info-severity finding must not block by default"
        );

        let explicit = observation_from_value(&serde_json::json!({
            "fingerprint": "correctness:review-loop:two",
            "blocking": false,
            "disposition": "fixed",
            "root_cause_fingerprint": "correctness:review-loop:root",
            "threads": ["PRRT_1", 42]
        }))
        .expect("observation");
        assert!(!explicit.blocking);
        assert_eq!(explicit.status, review_state::ReviewFindingStatus::Fixed);
        assert_eq!(
            explicit.root_cause_fingerprint.as_deref(),
            Some("correctness:review-loop:root")
        );
        assert_eq!(explicit.threads, vec!["PRRT_1".to_string()]);
    }

    #[test]
    fn finding_status_parses_every_supported_disposition() {
        assert_eq!(
            parse_finding_status("open").expect("open"),
            review_state::ReviewFindingStatus::Open
        );
        assert_eq!(
            parse_finding_status("fixed").expect("fixed"),
            review_state::ReviewFindingStatus::Fixed
        );
        assert_eq!(
            parse_finding_status("accepted").expect("accepted"),
            review_state::ReviewFindingStatus::Accepted
        );
        assert_eq!(
            parse_finding_status("preference").expect("preference"),
            review_state::ReviewFindingStatus::Preference
        );
        assert_eq!(
            parse_finding_status("follow-up").expect("follow-up"),
            review_state::ReviewFindingStatus::FollowUp
        );
        assert_eq!(
            parse_finding_status("wontfix")
                .expect_err("unsupported disposition")
                .kind(),
            "review_findings_invalid"
        );
    }

    #[test]
    fn expected_tip_guard_names_genesis_on_both_sides() {
        ensure_expected_tip(None, None).expect("genesis matches genesis");
        let error = ensure_expected_tip(Some("sha256:provider"), None)
            .expect_err("provider is ahead of genesis");
        assert_eq!(error.kind(), "review_state_conflict");
        let detail = error.detail().unwrap_or_default();
        assert!(
            detail.contains("expected_state=<genesis>"),
            "genesis must be spelled out: {detail}"
        );
        assert!(
            detail.contains("provider_state=sha256:provider"),
            "{detail}"
        );
    }
}
