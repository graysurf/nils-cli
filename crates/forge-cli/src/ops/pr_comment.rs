//! `pr comment` atom.
//!
//! Spec / ops: `cli.forge-cli.pr.comment.v1`. Appends a comment to a PR/MR.
//! Body can come from `--body`, `--body-file <path>`, or
//! `--body-file -` (stdin). Provider-bound comment bodies run the same
//! local-path privacy guard as other remote payloads before the backend call.
//! The envelope payload reports `{ provider, number, url }` where `url` is
//! the PR/MR URL (cheap re-fetch via `pr view`).

use std::ffi::OsString;
use std::fs;
use std::io::Read as _;

use nils_common::cli_contract::{OutputFormat, schema_version_for};
use serde::Serialize;

use crate::backend::{BackendCall, BackendProgram, BackendRunner, DryRunPayload};
use crate::cli::{BINARY, GlobalFlags, PrCommentArgs};
use crate::envelope::emit_success;
use crate::error::ForgeError;
use crate::ops::pr_view;
use crate::provider::{Provider, ProviderContext, detect, git_remote_url};
use crate::rate_limit::default_runner;
use crate::validations::no_local_path;

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
    let runner = default_runner();
    run_with(&runner, global, args, format, git_remote_url)
}

pub fn run_with<R: BackendRunner, F: Fn(&str) -> Option<String>>(
    runner: &R,
    global: &GlobalFlags,
    args: PrCommentArgs,
    format: OutputFormat,
    remote_url_lookup: F,
) -> Result<i32, ForgeError> {
    let ctx = detect(
        global.provider_hint(),
        &global.remote,
        global.repo.as_deref(),
        remote_url_lookup,
    )?;
    let body = read_body(args.body.as_deref(), args.body_file.as_deref())?;
    if body.trim().is_empty() {
        return Err(ForgeError::validation(
            schema_err(),
            "body_missing_summary",
            "comment body is empty (supply --body or --body-file)",
            None,
        ));
    }
    no_local_path(&body, "comment")?;
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
    let mut argv: Vec<OsString> = match ctx.provider {
        Provider::GitHub | Provider::Local => vec![
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

pub(crate) fn read_body(inline: Option<&str>, file: Option<&str>) -> Result<String, ForgeError> {
    read_body_with_file_flag(inline, file, "--body-file")
}

pub(crate) fn read_body_with_file_flag(
    inline: Option<&str>,
    file: Option<&str>,
    file_flag: &str,
) -> Result<String, ForgeError> {
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
            format!("failed to read {file_flag} '{path}'"),
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
    use crate::backend::{BackendCall, BackendSuccess};
    use crate::cli::ProviderFlag;
    use crate::provider::{DetectionSource, Provider};
    use nils_common::cli_contract::exit;
    use pretty_assertions::assert_eq;
    use std::cell::RefCell;
    use std::io::Write as _;

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
            repo: None,
            store_root: None,
            dry_run,
        }
    }

    fn args(id: u64, body: Option<&str>, body_file: Option<&str>) -> PrCommentArgs {
        PrCommentArgs {
            id,
            body: body.map(str::to_string),
            body_file: body_file.map(str::to_string),
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

    #[test]
    fn read_body_returns_file_contents() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        tmp.write_all(b"loaded body").unwrap();
        let body = read_body(None, Some(tmp.path().to_str().unwrap())).unwrap();
        assert_eq!(body, "loaded body");
    }

    #[test]
    fn read_body_missing_file_is_software_error() {
        let err = read_body(None, Some("/this/path/does/not/exist")).expect_err("missing");
        assert_eq!(err.kind(), "software_error");
    }

    #[test]
    fn run_with_rejects_empty_body() {
        let runner = ScriptedRunner::with_stdout(Vec::new());
        let global = flags(Some(ProviderFlag::Github), false);
        let err = run_with(
            &runner,
            &global,
            args(1, Some("   "), None),
            OutputFormat::Json,
            |_| None,
        )
        .expect_err("blank body");
        assert_eq!(err.kind(), "body_missing_summary");
    }

    #[test]
    fn run_with_rejects_missing_body_and_file() {
        let runner = ScriptedRunner::with_stdout(Vec::new());
        let global = flags(Some(ProviderFlag::Github), false);
        let err = run_with(
            &runner,
            &global,
            args(1, None, None),
            OutputFormat::Json,
            |_| None,
        )
        .expect_err("no body");
        assert_eq!(err.kind(), "body_missing_summary");
    }

    #[test]
    fn run_with_dry_run_emits_plan_envelope_github() {
        let runner = ScriptedRunner::with_stdout(Vec::new());
        let global = flags(Some(ProviderFlag::Github), true);
        let code = run_with(
            &runner,
            &global,
            args(7, Some("hello"), None),
            OutputFormat::Json,
            |_| None,
        )
        .expect("dry-run");
        assert_eq!(code, exit::SUCCESS);
        assert!(runner.captured.borrow().is_empty());
    }

    #[test]
    fn run_with_dry_run_text_format() {
        let runner = ScriptedRunner::with_stdout(Vec::new());
        let global = flags(Some(ProviderFlag::Gitlab), true);
        let code = run_with(
            &runner,
            &global,
            args(7, Some("hello"), None),
            OutputFormat::Text,
            |_| None,
        )
        .expect("dry-run text");
        assert_eq!(code, exit::SUCCESS);
    }

    #[test]
    fn run_with_happy_github_inline_body() {
        let runner = ScriptedRunner::with_stdout(vec!["", &github_view_json(42, "OPEN")]);
        let global = flags(Some(ProviderFlag::Github), false);
        let code = run_with(
            &runner,
            &global,
            args(42, Some("nice work"), None),
            OutputFormat::Json,
            |_| None,
        )
        .expect("happy");
        assert_eq!(code, exit::SUCCESS);
        let calls = runner.captured.borrow();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0][1..4], ["pr", "comment", "42"]);
        assert_eq!(calls[1][1..4], ["pr", "view", "42"]);
    }

    #[test]
    fn run_with_happy_gitlab_inline_body_text_format() {
        let runner = ScriptedRunner::with_stdout(vec!["", &gitlab_view_json(7, "opened")]);
        let global = flags(Some(ProviderFlag::Gitlab), false);
        let code = run_with(
            &runner,
            &global,
            args(7, Some("nice"), None),
            OutputFormat::Text,
            |_| None,
        )
        .expect("happy gitlab");
        assert_eq!(code, exit::SUCCESS);
    }

    #[test]
    fn run_with_reads_body_from_file_and_proceeds() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        tmp.write_all(b"file body").unwrap();
        let runner = ScriptedRunner::with_stdout(vec!["", &github_view_json(11, "OPEN")]);
        let global = flags(Some(ProviderFlag::Github), false);
        let code = run_with(
            &runner,
            &global,
            args(11, None, Some(tmp.path().to_str().unwrap())),
            OutputFormat::Json,
            |_| None,
        )
        .expect("happy with body file");
        assert_eq!(code, exit::SUCCESS);
    }

    #[test]
    fn run_with_propagates_provider_detection_failure() {
        let runner = ScriptedRunner::with_stdout(Vec::new());
        let global = flags(None, false);
        let err = run_with(
            &runner,
            &global,
            args(1, Some("hi"), None),
            OutputFormat::Json,
            |_| None,
        )
        .expect_err("no provider");
        assert_eq!(err.kind(), "provider_unsupported");
    }
}
