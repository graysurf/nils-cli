//! Authenticated recovery for provider-valid pending GitHub reviews.

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
    guard_plan: Vec<String>,
    snapshot_plan: Vec<String>,
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
        return emit_dry_run(&ctx, global, &args, format, &remote_url_lookup);
    }

    let view_output = runner.run(&pr_view::build_view_call(&ctx, &args.id.to_string()))?;
    let view = pr_view::parse_view_output(&ctx, &view_output)?;
    let snapshot = pr_reviews::compute_pending_guards_for_pr(runner, &ctx, view.number, &view.url)?;
    let pending = snapshot
        .reviews
        .iter()
        .find(|review| review.id == args.review)
        .ok_or_else(|| pending_not_found(args.id, &args.review))?;

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
        head_sha: snapshot.head_sha,
        review_id: pending.id.clone(),
        review_url: deleted_url,
        author: pending.author.clone(),
        deleted: true,
    };
    Ok(emit_success(schema_ok(), payload, format, render_text))
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
        guard_plan: guard.plan_argv(),
        snapshot_plan: snapshot.plan_argv(),
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
