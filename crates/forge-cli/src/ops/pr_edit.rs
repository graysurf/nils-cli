//! `pr edit` atom.
//!
//! Spec / ops: `cli.forge-cli.pr.edit.v1`. Mutates PR/MR title, body, base,
//! labels, reviewers. The `title_length` rule fires when `--title` is set;
//! `body_summary` + `body_test_plan` fire when a new body is supplied via
//! `--body` or `--body-file`. After mutation, the op re-fetches via
//! `pr view` so the envelope payload carries the canonical post-edit state.

use std::ffi::OsString;
use std::fs;
use std::io::Read as _;
use std::path::Path;

use nils_common::cli_contract::{OutputFormat, schema_version_for};
use tempfile::NamedTempFile;

use crate::backend::{BackendCall, BackendProgram, BackendRunner, DryRunPayload, ProcessRunner};
use crate::cli::{BINARY, GlobalFlags, PrEditArgs};
use crate::envelope::emit_success;
use crate::error::ForgeError;
use crate::ops::pr_view::{self, PrViewPayload};
use crate::provider::{Provider, ProviderContext, detect, git_remote_url};
use crate::validations::{BodyHeadings, body_summary, body_test_plan, no_local_path, title_length};

const SCHEMA: &str = "pr.edit";
const SCHEMA_VERSION: u32 = 1;

pub fn run(
    global: &GlobalFlags,
    args: PrEditArgs,
    format: OutputFormat,
) -> Result<i32, ForgeError> {
    let runner = ProcessRunner;
    run_with(&runner, global, args, format, git_remote_url)
}

pub fn run_with<R: BackendRunner, F: Fn(&str) -> Option<String>>(
    runner: &R,
    global: &GlobalFlags,
    args: PrEditArgs,
    format: OutputFormat,
    remote_url_lookup: F,
) -> Result<i32, ForgeError> {
    let ctx = detect(
        global.provider_hint(),
        &global.remote,
        global.repo.as_deref(),
        remote_url_lookup,
    )?;

    // Read the replacement body once; keep around so the temp file stays
    // alive through the backend call.
    let new_body = if args.body.is_some() || args.body_file.is_some() {
        Some(read_body(args.body.as_deref(), args.body_file.as_deref())?)
    } else {
        None
    };

    // Conditional validations per spec §"pr edit".
    if let Some(title) = &args.title {
        title_length(title)?;
        no_local_path(title, "title")?;
    }
    if let Some(body) = &new_body {
        let headings = BodyHeadings::default();
        body_summary(body, &headings)?;
        body_test_plan(body, &headings)?;
        no_local_path(body, "body")?;
    }

    let body_tempfile = match (&ctx.provider, &new_body) {
        (Provider::GitHub, Some(b)) => Some(write_body_tempfile(b)?),
        _ => None,
    };
    let body_path = body_tempfile.as_ref().map(|t| t.path().to_path_buf());

    let call = build_edit_call(&ctx, &args, new_body.as_deref(), body_path.as_deref());

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

    // Re-fetch via canonical `pr view` so the envelope mirrors the
    // post-edit state.
    let view_call = pr_view_call(&ctx, args.id);
    let view_output = runner.run(&view_call)?;
    let payload: PrViewPayload = pr_view::parse_view_output(&ctx, &view_output)?;
    Ok(emit_success(
        schema_version_for(BINARY, SCHEMA, SCHEMA_VERSION),
        payload,
        format,
        render_text,
    ))
}

fn build_edit_call(
    ctx: &ProviderContext,
    args: &PrEditArgs,
    body: Option<&str>,
    body_path: Option<&Path>,
) -> BackendCall {
    let program = BackendProgram::for_provider(ctx.provider);
    let mut argv: Vec<OsString> = Vec::new();
    match ctx.provider {
        Provider::GitHub | Provider::Local => {
            argv.push(OsString::from("pr"));
            argv.push(OsString::from("edit"));
            argv.push(OsString::from(args.id.to_string()));
            if let Some(t) = &args.title {
                argv.push(OsString::from("--title"));
                argv.push(OsString::from(t));
            }
            if let Some(p) = body_path {
                argv.push(OsString::from("--body-file"));
                argv.push(OsString::from(p));
            }
            if let Some(b) = &args.base {
                argv.push(OsString::from("--base"));
                argv.push(OsString::from(b));
            }
            for l in &args.add_labels {
                argv.push(OsString::from("--add-label"));
                argv.push(OsString::from(l));
            }
            for l in &args.remove_labels {
                argv.push(OsString::from("--remove-label"));
                argv.push(OsString::from(l));
            }
            for r in &args.add_reviewers {
                argv.push(OsString::from("--add-reviewer"));
                argv.push(OsString::from(r));
            }
        }
        Provider::GitLab => {
            argv.push(OsString::from("mr"));
            argv.push(OsString::from("update"));
            argv.push(OsString::from(args.id.to_string()));
            if let Some(t) = &args.title {
                argv.push(OsString::from("--title"));
                argv.push(OsString::from(t));
            }
            if let Some(b) = body {
                argv.push(OsString::from("--description"));
                argv.push(OsString::from(b));
            }
            if let Some(b) = &args.base {
                argv.push(OsString::from("--target-branch"));
                argv.push(OsString::from(b));
            }
            if !args.add_labels.is_empty() {
                argv.push(OsString::from("--label"));
                argv.push(OsString::from(args.add_labels.join(",")));
            }
            if !args.remove_labels.is_empty() {
                argv.push(OsString::from("--unlabel"));
                argv.push(OsString::from(args.remove_labels.join(",")));
            }
            if !args.add_reviewers.is_empty() {
                argv.push(OsString::from("--reviewer"));
                argv.push(OsString::from(args.add_reviewers.join(",")));
            }
        }
    }
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
                "failed to read body from stdin",
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

fn write_body_tempfile(body: &str) -> Result<NamedTempFile, ForgeError> {
    let tmp = tempfile::Builder::new()
        .prefix("forge-cli-edit-body-")
        .suffix(".md")
        .tempfile()
        .map_err(|e| {
            ForgeError::software(
                schema_err(),
                "failed to create body tempfile",
                Some(e.to_string()),
            )
        })?;
    fs::write(tmp.path(), body).map_err(|e| {
        ForgeError::software(
            schema_err(),
            "failed to write body tempfile",
            Some(e.to_string()),
        )
    })?;
    Ok(tmp)
}

fn schema_err() -> String {
    schema_version_for(BINARY, "error", 1)
}

fn render_text(payload: &PrViewPayload) {
    println!(
        "edited #{n} [{state}{draft}]: {url}",
        n = payload.number,
        state = payload.state,
        draft = if payload.draft { ",draft" } else { "" },
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

    fn args(id: u64) -> PrEditArgs {
        PrEditArgs {
            id,
            title: None,
            body: None,
            body_file: None,
            base: None,
            add_labels: vec![],
            remove_labels: vec![],
            add_reviewers: vec![],
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

    const VALID_BODY: &str = "## Summary\n\nDoes the thing.\n\n## Test plan\n\n- cargo test\n";

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

    #[test]
    fn build_edit_call_github_emits_add_remove_labels() {
        let mut a = args(5);
        a.add_labels = vec!["needs-review".into(), "p1".into()];
        a.remove_labels = vec!["wip".into()];
        let call = build_edit_call(&ctx(Provider::GitHub), &a, None, None);
        let plan = call.plan_argv();
        // --add-label appears once per label.
        let adds: Vec<_> = plan
            .windows(2)
            .filter(|w| w[0] == "--add-label")
            .map(|w| w[1].clone())
            .collect();
        assert_eq!(adds, vec!["needs-review".to_string(), "p1".to_string()]);
        let removes: Vec<_> = plan
            .windows(2)
            .filter(|w| w[0] == "--remove-label")
            .map(|w| w[1].clone())
            .collect();
        assert_eq!(removes, vec!["wip".to_string()]);
    }

    #[test]
    fn build_edit_call_gitlab_collapses_labels_to_csv() {
        let mut a = args(7);
        a.add_labels = vec!["needs-review".into(), "p1".into()];
        a.remove_labels = vec!["wip".into(), "ci-only".into()];
        let call = build_edit_call(&ctx(Provider::GitLab), &a, None, None);
        let plan = call.plan_argv();
        let label_idx = plan.iter().position(|s| s == "--label").unwrap();
        assert_eq!(plan[label_idx + 1], "needs-review,p1");
        let unlabel_idx = plan.iter().position(|s| s == "--unlabel").unwrap();
        assert_eq!(plan[unlabel_idx + 1], "wip,ci-only");
    }

    #[test]
    fn build_edit_call_github_uses_body_file() {
        let mut a = args(5);
        a.body = Some("ignored".into());
        let call = build_edit_call(
            &ctx(Provider::GitHub),
            &a,
            Some("x"),
            Some(Path::new("/tmp/body.md")),
        );
        let plan = call.plan_argv();
        let bf_idx = plan.iter().position(|s| s == "--body-file").unwrap();
        assert_eq!(plan[bf_idx + 1], "/tmp/body.md");
    }

    #[test]
    fn build_edit_call_gitlab_uses_description_inline() {
        let mut a = args(5);
        a.body = Some("inline body".into());
        let call = build_edit_call(&ctx(Provider::GitLab), &a, Some("inline body"), None);
        let plan = call.plan_argv();
        let d_idx = plan.iter().position(|s| s == "--description").unwrap();
        assert_eq!(plan[d_idx + 1], "inline body");
    }

    #[test]
    fn run_with_dry_run_github_with_body_writes_tempfile_and_returns_plan() {
        let runner = ScriptedRunner::with_stdout(Vec::new());
        let global = flags(Some(ProviderFlag::Github), true);
        let mut a = args(42);
        a.title = Some("update".into());
        a.body = Some(VALID_BODY.into());
        let code =
            run_with(&runner, &global, a, OutputFormat::Json, |_| None).expect("dry-run with body");
        assert_eq!(code, exit::SUCCESS);
        assert!(runner.captured.borrow().is_empty());
    }

    #[test]
    fn run_with_dry_run_gitlab_uses_description_inline() {
        let runner = ScriptedRunner::with_stdout(Vec::new());
        let global = flags(Some(ProviderFlag::Gitlab), true);
        let mut a = args(7);
        a.body = Some(VALID_BODY.into());
        a.add_labels = vec!["ready".into()];
        let code =
            run_with(&runner, &global, a, OutputFormat::Json, |_| None).expect("dry-run gitlab");
        assert_eq!(code, exit::SUCCESS);
    }

    #[test]
    fn run_with_happy_github_title_only_text_format() {
        let runner = ScriptedRunner::with_stdout(vec!["", &github_view_json(42)]);
        let global = flags(Some(ProviderFlag::Github), false);
        let mut a = args(42);
        a.title = Some("retitled".into());
        let code =
            run_with(&runner, &global, a, OutputFormat::Text, |_| None).expect("happy edit github");
        assert_eq!(code, exit::SUCCESS);
        let calls = runner.captured.borrow();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0][1..4], ["pr", "edit", "42"]);
    }

    #[test]
    fn run_with_happy_gitlab_labels_only_json_format() {
        let runner = ScriptedRunner::with_stdout(vec!["", &gitlab_view_json(7)]);
        let global = flags(Some(ProviderFlag::Gitlab), false);
        let mut a = args(7);
        a.add_labels = vec!["ready".into()];
        a.remove_labels = vec!["wip".into()];
        a.add_reviewers = vec!["alice".into()];
        a.base = Some("main".into());
        let code =
            run_with(&runner, &global, a, OutputFormat::Json, |_| None).expect("happy edit gitlab");
        assert_eq!(code, exit::SUCCESS);
        let calls = runner.captured.borrow();
        assert_eq!(calls[0][1..3], ["mr", "update"]);
    }

    #[test]
    fn run_with_reads_body_from_file_and_proceeds() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        tmp.write_all(VALID_BODY.as_bytes()).unwrap();
        let runner = ScriptedRunner::with_stdout(vec!["", &github_view_json(11)]);
        let global = flags(Some(ProviderFlag::Github), false);
        let mut a = args(11);
        a.body_file = Some(tmp.path().to_str().unwrap().into());
        let code = run_with(&runner, &global, a, OutputFormat::Json, |_| None)
            .expect("happy with body file");
        assert_eq!(code, exit::SUCCESS);
    }

    #[test]
    fn run_with_rejects_title_too_long() {
        let runner = ScriptedRunner::with_stdout(Vec::new());
        let global = flags(Some(ProviderFlag::Github), false);
        let mut a = args(1);
        a.title = Some("x".repeat(200));
        let err =
            run_with(&runner, &global, a, OutputFormat::Json, |_| None).expect_err("too long");
        assert_eq!(err.kind(), "title_too_long");
    }

    #[test]
    fn run_with_rejects_body_missing_summary() {
        let runner = ScriptedRunner::with_stdout(Vec::new());
        let global = flags(Some(ProviderFlag::Github), false);
        let mut a = args(1);
        a.body = Some("## Test plan\n\n- yes\n".into());
        let err = run_with(&runner, &global, a, OutputFormat::Json, |_| None)
            .expect_err("missing summary");
        assert_eq!(err.kind(), "body_missing_summary");
    }

    #[test]
    fn run_with_rejects_body_missing_test_plan() {
        let runner = ScriptedRunner::with_stdout(Vec::new());
        let global = flags(Some(ProviderFlag::Github), false);
        let mut a = args(1);
        a.body = Some("## Summary\n\nyes.\n".into());
        let err = run_with(&runner, &global, a, OutputFormat::Json, |_| None)
            .expect_err("missing test plan");
        assert_eq!(err.kind(), "body_missing_test_plan");
    }

    #[test]
    fn run_with_missing_body_file_returns_software_error() {
        let runner = ScriptedRunner::with_stdout(Vec::new());
        let global = flags(Some(ProviderFlag::Github), false);
        let mut a = args(1);
        a.body_file = Some("/no/such/path".into());
        let err = run_with(&runner, &global, a, OutputFormat::Json, |_| None)
            .expect_err("missing body file");
        assert_eq!(err.kind(), "software_error");
    }

    #[test]
    fn run_with_propagates_provider_detection_failure() {
        let runner = ScriptedRunner::with_stdout(Vec::new());
        let global = flags(None, false);
        let err = run_with(&runner, &global, args(1), OutputFormat::Json, |_| None)
            .expect_err("no provider");
        assert_eq!(err.kind(), "provider_unsupported");
    }

    #[test]
    fn run_with_invalid_view_json_is_software_error() {
        let runner = ScriptedRunner::with_stdout(vec!["", "not-json"]);
        let global = flags(Some(ProviderFlag::Github), false);
        let mut a = args(1);
        a.title = Some("ok".into());
        let err = run_with(&runner, &global, a, OutputFormat::Json, |_| None)
            .expect_err("view parse fails");
        assert_eq!(err.kind(), "software_error");
    }

    #[test]
    fn write_body_tempfile_persists_payload() {
        let tmp = write_body_tempfile("hello tempfile").expect("tempfile");
        let read = std::fs::read_to_string(tmp.path()).expect("read");
        assert_eq!(read, "hello tempfile");
    }
}
