//! `issue close` atom.
//!
//! Spec / ops: `cli.forge-cli.issue.close.v1`. Closes an issue and follows up
//! with `issue view` so the envelope reports the post-close state.

use std::ffi::OsString;

use nils_common::cli_contract::{OutputFormat, schema_version_for};
use serde::Serialize;

use crate::backend::{BackendCall, BackendProgram, BackendRunner, DryRunPayload, ProcessRunner};
use crate::cli::{BINARY, CloseReasonFlag, GlobalFlags};
use crate::envelope::emit_success;
use crate::error::ForgeError;
use crate::ops::issue_view;
use crate::provider::{Provider, ProviderContext, detect, git_remote_url};

const SCHEMA: &str = "issue.close";
const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct IssueClosePayload {
    pub provider: &'static str,
    pub number: u64,
    pub url: String,
    pub state: &'static str,
}

pub fn run(
    global: &GlobalFlags,
    id: u64,
    reason: Option<CloseReasonFlag>,
    format: OutputFormat,
) -> Result<i32, ForgeError> {
    if global.is_local() {
        let runner = crate::local::LocalRunner::from_global(global)?;
        return run_with(&runner, global, id, reason, format, git_remote_url);
    }
    let runner = ProcessRunner;
    run_with(&runner, global, id, reason, format, git_remote_url)
}

pub fn run_with<R: BackendRunner, F: Fn(&str) -> Option<String>>(
    runner: &R,
    global: &GlobalFlags,
    id: u64,
    reason: Option<CloseReasonFlag>,
    format: OutputFormat,
    remote_url_lookup: F,
) -> Result<i32, ForgeError> {
    let ctx = detect(
        global.provider_hint(),
        &global.remote,
        global.repo.as_deref(),
        remote_url_lookup,
    )?;
    let call = build_close_call(&ctx, id, reason);

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
        IssueClosePayload {
            provider: view.provider,
            number: view.number,
            url: view.url,
            state: view.state,
        },
        format,
        render_text,
    ))
}

fn build_close_call(
    ctx: &ProviderContext,
    id: u64,
    reason: Option<CloseReasonFlag>,
) -> BackendCall {
    let program = BackendProgram::for_provider(ctx.provider);
    let mut argv: Vec<OsString> = match ctx.provider {
        Provider::GitHub | Provider::GitLab | Provider::Local => vec![
            OsString::from("issue"),
            OsString::from("close"),
            OsString::from(id.to_string()),
        ],
    };
    // `--reason` is a GitHub-only state-reason concept (`gh issue close
    // --reason completed|"not planned"`). GitLab / Local have no equivalent,
    // so the flag is silently ignored there and never reaches the backend.
    if matches!(ctx.provider, Provider::GitHub)
        && let Some(reason) = reason
    {
        argv.push(OsString::from("--reason"));
        argv.push(OsString::from(reason.as_str()));
    }
    ctx.push_repo_override(&mut argv);
    BackendCall::new(program, argv)
}

fn render_text(payload: &IssueClosePayload) {
    println!(
        "closed {provider} issue #{number} ({state}): {url}",
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
    fn build_close_call_emits_issue_close_id_on_both_providers() {
        let gh = build_close_call(&ctx(Provider::GitHub), 5, None);
        assert_eq!(
            gh.plan_argv()[1..],
            ["issue".to_string(), "close".to_string(), "5".to_string()]
        );
        let gl = build_close_call(&ctx(Provider::GitLab), 9, None);
        assert_eq!(
            gl.plan_argv()[1..],
            ["issue".to_string(), "close".to_string(), "9".to_string()]
        );
    }

    #[test]
    fn build_close_call_appends_reason_only_on_github() {
        // GitHub: `--reason completed` / `--reason "not planned"` are appended.
        let completed =
            build_close_call(&ctx(Provider::GitHub), 5, Some(CloseReasonFlag::Completed));
        let plan = completed.plan_argv();
        let idx = plan
            .iter()
            .position(|s| s == "--reason")
            .expect("github close should carry --reason");
        assert_eq!(plan[idx + 1], "completed");

        let not_planned =
            build_close_call(&ctx(Provider::GitHub), 5, Some(CloseReasonFlag::NotPlanned));
        let plan = not_planned.plan_argv();
        let idx = plan
            .iter()
            .position(|s| s == "--reason")
            .expect("github close should carry --reason");
        assert_eq!(plan[idx + 1], "not planned");

        // GitLab and Local silently ignore the reason — no `--reason` flag.
        let gl = build_close_call(&ctx(Provider::GitLab), 9, Some(CloseReasonFlag::NotPlanned));
        assert!(
            !gl.plan_argv().iter().any(|s| s == "--reason"),
            "gitlab close must not carry --reason: {:?}",
            gl.plan_argv()
        );
        let local = build_close_call(&ctx(Provider::Local), 9, Some(CloseReasonFlag::Completed));
        assert!(
            !local.plan_argv().iter().any(|s| s == "--reason"),
            "local close must not carry --reason: {:?}",
            local.plan_argv()
        );
    }
}
