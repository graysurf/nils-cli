//! `issue list` atom.
//!
//! Spec / ops: `cli.forge-cli.issue.list.v1`. Returns a flat array of
//! issue summaries filtered by `--state`, `--label`, `--author`,
//! `--assignee`, `--limit`. Mirrors the shape of `pr list` so callers can
//! treat both subtrees uniformly. Labels are repeatable; the GitHub
//! backend joins them into a comma list per `gh issue list --label`'s
//! contract.

use std::ffi::OsString;

use nils_common::cli_contract::{OutputFormat, schema_version_for};
use serde::Serialize;

use crate::backend::{
    BackendCall, BackendProgram, BackendRunner, BackendSuccess, DryRunPayload, ProcessRunner,
};
use crate::cli::{BINARY, GlobalFlags, IssueListArgs, IssueStateFilter};
use crate::envelope::emit_success;
use crate::error::ForgeError;
use crate::ops::pr_state::normalize_state;
use crate::provider::{Provider, ProviderContext, detect, git_remote_url};

const SCHEMA: &str = "issue.list";
const SCHEMA_VERSION: u32 = 1;

const GH_JSON_FIELDS: &str = "number,url,state,title,labels,author,assignees";

/// Envelope payload for `cli.forge-cli.issue.list.v1`.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct IssueListPayload {
    pub provider: &'static str,
    pub items: Vec<IssueListItem>,
}

/// One row in the issue list envelope. Mirrors `IssueViewPayload`
/// minus the body field — list endpoints do not return issue bodies
/// on either backend.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct IssueListItem {
    pub number: u64,
    pub url: String,
    pub state: &'static str,
    pub title: String,
    pub labels: Vec<String>,
    pub author: Option<String>,
    pub assignees: Vec<String>,
}

pub fn run(
    global: &GlobalFlags,
    args: IssueListArgs,
    format: OutputFormat,
) -> Result<i32, ForgeError> {
    let runner = ProcessRunner;
    run_with(&runner, global, args, format, git_remote_url)
}

pub fn run_with<R: BackendRunner, F: Fn(&str) -> Option<String>>(
    runner: &R,
    global: &GlobalFlags,
    args: IssueListArgs,
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

fn build_list_call(ctx: &ProviderContext, args: &IssueListArgs) -> BackendCall {
    let program = BackendProgram::for_provider(ctx.provider);
    let limit = args.limit.max(1);
    let mut argv: Vec<OsString> = Vec::new();
    match ctx.provider {
        Provider::GitHub => {
            argv.push(OsString::from("issue"));
            argv.push(OsString::from("list"));
            argv.push(OsString::from("--state"));
            argv.push(OsString::from(args.state.as_str()));
            argv.push(OsString::from("--limit"));
            argv.push(OsString::from(limit.to_string()));
            if !args.labels.is_empty() {
                argv.push(OsString::from("--label"));
                argv.push(OsString::from(args.labels.join(",")));
            }
            if let Some(a) = &args.author {
                argv.push(OsString::from("--author"));
                argv.push(OsString::from(a));
            }
            if let Some(a) = &args.assignee {
                argv.push(OsString::from("--assignee"));
                argv.push(OsString::from(a));
            }
            argv.push(OsString::from("--json"));
            argv.push(OsString::from(GH_JSON_FIELDS));
        }
        Provider::GitLab => {
            argv.push(OsString::from("issue"));
            argv.push(OsString::from("list"));
            if let Some(flag) = gitlab_state_flag(args.state) {
                argv.push(OsString::from(flag));
            }
            argv.push(OsString::from("--per-page"));
            argv.push(OsString::from(limit.to_string()));
            for label in &args.labels {
                argv.push(OsString::from("--label"));
                argv.push(OsString::from(label));
            }
            if let Some(a) = &args.author {
                argv.push(OsString::from("--author"));
                argv.push(OsString::from(a));
            }
            if let Some(a) = &args.assignee {
                argv.push(OsString::from("--assignee"));
                argv.push(OsString::from(a));
            }
            argv.push(OsString::from("--output"));
            argv.push(OsString::from("json"));
        }
    }
    ctx.push_repo_override(&mut argv);
    BackendCall::new(program, argv)
}

fn gitlab_state_flag(state: IssueStateFilter) -> Option<&'static str> {
    match state {
        // `glab issue list` deprecated `--opened` in favor of "default when
        // `--closed` is not used" — and it prints the deprecation warning to
        // stdout, which breaks JSON parsing. Pass no flag for open.
        IssueStateFilter::Open => None,
        IssueStateFilter::Closed => Some("--closed"),
        IssueStateFilter::All => Some("--all"),
    }
}

pub fn parse_list_output(
    ctx: &ProviderContext,
    output: &BackendSuccess,
) -> Result<IssueListPayload, ForgeError> {
    let value: serde_json::Value = serde_json::from_str(output.stdout.trim()).map_err(|e| {
        ForgeError::software(
            schema_err(),
            "issue list JSON is invalid",
            Some(e.to_string()),
        )
    })?;
    let arr = value.as_array().ok_or_else(|| {
        ForgeError::software(
            schema_err(),
            "issue list JSON is not an array",
            Some(format!("got: {value}")),
        )
    })?;
    let items = arr
        .iter()
        .map(|raw| parse_item(raw, ctx))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(IssueListPayload {
        provider: ctx.provider.as_str(),
        items,
    })
}

fn parse_item(raw: &serde_json::Value, ctx: &ProviderContext) -> Result<IssueListItem, ForgeError> {
    match ctx.provider {
        Provider::GitHub => Ok(IssueListItem {
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
            labels: github_name_list(raw, "labels"),
            author: raw
                .get("author")
                .and_then(|v| v.get("login").and_then(|n| n.as_str()))
                .map(str::to_string),
            assignees: github_login_list(raw, "assignees"),
        }),
        Provider::GitLab => Ok(IssueListItem {
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
            labels: gitlab_label_list(raw),
            author: raw
                .get("author")
                .and_then(|v| v.get("username").and_then(|n| n.as_str()))
                .map(str::to_string),
            assignees: gitlab_username_list(raw, "assignees"),
        }),
    }
}

fn github_name_list(raw: &serde_json::Value, key: &str) -> Vec<String> {
    raw.get(key)
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| {
                    item.get("name")
                        .and_then(|n| n.as_str())
                        .map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default()
}

fn github_login_list(raw: &serde_json::Value, key: &str) -> Vec<String> {
    raw.get(key)
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| {
                    item.get("login")
                        .and_then(|n| n.as_str())
                        .map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default()
}

fn gitlab_label_list(raw: &serde_json::Value) -> Vec<String> {
    raw.get("labels")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| {
                    item.as_str().map(str::to_string).or_else(|| {
                        item.get("name")
                            .and_then(|n| n.as_str())
                            .map(str::to_string)
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn gitlab_username_list(raw: &serde_json::Value, key: &str) -> Vec<String> {
    raw.get(key)
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| {
                    item.get("username")
                        .and_then(|n| n.as_str())
                        .map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default()
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
        format!("missing required field '{key}' in issue list item"),
        None,
    )
}

fn schema_err() -> String {
    schema_version_for(BINARY, "error", 1)
}

fn render_text(payload: &IssueListPayload) {
    for item in &payload.items {
        let author = item.author.as_deref().unwrap_or("<unknown>");
        let labels = if item.labels.is_empty() {
            String::new()
        } else {
            format!(" {{{}}}", item.labels.join(","))
        };
        println!(
            "#{n} [{s}]{l} {t} ({a}) — {u}",
            n = item.number,
            s = item.state,
            l = labels,
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

    fn default_args() -> IssueListArgs {
        IssueListArgs {
            state: IssueStateFilter::Open,
            labels: Vec::new(),
            author: None,
            assignee: None,
            limit: 30,
        }
    }

    #[test]
    fn build_list_call_github_passes_state_and_limit() {
        let call = build_list_call(&ctx(Provider::GitHub), &default_args());
        let plan = call.plan_argv();
        assert_eq!(plan[1..3], ["issue".to_string(), "list".to_string()]);
        let s_idx = plan.iter().position(|s| s == "--state").unwrap();
        assert_eq!(plan[s_idx + 1], "open");
        let l_idx = plan.iter().position(|s| s == "--limit").unwrap();
        assert_eq!(plan[l_idx + 1], "30");
        // No --label flag when labels list is empty.
        assert!(!plan.iter().any(|s| s == "--label"));
    }

    #[test]
    fn build_list_call_github_joins_labels_into_csv() {
        let mut args = default_args();
        args.labels = vec!["plan".into(), "support-matrix".into()];
        let call = build_list_call(&ctx(Provider::GitHub), &args);
        let plan = call.plan_argv();
        let l_idx = plan.iter().position(|s| s == "--label").unwrap();
        assert_eq!(plan[l_idx + 1], "plan,support-matrix");
    }

    #[test]
    fn build_list_call_github_includes_optional_filters() {
        let mut args = default_args();
        args.labels = vec!["bug".into()];
        args.author = Some("alice".into());
        args.assignee = Some("bob".into());
        args.limit = 5;
        args.state = IssueStateFilter::Closed;
        let call = build_list_call(&ctx(Provider::GitHub), &args);
        let plan = call.plan_argv();
        let s_idx = plan.iter().position(|s| s == "--state").unwrap();
        assert_eq!(plan[s_idx + 1], "closed");
        let a_idx = plan.iter().position(|s| s == "--author").unwrap();
        assert_eq!(plan[a_idx + 1], "alice");
        let asn_idx = plan.iter().position(|s| s == "--assignee").unwrap();
        assert_eq!(plan[asn_idx + 1], "bob");
        let l_idx = plan.iter().position(|s| s == "--limit").unwrap();
        assert_eq!(plan[l_idx + 1], "5");
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
        assert!(!plan.contains(&"--all".to_string()));
    }

    #[test]
    fn build_list_call_gitlab_maps_state_to_flag_and_repeats_labels() {
        let mut args = default_args();
        args.state = IssueStateFilter::Closed;
        args.labels = vec!["plan".into(), "support-matrix".into()];
        let call = build_list_call(&ctx(Provider::GitLab), &args);
        let plan = call.plan_argv();
        assert!(plan.contains(&"--closed".to_string()));
        assert!(plan.contains(&"--per-page".to_string()));
        assert!(plan.contains(&"--output".to_string()));
        assert!(!plan.contains(&"-F".to_string()));
        let label_count = plan.iter().filter(|s| *s == "--label").count();
        assert_eq!(
            label_count, 2,
            "GitLab labels are passed via repeated --label"
        );
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

    #[test]
    fn parse_list_output_github_normalises_state_and_collects_labels() {
        let stdout = r#"[
          {"number":1,"url":"u1","state":"OPEN","title":"a",
           "labels":[{"name":"plan"},{"name":"support-matrix"}],
           "author":{"login":"alice"},"assignees":[{"login":"bob"}]},
          {"number":2,"url":"u2","state":"CLOSED","title":"b",
           "labels":[],"author":{"login":"carol"},"assignees":[]}
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
        assert_eq!(
            payload.items[0].labels,
            vec!["plan".to_string(), "support-matrix".to_string()]
        );
        assert_eq!(payload.items[0].author.as_deref(), Some("alice"));
        assert_eq!(payload.items[0].assignees, vec!["bob".to_string()]);
        assert_eq!(payload.items[1].state, "closed");
        assert!(payload.items[1].labels.is_empty());
    }

    #[test]
    fn parse_list_output_gitlab_accepts_string_or_object_labels() {
        let stdout = r#"[
          {"iid":7,"web_url":"u","state":"opened","title":"x",
           "labels":["plan", {"name":"support-matrix"}],
           "author":{"username":"alice"},"assignees":[{"username":"bob"}]}
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
        assert_eq!(
            payload.items[0].labels,
            vec!["plan".to_string(), "support-matrix".to_string()]
        );
        assert_eq!(payload.items[0].author.as_deref(), Some("alice"));
    }

    #[test]
    fn parse_list_output_accepts_empty_array_for_both_providers() {
        for provider in [Provider::GitHub, Provider::GitLab] {
            let payload = parse_list_output(
                &ctx(provider),
                &BackendSuccess {
                    stdout: "[]".into(),
                    stderr: String::new(),
                },
            )
            .unwrap();
            assert!(
                payload.items.is_empty(),
                "{provider:?} should accept empty array without erroring"
            );
        }
    }

    #[test]
    fn parse_list_output_rejects_non_array() {
        let err = parse_list_output(
            &ctx(Provider::GitHub),
            &BackendSuccess {
                stdout: r#"{"number":1}"#.into(),
                stderr: String::new(),
            },
        )
        .unwrap_err();
        let rendered = format!("{err:?}");
        assert!(
            rendered.contains("not an array"),
            "expected 'not an array' in error, got {rendered}"
        );
    }
}
