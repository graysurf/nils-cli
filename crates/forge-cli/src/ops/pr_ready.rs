//! `pr ready` atom.
//!
//! Spec / ops: `cli.forge-cli.pr.ready.v1`. Promotes a draft PR/MR to
//! ready-for-review. Validation: `worktree_clean`. After mutation the op
//! re-fetches via `pr view` so the envelope carries the canonical
//! post-ready state (`draft: false`).

use std::ffi::OsString;
use std::path::PathBuf;

use nils_common::cli_contract::{OutputFormat, schema_version_for};

use crate::backend::{BackendCall, BackendProgram, BackendRunner, DryRunPayload, ProcessRunner};
use crate::cli::{BINARY, GlobalFlags, PrReadyArgs};
use crate::envelope::emit_success;
use crate::error::ForgeError;
use crate::ops::pr_view::{self, PrViewPayload};
use crate::provider::{Provider, ProviderContext, detect, git_remote_url};
use crate::validations::{git_status_porcelain, worktree_clean};

const SCHEMA: &str = "pr.ready";
const SCHEMA_VERSION: u32 = 1;

pub fn run(
    global: &GlobalFlags,
    args: PrReadyArgs,
    format: OutputFormat,
) -> Result<i32, ForgeError> {
    let runner = ProcessRunner;
    let workdir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    run_with(
        &runner,
        global,
        args,
        format,
        git_remote_url,
        &workdir,
        git_status_porcelain,
    )
}

pub fn run_with<R, F, G>(
    runner: &R,
    global: &GlobalFlags,
    args: PrReadyArgs,
    format: OutputFormat,
    remote_url_lookup: F,
    workdir: &std::path::Path,
    git_status: G,
) -> Result<i32, ForgeError>
where
    R: BackendRunner,
    F: Fn(&str) -> Option<String>,
    G: FnOnce(&std::path::Path) -> Result<String, ForgeError>,
{
    let ctx = detect(
        global.provider_hint(),
        &global.remote,
        global.repo.as_deref(),
        remote_url_lookup,
    )?;
    worktree_clean(workdir, git_status)?;

    let call = build_ready_call(&ctx, args.id);
    if global.dry_run {
        let payload = DryRunPayload::new(ctx.provider, &call);
        return Ok(emit_success(
            schema_version_for(BINARY, SCHEMA, SCHEMA_VERSION),
            payload,
            format,
            |p| println!("would run: {plan}", plan = p.plan.join(" ")),
        ));
    }
    let payload = run_backend_and_fetch(runner, &ctx, args.id)?;
    Ok(emit_success(
        schema_version_for(BINARY, SCHEMA, SCHEMA_VERSION),
        payload,
        format,
        render_text,
    ))
}

/// Macro-facing entry point: validate worktree, mark ready, re-fetch view,
/// return the typed payload without emitting an envelope. Used by
/// `pr deliver` to capture this step's typed output.
pub fn compute<R, G>(
    runner: &R,
    ctx: &ProviderContext,
    id: u64,
    workdir: &std::path::Path,
    git_status: G,
) -> Result<PrViewPayload, ForgeError>
where
    R: BackendRunner,
    G: FnOnce(&std::path::Path) -> Result<String, ForgeError>,
{
    worktree_clean(workdir, git_status)?;
    run_backend_and_fetch(runner, ctx, id)
}

fn run_backend_and_fetch<R: BackendRunner>(
    runner: &R,
    ctx: &ProviderContext,
    id: u64,
) -> Result<PrViewPayload, ForgeError> {
    let _ = runner.run(&build_ready_call(ctx, id))?;
    let view_output = runner.run(&pr_view_call(ctx, id))?;
    pr_view::parse_view_output(ctx, &view_output)
}

fn build_ready_call(ctx: &ProviderContext, id: u64) -> BackendCall {
    let program = BackendProgram::for_provider(ctx.provider);
    let mut argv: Vec<OsString> = match ctx.provider {
        Provider::GitHub => vec![
            OsString::from("pr"),
            OsString::from("ready"),
            OsString::from(id.to_string()),
        ],
        Provider::GitLab => vec![
            OsString::from("mr"),
            OsString::from("update"),
            OsString::from(id.to_string()),
            OsString::from("--ready"),
        ],
    };
    ctx.push_repo_override(&mut argv);
    BackendCall::new(program, argv)
}

fn pr_view_call(ctx: &ProviderContext, id: u64) -> BackendCall {
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

fn render_text(payload: &PrViewPayload) {
    println!(
        "ready #{n} [{state}]: {url}",
        n = payload.number,
        state = payload.state,
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
            repo: None,
        }
    }

    #[test]
    fn build_ready_call_github_emits_pr_ready_id() {
        let call = build_ready_call(&ctx(Provider::GitHub), 5);
        let plan = call.plan_argv();
        assert_eq!(
            plan[1..],
            ["pr".to_string(), "ready".to_string(), "5".to_string()]
        );
    }

    #[test]
    fn build_ready_call_gitlab_emits_mr_update_ready() {
        let call = build_ready_call(&ctx(Provider::GitLab), 7);
        let plan = call.plan_argv();
        assert_eq!(
            plan[1..],
            [
                "mr".to_string(),
                "update".to_string(),
                "7".to_string(),
                "--ready".to_string()
            ]
        );
    }
}
