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
        Provider::GitHub | Provider::Local => vec![
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
        Provider::GitHub | Provider::Local => vec![
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
    use crate::backend::{BackendCall, BackendSuccess};
    use crate::cli::ProviderFlag;
    use crate::provider::DetectionSource;
    use nils_common::cli_contract::exit;
    use pretty_assertions::assert_eq;
    use std::cell::RefCell;

    fn ctx(p: Provider) -> ProviderContext {
        ProviderContext {
            provider: p,
            host: "example.com".into(),
            source: DetectionSource::Flag,
            repo: None,
        }
    }

    fn flags(provider: Option<ProviderFlag>, dry_run: bool) -> GlobalFlags {
        GlobalFlags {
            format: None,
            remote: "origin".into(),
            provider,
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
            assert!(!q.is_empty(), "ScriptedRunner ran out of stdout fixtures");
            Ok(BackendSuccess {
                stdout: q.remove(0),
                stderr: String::new(),
            })
        }
    }

    fn github_view_json(number: u64, state: &str) -> String {
        format!(
            r#"{{"number":{number},"url":"https://github.com/o/r/pull/{number}","state":"{state}","isDraft":false,"title":"t","headRefName":"feat/x","baseRefName":"main","mergeable":"MERGEABLE","mergedAt":null,"labels":[]}}"#
        )
    }

    fn gitlab_view_json(iid: u64, state: &str) -> String {
        format!(
            r#"{{"iid":{iid},"web_url":"https://gitlab.com/o/r/-/merge_requests/{iid}","state":"{state}","title":"t","source_branch":"feat/y","target_branch":"main","merge_status":"can_be_merged","draft":false,"labels":[]}}"#
        )
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

    #[test]
    fn build_close_call_pushes_repo_override() {
        let mut c = ctx(Provider::GitHub);
        c.repo = Some("acme/widget".into());
        let plan = build_close_call(&c, 9).plan_argv();
        let idx = plan.iter().position(|s| s == "--repo").expect("--repo");
        assert_eq!(plan[idx + 1], "acme/widget");
    }

    #[test]
    fn run_with_dry_run_emits_plan_envelope_github() {
        let runner = ScriptedRunner::with_stdout(Vec::new());
        let global = flags(Some(ProviderFlag::Github), true);
        let code =
            run_with(&runner, &global, 17, OutputFormat::Json, |_| None).expect("dry-run succeeds");
        assert_eq!(code, exit::SUCCESS);
        assert!(runner.captured.borrow().is_empty(), "dry-run skips backend");
    }

    #[test]
    fn run_with_dry_run_text_path_renders_plan() {
        let runner = ScriptedRunner::with_stdout(Vec::new());
        let global = flags(Some(ProviderFlag::Github), true);
        let code =
            run_with(&runner, &global, 17, OutputFormat::Text, |_| None).expect("dry-run text");
        assert_eq!(code, exit::SUCCESS);
    }

    #[test]
    fn run_with_happy_github_closes_then_views() {
        let runner = ScriptedRunner::with_stdout(vec!["", &github_view_json(42, "CLOSED")]);
        let global = flags(Some(ProviderFlag::Github), false);
        let code = run_with(&runner, &global, 42, OutputFormat::Json, |_| None)
            .expect("happy github close");
        assert_eq!(code, exit::SUCCESS);
        let calls = runner.captured.borrow();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0][1..4], ["pr", "close", "42"]);
        assert_eq!(calls[1][1..4], ["pr", "view", "42"]);
    }

    #[test]
    fn run_with_happy_gitlab_closes_then_views() {
        let runner = ScriptedRunner::with_stdout(vec!["", &gitlab_view_json(7, "closed")]);
        let global = flags(Some(ProviderFlag::Gitlab), false);
        let code = run_with(&runner, &global, 7, OutputFormat::Json, |_| None)
            .expect("happy gitlab close");
        assert_eq!(code, exit::SUCCESS);
    }

    #[test]
    fn run_with_text_format_renders_close_summary() {
        let runner = ScriptedRunner::with_stdout(vec!["", &github_view_json(42, "CLOSED")]);
        let global = flags(Some(ProviderFlag::Github), false);
        let code =
            run_with(&runner, &global, 42, OutputFormat::Text, |_| None).expect("text format");
        assert_eq!(code, exit::SUCCESS);
    }

    #[test]
    fn run_with_propagates_provider_detection_failure() {
        let runner = ScriptedRunner::with_stdout(Vec::new());
        let global = flags(None, false);
        let err = run_with(&runner, &global, 1, OutputFormat::Json, |_| None)
            .expect_err("no remote, no flag");
        assert_eq!(err.kind(), "provider_unsupported");
    }

    #[test]
    fn run_with_bubbles_invalid_view_json_as_software_error() {
        let runner = ScriptedRunner::with_stdout(vec!["", "not-json"]);
        let global = flags(Some(ProviderFlag::Github), false);
        let err = run_with(&runner, &global, 1, OutputFormat::Json, |_| None)
            .expect_err("view JSON parse fails");
        assert_eq!(err.kind(), "software_error");
    }
}
