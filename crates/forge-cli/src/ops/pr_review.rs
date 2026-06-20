//! `pr review` atom.
//!
//! Spec / ops: `cli.forge-cli.pr.review.v1`. This is intentionally a posting
//! primitive, not a review-orchestration engine: callers pass an already
//! rendered review outcome, and forge-cli posts it to the PR/MR plus an
//! optional compact issue activity mirror.

use std::ffi::OsString;

use nils_common::cli_contract::{OutputFormat, schema_version_for};
use serde::Serialize;

use crate::backend::{BackendCall, BackendProgram, BackendRunner, ProcessRunner};
use crate::cli::{BINARY, GlobalFlags, PrReviewArgs, PrReviewDecision};
use crate::envelope::emit_success;
use crate::error::ForgeError;
use crate::ops::pr_comment;
use crate::provider::{Provider, ProviderContext, detect, git_remote_url};
use crate::validations::{no_escaped_control_markdown, no_local_path};

const SCHEMA: &str = "pr.review";
const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PrReviewPayload {
    pub provider: &'static str,
    pub number: u64,
    pub decision: &'static str,
    pub pr_comment_url: String,
    pub issue_number: Option<u64>,
    pub issue_comment_url: Option<String>,
    pub mirrored: bool,
    pub lenses: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
struct PrReviewDryRunPayload {
    provider: &'static str,
    number: u64,
    decision: &'static str,
    plan: Vec<String>,
    issue_number: Option<u64>,
    issue_plan: Option<Vec<String>>,
    mirror_issue: bool,
    lenses: Vec<String>,
}

pub fn run(
    global: &GlobalFlags,
    args: PrReviewArgs,
    format: OutputFormat,
) -> Result<i32, ForgeError> {
    let runner = ProcessRunner;
    run_with(&runner, global, args, format, git_remote_url)
}

pub fn run_with<R: BackendRunner, F: Fn(&str) -> Option<String>>(
    runner: &R,
    global: &GlobalFlags,
    args: PrReviewArgs,
    format: OutputFormat,
    remote_url_lookup: F,
) -> Result<i32, ForgeError> {
    let ctx = detect(
        global.provider_hint(),
        &global.remote,
        global.repo.as_deref(),
        remote_url_lookup,
    )?;
    if ctx.provider == Provider::Local {
        return Err(ForgeError::provider_unsupported(
            schema_err(),
            "pr review is not supported by provider 'local' in v1",
            None,
        ));
    }

    let body = pr_comment::read_body_with_file_flag(
        args.comment.as_deref(),
        args.comment_file.as_deref(),
        "--comment-file",
    )?;
    if body.trim().is_empty() {
        return Err(ForgeError::validation(
            schema_err(),
            "body_missing_summary",
            "review comment body is empty (supply --comment or --comment-file)",
            None,
        ));
    }
    no_local_path(&body, "review comment")?;
    no_escaped_control_markdown(&body)?;

    let pr_call = build_pr_comment_call(&ctx, args.id, &body);

    if global.dry_run {
        let issue_plan = if args.mirror_issue {
            args.issue.map(|issue| {
                let mirror_body = build_issue_mirror_body(
                    args.id,
                    args.decision,
                    &args.lenses,
                    "<pr-comment-url-unavailable-in-dry-run>",
                );
                build_issue_comment_call(&ctx, issue, &mirror_body).plan_argv()
            })
        } else {
            None
        };
        return Ok(emit_success(
            schema_version(),
            PrReviewDryRunPayload {
                provider: ctx.provider.as_str(),
                number: args.id,
                decision: args.decision.as_str(),
                plan: pr_call.plan_argv(),
                issue_number: args.issue,
                issue_plan,
                mirror_issue: args.mirror_issue,
                lenses: args.lenses.clone(),
            },
            format,
            |p| {
                println!("would post review outcome: {plan}", plan = p.plan.join(" "));
                if let Some(issue_plan) = p.issue_plan.as_ref() {
                    println!(
                        "would mirror issue activity: {plan}",
                        plan = issue_plan.join(" ")
                    );
                }
            },
        ));
    }

    let pr_output = runner.run(&pr_call)?;
    let pr_comment_url = first_url(&pr_output.stdout).unwrap_or_default();

    let issue_comment_url = if args.mirror_issue {
        let issue_number = args.issue.ok_or_else(|| {
            ForgeError::validation(
                schema_err(),
                "issue_required",
                "--mirror-issue requires --issue <ISSUE_NUMBER>",
                None,
            )
        })?;
        let mirror_body =
            build_issue_mirror_body(args.id, args.decision, &args.lenses, &pr_comment_url);
        no_escaped_control_markdown(&mirror_body)?;
        let issue_call = build_issue_comment_call(&ctx, issue_number, &mirror_body);
        let issue_output = runner.run(&issue_call)?;
        first_url(&issue_output.stdout)
    } else {
        None
    };

    Ok(emit_success(
        schema_version(),
        PrReviewPayload {
            provider: ctx.provider.as_str(),
            number: args.id,
            decision: args.decision.as_str(),
            pr_comment_url,
            issue_number: args.issue,
            issue_comment_url,
            mirrored: args.mirror_issue,
            lenses: args.lenses,
        },
        format,
        render_text,
    ))
}

fn build_pr_comment_call(ctx: &ProviderContext, id: u64, body: &str) -> BackendCall {
    let program = BackendProgram::for_provider(ctx.provider);
    let mut argv: Vec<OsString> = match ctx.provider {
        Provider::GitHub => github_issue_comment_argv(ctx, id, body),
        Provider::GitLab => vec![
            OsString::from("mr"),
            OsString::from("note"),
            OsString::from(id.to_string()),
            OsString::from("--message"),
            OsString::from(body),
        ],
        Provider::Local => vec![
            OsString::from("issue"),
            OsString::from("comment"),
            OsString::from(id.to_string()),
            OsString::from("--body"),
            OsString::from(body),
        ],
    };
    if ctx.provider == Provider::GitLab {
        ctx.push_repo_override(&mut argv);
    }
    BackendCall::new(program, argv)
}

fn build_issue_comment_call(ctx: &ProviderContext, id: u64, body: &str) -> BackendCall {
    let program = BackendProgram::for_provider(ctx.provider);
    let mut argv: Vec<OsString> = match ctx.provider {
        Provider::GitHub => github_issue_comment_argv(ctx, id, body),
        Provider::GitLab => vec![
            OsString::from("issue"),
            OsString::from("note"),
            OsString::from(id.to_string()),
            OsString::from("--message"),
            OsString::from(body),
        ],
        Provider::Local => vec![
            OsString::from("issue"),
            OsString::from("comment"),
            OsString::from(id.to_string()),
            OsString::from("--body"),
            OsString::from(body),
        ],
    };
    if ctx.provider == Provider::GitLab {
        ctx.push_repo_override(&mut argv);
    }
    BackendCall::new(program, argv)
}

fn github_issue_comment_argv(ctx: &ProviderContext, id: u64, body: &str) -> Vec<OsString> {
    let endpoint = ctx
        .repo
        .as_deref()
        .map(|repo| format!("repos/{repo}/issues/{id}/comments"))
        .unwrap_or_else(|| format!("repos/{{owner}}/{{repo}}/issues/{id}/comments"));
    let mut argv = vec![OsString::from("api")];
    ctx.push_github_api_hostname(&mut argv);
    argv.extend([
        OsString::from(endpoint),
        OsString::from("--method"),
        OsString::from("POST"),
        OsString::from("--raw-field"),
        OsString::from(format!("body={body}")),
        OsString::from("--jq"),
        OsString::from(".html_url"),
    ]);
    argv
}

fn build_issue_mirror_body(
    pr_number: u64,
    decision: PrReviewDecision,
    lenses: &[String],
    pr_comment_url: &str,
) -> String {
    let lenses = if lenses.is_empty() {
        "unspecified".to_string()
    } else {
        lenses.join(", ")
    };
    format!(
        "PR review outcome posted.\n\n- pr: #{pr_number}\n- decision: {decision}\n- lenses: {lenses}\n- review_url={pr_comment_url}\n",
        decision = decision.as_str(),
    )
}

fn first_url(stdout: &str) -> Option<String> {
    stdout.split_whitespace().find_map(|token| {
        let url = token.trim_matches(|ch: char| {
            matches!(
                ch,
                '"' | '\'' | '`' | '<' | '>' | '(' | ')' | '[' | ']' | ','
            )
        });
        (url.starts_with("http://") || url.starts_with("https://") || url.starts_with("local://"))
            .then(|| url.to_string())
    })
}

fn schema_version() -> String {
    schema_version_for(BINARY, SCHEMA, SCHEMA_VERSION)
}

fn schema_err() -> String {
    schema_version_for(BINARY, "error", 1)
}

fn render_text(payload: &PrReviewPayload) {
    if let Some(issue_url) = payload.issue_comment_url.as_deref() {
        println!(
            "posted {decision} review outcome on {provider} #{number}: {pr_url}; mirrored issue activity: {issue_url}",
            decision = payload.decision,
            provider = payload.provider,
            number = payload.number,
            pr_url = payload.pr_comment_url,
        );
    } else {
        println!(
            "posted {decision} review outcome on {provider} #{number}: {pr_url}",
            decision = payload.decision,
            provider = payload.provider,
            number = payload.number,
            pr_url = payload.pr_comment_url,
        );
    }
}
