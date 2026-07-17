//! Authenticated recovery for provider-valid pending GitHub reviews.

use std::fs;
use std::io::Read;

use nils_common::cli_contract::{OutputFormat, schema_version_for};
use serde::Serialize;

use crate::backend::{BackendRunner, BackendSuccess};
use crate::cli::{BINARY, GlobalFlags, PrPendingReviewDeleteArgs};
use crate::envelope::emit_success;
use crate::error::ForgeError;
use crate::ops::{pr_review, pr_reviews, pr_view};
use crate::provider::{Provider, ProviderContext, detect, git_remote_url};
use crate::rate_limit::default_runner;

const SCHEMA: &str = "pr.pending-review.delete";
const SCHEMA_VERSION: u32 = 1;

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

    let pending = &target.review;
    let deleted_output = runner.run(&pr_review::build_github_delete_pending_review_call(
        &ctx,
        &pending.id,
    ))?;
    let (deleted_id, deleted_url) = parse_deleted_review(&deleted_output)?;
    if deleted_id != pending.id {
        return Err(ForgeError::software(
            schema_err(),
            "GitHub returned a different review after pending-review deletion",
            Some(format!(
                "expected_review_id={}; provider_review_id={deleted_id}",
                pending.id
            )),
        ));
    }

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
            "pr pending-review delete is GitHub-only in v1 (provider: {})",
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
