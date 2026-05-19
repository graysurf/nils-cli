//! `issue create` atom.
//!
//! Spec / ops: `cli.forge-cli.issue.create.v1`. The only validation is
//! `title_length` (≤70 chars per spec). Body comes from `--body`,
//! `--body-file <path>`, or `--body-file -` (stdin). The body is always
//! materialised into a tempfile and handed to the backend as `--body-file`
//! (gh) or `--description` (glab) so argv stays stable when bodies contain
//! shell-hostile characters.

use std::ffi::OsString;
use std::fs;
use std::io::Read as _;

use nils_common::cli_contract::{OutputFormat, schema_version_for};
use serde::Serialize;
use tempfile::NamedTempFile;

use crate::backend::{
    BackendCall, BackendProgram, BackendRunner, BackendSuccess, DryRunPayload, ProcessRunner,
};
use crate::cli::{BINARY, GlobalFlags, IssueCreateArgs};
use crate::envelope::emit_success;
use crate::error::ForgeError;
use crate::ops::issue_view;
use crate::provider::{Provider, ProviderContext, detect, git_remote_url};
use crate::validations::title_length;

const SCHEMA: &str = "issue.create";
const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct IssueCreatePayload {
    pub provider: &'static str,
    pub number: u64,
    pub url: String,
    pub title: String,
    pub state: &'static str,
    pub labels: Vec<String>,
    pub assignees: Vec<String>,
}

pub fn run(
    global: &GlobalFlags,
    args: IssueCreateArgs,
    format: OutputFormat,
) -> Result<i32, ForgeError> {
    let runner = ProcessRunner;
    run_with(&runner, global, args, format, git_remote_url)
}

pub fn run_with<R: BackendRunner, F: Fn(&str) -> Option<String>>(
    runner: &R,
    global: &GlobalFlags,
    args: IssueCreateArgs,
    format: OutputFormat,
    remote_url_lookup: F,
) -> Result<i32, ForgeError> {
    let ctx = detect(global.provider_hint(), &global.remote, remote_url_lookup)?;
    title_length(&args.title)?;
    let body = read_body(args.body.as_deref(), args.body_file.as_deref())?;
    let body_tempfile = write_body_tempfile(&body)?;
    let body_path = body_tempfile.path().to_path_buf();
    let call = build_create_call(
        &ctx,
        &args.title,
        &body,
        &body_path,
        &args.labels,
        &args.assignees,
    );

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
    let (number, url) = parse_create_output(&ctx, &output)?;

    // Re-fetch via issue view so the envelope matches the spec data shape.
    let view_output = runner.run(&issue_view::build_view_call(&ctx, number))?;
    let view = issue_view::parse_view_output(&ctx, &view_output)?;
    Ok(emit_success(
        schema_version_for(BINARY, SCHEMA, SCHEMA_VERSION),
        IssueCreatePayload {
            provider: view.provider,
            number: view.number,
            url: if url.is_empty() { view.url } else { url },
            title: view.title,
            state: view.state,
            labels: view.labels,
            assignees: view.assignees,
        },
        format,
        render_text,
    ))
}

fn build_create_call(
    ctx: &ProviderContext,
    title: &str,
    body: &str,
    body_path: &std::path::Path,
    labels: &[String],
    assignees: &[String],
) -> BackendCall {
    let program = BackendProgram::for_provider(ctx.provider);
    let mut argv: Vec<OsString> = match ctx.provider {
        Provider::GitHub => vec![
            OsString::from("issue"),
            OsString::from("create"),
            OsString::from("--title"),
            OsString::from(title),
            OsString::from("--body-file"),
            OsString::from(body_path),
        ],
        Provider::GitLab => vec![
            OsString::from("issue"),
            OsString::from("create"),
            OsString::from("--title"),
            OsString::from(title),
            OsString::from("--description"),
            OsString::from(body),
        ],
    };
    match ctx.provider {
        Provider::GitHub => {
            for l in labels {
                argv.push(OsString::from("--label"));
                argv.push(OsString::from(l));
            }
            for a in assignees {
                argv.push(OsString::from("--assignee"));
                argv.push(OsString::from(a));
            }
        }
        Provider::GitLab => {
            for l in labels {
                argv.push(OsString::from("--label"));
                argv.push(OsString::from(l));
            }
            for a in assignees {
                argv.push(OsString::from("--assignee"));
                argv.push(OsString::from(a));
            }
        }
    }
    BackendCall::new(program, argv)
}

fn parse_create_output(
    ctx: &ProviderContext,
    output: &BackendSuccess,
) -> Result<(u64, String), ForgeError> {
    // gh / glab both print a URL trailing the create command; pull the last
    // line that looks like a URL and infer the number from its tail.
    let url = output
        .stdout
        .lines()
        .rev()
        .find(|l| l.trim_start().starts_with("http"))
        .unwrap_or("")
        .trim()
        .to_string();
    if url.is_empty() {
        return Err(ForgeError::software(
            schema_err(),
            "issue create did not print a URL",
            Some(format!("stdout={:?}", output.stdout)),
        ));
    }
    let number = url
        .rsplit('/')
        .find(|seg| seg.chars().all(|c| c.is_ascii_digit()) && !seg.is_empty())
        .and_then(|seg| seg.parse::<u64>().ok())
        .ok_or_else(|| {
            ForgeError::software(
                schema_err(),
                format!(
                    "could not parse issue number from URL '{url}' ({})",
                    ctx.provider.as_str()
                ),
                None,
            )
        })?;
    Ok((number, url))
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

fn write_body_tempfile(body: &str) -> Result<NamedTempFile, ForgeError> {
    let tmp = tempfile::Builder::new()
        .prefix("forge-cli-issue-body-")
        .suffix(".md")
        .tempfile()
        .map_err(|e| {
            ForgeError::software(
                schema_err(),
                "failed to create issue body tempfile",
                Some(e.to_string()),
            )
        })?;
    fs::write(tmp.path(), body).map_err(|e| {
        ForgeError::software(
            schema_err(),
            "failed to write issue body tempfile",
            Some(e.to_string()),
        )
    })?;
    Ok(tmp)
}

fn schema_err() -> String {
    schema_version_for(BINARY, "error", 1)
}

fn render_text(payload: &IssueCreatePayload) {
    println!(
        "opened {provider} issue #{number}: {url}",
        provider = payload.provider,
        number = payload.number,
        url = payload.url,
    );
}

// `_path` placeholder kept for IDE breadcrumb when grepping for body-file
// callers; the real path comes from the tempfile handle.
#[allow(dead_code)]
const _PATH_BREADCRUMB: &str = "body-file -> tempfile";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::DetectionSource;
    use pretty_assertions::assert_eq;
    use std::path::PathBuf;

    fn ctx(p: Provider) -> ProviderContext {
        ProviderContext {
            provider: p,
            host: "x".into(),
            source: DetectionSource::Flag,
        }
    }

    #[test]
    fn build_create_call_github_uses_body_file_and_labels() {
        let path = PathBuf::from("/tmp/body.md");
        let call = build_create_call(
            &ctx(Provider::GitHub),
            "title",
            "body",
            &path,
            &["bug".into(), "p1".into()],
            &["alice".into()],
        );
        let plan = call.plan_argv();
        assert_eq!(plan[1..3], ["issue".to_string(), "create".to_string()]);
        let title_idx = plan.iter().position(|s| s == "--title").unwrap();
        assert_eq!(plan[title_idx + 1], "title");
        let bf_idx = plan.iter().position(|s| s == "--body-file").unwrap();
        assert_eq!(plan[bf_idx + 1], "/tmp/body.md");
        // labels and assignees come through as repeated flag pairs.
        let label_count = plan.iter().filter(|s| s.as_str() == "--label").count();
        assert_eq!(label_count, 2);
        let asg_count = plan.iter().filter(|s| s.as_str() == "--assignee").count();
        assert_eq!(asg_count, 1);
    }

    #[test]
    fn build_create_call_gitlab_uses_description_inline() {
        let path = PathBuf::from("/tmp/body.md");
        let call = build_create_call(&ctx(Provider::GitLab), "t", "lengthy body", &path, &[], &[]);
        let plan = call.plan_argv();
        let d = plan.iter().position(|s| s == "--description").unwrap();
        assert_eq!(plan[d + 1], "lengthy body");
        assert!(!plan.iter().any(|s| s == "--body-file"));
    }

    #[test]
    fn parse_create_output_extracts_number_from_url() {
        let out = BackendSuccess {
            stdout: "creating issue...\nhttps://github.com/acme/widgets/issues/42\n".into(),
            stderr: String::new(),
        };
        let (n, u) = parse_create_output(&ctx(Provider::GitHub), &out).unwrap();
        assert_eq!(n, 42);
        assert!(u.ends_with("/42"));
    }

    #[test]
    fn parse_create_output_errors_when_no_url() {
        let out = BackendSuccess {
            stdout: "(no url)".into(),
            stderr: String::new(),
        };
        let err = parse_create_output(&ctx(Provider::GitHub), &out).expect_err("must fail");
        assert_eq!(err.kind(), "software_error");
    }

    #[test]
    fn read_body_prefers_inline_over_file() {
        assert_eq!(
            read_body(Some("inline"), Some("/no/such")).unwrap(),
            "inline"
        );
    }
}
