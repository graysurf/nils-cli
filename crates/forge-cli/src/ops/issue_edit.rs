//! `issue edit` atom.
//!
//! Spec / ops: `cli.forge-cli.issue.edit.v1`. Supports partial mutation of
//! title / body / labels / assignees. `title_length` runs only when `--title`
//! is supplied. Backend argv reflects exactly the subset of flags the user
//! passed so we don't accidentally clear labels by sending empty values.

use std::ffi::OsString;
use std::fs;
use std::io::Read as _;

use nils_common::cli_contract::{OutputFormat, schema_version_for};
use serde::Serialize;

use crate::backend::{BackendCall, BackendProgram, BackendRunner, DryRunPayload, ProcessRunner};
use crate::cli::{BINARY, GlobalFlags, IssueEditArgs};
use crate::envelope::emit_success;
use crate::error::ForgeError;
use crate::ops::issue_view;
use crate::provider::{Provider, ProviderContext, detect, git_remote_url};
use crate::validations::title_length;

const SCHEMA: &str = "issue.edit";
const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct IssueEditPayload {
    pub provider: &'static str,
    pub number: u64,
    pub url: String,
    pub state: &'static str,
    pub title: String,
    pub labels: Vec<String>,
    pub assignees: Vec<String>,
}

pub fn run(
    global: &GlobalFlags,
    args: IssueEditArgs,
    format: OutputFormat,
) -> Result<i32, ForgeError> {
    let runner = ProcessRunner;
    run_with(&runner, global, args, format, git_remote_url)
}

pub fn run_with<R: BackendRunner, F: Fn(&str) -> Option<String>>(
    runner: &R,
    global: &GlobalFlags,
    args: IssueEditArgs,
    format: OutputFormat,
    remote_url_lookup: F,
) -> Result<i32, ForgeError> {
    let ctx = detect(
        global.provider_hint(),
        &global.remote,
        global.repo.as_deref(),
        remote_url_lookup,
    )?;
    if let Some(ref t) = args.title {
        title_length(t)?;
    }
    let body = if args.body.is_some() || args.body_file.is_some() {
        Some(read_body(args.body.as_deref(), args.body_file.as_deref())?)
    } else {
        None
    };
    let call = build_edit_call(&ctx, &args, body.as_deref());

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
    let view_output = runner.run(&issue_view::build_view_call(&ctx, args.id))?;
    let view = issue_view::parse_view_output(&ctx, &view_output)?;
    Ok(emit_success(
        schema_version_for(BINARY, SCHEMA, SCHEMA_VERSION),
        IssueEditPayload {
            provider: view.provider,
            number: view.number,
            url: view.url,
            state: view.state,
            title: view.title,
            labels: view.labels,
            assignees: view.assignees,
        },
        format,
        render_text,
    ))
}

fn build_edit_call(ctx: &ProviderContext, args: &IssueEditArgs, body: Option<&str>) -> BackendCall {
    let program = BackendProgram::for_provider(ctx.provider);
    let mut argv: Vec<OsString> = match ctx.provider {
        Provider::GitHub => vec![
            OsString::from("issue"),
            OsString::from("edit"),
            OsString::from(args.id.to_string()),
        ],
        Provider::GitLab => vec![
            OsString::from("issue"),
            OsString::from("update"),
            OsString::from(args.id.to_string()),
        ],
    };
    if let Some(t) = &args.title {
        argv.push(OsString::from("--title"));
        argv.push(OsString::from(t));
    }
    if let Some(b) = body {
        let body_flag = match ctx.provider {
            Provider::GitHub => "--body",
            Provider::GitLab => "--description",
        };
        argv.push(OsString::from(body_flag));
        argv.push(OsString::from(b));
    }
    for label in &args.add_label {
        let flag = match ctx.provider {
            Provider::GitHub => "--add-label",
            Provider::GitLab => "--label",
        };
        argv.push(OsString::from(flag));
        argv.push(OsString::from(label));
    }
    for label in &args.remove_label {
        let flag = match ctx.provider {
            Provider::GitHub => "--remove-label",
            Provider::GitLab => "--unlabel",
        };
        argv.push(OsString::from(flag));
        argv.push(OsString::from(label));
    }
    for assignee in &args.add_assignee {
        let flag = match ctx.provider {
            Provider::GitHub => "--add-assignee",
            Provider::GitLab => "--assignee",
        };
        argv.push(OsString::from(flag));
        argv.push(OsString::from(assignee));
    }
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
                "failed to read issue body from stdin",
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

fn schema_err() -> String {
    schema_version_for(BINARY, "error", 1)
}

fn render_text(payload: &IssueEditPayload) {
    println!(
        "edited {provider} issue #{number} ({state}): {url}",
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

    fn args() -> IssueEditArgs {
        IssueEditArgs {
            id: 7,
            title: None,
            body: None,
            body_file: None,
            add_label: vec![],
            remove_label: vec![],
            add_assignee: vec![],
        }
    }

    #[test]
    fn build_edit_call_github_emits_issue_edit_id() {
        let call = build_edit_call(&ctx(Provider::GitHub), &args(), None);
        let plan = call.plan_argv();
        assert_eq!(
            plan[1..4],
            ["issue".to_string(), "edit".to_string(), "7".to_string()]
        );
    }

    #[test]
    fn build_edit_call_gitlab_emits_issue_update_id() {
        let call = build_edit_call(&ctx(Provider::GitLab), &args(), None);
        let plan = call.plan_argv();
        assert_eq!(
            plan[1..4],
            ["issue".to_string(), "update".to_string(), "7".to_string()]
        );
    }

    #[test]
    fn build_edit_call_github_passes_through_optional_title_and_body() {
        let mut a = args();
        a.title = Some("new title".into());
        let call = build_edit_call(&ctx(Provider::GitHub), &a, Some("new body"));
        let plan = call.plan_argv();
        let t = plan.iter().position(|s| s == "--title").unwrap();
        assert_eq!(plan[t + 1], "new title");
        let b = plan.iter().position(|s| s == "--body").unwrap();
        assert_eq!(plan[b + 1], "new body");
    }

    #[test]
    fn build_edit_call_gitlab_uses_label_unlabel_flags() {
        let mut a = args();
        a.add_label = vec!["bug".into(), "p1".into()];
        a.remove_label = vec!["wontfix".into()];
        let call = build_edit_call(&ctx(Provider::GitLab), &a, None);
        let plan = call.plan_argv();
        let labels: Vec<usize> = plan
            .iter()
            .enumerate()
            .filter(|(_, s)| s.as_str() == "--label")
            .map(|(i, _)| i)
            .collect();
        assert_eq!(labels.len(), 2);
        assert_eq!(plan[labels[0] + 1], "bug");
        let unlabel = plan.iter().position(|s| s == "--unlabel").unwrap();
        assert_eq!(plan[unlabel + 1], "wontfix");
    }

    #[test]
    fn build_edit_call_github_uses_add_label_remove_label_flags() {
        let mut a = args();
        a.add_label = vec!["bug".into()];
        a.remove_label = vec!["stale".into()];
        let call = build_edit_call(&ctx(Provider::GitHub), &a, None);
        let plan = call.plan_argv();
        let al = plan.iter().position(|s| s == "--add-label").unwrap();
        let rl = plan.iter().position(|s| s == "--remove-label").unwrap();
        assert_eq!(plan[al + 1], "bug");
        assert_eq!(plan[rl + 1], "stale");
    }
}
