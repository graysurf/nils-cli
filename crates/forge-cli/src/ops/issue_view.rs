//! `issue view` atom + shared issue JSON parser.
//!
//! Spec / ops: `cli.forge-cli.issue.view.v1`. Both backends emit a single JSON
//! object that we normalize into [`IssueViewPayload`]. The parser is `pub`
//! because `issue create / edit / close / reopen / comment` all re-fetch the
//! canonical view after their mutating call so the envelope reports the
//! post-action state.

use std::ffi::OsString;

use nils_common::cli_contract::{OutputFormat, schema_version_for};
use serde::Serialize;

use crate::backend::{
    BackendCall, BackendProgram, BackendRunner, BackendSuccess, DryRunPayload, ProcessRunner,
};
use crate::cli::{BINARY, GlobalFlags};
use crate::envelope::emit_success;
use crate::error::ForgeError;
use crate::ops::pr_state::normalize_state;
use crate::provider::{Provider, ProviderContext, detect, git_remote_url};

pub const SCHEMA: &str = "issue.view";
pub const SCHEMA_VERSION: u32 = 1;

const GH_JSON_FIELDS: &str = "number,url,state,title,labels,assignees,body";

/// Envelope payload for `cli.forge-cli.issue.view.v1`.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct IssueViewPayload {
    pub provider: &'static str,
    pub number: u64,
    pub url: String,
    pub state: &'static str,
    pub title: String,
    pub body: String,
    pub labels: Vec<String>,
    pub assignees: Vec<String>,
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
    let ctx = detect(global.provider_hint(), &global.remote, remote_url_lookup)?;
    let call = build_view_call(&ctx, id);

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
    let payload = parse_view_output(&ctx, &output)?;
    Ok(emit_success(
        schema_version_for(BINARY, SCHEMA, SCHEMA_VERSION),
        payload,
        format,
        render_text,
    ))
}

pub fn build_view_call(ctx: &ProviderContext, id: u64) -> BackendCall {
    let program = BackendProgram::for_provider(ctx.provider);
    let argv: Vec<OsString> = match ctx.provider {
        Provider::GitHub => vec![
            OsString::from("issue"),
            OsString::from("view"),
            OsString::from(id.to_string()),
            OsString::from("--json"),
            OsString::from(GH_JSON_FIELDS),
        ],
        Provider::GitLab => vec![
            OsString::from("issue"),
            OsString::from("view"),
            OsString::from(id.to_string()),
            OsString::from("-F"),
            OsString::from("json"),
        ],
    };
    BackendCall::new(program, argv)
}

pub fn parse_view_output(
    ctx: &ProviderContext,
    output: &BackendSuccess,
) -> Result<IssueViewPayload, ForgeError> {
    let value: serde_json::Value = serde_json::from_str(output.stdout.trim()).map_err(|e| {
        ForgeError::software(
            schema_err(),
            "issue view JSON is invalid",
            Some(e.to_string()),
        )
    })?;
    match ctx.provider {
        Provider::GitHub => parse_github(&value, ctx),
        Provider::GitLab => parse_gitlab(&value, ctx),
    }
}

fn parse_github(
    value: &serde_json::Value,
    ctx: &ProviderContext,
) -> Result<IssueViewPayload, ForgeError> {
    let state = normalize_state(
        value.get("state").and_then(|v| v.as_str()).unwrap_or(""),
        ctx.provider,
    )?;
    Ok(IssueViewPayload {
        provider: ctx.provider.as_str(),
        number: value
            .get("number")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| missing("number"))?,
        url: required_str(value, "url")?,
        state,
        title: required_str(value, "title")?,
        body: value
            .get("body")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        labels: github_name_list(value, "labels"),
        assignees: github_assignees(value),
    })
}

fn parse_gitlab(
    value: &serde_json::Value,
    ctx: &ProviderContext,
) -> Result<IssueViewPayload, ForgeError> {
    let state = normalize_state(
        value.get("state").and_then(|v| v.as_str()).unwrap_or(""),
        ctx.provider,
    )?;
    Ok(IssueViewPayload {
        provider: ctx.provider.as_str(),
        number: value
            .get("iid")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| missing("iid"))?,
        url: required_str(value, "web_url")?,
        state,
        title: required_str(value, "title")?,
        body: value
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        labels: gitlab_label_list(value),
        assignees: gitlab_assignees(value),
    })
}

fn github_name_list(value: &serde_json::Value, key: &str) -> Vec<String> {
    value
        .get(key)
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

fn github_assignees(value: &serde_json::Value) -> Vec<String> {
    value
        .get("assignees")
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

fn gitlab_label_list(value: &serde_json::Value) -> Vec<String> {
    value
        .get("labels")
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

fn gitlab_assignees(value: &serde_json::Value) -> Vec<String> {
    value
        .get("assignees")
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

fn required_str(value: &serde_json::Value, key: &str) -> Result<String, ForgeError> {
    value
        .get(key)
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| missing(key))
}

fn missing(key: &str) -> ForgeError {
    ForgeError::software(
        schema_err(),
        format!("missing required field '{key}' in issue view JSON"),
        None,
    )
}

pub(super) fn schema_err() -> String {
    schema_version_for(BINARY, "error", 1)
}

fn render_text(payload: &IssueViewPayload) {
    println!(
        "#{number} [{state}] {title}\n  {url}",
        number = payload.number,
        state = payload.state,
        title = payload.title,
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
            host: "example.com".into(),
            source: DetectionSource::Flag,
        }
    }

    #[test]
    fn build_view_call_github_uses_json_fields() {
        let call = build_view_call(&ctx(Provider::GitHub), 5);
        let plan = call.plan_argv();
        assert_eq!(plan[1..3], ["issue".to_string(), "view".to_string()]);
        assert!(plan.iter().any(|s| s == "--json"));
    }

    #[test]
    fn parse_github_open_issue() {
        let output = BackendSuccess {
            stdout: r#"{"number":7,"url":"u","state":"OPEN","title":"t","body":"b","labels":[{"name":"a"},{"name":"b"}],"assignees":[{"login":"x"}]}"#.into(),
            stderr: String::new(),
        };
        let p = parse_view_output(&ctx(Provider::GitHub), &output).unwrap();
        assert_eq!(p.state, "open");
        assert_eq!(p.labels, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(p.assignees, vec!["x".to_string()]);
        assert_eq!(p.body, "b");
    }

    #[test]
    fn parse_gitlab_opened_normalises_to_open() {
        let output = BackendSuccess {
            stdout: r#"{"iid":3,"web_url":"u","state":"opened","title":"t","description":"d","labels":["a"],"assignees":[{"username":"y"}]}"#.into(),
            stderr: String::new(),
        };
        let p = parse_view_output(&ctx(Provider::GitLab), &output).unwrap();
        assert_eq!(p.number, 3);
        assert_eq!(p.state, "open");
        assert_eq!(p.labels, vec!["a".to_string()]);
        assert_eq!(p.assignees, vec!["y".to_string()]);
        assert_eq!(p.body, "d");
    }

    #[test]
    fn parse_gitlab_closed_normalises_to_closed() {
        let output = BackendSuccess {
            stdout: r#"{"iid":3,"web_url":"u","state":"closed","title":"t","description":"","labels":[],"assignees":[]}"#.into(),
            stderr: String::new(),
        };
        let p = parse_view_output(&ctx(Provider::GitLab), &output).unwrap();
        assert_eq!(p.state, "closed");
    }
}
