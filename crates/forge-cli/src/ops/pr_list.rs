//! `pr list` atom.
//!
//! Spec / ops: `cli.forge-cli.pr.list.v1`. Returns a flat array of PR/MR
//! summaries filtered by `--state`, `--author`, `--head`, `--limit`. Both
//! backends emit a JSON array; the normalizer flattens the per-provider
//! field names into the canonical envelope shape.

use std::ffi::OsString;

use nils_common::cli_contract::{OutputFormat, schema_version_for};
use serde::Serialize;

use crate::backend::{BackendCall, BackendProgram, BackendRunner, BackendSuccess, DryRunPayload};
use crate::cli::{BINARY, GlobalFlags, PrListArgs, PrStateFilter};
use crate::envelope::emit_success;
use crate::error::ForgeError;
use crate::ops::pr_state::normalize_state;
use crate::provider::{Provider, ProviderContext, detect, git_remote_url};
use crate::rate_limit::default_runner;

const SCHEMA: &str = "pr.list";
const SCHEMA_VERSION: u32 = 1;

const GH_JSON_FIELDS: &str = "number,url,state,title,headRefName,headRepository,author";

/// Envelope payload for `cli.forge-cli.pr.list.v1`.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PrListPayload {
    pub provider: &'static str,
    pub items: Vec<PrListItem>,
}

/// One row in the list envelope.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PrListItem {
    pub number: u64,
    pub url: String,
    pub state: &'static str,
    pub title: String,
    pub head: String,
    #[serde(skip_serializing)]
    pub head_repository: Option<String>,
    pub author: Option<String>,
}

pub fn run(
    global: &GlobalFlags,
    args: PrListArgs,
    format: OutputFormat,
) -> Result<i32, ForgeError> {
    let runner = default_runner();
    run_with(&runner, global, args, format, git_remote_url)
}

pub fn run_with<R: BackendRunner, F: Fn(&str) -> Option<String>>(
    runner: &R,
    global: &GlobalFlags,
    args: PrListArgs,
    format: OutputFormat,
    remote_url_lookup: F,
) -> Result<i32, ForgeError> {
    let ctx = detect(
        global.provider_hint(),
        &global.remote,
        global.repo.as_deref(),
        remote_url_lookup,
    )?;
    let call = build_list_call(&ctx, &args);

    if global.dry_run {
        let payload = DryRunPayload::new(ctx.provider, &call);
        return Ok(emit_success(
            schema_version_for(BINARY, SCHEMA, SCHEMA_VERSION),
            payload,
            format,
            |p| println!("would run: {plan}", plan = p.plan.join(" ")),
        ));
    }

    let output = runner.run(&call)?;
    let payload = parse_list_output(&ctx, &output)?;
    Ok(emit_success(
        schema_version_for(BINARY, SCHEMA, SCHEMA_VERSION),
        payload,
        format,
        render_text,
    ))
}

/// Macro-facing entry point: build the list call, run it, and return the
/// typed payload without emitting an envelope. Used by `pr deliver` for the
/// head-branch adopt lookup.
pub(crate) fn compute<R: BackendRunner>(
    runner: &R,
    ctx: &ProviderContext,
    args: &PrListArgs,
) -> Result<PrListPayload, ForgeError> {
    let call = build_list_call(ctx, args);
    let output = runner.run(&call)?;
    parse_list_output(ctx, &output)
}

pub(crate) fn build_list_call(ctx: &ProviderContext, args: &PrListArgs) -> BackendCall {
    let program = BackendProgram::for_provider(ctx.provider);
    let limit = args.limit.max(1);
    let mut argv: Vec<OsString> = Vec::new();
    match ctx.provider {
        Provider::GitHub | Provider::Local => {
            argv.push(OsString::from("pr"));
            argv.push(OsString::from("list"));
            argv.push(OsString::from("--state"));
            argv.push(OsString::from(args.state.as_str()));
            argv.push(OsString::from("--limit"));
            argv.push(OsString::from(limit.to_string()));
            if let Some(a) = &args.author {
                argv.push(OsString::from("--author"));
                argv.push(OsString::from(a));
            }
            if let Some(h) = &args.head {
                argv.push(OsString::from("--head"));
                argv.push(OsString::from(h));
            }
            argv.push(OsString::from("--json"));
            argv.push(OsString::from(GH_JSON_FIELDS));
        }
        Provider::GitLab => {
            argv.push(OsString::from("mr"));
            argv.push(OsString::from("list"));
            if let Some(flag) = gitlab_state_flag(args.state) {
                argv.push(OsString::from(flag));
            }
            argv.push(OsString::from("--per-page"));
            argv.push(OsString::from(limit.to_string()));
            if let Some(a) = &args.author {
                argv.push(OsString::from("--author"));
                argv.push(OsString::from(a));
            }
            if let Some(h) = &args.head {
                argv.push(OsString::from("--source-branch"));
                argv.push(OsString::from(h));
            }
            argv.push(OsString::from("--output"));
            argv.push(OsString::from("json"));
        }
    }
    ctx.push_repo_override(&mut argv);
    BackendCall::new(program, argv)
}

fn gitlab_state_flag(state: PrStateFilter) -> Option<&'static str> {
    // glab deprecated `--opened` in favor of "default when --closed/--merged
    // are not used" — and it prints the deprecation warning to stdout, which
    // breaks JSON parsing. Pass no flag for open.
    match state {
        PrStateFilter::Open => None,
        PrStateFilter::Closed => Some("--closed"),
        PrStateFilter::Merged => Some("--merged"),
        PrStateFilter::All => Some("--all"),
    }
}

pub fn parse_list_output(
    ctx: &ProviderContext,
    output: &BackendSuccess,
) -> Result<PrListPayload, ForgeError> {
    let value: serde_json::Value = serde_json::from_str(output.stdout.trim()).map_err(|e| {
        ForgeError::software(schema_err(), "pr list JSON is invalid", Some(e.to_string()))
    })?;
    let arr = value.as_array().ok_or_else(|| {
        ForgeError::software(
            schema_err(),
            "pr list JSON is not an array",
            Some(format!("got: {value}")),
        )
    })?;
    let items = arr
        .iter()
        .map(|raw| parse_item(raw, ctx))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(PrListPayload {
        provider: ctx.provider.as_str(),
        items,
    })
}

fn parse_item(raw: &serde_json::Value, ctx: &ProviderContext) -> Result<PrListItem, ForgeError> {
    match ctx.provider {
        Provider::GitHub | Provider::Local => Ok(PrListItem {
            number: raw
                .get("number")
                .and_then(|v| v.as_u64())
                .ok_or_else(|| missing("number"))?,
            url: required_str(raw, "url")?,
            state: normalize_state(
                raw.get("state").and_then(|v| v.as_str()).unwrap_or(""),
                ctx.provider,
            )?,
            title: required_str(raw, "title")?,
            head: required_str(raw, "headRefName")?,
            head_repository: raw
                .get("headRepository")
                .and_then(|repository| repository.get("nameWithOwner"))
                .and_then(|value| value.as_str())
                .map(|value| value.to_ascii_lowercase())
                .filter(|value| !value.is_empty()),
            author: raw
                .get("author")
                .and_then(|v| v.get("login").and_then(|n| n.as_str()))
                .map(str::to_string),
        }),
        Provider::GitLab => Ok(PrListItem {
            number: raw
                .get("iid")
                .and_then(|v| v.as_u64())
                .ok_or_else(|| missing("iid"))?,
            url: required_str(raw, "web_url")?,
            state: normalize_state(
                raw.get("state").and_then(|v| v.as_str()).unwrap_or(""),
                ctx.provider,
            )?,
            title: required_str(raw, "title")?,
            head: required_str(raw, "source_branch")?,
            head_repository: None,
            author: raw
                .get("author")
                .and_then(|v| v.get("username").and_then(|n| n.as_str()))
                .map(str::to_string),
        }),
    }
}

fn required_str(raw: &serde_json::Value, key: &str) -> Result<String, ForgeError> {
    raw.get(key)
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| missing(key))
}

fn missing(key: &str) -> ForgeError {
    ForgeError::software(
        schema_err(),
        format!("missing required field '{key}' in pr list item"),
        None,
    )
}

fn schema_err() -> String {
    schema_version_for(BINARY, "error", 1)
}

fn render_text(payload: &PrListPayload) {
    for item in &payload.items {
        let author = item.author.as_deref().unwrap_or("<unknown>");
        println!(
            "#{n} [{s}] {t} ({a}) — {u}",
            n = item.number,
            s = item.state,
            t = item.title,
            a = author,
            u = item.url,
        );
    }
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

    fn default_args() -> PrListArgs {
        PrListArgs {
            state: PrStateFilter::Open,
            author: None,
            head: None,
            limit: 30,
        }
    }

    #[test]
    fn build_list_call_github_passes_state_and_limit() {
        let call = build_list_call(&ctx(Provider::GitHub), &default_args());
        let plan = call.plan_argv();
        let s_idx = plan.iter().position(|s| s == "--state").unwrap();
        assert_eq!(plan[s_idx + 1], "open");
        let l_idx = plan.iter().position(|s| s == "--limit").unwrap();
        assert_eq!(plan[l_idx + 1], "30");
    }

    #[test]
    fn build_list_call_github_includes_optional_filters() {
        let mut args = default_args();
        args.author = Some("alice".into());
        args.head = Some("feat/x".into());
        args.limit = 1;
        args.state = PrStateFilter::Merged;
        let call = build_list_call(&ctx(Provider::GitHub), &args);
        let plan = call.plan_argv();
        let a_idx = plan.iter().position(|s| s == "--author").unwrap();
        assert_eq!(plan[a_idx + 1], "alice");
        let h_idx = plan.iter().position(|s| s == "--head").unwrap();
        assert_eq!(plan[h_idx + 1], "feat/x");
        let l_idx = plan.iter().position(|s| s == "--limit").unwrap();
        assert_eq!(plan[l_idx + 1], "1");
        let s_idx = plan.iter().position(|s| s == "--state").unwrap();
        assert_eq!(plan[s_idx + 1], "merged");
    }

    #[test]
    fn build_list_call_gitlab_omits_opened_flag_for_default_open_state() {
        let call = build_list_call(&ctx(Provider::GitLab), &default_args());
        let plan = call.plan_argv();
        assert!(
            !plan.contains(&"--opened".to_string()),
            "glab deprecated --opened and prints a warning to stdout; pass no flag for open"
        );
        assert!(!plan.contains(&"--closed".to_string()));
        assert!(!plan.contains(&"--merged".to_string()));
        assert!(!plan.contains(&"--all".to_string()));
    }

    #[test]
    fn build_list_call_gitlab_maps_state_to_flag() {
        let mut args = default_args();
        args.state = PrStateFilter::Merged;
        let call = build_list_call(&ctx(Provider::GitLab), &args);
        let plan = call.plan_argv();
        assert!(plan.contains(&"--merged".to_string()));
        assert!(plan.contains(&"--per-page".to_string()));
        assert!(plan.contains(&"--output".to_string()));
        assert!(!plan.contains(&"-F".to_string()));
    }

    #[test]
    fn parse_list_output_github() {
        let stdout = r#"[
          {"number":1,"url":"u1","state":"OPEN","title":"a","headRefName":"feat/a","author":{"login":"alice"}},
          {"number":2,"url":"u2","state":"CLOSED","title":"b","headRefName":"feat/b","author":{"login":"bob"}}
        ]"#;
        let payload = parse_list_output(
            &ctx(Provider::GitHub),
            &BackendSuccess {
                stdout: stdout.into(),
                stderr: String::new(),
            },
        )
        .unwrap();
        assert_eq!(payload.items.len(), 2);
        assert_eq!(payload.items[0].state, "open");
        assert_eq!(payload.items[1].state, "closed");
        assert_eq!(payload.items[0].author.as_deref(), Some("alice"));
    }

    #[test]
    fn parse_list_output_gitlab() {
        let stdout = r#"[
          {"iid":7,"web_url":"u","state":"opened","title":"x","source_branch":"feat/x","author":{"username":"alice"}}
        ]"#;
        let payload = parse_list_output(
            &ctx(Provider::GitLab),
            &BackendSuccess {
                stdout: stdout.into(),
                stderr: String::new(),
            },
        )
        .unwrap();
        assert_eq!(payload.items[0].number, 7);
        assert_eq!(payload.items[0].state, "open");
    }

    #[test]
    fn build_list_call_clamps_zero_limit_to_one() {
        let mut args = default_args();
        args.limit = 0;
        let call = build_list_call(&ctx(Provider::GitHub), &args);
        let plan = call.plan_argv();
        let l_idx = plan.iter().position(|s| s == "--limit").unwrap();
        assert_eq!(plan[l_idx + 1], "1");
    }

    mod run_with {
        use super::*;
        use crate::cli::ProviderFlag;
        use nils_common::cli_contract::exit;
        use pretty_assertions::assert_eq;
        use std::cell::RefCell;

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

        fn github_list_json() -> &'static str {
            r#"[{"number":1,"url":"u1","state":"OPEN","title":"a","headRefName":"feat/a","author":{"login":"alice"}}]"#
        }

        fn gitlab_list_json() -> &'static str {
            r#"[{"iid":1,"web_url":"u1","state":"opened","title":"a","source_branch":"feat/a","author":{"username":"alice"}}]"#
        }

        #[test]
        fn dry_run_github_emits_plan_envelope() {
            let runner = ScriptedRunner::with_stdout(Vec::new());
            let global = flags(Some(ProviderFlag::Github), true);
            let code = run_with(&runner, &global, default_args(), OutputFormat::Json, |_| {
                None
            })
            .expect("dry-run");
            assert_eq!(code, exit::SUCCESS);
            assert!(runner.captured.borrow().is_empty());
        }

        #[test]
        fn dry_run_text_format() {
            let runner = ScriptedRunner::with_stdout(Vec::new());
            let global = flags(Some(ProviderFlag::Gitlab), true);
            let code = run_with(&runner, &global, default_args(), OutputFormat::Text, |_| {
                None
            })
            .expect("dry-run text");
            assert_eq!(code, exit::SUCCESS);
        }

        #[test]
        fn happy_github_returns_items() {
            let runner = ScriptedRunner::with_stdout(vec![github_list_json()]);
            let global = flags(Some(ProviderFlag::Github), false);
            let code = run_with(&runner, &global, default_args(), OutputFormat::Json, |_| {
                None
            })
            .expect("happy github");
            assert_eq!(code, exit::SUCCESS);
            let calls = runner.captured.borrow();
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0][1..3], ["pr", "list"]);
        }

        #[test]
        fn happy_gitlab_returns_items_text_format() {
            let runner = ScriptedRunner::with_stdout(vec![gitlab_list_json()]);
            let global = flags(Some(ProviderFlag::Gitlab), false);
            let code = run_with(&runner, &global, default_args(), OutputFormat::Text, |_| {
                None
            })
            .expect("happy gitlab text");
            assert_eq!(code, exit::SUCCESS);
        }

        #[test]
        fn render_text_handles_missing_author() {
            let runner = ScriptedRunner::with_stdout(vec![
                r#"[{"number":3,"url":"u3","state":"OPEN","title":"c","headRefName":"feat/c"}]"#,
            ]);
            let global = flags(Some(ProviderFlag::Github), false);
            let code = run_with(&runner, &global, default_args(), OutputFormat::Text, |_| {
                None
            })
            .expect("happy github text no-author");
            assert_eq!(code, exit::SUCCESS);
        }

        #[test]
        fn propagates_provider_detection_failure() {
            let runner = ScriptedRunner::with_stdout(Vec::new());
            let global = flags(None, false);
            let err = run_with(&runner, &global, default_args(), OutputFormat::Json, |_| {
                None
            })
            .expect_err("no provider");
            assert_eq!(err.kind(), "provider_unsupported");
        }

        #[test]
        fn invalid_json_array_is_software_error() {
            let runner = ScriptedRunner::with_stdout(vec!["not-json"]);
            let global = flags(Some(ProviderFlag::Github), false);
            let err = run_with(&runner, &global, default_args(), OutputFormat::Json, |_| {
                None
            })
            .expect_err("invalid");
            assert_eq!(err.kind(), "software_error");
        }

        #[test]
        fn non_array_json_is_software_error() {
            let runner = ScriptedRunner::with_stdout(vec!["{}"]);
            let global = flags(Some(ProviderFlag::Github), false);
            let err = run_with(&runner, &global, default_args(), OutputFormat::Json, |_| {
                None
            })
            .expect_err("not array");
            assert_eq!(err.kind(), "software_error");
        }

        #[test]
        fn parse_item_missing_number_is_software_error() {
            let runner = ScriptedRunner::with_stdout(vec![
                r#"[{"url":"u","state":"OPEN","title":"t","headRefName":"h"}]"#,
            ]);
            let global = flags(Some(ProviderFlag::Github), false);
            let err = run_with(&runner, &global, default_args(), OutputFormat::Json, |_| {
                None
            })
            .expect_err("missing number");
            assert_eq!(err.kind(), "software_error");
        }

        #[test]
        fn parse_item_unknown_state_is_software_error() {
            let runner = ScriptedRunner::with_stdout(vec![
                r#"[{"number":1,"url":"u","state":"INVENTED","title":"t","headRefName":"h"}]"#,
            ]);
            let global = flags(Some(ProviderFlag::Github), false);
            let err = run_with(&runner, &global, default_args(), OutputFormat::Json, |_| {
                None
            })
            .expect_err("unknown state");
            assert_eq!(err.kind(), "software_error");
        }
    }
}
