//! `pr ready` atom.
//!
//! Spec / ops: `cli.forge-cli.pr.ready.v1`. Promotes a draft PR/MR to
//! ready-for-review. Validation: `worktree_clean`. After mutation the op
//! re-fetches via `pr view` so the envelope carries the canonical
//! post-ready state (`draft: false`).

use std::ffi::OsString;
use std::path::PathBuf;

use nils_common::cli_contract::{OutputFormat, schema_version_for};

use crate::backend::{BackendCall, BackendProgram, BackendRunner, DryRunPayload};
use crate::cli::{BINARY, GlobalFlags, PrReadyArgs};
use crate::envelope::emit_success;
use crate::error::ForgeError;
use crate::ops::pr_view::{self, PrViewPayload};
use crate::provider::{Provider, ProviderContext, detect, git_remote_url};
use crate::rate_limit::default_runner;
use crate::validations::{git_status_porcelain, worktree_clean};

const SCHEMA: &str = "pr.ready";
const SCHEMA_VERSION: u32 = 1;

pub fn run(
    global: &GlobalFlags,
    args: PrReadyArgs,
    format: OutputFormat,
) -> Result<i32, ForgeError> {
    let runner = default_runner();
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

pub(crate) fn build_ready_call(ctx: &ProviderContext, id: u64) -> BackendCall {
    let program = BackendProgram::for_provider(ctx.provider);
    let mut argv: Vec<OsString> = match ctx.provider {
        Provider::GitHub | Provider::Local => vec![
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
        Provider::GitHub | Provider::Local => vec![
            OsString::from("pr"),
            OsString::from("view"),
            OsString::from(id.to_string()),
            OsString::from("--json"),
            OsString::from(pr_view::GH_JSON_FIELDS),
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
    use crate::backend::{BackendCall, BackendSuccess};
    use crate::cli::ProviderFlag;
    use crate::provider::{DetectionSource, Provider};
    use nils_common::cli_contract::exit;
    use pretty_assertions::assert_eq;
    use std::cell::RefCell;
    use std::path::Path;

    fn ctx(p: Provider) -> ProviderContext {
        ProviderContext {
            provider: p,
            host: "x".into(),
            source: DetectionSource::Flag,
            repo: None,
        }
    }

    fn flags(provider: Option<ProviderFlag>, dry_run: bool) -> GlobalFlags {
        GlobalFlags {
            format: None,
            remote: "origin".into(),
            provider,
            host: None,
            repo: None,
            store_root: None,
            dry_run,
        }
    }

    struct ScriptedRunner {
        outputs: RefCell<Vec<String>>,
        captured: RefCell<Vec<Vec<String>>>,
    }

    impl ScriptedRunner {
        fn with_stdout(outs: Vec<&str>) -> Self {
            Self {
                outputs: RefCell::new(outs.into_iter().map(|s| s.to_string()).collect()),
                captured: RefCell::new(Vec::new()),
            }
        }
    }

    impl BackendRunner for ScriptedRunner {
        fn run(&self, call: &BackendCall) -> Result<BackendSuccess, ForgeError> {
            self.captured.borrow_mut().push(call.plan_argv());
            let mut q = self.outputs.borrow_mut();
            assert!(!q.is_empty(), "ScriptedRunner ran out of fixtures");
            Ok(BackendSuccess {
                stdout: q.remove(0),
                stderr: String::new(),
            })
        }
    }

    fn github_view_json(number: u64) -> String {
        format!(
            r#"{{"number":{number},"url":"https://github.com/o/r/pull/{number}","state":"OPEN","isDraft":false,"title":"t","headRefName":"feat/x","baseRefName":"main","mergeable":"MERGEABLE","mergedAt":null,"labels":[]}}"#
        )
    }

    fn gitlab_view_json(iid: u64) -> String {
        format!(
            r#"{{"iid":{iid},"web_url":"https://gitlab.com/o/r/-/merge_requests/{iid}","state":"opened","title":"t","source_branch":"feat/y","target_branch":"main","merge_status":"can_be_merged","draft":false,"labels":[]}}"#
        )
    }

    fn clean_status(_: &Path) -> Result<String, ForgeError> {
        Ok(String::new())
    }

    fn dirty_status(_: &Path) -> Result<String, ForgeError> {
        Ok(" M src/lib.rs\n".into())
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

    #[test]
    fn run_with_dry_run_github_emits_plan_envelope() {
        let runner = ScriptedRunner::with_stdout(Vec::new());
        let global = flags(Some(ProviderFlag::Github), true);
        let code = run_with(
            &runner,
            &global,
            PrReadyArgs { id: 9 },
            OutputFormat::Json,
            |_| None,
            Path::new("."),
            clean_status,
        )
        .expect("dry-run github");
        assert_eq!(code, exit::SUCCESS);
        assert!(runner.captured.borrow().is_empty());
    }

    #[test]
    fn run_with_dry_run_gitlab_text_format() {
        let runner = ScriptedRunner::with_stdout(Vec::new());
        let global = flags(Some(ProviderFlag::Gitlab), true);
        let code = run_with(
            &runner,
            &global,
            PrReadyArgs { id: 9 },
            OutputFormat::Text,
            |_| None,
            Path::new("."),
            clean_status,
        )
        .expect("dry-run gitlab text");
        assert_eq!(code, exit::SUCCESS);
    }

    #[test]
    fn run_with_happy_github_marks_ready_and_views() {
        let runner = ScriptedRunner::with_stdout(vec!["", &github_view_json(42)]);
        let global = flags(Some(ProviderFlag::Github), false);
        let code = run_with(
            &runner,
            &global,
            PrReadyArgs { id: 42 },
            OutputFormat::Json,
            |_| None,
            Path::new("."),
            clean_status,
        )
        .expect("happy github");
        assert_eq!(code, exit::SUCCESS);
        let calls = runner.captured.borrow();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0][1..4], ["pr", "ready", "42"]);
    }

    #[test]
    fn run_with_happy_gitlab_text_format() {
        let runner = ScriptedRunner::with_stdout(vec!["", &gitlab_view_json(7)]);
        let global = flags(Some(ProviderFlag::Gitlab), false);
        let code = run_with(
            &runner,
            &global,
            PrReadyArgs { id: 7 },
            OutputFormat::Text,
            |_| None,
            Path::new("."),
            clean_status,
        )
        .expect("happy gitlab");
        assert_eq!(code, exit::SUCCESS);
    }

    #[test]
    fn run_with_rejects_dirty_worktree_before_backend() {
        let runner = ScriptedRunner::with_stdout(Vec::new());
        let global = flags(Some(ProviderFlag::Github), false);
        let err = run_with(
            &runner,
            &global,
            PrReadyArgs { id: 1 },
            OutputFormat::Json,
            |_| None,
            Path::new("."),
            dirty_status,
        )
        .expect_err("dirty");
        assert_eq!(err.kind(), "dirty_worktree");
        assert!(runner.captured.borrow().is_empty());
    }

    #[test]
    fn run_with_propagates_provider_detection_failure() {
        let runner = ScriptedRunner::with_stdout(Vec::new());
        let global = flags(None, false);
        let err = run_with(
            &runner,
            &global,
            PrReadyArgs { id: 1 },
            OutputFormat::Json,
            |_| None,
            Path::new("."),
            clean_status,
        )
        .expect_err("no provider");
        assert_eq!(err.kind(), "provider_unsupported");
    }

    #[test]
    fn run_with_invalid_view_json_is_software_error() {
        let runner = ScriptedRunner::with_stdout(vec!["", "not-json"]);
        let global = flags(Some(ProviderFlag::Github), false);
        let err = run_with(
            &runner,
            &global,
            PrReadyArgs { id: 1 },
            OutputFormat::Json,
            |_| None,
            Path::new("."),
            clean_status,
        )
        .expect_err("invalid json");
        assert_eq!(err.kind(), "software_error");
    }

    #[test]
    fn compute_returns_typed_payload_after_view() {
        let runner = ScriptedRunner::with_stdout(vec!["", &github_view_json(11)]);
        let payload = compute(
            &runner,
            &ctx(Provider::GitHub),
            11,
            Path::new("."),
            clean_status,
        )
        .expect("compute");
        assert_eq!(payload.number, 11);
        assert_eq!(payload.provider, "github");
        assert!(!payload.draft);
    }

    #[test]
    fn compute_rejects_dirty_worktree() {
        let runner = ScriptedRunner::with_stdout(Vec::new());
        let err = compute(
            &runner,
            &ctx(Provider::GitHub),
            1,
            Path::new("."),
            dirty_status,
        )
        .expect_err("dirty");
        assert_eq!(err.kind(), "dirty_worktree");
    }
}
