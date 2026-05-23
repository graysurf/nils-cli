//! `pr close` atom.
//!
//! Spec / ops: `cli.forge-cli.pr.close.v1`. Closes a PR/MR without merging.
//! Emits `{ number, url, state }` after the close call by re-fetching via
//! `pr view`. Both backends print no envelope-ready JSON from the close
//! call itself, so the follow-up view ensures the envelope reports the
//! canonical post-close state (`closed`).

use std::ffi::OsString;

use nils_common::cli_contract::{OutputFormat, schema_version_for};
use serde::Serialize;

use crate::backend::{BackendCall, BackendProgram, BackendRunner, DryRunPayload, ProcessRunner};
use crate::cli::{BINARY, GlobalFlags};
use crate::envelope::emit_success;
use crate::error::ForgeError;
use crate::ops::pr_view;
use crate::provider::{Provider, ProviderContext, detect, git_remote_url};

const SCHEMA: &str = "pr.close";
const SCHEMA_VERSION: u32 = 1;

/// Envelope payload for `cli.forge-cli.pr.close.v1`.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PrClosePayload {
    pub provider: &'static str,
    pub number: u64,
    pub url: String,
    pub state: &'static str,
}

pub fn run(global: &GlobalFlags, id: u64, format: OutputFormat) -> Result<i32, ForgeError> {
    let runner = ProcessRunner;
    run_with(&runner, global, id, format, git_remote_url)
}

pub fn run_with<R: BackendRunner, F: Fn(&str) -> Option<String>>(
    runner: &R,
    global: &GlobalFlags,
    id: u64,
    format: OutputFormat,
    remote_url_lookup: F,
) -> Result<i32, ForgeError> {
    let ctx = detect(
        global.provider_hint(),
        &global.remote,
        global.repo.as_deref(),
        remote_url_lookup,
    )?;
    let call = build_close_call(&ctx, id);

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

    // Re-fetch via the canonical `pr view` to populate the envelope shape.
    let view_call = build_view_call(&ctx, id);
    let view_output = runner.run(&view_call)?;
    let payload = pr_view::parse_view_output(&ctx, &view_output)?;
    Ok(emit_success(
        schema_version_for(BINARY, SCHEMA, SCHEMA_VERSION),
        PrClosePayload {
            provider: payload.provider,
            number: payload.number,
            url: payload.url,
            state: payload.state,
        },
        format,
        render_text,
    ))
}

fn build_close_call(ctx: &ProviderContext, id: u64) -> BackendCall {
    let program = BackendProgram::for_provider(ctx.provider);
    let mut argv: Vec<OsString> = match ctx.provider {
        Provider::GitHub => vec![
            OsString::from("pr"),
            OsString::from("close"),
            OsString::from(id.to_string()),
        ],
        Provider::GitLab => vec![
            OsString::from("mr"),
            OsString::from("close"),
            OsString::from(id.to_string()),
        ],
    };
    ctx.push_repo_override(&mut argv);
    BackendCall::new(program, argv)
}

fn build_view_call(ctx: &ProviderContext, id: u64) -> BackendCall {
    let program = BackendProgram::for_provider(ctx.provider);
    let mut argv: Vec<OsString> = match ctx.provider {
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
    ctx.push_repo_override(&mut argv);
    BackendCall::new(program, argv)
}

fn render_text(payload: &PrClosePayload) {
    println!(
        "closed {provider} #{number} ({state}): {url}",
        provider = payload.provider,
        number = payload.number,
        state = payload.state,
        url = payload.url,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::DetectionSource;
    use pretty_assertions::assert_eq;

    fn ctx(p: Provider) -> ProviderContext {
        ProviderContext {
            provider: p,
            host: "example.com".into(),
            source: DetectionSource::Flag,
            repo: None,
        }
    }

    #[test]
    fn build_close_call_github_emits_pr_close_id() {
        let call = build_close_call(&ctx(Provider::GitHub), 42);
        let plan = call.plan_argv();
        assert_eq!(
            plan[1..],
            ["pr".to_string(), "close".to_string(), "42".to_string()]
        );
    }

    #[test]
    fn build_close_call_gitlab_emits_mr_close_id() {
        let call = build_close_call(&ctx(Provider::GitLab), 7);
        let plan = call.plan_argv();
        assert_eq!(
            plan[1..],
            ["mr".to_string(), "close".to_string(), "7".to_string()]
        );
    }
}
