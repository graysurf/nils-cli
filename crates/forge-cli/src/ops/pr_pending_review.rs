//! Authenticated recovery for provider-valid pending GitHub reviews.

use std::env;
use std::fmt::Write as _;
use std::fs::{self, File, OpenOptions};
use std::io::Read;
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use nils_common::cli_contract::{OutputFormat, schema_version_for};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::backend::{BackendRunner, BackendSuccess};
use crate::cli::{
    BINARY, GlobalFlags, PrPendingReviewDeleteArgs, PrPendingReviewDiscardArgs,
    PrPendingReviewInspectArgs, PrPendingReviewResumeSubmitArgs, PrPendingReviewSubmitArgs,
    PrReviewDecision,
};
use crate::envelope::emit_success;
use crate::error::ForgeError;
use crate::ops::{pr_review, pr_reviews, pr_view};
use crate::provider::{Provider, ProviderContext, detect, git_remote_url};
use crate::rate_limit::default_runner;

const SCHEMA: &str = "pr.pending-review.delete";
const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PrPendingReviewInspectPayload {
    pub provider: &'static str,
    pub number: u64,
    pub url: String,
    pub snapshot: pr_reviews::PendingReviewSnapshot,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PrPendingReviewSubmitPayload {
    pub provider: &'static str,
    pub number: u64,
    pub url: String,
    pub review_id: String,
    pub review_url: String,
    pub head_sha: String,
    pub commit_sha: String,
    pub snapshot_digest: String,
    pub snapshot_provenance: &'static str,
    pub review_run_id: Option<String>,
    pub submitted: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PrPendingReviewResumeSubmitPayload {
    pub provider: &'static str,
    pub number: u64,
    pub url: String,
    pub review_id: String,
    pub review_url: String,
    pub head_sha: String,
    pub commit_sha: String,
    pub snapshot_digest: Option<String>,
    pub snapshot_provenance: &'static str,
    pub review_run_id: String,
    pub submitted: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SubmitContract {
    DirectV1,
    ResumeV2,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PrPendingReviewDiscardPayload {
    pub provider: &'static str,
    pub number: u64,
    pub url: String,
    pub review_id: String,
    pub review_url: String,
    pub head_sha: String,
    pub commit_sha: String,
    pub snapshot_digest: String,
    pub inline_comment_count: usize,
    pub discarded: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PrPendingReviewDeletePayload {
    pub provider: &'static str,
    pub number: u64,
    pub url: String,
    pub head_sha: String,
    pub commit_sha: String,
    pub review_id: String,
    pub review_url: String,
    pub author: String,
    pub deleted: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct PrPendingReviewDeleteDryRunPayload {
    provider: &'static str,
    number: u64,
    review_id: String,
    expected_head: String,
    expected_commit: String,
    expected_inline_comment_count: u64,
    confirmed_abandoned: bool,
    guard_plan: Vec<String>,
    snapshot_plan: Vec<String>,
    target_plan: Vec<String>,
    delete_plan: Vec<String>,
}

pub fn run_inspect(
    global: &GlobalFlags,
    args: PrPendingReviewInspectArgs,
    format: OutputFormat,
) -> Result<i32, ForgeError> {
    let runner = default_runner();
    run_inspect_with(&runner, global, args, format, git_remote_url)
}

pub fn run_inspect_with<R: BackendRunner, F: Fn(&str) -> Option<String>>(
    runner: &R,
    global: &GlobalFlags,
    args: PrPendingReviewInspectArgs,
    format: OutputFormat,
    remote_url_lookup: F,
) -> Result<i32, ForgeError> {
    let (ctx, view, snapshot) =
        load_pending_snapshot(runner, global, args.id, &args.review, &remote_url_lookup)?;
    Ok(emit_success(
        schema_version_for(BINARY, "pr.pending-review.inspect", 1),
        PrPendingReviewInspectPayload {
            provider: ctx.provider.as_str(),
            number: view.number,
            url: view.url,
            snapshot,
        },
        format,
        |payload| {
            println!(
                "pending review {review} on #{number}: {comments} inline comment(s) [{provenance}]\n  {url}",
                review = payload.snapshot.review_id,
                number = payload.number,
                comments = payload.snapshot.inline_comments.len(),
                provenance = payload.snapshot.provenance,
                url = payload.snapshot.review_url,
            );
        },
    ))
}

pub fn run_resume_submit(
    global: &GlobalFlags,
    args: PrPendingReviewResumeSubmitArgs,
    format: OutputFormat,
) -> Result<i32, ForgeError> {
    let runner = default_runner();
    run_resume_submit_with(&runner, global, args, format, git_remote_url)
}

pub fn run_resume_submit_with<R: BackendRunner, F: Fn(&str) -> Option<String>>(
    runner: &R,
    global: &GlobalFlags,
    args: PrPendingReviewResumeSubmitArgs,
    format: OutputFormat,
    remote_url_lookup: F,
) -> Result<i32, ForgeError> {
    let (ctx, view, snapshot) =
        load_pending_snapshot_optional(runner, global, args.id, &args.review, &remote_url_lookup)?;
    let Some(snapshot) = snapshot else {
        return emit_already_submitted(
            runner,
            &ctx,
            view,
            &args.review,
            &args.review_run_id,
            &args.expected_head,
            &args.expected_commit,
            &args.expected_snapshot,
            args.decision,
            format,
        );
    };
    ensure_pending_author(&snapshot)?;
    let _lease = acquire_pending_review_lease(&ctx, &view, &snapshot.author)?;
    let snapshot = reload_pending_snapshot(runner, &ctx, &view, &args.review)?;
    validate_snapshot_cas(
        &snapshot,
        &args.expected_head,
        &args.expected_commit,
        &args.expected_snapshot,
    )?;
    if snapshot.provenance != "receipt-bound"
        || snapshot.review_run_id.as_deref() != Some(args.review_run_id.as_str())
    {
        return Err(ForgeError::validation(
            schema_err(),
            "pending_review_manifest_mismatch",
            "the pending review is not bound to the requested review transaction",
            Some(format!(
                "expected_run={}; observed_run={}",
                args.review_run_id,
                snapshot.review_run_id.as_deref().unwrap_or("<unmarked>")
            )),
        ));
    }
    let receipt = validate_review_run_receipt(
        runner,
        &ctx,
        &view,
        &args.review_run_id,
        &args.expected_head,
        args.decision,
    )?;
    validate_snapshot_against_receipt(&snapshot, &receipt)?;
    submit_snapshot(
        runner,
        &ctx,
        view,
        snapshot,
        args.decision,
        SubmitContract::ResumeV2,
        format,
    )
}

pub fn run_submit(
    global: &GlobalFlags,
    args: PrPendingReviewSubmitArgs,
    format: OutputFormat,
) -> Result<i32, ForgeError> {
    let runner = default_runner();
    run_submit_with(&runner, global, args, format, git_remote_url)
}

pub fn run_submit_with<R: BackendRunner, F: Fn(&str) -> Option<String>>(
    runner: &R,
    global: &GlobalFlags,
    args: PrPendingReviewSubmitArgs,
    format: OutputFormat,
    remote_url_lookup: F,
) -> Result<i32, ForgeError> {
    let (ctx, view, snapshot) =
        load_pending_snapshot(runner, global, args.id, &args.review, &remote_url_lookup)?;
    ensure_pending_author(&snapshot)?;
    let _lease = acquire_pending_review_lease(&ctx, &view, &snapshot.author)?;
    let snapshot = reload_pending_snapshot(runner, &ctx, &view, &args.review)?;
    validate_snapshot_cas(
        &snapshot,
        &args.expected_head,
        &args.expected_commit,
        &args.expected_snapshot,
    )?;
    if snapshot.provenance != "unmarked" {
        return Err(ForgeError::validation(
            schema_err(),
            "pending_review_manifest_mismatch",
            "guarded unmarked submit accepts only an unmarked pending review",
            Some(format!("provenance={}", snapshot.provenance)),
        ));
    }
    submit_snapshot(
        runner,
        &ctx,
        view,
        snapshot,
        args.decision,
        SubmitContract::DirectV1,
        format,
    )
}

pub fn run_discard(
    global: &GlobalFlags,
    args: PrPendingReviewDiscardArgs,
    format: OutputFormat,
) -> Result<i32, ForgeError> {
    let runner = default_runner();
    run_discard_with(&runner, global, args, format, git_remote_url)
}

pub fn run_discard_with<R: BackendRunner, F: Fn(&str) -> Option<String>>(
    runner: &R,
    global: &GlobalFlags,
    args: PrPendingReviewDiscardArgs,
    format: OutputFormat,
    remote_url_lookup: F,
) -> Result<i32, ForgeError> {
    let (ctx, view, snapshot) =
        load_pending_snapshot(runner, global, args.id, &args.review, &remote_url_lookup)?;
    ensure_pending_author(&snapshot)?;
    let _lease = acquire_pending_review_lease(&ctx, &view, &snapshot.author)?;
    let snapshot = reload_pending_snapshot(runner, &ctx, &view, &args.review)?;
    validate_snapshot_cas(
        &snapshot,
        &args.expected_head,
        &args.expected_commit,
        &args.expected_snapshot,
    )?;
    if !snapshot.viewer_did_author || !snapshot.viewer_can_delete {
        return Err(ForgeError::validation(
            schema_err(),
            "pending_review_identity_mismatch",
            "the invoking GitHub identity cannot discard this pending review",
            Some(format!("review_id={}", snapshot.review_id)),
        ));
    }
    if !snapshot.inline_comments.is_empty() && !args.confirm_inline_content_loss {
        return Err(ForgeError::validation(
            schema_err(),
            "pending_review_inline_discard_approval_required",
            "discarding inline review content requires --confirm-inline-content-loss",
            Some(format!(
                "review_id={}; inline_comment_count={}",
                snapshot.review_id,
                snapshot.inline_comments.len()
            )),
        ));
    }
    let mutation = runner.run(&pr_review::build_github_delete_pending_review_call(
        &ctx,
        &snapshot.review_id,
    ));
    let deleted_url = reconcile_deleted_snapshot(runner, &ctx, &snapshot, mutation)?;
    let commit_sha = snapshot.commit_sha.clone().expect("CAS checked commit");
    let inline_comment_count = snapshot.inline_comments.len();
    Ok(emit_success(
        schema_version_for(BINARY, "pr.pending-review.discard", 1),
        PrPendingReviewDiscardPayload {
            provider: ctx.provider.as_str(),
            number: view.number,
            url: view.url,
            review_id: snapshot.review_id,
            review_url: deleted_url,
            head_sha: snapshot.head_sha,
            commit_sha,
            snapshot_digest: snapshot.snapshot_digest,
            inline_comment_count,
            discarded: true,
        },
        format,
        |payload| {
            println!(
                "discarded pending review {review} from #{number}\n  {url}",
                review = payload.review_id,
                number = payload.number,
                url = payload.review_url
            );
        },
    ))
}

pub fn run_delete(
    global: &GlobalFlags,
    args: PrPendingReviewDeleteArgs,
    format: OutputFormat,
) -> Result<i32, ForgeError> {
    let runner = default_runner();
    run_delete_with(&runner, global, args, format, git_remote_url)
}

pub fn run_delete_with<R: BackendRunner, F: Fn(&str) -> Option<String>>(
    runner: &R,
    global: &GlobalFlags,
    args: PrPendingReviewDeleteArgs,
    format: OutputFormat,
    remote_url_lookup: F,
) -> Result<i32, ForgeError> {
    let ctx = detect(
        global.provider_hint(),
        &global.remote,
        global.repo.as_deref(),
        &remote_url_lookup,
    )?;
    ensure_github(&ctx)?;

    if global.dry_run {
        validate_expected_body_file_for_dry_run(&args)?;
        return emit_dry_run(&ctx, global, &args, format, &remote_url_lookup);
    }

    let expected_body = read_expected_body(&args)?;

    let view_output = runner.run(&pr_view::build_view_call(&ctx, &args.id.to_string()))?;
    let view = pr_view::parse_view_output(&ctx, &view_output)?;
    let snapshot = pr_reviews::compute_pending_guard_for_pr(
        runner,
        &ctx,
        view.number,
        &view.url,
        &args.review,
    )?;
    validate_expected_head(&snapshot.head_sha, &args)?;
    let pending = snapshot
        .reviews
        .iter()
        .find(|review| review.id == args.review)
        .ok_or_else(|| pending_not_found(args.id, &args.review))?;

    validate_pending_guard(pending, &snapshot.head_sha, &args, &expected_body)?;

    let target = pr_reviews::compute_pending_target(runner, &ctx, &args.review)?
        .ok_or_else(|| pending_not_found(args.id, &args.review))?;
    if target.number != view.number || target.pr_url != view.url || target.review.id != args.review
    {
        return Err(ForgeError::validation(
            schema_err(),
            "pending_review_pr_mismatch",
            "the pending review target no longer belongs to the named pull request",
            Some(format!(
                "expected_pr={}; provider_pr={}; review_id={}",
                view.number, target.number, args.review
            )),
        ));
    }
    validate_pending_guard(&target.review, &target.head_sha, &args, &expected_body)?;
    if target.inline_comment_count != 0 {
        return Err(ForgeError::validation(
            schema_err(),
            "pending_review_inline_comments_present",
            "pending reviews with inline draft comments require manual recovery",
            Some(format!(
                "review_id={}; inline_comment_count={}",
                target.review.id, target.inline_comment_count
            )),
        ));
    }

    let _lease = acquire_pending_review_lease(&ctx, &view, &target.review.author)?;
    let target = pr_reviews::compute_pending_target(runner, &ctx, &args.review)?
        .ok_or_else(|| pending_not_found(args.id, &args.review))?;
    if target.number != view.number || target.pr_url != view.url || target.review.id != args.review
    {
        return Err(ForgeError::validation(
            schema_err(),
            "pending_review_pr_mismatch",
            "the pending review target no longer belongs to the named pull request",
            Some(format!(
                "expected_pr={}; provider_pr={}; review_id={}",
                view.number, target.number, args.review
            )),
        ));
    }
    validate_pending_guard(&target.review, &target.head_sha, &args, &expected_body)?;
    if target.inline_comment_count != 0 {
        return Err(ForgeError::validation(
            schema_err(),
            "pending_review_inline_comments_present",
            "pending reviews with inline draft comments require manual recovery",
            Some(format!(
                "review_id={}; inline_comment_count={}",
                target.review.id, target.inline_comment_count
            )),
        ));
    }

    let pending = &target.review;
    let mutation = runner.run(&pr_review::build_github_delete_pending_review_call(
        &ctx,
        &pending.id,
    ));
    let deleted_url = reconcile_deleted_target(runner, &ctx, &target, mutation)?;

    let payload = PrPendingReviewDeletePayload {
        provider: ctx.provider.as_str(),
        number: view.number,
        url: view.url,
        head_sha: target.head_sha,
        commit_sha: pending
            .commit_sha
            .clone()
            .expect("commit presence checked before mutation"),
        review_id: pending.id.clone(),
        review_url: deleted_url,
        author: pending.author.clone(),
        deleted: true,
    };
    Ok(emit_success(schema_ok(), payload, format, render_text))
}

fn load_pending_snapshot<R: BackendRunner, F: Fn(&str) -> Option<String>>(
    runner: &R,
    global: &GlobalFlags,
    number: u64,
    review_id: &str,
    remote_url_lookup: &F,
) -> Result<
    (
        ProviderContext,
        pr_view::PrViewPayload,
        pr_reviews::PendingReviewSnapshot,
    ),
    ForgeError,
> {
    let (ctx, view, snapshot) =
        load_pending_snapshot_optional(runner, global, number, review_id, remote_url_lookup)?;
    let snapshot = snapshot.ok_or_else(|| pending_not_found(number, review_id))?;
    Ok((ctx, view, snapshot))
}

fn load_pending_snapshot_optional<R: BackendRunner, F: Fn(&str) -> Option<String>>(
    runner: &R,
    global: &GlobalFlags,
    number: u64,
    review_id: &str,
    remote_url_lookup: &F,
) -> Result<
    (
        ProviderContext,
        pr_view::PrViewPayload,
        Option<pr_reviews::PendingReviewSnapshot>,
    ),
    ForgeError,
> {
    let ctx = detect(
        global.provider_hint(),
        &global.remote,
        global.repo.as_deref(),
        remote_url_lookup,
    )?;
    ensure_github(&ctx)?;
    if global.dry_run {
        return Err(ForgeError::validation(
            schema_err(),
            "pending_review_snapshot_required",
            "pending-review recovery dry-run requires a live inspect snapshot; run pr pending-review inspect first",
            Some(format!("pr={number}; review_id={review_id}")),
        ));
    }
    let view_output = runner.run(&pr_view::build_view_call(&ctx, &number.to_string()))?;
    let view = pr_view::parse_view_output(&ctx, &view_output)?;
    let snapshot = pr_reviews::compute_pending_snapshot(runner, &ctx, review_id)?;
    if let Some(snapshot) = snapshot.as_ref()
        && (snapshot.number != view.number
            || snapshot.pr_url != view.url
            || snapshot.review_id != review_id)
    {
        return Err(ForgeError::validation(
            schema_err(),
            "pending_review_pr_mismatch",
            "the pending review target does not belong to the named pull request",
            Some(format!(
                "expected_pr={}; provider_pr={}; review_id={review_id}",
                view.number, snapshot.number
            )),
        ));
    }
    Ok((ctx, view, snapshot))
}

#[allow(clippy::too_many_arguments)]
fn emit_already_submitted<R: BackendRunner>(
    runner: &R,
    ctx: &ProviderContext,
    view: pr_view::PrViewPayload,
    review_id: &str,
    review_run_id: &str,
    expected_head: &str,
    expected_commit: &str,
    _expected_snapshot: &str,
    decision: PrReviewDecision,
    format: OutputFormat,
) -> Result<i32, ForgeError> {
    let initial_reviews = pr_reviews::compute_for_pr(runner, ctx, view.number, &view.url)?;
    if initial_reviews.viewer_login.is_empty() {
        return Err(ForgeError::validation(
            schema_err(),
            "pending_review_identity_mismatch",
            "the authenticated GitHub viewer identity is unavailable",
            Some(format!("review_id={review_id}")),
        ));
    }
    let _lease = acquire_pending_review_lease(ctx, &view, &initial_reviews.viewer_login)?;
    if pr_reviews::compute_pending_snapshot(runner, ctx, review_id)?.is_some() {
        return Err(expected_mismatch(
            "pending_review_manifest_mismatch",
            "the pending review changed while entering submitted-review recovery",
            format!("review_id={review_id}"),
        ));
    }
    let reviews = pr_reviews::compute_for_pr(runner, ctx, view.number, &view.url)?;
    if reviews.head_sha != expected_head {
        return Err(expected_mismatch(
            "pending_review_head_changed",
            "the pull-request head changed before pending-review recovery",
            format!(
                "expected_head={expected_head}; provider_head={}",
                reviews.head_sha
            ),
        ));
    }
    let mut matches = reviews
        .current_head_reviews
        .iter()
        .chain(reviews.stale_reviews.iter())
        .filter(|review| {
            !reviews.viewer_login.is_empty()
                && review.author == reviews.viewer_login
                && !review.summary_truncated
                && crate::ops::review_state::parse_review_run_id(&review.summary).as_deref()
                    == Some(review_run_id)
        });
    let submitted = matches
        .next()
        .ok_or_else(|| pending_not_found(view.number, review_id))?;
    if matches.next().is_some() {
        return Err(ForgeError::validation(
            schema_err(),
            "review_state_conflict",
            "multiple submitted reviews claim the same review-run id",
            Some(format!("review_run_id={review_run_id}")),
        ));
    }
    let expected_state = match decision {
        PrReviewDecision::CommentsOnly => "COMMENTED",
        PrReviewDecision::Approve => "APPROVED",
        PrReviewDecision::RequestChanges => "CHANGES_REQUESTED",
    };
    if submitted.id != review_id
        || submitted.commit_sha != expected_commit
        || submitted.state != expected_state
    {
        return Err(expected_mismatch(
            "pending_review_manifest_mismatch",
            "the submitted review does not match the requested recovery transaction",
            format!(
                "expected_review={review_id}; provider_review={}; expected_commit={expected_commit}; provider_commit={}; expected_state={expected_state}; provider_state={}",
                submitted.id, submitted.commit_sha, submitted.state
            ),
        ));
    }
    let receipt =
        validate_review_run_receipt(runner, ctx, &view, review_run_id, expected_head, decision)?;
    let submitted_snapshot =
        pr_reviews::compute_submitted_review_snapshot(runner, ctx, review_id, expected_state)?
            .ok_or_else(|| {
                expected_mismatch(
                    "pending_review_manifest_mismatch",
                    "the submitted review content snapshot is unavailable",
                    format!("review_id={review_id}; expected_state={expected_state}"),
                )
            })?;
    if submitted_snapshot.number != view.number
        || submitted_snapshot.pr_url != view.url
        || submitted_snapshot.review_id != submitted.id
        || submitted_snapshot.review_url != submitted.url
        || submitted_snapshot.head_sha != expected_head
        || submitted_snapshot.commit_sha.as_deref() != Some(expected_commit)
        || !submitted_snapshot.viewer_did_author
        || submitted_snapshot.author != reviews.viewer_login
        || submitted_snapshot.review_run_id.as_deref() != Some(review_run_id)
    {
        return Err(expected_mismatch(
            "pending_review_manifest_mismatch",
            "the submitted review content does not match the requested recovery transaction",
            format!(
                "expected_review={review_id}; provider_review={}; expected_head={expected_head}; provider_head={}; expected_commit={expected_commit}; provider_commit={}; expected_viewer={}; provider_author={}",
                submitted_snapshot.review_id,
                submitted_snapshot.head_sha,
                submitted_snapshot
                    .commit_sha
                    .as_deref()
                    .unwrap_or("<missing>"),
                reviews.viewer_login,
                submitted_snapshot.author,
            ),
        ));
    }
    validate_snapshot_against_receipt(&submitted_snapshot, &receipt)?;
    let payload = PrPendingReviewResumeSubmitPayload {
        provider: ctx.provider.as_str(),
        number: view.number,
        url: view.url,
        review_id: submitted.id.clone(),
        review_url: submitted.url.clone(),
        head_sha: reviews.head_sha,
        commit_sha: submitted.commit_sha.clone(),
        snapshot_digest: None,
        snapshot_provenance: "pending-snapshot-unverified",
        review_run_id: review_run_id.to_string(),
        submitted: true,
    };
    Ok(emit_success(
        schema_version_for(BINARY, "pr.pending-review.resume-submit", 2),
        payload,
        format,
        |payload| {
            println!(
                "pending review {review} was already submitted on #{number}\n  {url}",
                review = payload.review_id,
                number = payload.number,
                url = payload.review_url
            );
        },
    ))
}

fn validate_review_run_receipt<R: BackendRunner>(
    runner: &R,
    ctx: &ProviderContext,
    view: &pr_view::PrViewPayload,
    review_run_id: &str,
    expected_head: &str,
    decision: PrReviewDecision,
) -> Result<crate::ops::review_state::ReviewRunReceipt, ForgeError> {
    let repository =
        crate::ops::pr_comments::github_repo_slug_from_url(&view.url).ok_or_else(|| {
            ForgeError::software(
                schema_err(),
                "unable to derive GitHub owner/repo from PR url",
                Some(format!("url={}", view.url)),
            )
        })?;
    let chain = pr_review::read_review_state_chain(runner, ctx, &repository, view.number)?;
    let receipts = chain
        .records
        .iter()
        .filter_map(|record| match &record.payload {
            crate::ops::review_state::ReviewStatePayload::ReviewRunReceipt { receipt }
                if receipt.review_run_id == review_run_id =>
            {
                Some(receipt)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    if receipts.len() != 1 {
        return Err(ForgeError::validation(
            schema_err(),
            "review_state_conflict",
            "the review does not have exactly one immutable review-run receipt",
            Some(format!(
                "review_run_id={review_run_id}; receipt_count={}",
                receipts.len()
            )),
        ));
    }
    let receipt = receipts[0];
    if receipt.expected_head != expected_head || receipt.decision != decision.as_str() {
        return Err(expected_mismatch(
            "pending_review_manifest_mismatch",
            "the review receipt differs from the requested recovery transaction",
            format!(
                "expected_head={expected_head}; receipt_head={}; expected_decision={}; receipt_decision={}",
                receipt.expected_head,
                decision.as_str(),
                receipt.decision
            ),
        ));
    }
    let computed_run_id = crate::ops::review_state::compute_review_run_id(
        &repository,
        view.number,
        &receipt.expected_head,
        receipt.round,
        &receipt.route_lenses,
        &receipt.decision,
        &receipt.summary_digest,
        &receipt.inline_manifest,
    )?;
    if computed_run_id != receipt.review_run_id {
        return Err(ForgeError::validation(
            schema_err(),
            "review_state_conflict",
            "the immutable review receipt has an invalid review-run id",
            Some(format!(
                "expected_run={computed_run_id}; receipt_run={}",
                receipt.review_run_id
            )),
        ));
    }
    Ok(receipt.clone())
}

fn validate_snapshot_against_receipt(
    snapshot: &pr_reviews::PendingReviewSnapshot,
    receipt: &crate::ops::review_state::ReviewRunReceipt,
) -> Result<(), ForgeError> {
    let summary_digest = crate::ops::review_state::sha256_digest(snapshot.semantic_body.as_bytes());
    if summary_digest != receipt.summary_digest
        || snapshot.inline_comments.len() != receipt.inline_manifest.len()
    {
        return Err(expected_mismatch(
            "pending_review_manifest_mismatch",
            "the pending review summary or inline-comment count differs from the immutable receipt",
            format!(
                "expected_summary_digest={}; observed_summary_digest={summary_digest}; expected_comments={}; observed_comments={}",
                receipt.summary_digest,
                receipt.inline_manifest.len(),
                snapshot.inline_comments.len()
            ),
        ));
    }
    for (index, (comment, expected)) in snapshot
        .inline_comments
        .iter()
        .zip(&receipt.inline_manifest)
        .enumerate()
    {
        let file_anchor_matches = comment.subject_type == "FILE"
            && expected.subject_type == "FILE"
            && comment.line.is_none()
            && comment.diff_side.is_none()
            && comment.start_line.is_none()
            && comment.start_diff_side.is_none();
        let line_anchor_matches = comment.subject_type != "FILE"
            && expected.subject_type != "FILE"
            && comment.line == expected.line
            && comment.diff_side.as_deref() == Some(expected.side.as_str())
            && comment.start_line == expected.start_line
            && comment.start_diff_side == expected.start_side;
        let matches = expected.index == index
            && comment.review_run_id.as_deref() == Some(receipt.review_run_id.as_str())
            && comment.path == expected.path
            && comment.subject_type == expected.subject_type
            && (file_anchor_matches || line_anchor_matches)
            && comment.body_digest == expected.body_digest;
        if !matches {
            return Err(expected_mismatch(
                "pending_review_manifest_mismatch",
                "an inline pending-review comment differs from the immutable receipt",
                format!(
                    "review_id={}; comment_id={}; manifest_index={index}",
                    snapshot.review_id, comment.id
                ),
            ));
        }
    }
    Ok(())
}

fn validate_snapshot_cas(
    snapshot: &pr_reviews::PendingReviewSnapshot,
    expected_head: &str,
    expected_commit: &str,
    expected_snapshot: &str,
) -> Result<(), ForgeError> {
    if snapshot.head_sha != expected_head {
        return Err(expected_mismatch(
            "pending_review_head_changed",
            "the pull-request head changed before pending-review recovery",
            format!(
                "expected_head={expected_head}; provider_head={}",
                snapshot.head_sha
            ),
        ));
    }
    if snapshot.commit_sha.as_deref() != Some(expected_commit) {
        return Err(expected_mismatch(
            "pending_review_commit_mismatch",
            "the pending review is bound to a different commit",
            format!(
                "expected_commit={expected_commit}; provider_commit={}",
                snapshot.commit_sha.as_deref().unwrap_or("<missing>")
            ),
        ));
    }
    if snapshot.snapshot_digest != expected_snapshot {
        return Err(expected_mismatch(
            "pending_review_manifest_mismatch",
            "the pending review snapshot changed before recovery",
            format!(
                "expected_snapshot={expected_snapshot}; provider_snapshot={}",
                snapshot.snapshot_digest
            ),
        ));
    }
    Ok(())
}

fn submit_snapshot<R: BackendRunner>(
    runner: &R,
    ctx: &ProviderContext,
    view: pr_view::PrViewPayload,
    snapshot: pr_reviews::PendingReviewSnapshot,
    decision: PrReviewDecision,
    contract: SubmitContract,
    format: OutputFormat,
) -> Result<i32, ForgeError> {
    if !snapshot.viewer_did_author {
        return Err(ForgeError::validation(
            schema_err(),
            "pending_review_identity_mismatch",
            "the invoking GitHub identity is not the pending review author",
            Some(format!(
                "review_id={}; review_author={}",
                snapshot.review_id, snapshot.author
            )),
        ));
    }
    let mutation = runner.run(&pr_review::build_github_submit_review_call(
        ctx,
        &snapshot.review_id,
        decision.to_github_event(),
        Some(snapshot.body.as_str()),
    ));
    let expected_state = match decision {
        PrReviewDecision::CommentsOnly => "COMMENTED",
        PrReviewDecision::Approve => "APPROVED",
        PrReviewDecision::RequestChanges => "CHANGES_REQUESTED",
    };
    let submitted = pr_reviews::compute_submitted_review_snapshot(
        runner,
        ctx,
        &snapshot.review_id,
        expected_state,
    )?
    .ok_or_else(|| reconciliation_failed(&snapshot.review_id, mutation.as_ref().err()))?;
    validate_submitted_reconciliation(&snapshot, &submitted)?;
    if let Ok(output) = mutation.as_ref()
        && let Some(returned_url) = pr_review::parse_submitted_review_url(output)
        && returned_url != submitted.review_url
    {
        return Err(reconciliation_failed(
            &snapshot.review_id,
            Some(&ForgeError::software(
                schema_err(),
                "GitHub returned a different submitted review URL",
                Some(format!(
                    "mutation_url={returned_url}; reconciled_url={}",
                    submitted.review_url
                )),
            )),
        ));
    }
    let commit_sha = snapshot.commit_sha.clone().expect("CAS checked commit");
    let provider = ctx.provider.as_str();
    let number = view.number;
    let url = view.url;
    let review_id = snapshot.review_id;
    let review_url = submitted.review_url;
    let head_sha = snapshot.head_sha;
    let snapshot_digest = snapshot.snapshot_digest;
    let review_run_id = snapshot.review_run_id;
    Ok(match contract {
        SubmitContract::DirectV1 => emit_success(
            schema_version_for(BINARY, "pr.pending-review.submit", 1),
            PrPendingReviewSubmitPayload {
                provider,
                number,
                url,
                review_id,
                review_url,
                head_sha,
                commit_sha,
                snapshot_digest,
                snapshot_provenance: "pending-cas+submitted-reconciled",
                review_run_id,
                submitted: true,
            },
            format,
            |payload| {
                println!(
                    "submitted pending review {review} on #{number}\n  {url}",
                    review = payload.review_id,
                    number = payload.number,
                    url = payload.review_url
                );
            },
        ),
        SubmitContract::ResumeV2 => emit_success(
            schema_version_for(BINARY, "pr.pending-review.resume-submit", 2),
            PrPendingReviewResumeSubmitPayload {
                provider,
                number,
                url,
                review_id,
                review_url,
                head_sha,
                commit_sha,
                snapshot_digest: Some(snapshot_digest),
                snapshot_provenance: "pending-cas+submitted-reconciled",
                review_run_id: review_run_id.expect("receipt-bound recovery checked review run"),
                submitted: true,
            },
            format,
            |payload| {
                println!(
                    "submitted pending review {review} on #{number}\n  {url}",
                    review = payload.review_id,
                    number = payload.number,
                    url = payload.review_url
                );
            },
        ),
    })
}

fn ensure_pending_author(snapshot: &pr_reviews::PendingReviewSnapshot) -> Result<(), ForgeError> {
    if snapshot.viewer_did_author {
        return Ok(());
    }
    Err(ForgeError::validation(
        schema_err(),
        "pending_review_identity_mismatch",
        "the invoking GitHub identity is not the pending review author",
        Some(format!(
            "review_id={}; review_author={}",
            snapshot.review_id, snapshot.author
        )),
    ))
}

fn reload_pending_snapshot<R: BackendRunner>(
    runner: &R,
    ctx: &ProviderContext,
    view: &pr_view::PrViewPayload,
    review_id: &str,
) -> Result<pr_reviews::PendingReviewSnapshot, ForgeError> {
    let snapshot = pr_reviews::compute_pending_snapshot(runner, ctx, review_id)?
        .ok_or_else(|| pending_not_found(view.number, review_id))?;
    if snapshot.number != view.number
        || snapshot.pr_url != view.url
        || snapshot.review_id != review_id
    {
        return Err(ForgeError::validation(
            schema_err(),
            "pending_review_pr_mismatch",
            "the pending review target does not belong to the named pull request",
            Some(format!(
                "expected_pr={}; provider_pr={}; review_id={review_id}",
                view.number, snapshot.number
            )),
        ));
    }
    ensure_pending_author(&snapshot)?;
    Ok(snapshot)
}

fn validate_submitted_reconciliation(
    pending: &pr_reviews::PendingReviewSnapshot,
    submitted: &pr_reviews::PendingReviewSnapshot,
) -> Result<(), ForgeError> {
    let matches = submitted.number == pending.number
        && submitted.pr_url == pending.pr_url
        && submitted.head_sha == pending.head_sha
        && submitted.review_id == pending.review_id
        && submitted.review_url == pending.review_url
        && submitted.author == pending.author
        && submitted.commit_sha == pending.commit_sha
        && submitted.body == pending.body
        && submitted.semantic_body == pending.semantic_body
        && submitted.viewer_did_author
        && submitted.review_run_id == pending.review_run_id
        && submitted.provenance == pending.provenance
        && submitted.inline_comments == pending.inline_comments;
    if matches {
        return Ok(());
    }
    Err(reconciliation_failed(&pending.review_id, None))
}

fn reconciliation_failed(review_id: &str, mutation_error: Option<&ForgeError>) -> ForgeError {
    ForgeError::validation(
        schema_err(),
        "pending_review_reconciliation_failed",
        "the pending-review mutation could not be reconciled to the exact provider state",
        Some(match mutation_error {
            Some(error) => format!("review_id={review_id}; mutation_error={error}"),
            None => format!("review_id={review_id}"),
        }),
    )
}

fn reconcile_deleted_snapshot<R: BackendRunner>(
    runner: &R,
    ctx: &ProviderContext,
    snapshot: &pr_reviews::PendingReviewSnapshot,
    mutation: Result<BackendSuccess, ForgeError>,
) -> Result<String, ForgeError> {
    let remaining = pr_reviews::compute_pending_snapshot(runner, ctx, &snapshot.review_id)?;
    let deleted_url = reconciled_deleted_url(mutation, &snapshot.review_id, &snapshot.review_url)?;
    if remaining.is_some() {
        return Err(reconciliation_failed(&snapshot.review_id, None));
    }
    Ok(deleted_url)
}

fn reconcile_deleted_target<R: BackendRunner>(
    runner: &R,
    ctx: &ProviderContext,
    target: &pr_reviews::PendingReviewTarget,
    mutation: Result<BackendSuccess, ForgeError>,
) -> Result<String, ForgeError> {
    let remaining = pr_reviews::compute_pending_target(runner, ctx, &target.review.id)?;
    let deleted_url = reconciled_deleted_url(mutation, &target.review.id, &target.review.url)?;
    if remaining.is_some() {
        return Err(reconciliation_failed(&target.review.id, None));
    }
    Ok(deleted_url)
}

fn reconciled_deleted_url(
    mutation: Result<BackendSuccess, ForgeError>,
    expected_id: &str,
    fallback_url: &str,
) -> Result<String, ForgeError> {
    let Ok(output) = mutation else {
        return Ok(fallback_url.to_string());
    };
    let (deleted_id, deleted_url) = parse_deleted_review(&output)?;
    if deleted_id != expected_id {
        return Err(ForgeError::software(
            schema_err(),
            "GitHub returned a different review after pending-review deletion",
            Some(format!(
                "expected_review_id={expected_id}; provider_review_id={deleted_id}"
            )),
        ));
    }
    Ok(deleted_url)
}

pub(crate) struct PendingReviewLease(File);

impl Drop for PendingReviewLease {
    fn drop(&mut self) {
        unlock_file(self.0.as_raw_fd());
    }
}

fn acquire_pending_review_lease(
    ctx: &ProviderContext,
    view: &pr_view::PrViewPayload,
    viewer: &str,
) -> Result<PendingReviewLease, ForgeError> {
    acquire_pending_review_lease_for(ctx, &view.url, view.number, viewer)
}

pub(crate) fn acquire_pending_review_lease_for(
    ctx: &ProviderContext,
    pr_url: &str,
    number: u64,
    viewer: &str,
) -> Result<PendingReviewLease, ForgeError> {
    if viewer.is_empty() {
        return Err(ForgeError::validation(
            schema_err(),
            "pending_review_identity_mismatch",
            "the authenticated GitHub viewer identity is unavailable",
            Some(format!("pr={number}")),
        ));
    }
    let repository = crate::ops::pr_comments::github_repo_slug_from_url(pr_url)
        .or_else(|| ctx.repo.clone())
        .ok_or_else(|| {
            ForgeError::validation(
                schema_err(),
                "repo_required",
                "pending-review recovery requires a repository slug",
                None,
            )
        })?;
    let state_root = pending_review_state_root()?;
    let lease_dir = state_root.join("forge-cli").join("pending-review-leases");
    let key = format!(
        "{}\n{}\n{}\n{}\n{}",
        ctx.provider.as_str(),
        ctx.host.to_ascii_lowercase(),
        repository.to_ascii_lowercase(),
        number,
        viewer.to_ascii_lowercase()
    );
    acquire_pending_review_lease_at(&lease_dir, &key)
}

fn pending_review_state_root() -> Result<PathBuf, ForgeError> {
    let path = env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))
        .ok_or_else(|| lease_unsafe("neither XDG_STATE_HOME nor HOME is available"))?;
    if !path.is_absolute() {
        return Err(lease_unsafe("pending-review state root is not absolute"));
    }
    Ok(path)
}

fn acquire_pending_review_lease_at(
    lease_dir: &Path,
    key: &str,
) -> Result<PendingReviewLease, ForgeError> {
    if !lease_dir.exists() {
        fs::create_dir_all(lease_dir)
            .map_err(|error| lease_unsafe(&format!("failed to create lease directory: {error}")))?;
        fs::set_permissions(lease_dir, fs::Permissions::from_mode(0o700))
            .map_err(|error| lease_unsafe(&format!("failed to secure lease directory: {error}")))?;
    }
    let directory = fs::symlink_metadata(lease_dir)
        .map_err(|error| lease_unsafe(&format!("failed to inspect lease directory: {error}")))?;
    if !directory.file_type().is_dir()
        || directory.uid() != effective_uid()
        || directory.mode() & 0o077 != 0
    {
        return Err(lease_unsafe(
            "pending-review lease directory is not a private viewer-owned directory",
        ));
    }

    let digest = Sha256::digest(key.as_bytes());
    let mut digest_hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        write!(&mut digest_hex, "{byte:02x}").expect("writing to String cannot fail");
    }
    let lock_path = lease_dir.join(format!("{digest_hex}.lock"));
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    let file = options
        .open(&lock_path)
        .map_err(|error| lease_unsafe(&format!("failed to open lease file: {error}")))?;
    let metadata = file
        .metadata()
        .map_err(|error| lease_unsafe(&format!("failed to inspect lease file: {error}")))?;
    if !metadata.file_type().is_file()
        || metadata.uid() != effective_uid()
        || metadata.mode() & 0o077 != 0
    {
        return Err(lease_unsafe(
            "pending-review lease file is not a private viewer-owned regular file",
        ));
    }
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        let error = std::io::Error::last_os_error();
        let raw = error.raw_os_error();
        if raw == Some(libc::EWOULDBLOCK) || raw == Some(libc::EAGAIN) {
            return Err(ForgeError::unavailable(
                schema_err(),
                "pending_review_lease_busy",
                "another trusted forge-cli process is mutating this viewer's pending review",
                None,
            ));
        }
        return Err(lease_unsafe(&format!("failed to acquire lease: {error}")));
    }
    Ok(PendingReviewLease(file))
}

fn lease_unsafe(detail: &str) -> ForgeError {
    ForgeError::validation(
        schema_err(),
        "pending_review_lease_unsafe",
        "the pending-review mutation lease cannot be used safely",
        Some(detail.to_string()),
    )
}

fn effective_uid() -> u32 {
    unsafe { libc::geteuid() }
}

fn unlock_file(fd: RawFd) {
    let _ = unsafe { libc::flock(fd, libc::LOCK_UN) };
}

fn validate_pending_guard(
    pending: &pr_reviews::PendingReviewGuard,
    provider_head: &str,
    args: &PrPendingReviewDeleteArgs,
    expected_body: &str,
) -> Result<(), ForgeError> {
    validate_expected_head(provider_head, args)?;
    if !pending.viewer_did_author {
        return Err(ForgeError::validation(
            schema_err(),
            "pending_review_author_mismatch",
            "the pending review is not authored by the invoking GitHub identity",
            Some(format!(
                "review_id={}; review_author={}",
                pending.id, pending.author
            )),
        ));
    }
    if !pending.viewer_can_delete {
        return Err(ForgeError::validation(
            schema_err(),
            "pending_review_not_deletable",
            "the invoking GitHub identity cannot delete the pending review",
            Some(format!("review_id={}", pending.id)),
        ));
    }
    if pending.commit_sha.as_deref() != Some(args.expected_commit.as_str()) {
        return Err(expected_mismatch(
            "pending_review_commit_mismatch",
            "the pending review is bound to a different commit",
            format!(
                "review_id={}; expected_commit={}; provider_commit={}",
                pending.id,
                args.expected_commit,
                pending.commit_sha.as_deref().unwrap_or("<missing>")
            ),
        ));
    }
    if normalize_body(&pending.body) != normalize_body(expected_body) {
        return Err(expected_mismatch(
            "pending_review_body_mismatch",
            "the pending review body changed before deletion",
            format!(
                "review_id={}; expected_body_bytes={}; provider_body_bytes={}",
                pending.id,
                expected_body.len(),
                pending.body.len()
            ),
        ));
    }
    Ok(())
}

fn validate_expected_head(
    provider_head: &str,
    args: &PrPendingReviewDeleteArgs,
) -> Result<(), ForgeError> {
    if provider_head == args.expected_head {
        return Ok(());
    }
    Err(expected_mismatch(
        "pending_review_head_mismatch",
        "the pull-request head changed before pending-review deletion",
        format!(
            "expected_head={}; provider_head={provider_head}",
            args.expected_head
        ),
    ))
}

fn emit_dry_run<F: Fn(&str) -> Option<String>>(
    ctx: &ProviderContext,
    global: &GlobalFlags,
    args: &PrPendingReviewDeleteArgs,
    format: OutputFormat,
    remote_url_lookup: &F,
) -> Result<i32, ForgeError> {
    let slug = pr_reviews::resolve_repo_slug(ctx, &global.remote, remote_url_lookup)?;
    let (owner, name) = pr_reviews::split_slug(&slug)?;
    let guard = pr_view::build_view_call(ctx, &args.id.to_string());
    let snapshot = pr_reviews::build_github_pending_reviews_call(ctx, owner, name, args.id, None);
    let delete = pr_review::build_github_delete_pending_review_call(ctx, &args.review);
    let payload = PrPendingReviewDeleteDryRunPayload {
        provider: ctx.provider.as_str(),
        number: args.id,
        review_id: args.review.clone(),
        expected_head: args.expected_head.clone(),
        expected_commit: args.expected_commit.clone(),
        expected_inline_comment_count: 0,
        confirmed_abandoned: args.confirm_abandoned,
        guard_plan: guard.plan_argv(),
        snapshot_plan: snapshot.plan_argv(),
        target_plan: pr_reviews::build_github_pending_review_target_call(ctx, &args.review)
            .plan_argv(),
        delete_plan: delete.plan_argv(),
    };
    Ok(emit_success(schema_ok(), payload, format, |payload| {
        println!(
            "would verify and delete pending review {review} from #{number}",
            review = payload.review_id,
            number = payload.number
        );
    }))
}

fn ensure_github(ctx: &ProviderContext) -> Result<(), ForgeError> {
    if matches!(ctx.provider, Provider::GitHub) {
        return Ok(());
    }
    Err(ForgeError::provider_unsupported(
        schema_err(),
        format!(
            "pr pending-review recovery is GitHub-only in v1 (provider: {})",
            ctx.provider.as_str()
        ),
        None,
    ))
}

fn parse_deleted_review(output: &BackendSuccess) -> Result<(String, String), ForgeError> {
    let value: serde_json::Value = serde_json::from_str(output.stdout.trim()).map_err(|err| {
        ForgeError::software(
            schema_err(),
            "pending-review delete response is invalid JSON",
            Some(err.to_string()),
        )
    })?;
    if value
        .get("errors")
        .and_then(|errors| errors.as_array())
        .is_some_and(|errors| !errors.is_empty())
    {
        return Err(ForgeError::backend_error(
            schema_err(),
            "GitHub rejected pending-review deletion",
            Some(format!(
                "graphql_errors={}",
                value["errors"].as_array().map_or(0, Vec::len)
            )),
        ));
    }
    let id = required_pointer_string(
        &value,
        "/data/deletePullRequestReview/pullRequestReview/id",
        "deleted review id",
    )?;
    let url = required_pointer_string(
        &value,
        "/data/deletePullRequestReview/pullRequestReview/url",
        "deleted review url",
    )?;
    Ok((id, url))
}

fn required_pointer_string(
    value: &serde_json::Value,
    pointer: &str,
    field: &str,
) -> Result<String, ForgeError> {
    value
        .pointer(pointer)
        .and_then(|item| item.as_str())
        .filter(|item| !item.is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            ForgeError::software(
                schema_err(),
                "pending-review delete response is missing a required field",
                Some(format!("field={field}")),
            )
        })
}

fn pending_not_found(number: u64, review_id: &str) -> ForgeError {
    ForgeError::validation(
        schema_err(),
        "pending_review_not_found",
        "the named review is not a pending review on the target pull request",
        Some(format!("pr={number}; review_id={review_id}")),
    )
}

fn validate_expected_body_file_for_dry_run(
    args: &PrPendingReviewDeleteArgs,
) -> Result<(), ForgeError> {
    if let Some(body) = args.expected_body.as_deref() {
        return validate_expected_body_size(body);
    }
    if args.expected_body_file.as_deref() == Some("-") {
        return Ok(());
    }
    let path = args
        .expected_body_file
        .as_deref()
        .expect("clap requires one expected body source");
    let file = fs::File::open(path).map_err(|_| expected_body_read_error())?;
    read_expected_body_bounded(file).map(|_| ())
}

fn read_expected_body(args: &PrPendingReviewDeleteArgs) -> Result<String, ForgeError> {
    if let Some(body) = args.expected_body.as_deref() {
        validate_expected_body_size(body)?;
        return Ok(body.to_string());
    }
    let path = args
        .expected_body_file
        .as_deref()
        .expect("clap requires one expected body source");
    if path == "-" {
        return read_expected_body_bounded(std::io::stdin().lock());
    }
    let file = fs::File::open(path).map_err(|_| expected_body_read_error())?;
    read_expected_body_bounded(file)
}

fn read_expected_body_bounded<R: Read>(reader: R) -> Result<String, ForgeError> {
    let mut body = Vec::new();
    reader
        .take((pr_reviews::MAX_PENDING_REVIEW_BODY_BYTES + 1) as u64)
        .read_to_end(&mut body)
        .map_err(|_| expected_body_read_error())?;
    validate_expected_body_byte_len(body.len())?;
    String::from_utf8(body).map_err(|_| expected_body_read_error())
}

fn validate_expected_body_size(body: &str) -> Result<(), ForgeError> {
    validate_expected_body_byte_len(body.len())
}

fn validate_expected_body_byte_len(body_bytes: usize) -> Result<(), ForgeError> {
    if body_bytes <= pr_reviews::MAX_PENDING_REVIEW_BODY_BYTES {
        return Ok(());
    }
    Err(ForgeError::validation(
        schema_err(),
        "pending_review_body_too_large",
        "the expected pending-review body exceeds the recovery safety limit",
        Some(format!(
            "body_bytes={}; max_bytes={}",
            body_bytes,
            pr_reviews::MAX_PENDING_REVIEW_BODY_BYTES
        )),
    ))
}

fn expected_body_read_error() -> ForgeError {
    ForgeError::software(schema_err(), "failed to read --expected-body-file", None)
}

fn expected_mismatch(code: &'static str, message: &'static str, detail: String) -> ForgeError {
    ForgeError::validation(schema_err(), code, message, Some(detail))
}

fn normalize_body(body: &str) -> &str {
    body.trim_end_matches(['\r', '\n'])
}

fn schema_ok() -> String {
    schema_version_for(BINARY, SCHEMA, SCHEMA_VERSION)
}

fn schema_err() -> String {
    schema_version_for(BINARY, "error", 1)
}

fn render_text(payload: &PrPendingReviewDeletePayload) {
    println!(
        "deleted pending review {review} by {author} from #{number}\n  {url}",
        review = payload.review_id,
        author = payload.author,
        number = payload.number,
        url = payload.review_url,
    );
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use tempfile::TempDir;

    use super::*;

    #[test]
    fn pending_review_lease_excludes_a_concurrent_holder() {
        let temp = TempDir::new().expect("tempdir");
        let lease_dir = temp.path().join("leases");
        let first = acquire_pending_review_lease_at(&lease_dir, "github/repo/7/viewer")
            .expect("first lease");
        let error = match acquire_pending_review_lease_at(&lease_dir, "github/repo/7/viewer") {
            Err(error) => error,
            Ok(_) => panic!("concurrent holder must be rejected"),
        };
        assert_eq!(error.kind(), "pending_review_lease_busy");
        drop(first);
        acquire_pending_review_lease_at(&lease_dir, "github/repo/7/viewer")
            .expect("lease is available after release");
    }

    #[test]
    fn pending_review_lease_rejects_a_symlinked_directory() {
        let temp = TempDir::new().expect("tempdir");
        let target = temp.path().join("target");
        fs::create_dir(&target).expect("target dir");
        fs::set_permissions(&target, fs::Permissions::from_mode(0o700)).expect("private target");
        let lease_dir = temp.path().join("leases");
        symlink(&target, &lease_dir).expect("symlink lease dir");

        let error = match acquire_pending_review_lease_at(&lease_dir, "github/repo/7/viewer") {
            Err(error) => error,
            Ok(_) => panic!("symlinked lease directory must be rejected"),
        };
        assert_eq!(error.kind(), "pending_review_lease_unsafe");
    }
}
