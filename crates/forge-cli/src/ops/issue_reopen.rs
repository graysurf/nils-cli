//! `issue reopen` atom.
//!
//! Spec / ops: `cli.forge-cli.issue.reopen.v1`. Reopens a closed issue and
//! follows up with `issue view` so the envelope reports the post-action state.

use std::ffi::OsString;

use nils_common::cli_contract::{OutputFormat, schema_version_for};
use serde::Serialize;

use crate::backend::{BackendCall, BackendProgram, BackendRunner, DryRunPayload};
use crate::cli::{BINARY, GlobalFlags};
use crate::envelope::emit_success;
use crate::error::ForgeError;
use crate::ops::issue_view;
use crate::provider::{Provider, ProviderContext, detect, git_remote_url};
use crate::rate_limit::default_runner;

const SCHEMA: &str = "issue.reopen";
const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct IssueReopenPayload {
    pub provider: &'static str,
    pub number: u64,
    pub url: String,
    pub state: &'static str,
}

pub fn run(global: &GlobalFlags, id: u64, format: OutputFormat) -> Result<i32, ForgeError> {
    let runner = default_runner();
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
    let call = build_reopen_call(&ctx, id);

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
    let view_output = runner.run(&issue_view::build_view_call(&ctx, id))?;
    let view = issue_view::parse_view_output(&ctx, &view_output)?;
    Ok(emit_success(
        schema_version_for(BINARY, SCHEMA, SCHEMA_VERSION),
        IssueReopenPayload {
            provider: view.provider,
            number: view.number,
            url: view.url,
            state: view.state,
        },
        format,
        render_text,
    ))
}

fn build_reopen_call(ctx: &ProviderContext, id: u64) -> BackendCall {
    let program = BackendProgram::for_provider(ctx.provider);
    let mut argv: Vec<OsString> = match ctx.provider {
        Provider::GitHub | Provider::GitLab | Provider::Local => vec![
            OsString::from("issue"),
            OsString::from("reopen"),
            OsString::from(id.to_string()),
        ],
    };
    ctx.push_repo_override(&mut argv);
    BackendCall::new(program, argv)
}

fn render_text(payload: &IssueReopenPayload) {
    println!(
        "reopened {provider} issue #{number} ({state}): {url}",
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
            host: "x".into(),
            source: DetectionSource::Flag,
            repo: None,
        }
    }

    #[test]
    fn build_reopen_call_emits_issue_reopen_id_on_both_providers() {
        let gh = build_reopen_call(&ctx(Provider::GitHub), 5);
        assert_eq!(
            gh.plan_argv()[1..],
            ["issue".to_string(), "reopen".to_string(), "5".to_string()]
        );
        let gl = build_reopen_call(&ctx(Provider::GitLab), 9);
        assert_eq!(
            gl.plan_argv()[1..],
            ["issue".to_string(), "reopen".to_string(), "9".to_string()]
        );
    }
}
