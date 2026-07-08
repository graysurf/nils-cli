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
    if global.is_local() {
        let runner = crate::local::LocalRunner::from_global(global)?;
        return run_with(&runner, global, args, format, git_remote_url);
    }
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
    // Labeled GitHub lookups take the REST route to dodge the SearchType cache
    // (see `github_rest_list`). It scans page by page and stops early, so a
    // dry run shows the first page's command.
    if github_rest_list(&ctx, &args) {
        if global.dry_run {
            let call = build_github_rest_call(&ctx, &args, 1);
            let payload = DryRunPayload::new(ctx.provider, &call);
            return Ok(emit_success(
                schema_version_for(BINARY, SCHEMA, SCHEMA_VERSION),
                payload,
                format,
                |p| println!("would run: {plan}", plan = p.plan.join(" ")),
            ));
        }
        let payload = run_github_rest_list(runner, &ctx, &args)?;
        return Ok(emit_success(
            schema_version_for(BINARY, SCHEMA, SCHEMA_VERSION),
            payload,
            format,
            render_text,
        ));
    }

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
        Provider::GitHub | Provider::Local => {
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

/// Labels with surrounding whitespace trimmed and blanks dropped.
fn effective_labels(args: &IssueListArgs) -> Vec<&str> {
    args.labels
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Whether the author/assignee filter uses gh's `@me` authenticated-user
/// shortcut. `gh issue list` resolves `@me`, but the REST issues endpoint's
/// `creator`/`assignee` params take a literal login, so a REST route would send
/// `@me` verbatim and match nothing. Such lookups stay on `gh issue list`.
fn uses_me_shortcut(args: &IssueListArgs) -> bool {
    let is_me = |v: &Option<String>| {
        v.as_deref()
            .is_some_and(|s| s.trim().eq_ignore_ascii_case("@me"))
    };
    is_me(&args.author) || is_me(&args.assignee)
}

/// Whether a GitHub issue-list lookup should route through the REST
/// `repos/<slug>/issues` endpoint instead of `gh issue list`.
///
/// `gh issue list --label` resolves through gh's GraphQL `SearchType` query,
/// which gh caches for 24h. If that cache captures a transient
/// `X-Ratelimit-Remaining: 0`, every later labeled list is refused for up to a
/// day even while the real quota is healthy (cli/cli#12812) — which hard-blocks
/// `plan-issue record open` dedup (sympoies/nils-cli#1050). A REST list never
/// touches that cache path. Only the labeled GitHub path is affected: the
/// no-label list and GitLab are already safe, REST needs a repo slug to build
/// the endpoint (fall back to `gh issue list` when none is known), and an
/// `@me` author/assignee filter stays on `gh issue list` so the shortcut still
/// resolves (see `uses_me_shortcut`).
fn github_rest_list(ctx: &ProviderContext, args: &IssueListArgs) -> bool {
    matches!(ctx.provider, Provider::GitHub)
        && ctx.repo.is_some()
        && !effective_labels(args).is_empty()
        && !uses_me_shortcut(args)
}

/// GitHub caps REST `per_page` at 100; request full pages to minimise round
/// trips during the scan.
const REST_PER_PAGE: u32 = 100;

/// Safety cap on pages walked by [`run_github_rest_list`]. The scan already
/// stops as soon as it has enough issues or reaches the final page; this only
/// bounds the degenerate case of a label shared by thousands of pull requests
/// and almost no issues, so it never walks unboundedly. Issue-only tracker
/// scans (`record open` uses `--limit 200`) settle in one or two pages and
/// never approach it.
const REST_MAX_PAGES: u32 = 20;

/// Build the page-`page` `gh api` REST call for a labeled GitHub issue-list
/// lookup. Callers must have confirmed [`github_rest_list`] first (repo slug
/// present).
fn build_github_rest_call(ctx: &ProviderContext, args: &IssueListArgs, page: u32) -> BackendCall {
    let slug = ctx.repo.as_deref().unwrap_or_default();
    let labels_csv = effective_labels(args).join(",");
    let mut argv: Vec<OsString> = vec![OsString::from("api")];
    // Target the detected host so GitHub Enterprise remotes hit the right API
    // (`gh api` defaults to github.com without `--hostname`).
    ctx.push_github_api_hostname(&mut argv);
    // `-X GET` keeps `-f` params in the query string; without it, `gh api`
    // switches to POST once any field is present. One explicit page per call
    // (no `--paginate`) lets the scan loop stop early instead of walking every
    // page.
    argv.extend([
        OsString::from("-X"),
        OsString::from("GET"),
        OsString::from(format!("repos/{slug}/issues")),
        OsString::from("-f"),
        OsString::from(format!("state={}", args.state.as_str())),
        OsString::from("-f"),
        OsString::from(format!("labels={labels_csv}")),
    ]);
    if let Some(a) = &args.author {
        argv.push(OsString::from("-f"));
        argv.push(OsString::from(format!("creator={a}")));
    }
    if let Some(a) = &args.assignee {
        argv.push(OsString::from("-f"));
        argv.push(OsString::from(format!("assignee={a}")));
    }
    argv.push(OsString::from("-f"));
    argv.push(OsString::from(format!("per_page={REST_PER_PAGE}")));
    argv.push(OsString::from("-f"));
    argv.push(OsString::from(format!("page={page}")));
    BackendCall::new(BackendProgram::Gh, argv)
}

/// Scan `repos/<slug>/issues` page by page until `--limit` issues are
/// collected, `--limit` cannot be satisfied (a short final page), or the page
/// cap is hit. Fetching one page at a time and stopping early keeps a small
/// `--limit` cheap even on repositories with a huge matching set, and keeps
/// paging when a page is dominated by pull requests so the limit is still
/// filled with real issues.
fn run_github_rest_list<R: BackendRunner>(
    runner: &R,
    ctx: &ProviderContext,
    args: &IssueListArgs,
) -> Result<IssueListPayload, ForgeError> {
    let limit = args.limit.max(1) as usize;
    let mut items: Vec<IssueListItem> = Vec::new();
    for page in 1..=REST_MAX_PAGES {
        let call = build_github_rest_call(ctx, args, page);
        let output = runner.run(&call)?;
        let (rows, mut issues) = parse_rest_page(&output)?;
        items.append(&mut issues);
        // The REST endpoint returns fewer than `per_page` rows only on the
        // final page, so a short page means the matching set is exhausted.
        if items.len() >= limit || (rows as u32) < REST_PER_PAGE {
            break;
        }
    }
    items.truncate(limit);
    Ok(IssueListPayload {
        provider: ctx.provider.as_str(),
        items,
    })
}

/// Parse one flat page of `gh api repos/<slug>/issues`, returning the raw row
/// count (issues + PRs — used to detect the final page) alongside the non-PR
/// issue rows.
///
/// The REST issues endpoint also returns pull requests (which carry a
/// `pull_request` key that plain issues never have); drop them. REST field
/// names differ from `gh issue list --json`: `html_url` for the URL and `user`
/// for the author.
fn parse_rest_page(output: &BackendSuccess) -> Result<(usize, Vec<IssueListItem>), ForgeError> {
    let value: serde_json::Value = serde_json::from_str(output.stdout.trim()).map_err(|e| {
        ForgeError::software(
            schema_err(),
            "issue list JSON is invalid",
            Some(e.to_string()),
        )
    })?;
    let rows = value.as_array().ok_or_else(|| {
        ForgeError::software(
            schema_err(),
            "issue list JSON is not an array",
            Some(format!("got: {value}")),
        )
    })?;
    let mut items = Vec::new();
    for raw in rows {
        if raw.get("pull_request").is_some() {
            continue;
        }
        items.push(parse_item_github_rest(raw)?);
    }
    Ok((rows.len(), items))
}

fn parse_item_github_rest(raw: &serde_json::Value) -> Result<IssueListItem, ForgeError> {
    Ok(IssueListItem {
        number: raw
            .get("number")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| missing("number"))?,
        url: required_str(raw, "html_url")?,
        state: normalize_state(
            raw.get("state").and_then(|v| v.as_str()).unwrap_or(""),
            Provider::GitHub,
        )?,
        title: required_str(raw, "title")?,
        labels: github_name_list(raw, "labels"),
        author: raw
            .get("user")
            .and_then(|v| v.get("login").and_then(|n| n.as_str()))
            .map(str::to_string),
        assignees: github_login_list(raw, "assignees"),
    })
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
        Provider::GitHub | Provider::Local => Ok(IssueListItem {
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

    fn ctx_with_repo(p: Provider) -> ProviderContext {
        ProviderContext {
            provider: p,
            host: "github.com".into(),
            source: DetectionSource::Flag,
            repo: Some("acme/widgets".into()),
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

    #[test]
    fn github_rest_list_routes_labeled_github_with_slug() {
        // sympoies/nils-cli#1050: `gh issue list --label` resolves through gh's
        // GraphQL `SearchType` query, which gh caches for 24h; a stale
        // `X-Ratelimit-Remaining: 0` there hard-blocks `plan-issue record open`
        // dedup. The REST route avoids that cache — but only for a labeled
        // GitHub lookup with a known slug.
        let mut args = default_args();
        args.labels = vec!["plan".into()];
        assert!(github_rest_list(&ctx_with_repo(Provider::GitHub), &args));
    }

    #[test]
    fn github_rest_list_skips_when_no_labels() {
        // A no-label list never hits the SearchType cache; stay on `gh issue
        // list`.
        assert!(!github_rest_list(
            &ctx_with_repo(Provider::GitHub),
            &default_args()
        ));
    }

    #[test]
    fn github_rest_list_skips_without_repo_slug() {
        // No slug → no REST endpoint to build; fall back to `gh issue list`.
        let mut args = default_args();
        args.labels = vec!["plan".into()];
        assert!(!github_rest_list(&ctx(Provider::GitHub), &args));
    }

    #[test]
    fn github_rest_list_skips_at_me_filter() {
        // REST's `creator`/`assignee` take a literal login and cannot resolve
        // gh's `@me` shortcut, so an `@me` author/assignee stays on `gh issue
        // list` (checked case-insensitively for both fields).
        let mut assignee = default_args();
        assignee.labels = vec!["plan".into()];
        assignee.assignee = Some("@me".into());
        assert!(!github_rest_list(
            &ctx_with_repo(Provider::GitHub),
            &assignee
        ));

        let mut author = default_args();
        author.labels = vec!["plan".into()];
        author.author = Some("@ME".into());
        assert!(!github_rest_list(&ctx_with_repo(Provider::GitHub), &author));
    }

    #[test]
    fn github_rest_list_skips_gitlab() {
        // The SearchType cache bug is gh-specific; GitLab is unaffected.
        let mut args = default_args();
        args.labels = vec!["plan".into()];
        assert!(!github_rest_list(&ctx_with_repo(Provider::GitLab), &args));
    }

    #[test]
    fn build_github_rest_call_targets_issues_endpoint_with_filters_and_page() {
        let mut args = default_args();
        args.labels = vec!["plan".into(), "state::tracking".into()];
        let call = build_github_rest_call(&ctx_with_repo(Provider::GitHub), &args, 2);
        let plan = call.plan_argv();
        assert_eq!(plan[1], "api", "expected a gh REST call: {plan:?}");
        assert!(
            !plan.iter().any(|s| s == "list"),
            "must not use the SearchType-backed `gh issue list`: {plan:?}"
        );
        assert!(
            plan.iter().any(|s| s == "repos/acme/widgets/issues"),
            "REST call must target the repo issues endpoint: {plan:?}"
        );
        // `-X GET` keeps `-f` params in the query string instead of a POST body.
        let m_idx = plan.iter().position(|s| s == "-X").expect("method flag");
        assert_eq!(plan[m_idx + 1], "GET");
        assert!(
            plan.iter().any(|s| s == "labels=plan,state::tracking"),
            "labels must stay a server-side comma filter: {plan:?}"
        );
        assert!(plan.iter().any(|s| s == "state=open"), "{plan:?}");
        assert!(plan.iter().any(|s| s == "per_page=100"), "{plan:?}");
        assert!(plan.iter().any(|s| s == "page=2"), "{plan:?}");
        // The scan loop drives paging so it can stop early: no `--paginate`.
        assert!(!plan.iter().any(|s| s == "--paginate"), "{plan:?}");
        // A github.com host needs no `--hostname`.
        assert!(!plan.iter().any(|s| s == "--hostname"), "{plan:?}");
    }

    #[test]
    fn build_github_rest_call_passes_enterprise_hostname() {
        // A GHES remote must send the REST request to the detected host, with
        // `--hostname` right after `api` (before the endpoint).
        let ctx = ProviderContext {
            provider: Provider::GitHub,
            host: "ghe.example.com".into(),
            source: DetectionSource::Flag,
            repo: Some("acme/widgets".into()),
        };
        let mut args = default_args();
        args.labels = vec!["plan".into()];
        let call = build_github_rest_call(&ctx, &args, 1);
        let plan = call.plan_argv();
        assert_eq!(plan[1], "api", "{plan:?}");
        assert_eq!(plan[2], "--hostname", "{plan:?}");
        assert_eq!(plan[3], "ghe.example.com", "{plan:?}");
    }

    #[test]
    fn parse_rest_page_excludes_prs_and_maps_rest_fields() {
        // A page is a flat array of REST objects; PRs carry a `pull_request`
        // key and are dropped, but still count toward the raw row total used to
        // detect the final page. REST fields are `html_url`/`user`.
        let stdout = r#"[
          {"number":10,"html_url":"h10","state":"open","title":"tracker",
           "labels":[{"name":"plan"}],"user":{"login":"alice"},
           "assignees":[{"login":"bob"}]},
          {"number":11,"html_url":"h11","state":"open","title":"a pr",
           "labels":[],"user":{"login":"carol"},"assignees":[],
           "pull_request":{"url":"p"}},
          {"number":12,"html_url":"h12","state":"closed","title":"old",
           "labels":[],"user":{"login":"dave"},"assignees":[]}
        ]"#;
        let (rows, items) = parse_rest_page(&BackendSuccess {
            stdout: stdout.into(),
            stderr: String::new(),
        })
        .unwrap();
        assert_eq!(rows, 3, "raw row count must include the PR for paging");
        assert_eq!(
            items.iter().map(|i| i.number).collect::<Vec<_>>(),
            vec![10, 12],
            "PR #11 excluded"
        );
        assert_eq!(items[0].url, "h10");
        assert_eq!(items[0].state, "open");
        assert_eq!(items[0].labels, vec!["plan".to_string()]);
        assert_eq!(items[0].author.as_deref(), Some("alice"));
        assert_eq!(items[0].assignees, vec!["bob".to_string()]);
        assert_eq!(items[1].state, "closed");
    }

    /// A `BackendRunner` that serves scripted pages keyed by the `page=N` field
    /// in the call's argv, recording which pages were requested.
    struct PagedRunner {
        pages: Vec<String>,
        requested: std::cell::RefCell<Vec<u32>>,
    }

    impl PagedRunner {
        fn new(pages: &[String]) -> Self {
            Self {
                pages: pages.to_vec(),
                requested: std::cell::RefCell::new(Vec::new()),
            }
        }

        fn requested_pages(&self) -> Vec<u32> {
            self.requested.borrow().clone()
        }
    }

    impl BackendRunner for PagedRunner {
        fn run(&self, call: &BackendCall) -> Result<BackendSuccess, ForgeError> {
            let page = call
                .argv
                .iter()
                .filter_map(|a| a.to_str())
                .find_map(|s| s.strip_prefix("page="))
                .and_then(|n| n.parse::<u32>().ok())
                .unwrap_or(1);
            self.requested.borrow_mut().push(page);
            let body = self
                .pages
                .get((page - 1) as usize)
                .cloned()
                .unwrap_or_else(|| "[]".to_string());
            Ok(BackendSuccess {
                stdout: body,
                stderr: String::new(),
            })
        }
    }

    fn issue_json(n: u64) -> String {
        format!(
            r#"{{"number":{n},"html_url":"h{n}","state":"open","title":"t","labels":[],"user":{{"login":"a"}},"assignees":[]}}"#
        )
    }

    fn page_of(bodies: Vec<String>) -> String {
        format!("[{}]", bodies.join(","))
    }

    fn rest_args(limit: u32) -> IssueListArgs {
        let mut args = default_args();
        args.labels = vec!["plan".into()];
        args.limit = limit;
        args
    }

    #[test]
    fn run_github_rest_list_stops_at_short_page() {
        // A page shorter than `per_page` is the final page: one request, done.
        let runner = PagedRunner::new(&[page_of(vec![issue_json(1), issue_json(2)])]);
        let payload =
            run_github_rest_list(&runner, &ctx_with_repo(Provider::GitHub), &rest_args(30))
                .unwrap();
        assert_eq!(runner.requested_pages(), vec![1]);
        assert_eq!(
            payload.items.iter().map(|i| i.number).collect::<Vec<_>>(),
            vec![1, 2]
        );
    }

    #[test]
    fn run_github_rest_list_keeps_paging_past_a_pr_dominated_page() {
        // A full page of only PRs yields zero issues but is not the final page,
        // so the scan must continue and fill the limit from later pages — else
        // a labeled list could return fewer issues than `--limit`.
        let all_prs = page_of(vec![
            r#"{"pull_request":{}}"#.to_string();
            REST_PER_PAGE as usize
        ]);
        let page_two = page_of(vec![issue_json(1), issue_json(2), issue_json(3)]);
        let runner = PagedRunner::new(&[all_prs, page_two]);
        let payload =
            run_github_rest_list(&runner, &ctx_with_repo(Provider::GitHub), &rest_args(2)).unwrap();
        assert_eq!(
            runner.requested_pages(),
            vec![1, 2],
            "must not stop after an all-PR full page"
        );
        assert_eq!(
            payload.items.iter().map(|i| i.number).collect::<Vec<_>>(),
            vec![1, 2],
            "collected issues truncated to --limit"
        );
    }

    #[test]
    fn run_github_rest_list_stops_once_limit_reached_without_walking_all_pages() {
        // Once `--limit` issues are collected the scan stops, even though a full
        // first page means more could exist — it must not walk the whole set.
        let full = page_of((1..=REST_PER_PAGE as u64).map(issue_json).collect());
        let unexpected = page_of(vec![issue_json(999)]);
        let runner = PagedRunner::new(&[full, unexpected]);
        let payload =
            run_github_rest_list(&runner, &ctx_with_repo(Provider::GitHub), &rest_args(50))
                .unwrap();
        assert_eq!(
            runner.requested_pages(),
            vec![1],
            "must stop after the limit is reached without fetching page 2"
        );
        assert_eq!(payload.items.len(), 50);
    }
}
