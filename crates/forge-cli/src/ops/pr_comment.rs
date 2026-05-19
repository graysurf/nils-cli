//! `pr comment` atom.
//!
//! Spec / ops: `cli.forge-cli.pr.comment.v1`. Appends a comment to a PR/MR.
//! Body can come from `--body`, `--body-file <path>`, or
//! `--body-file -` (stdin). No lock-down validation runs — comments don't
//! mutate PR state. The envelope payload reports `{ provider, number,
//! url }` where `url` is the PR/MR URL (cheap re-fetch via `pr view`).

use std::ffi::OsString;
use std::fs;
use std::io::Read as _;

use nils_common::cli_contract::{OutputFormat, schema_version_for};
use serde::Serialize;

use crate::backend::{BackendCall, BackendProgram, BackendRunner, DryRunPayload, ProcessRunner};
use crate::cli::{BINARY, GlobalFlags, PrCommentArgs};
use crate::envelope::emit_success;
use crate::error::ForgeError;
use crate::ops::pr_view;
use crate::provider::{Provider, ProviderContext, detect, git_remote_url};

const SCHEMA: &str = "pr.comment";
const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PrCommentPayload {
    pub provider: &'static str,
    pub number: u64,
    pub url: String,
}

pub fn run(
    global: &GlobalFlags,
    args: PrCommentArgs,
    format: OutputFormat,
) -> Result<i32, ForgeError> {
    let runner = ProcessRunner;
    run_with(&runner, global, args, format, git_remote_url)
}

pub fn run_with<R: BackendRunner, F: Fn(&str) -> Option<String>>(
    runner: &R,
    global: &GlobalFlags,
    args: PrCommentArgs,
    format: OutputFormat,
    remote_url_lookup: F,
) -> Result<i32, ForgeError> {
    let ctx = detect(global.provider_hint(), &global.remote, remote_url_lookup)?;
    let body = read_body(args.body.as_deref(), args.body_file.as_deref())?;
    if body.trim().is_empty() {
        return Err(ForgeError::validation(
            schema_err(),
            "body_missing_summary",
            "comment body is empty (supply --body or --body-file)",
            None,
        ));
    }
    let call = build_comment_call(&ctx, args.id, &body);

    if global.dry_run {
        let payload = DryRunPayload::new(ctx.provider, &call);
        return Ok(emit_success(
            schema_version_for(BINARY, SCHEMA, SCHEMA_VERSION),
            payload,
            format,
            |p| println!("would run: {plan}", plan = p.plan.join(" ")),
        ));
    }

    let _ = runner.run(&call)?;

    // Re-fetch the PR/MR URL via the canonical view call so consumers can
    // chain the envelope into next steps. Body isn't included.
    let view_call = pr_view_call(&ctx, args.id);
    let view_output = runner.run(&view_call)?;
    let view = pr_view::parse_view_output(&ctx, &view_output)?;
    Ok(emit_success(
        schema_version_for(BINARY, SCHEMA, SCHEMA_VERSION),
        PrCommentPayload {
            provider: view.provider,
            number: view.number,
            url: view.url,
        },
        format,
        render_text,
    ))
}

fn build_comment_call(ctx: &ProviderContext, id: u64, body: &str) -> BackendCall {
    let program = BackendProgram::for_provider(ctx.provider);
    let argv: Vec<OsString> = match ctx.provider {
        Provider::GitHub => vec![
            OsString::from("pr"),
            OsString::from("comment"),
            OsString::from(id.to_string()),
            OsString::from("--body"),
            OsString::from(body),
        ],
        Provider::GitLab => vec![
            OsString::from("mr"),
            OsString::from("note"),
            OsString::from(id.to_string()),
            OsString::from("--message"),
            OsString::from(body),
        ],
    };
    BackendCall::new(program, argv)
}

fn pr_view_call(ctx: &ProviderContext, id: u64) -> BackendCall {
    let program = BackendProgram::for_provider(ctx.provider);
    let argv: Vec<OsString> = match ctx.provider {
        Provider::GitHub => vec![
            OsString::from("pr"),
            OsString::from("view"),
            OsString::from(id.to_string()),
            OsString::from("--json"),
            OsString::from(
                "number,url,state,isDraft,title,headRefName,baseRefName,mergeable,mergedAt,labels",
            ),
        ],
        Provider::GitLab => vec![
            OsString::from("mr"),
            OsString::from("view"),
            OsString::from(id.to_string()),
            OsString::from("-F"),
            OsString::from("json"),
        ],
    };
    BackendCall::new(program, argv)
}

fn read_body(inline: Option<&str>, file: Option<&str>) -> Result<String, ForgeError> {
    if let Some(s) = inline {
        return Ok(s.to_string());
    }
    let Some(path) = file else {
        return Ok(String::new());
    };
    if path == "-" {
        let mut buf = String::new();
        std::io::stdin().read_to_string(&mut buf).map_err(|e| {
            ForgeError::software(
                schema_err(),
                "failed to read comment body from stdin",
                Some(e.to_string()),
            )
        })?;
        return Ok(buf);
    }
    fs::read_to_string(path).map_err(|e| {
        ForgeError::software(
            schema_err(),
            format!("failed to read --body-file '{path}'"),
            Some(e.to_string()),
        )
    })
}

fn schema_err() -> String {
    schema_version_for(BINARY, "error", 1)
}

fn render_text(payload: &PrCommentPayload) {
    println!(
        "commented on {provider} #{number}: {url}",
        provider = payload.provider,
        number = payload.number,
        url = payload.url,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{DetectionSource, Provider};
    use pretty_assertions::assert_eq;

    fn ctx(p: Provider) -> ProviderContext {
        ProviderContext {
            provider: p,
            host: "x".into(),
            source: DetectionSource::Flag,
        }
    }

    #[test]
    fn build_comment_call_github_uses_pr_comment_body() {
        let call = build_comment_call(&ctx(Provider::GitHub), 5, "hello");
        let plan = call.plan_argv();
        assert_eq!(
            plan[1..4],
            ["pr".to_string(), "comment".to_string(), "5".to_string()]
        );
        let b = plan.iter().position(|s| s == "--body").unwrap();
        assert_eq!(plan[b + 1], "hello");
    }

    #[test]
    fn build_comment_call_gitlab_uses_mr_note_message() {
        let call = build_comment_call(&ctx(Provider::GitLab), 7, "hello");
        let plan = call.plan_argv();
        assert_eq!(
            plan[1..4],
            ["mr".to_string(), "note".to_string(), "7".to_string()]
        );
        let m = plan.iter().position(|s| s == "--message").unwrap();
        assert_eq!(plan[m + 1], "hello");
    }

    #[test]
    fn read_body_prefers_inline_over_file() {
        assert_eq!(
            read_body(Some("inline"), Some("/no/such")).unwrap(),
            "inline"
        );
    }

    #[test]
    fn read_body_returns_empty_when_neither_set() {
        assert_eq!(read_body(None, None).unwrap(), "");
    }
}
