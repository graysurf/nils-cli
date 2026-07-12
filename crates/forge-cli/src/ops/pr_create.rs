//! `pr create` atom.
//!
//! Spec: `crates/forge-cli/docs/specs/forge-cli-spec-v1.md` §"pr create" +
//! `crates/forge-cli/docs/specs/forge-cli-ops-v1.yaml::operations.pr.create`.
//! Schema literal: `cli.forge-cli.pr.create.v1`.
//!
//! The atom carries the heaviest validation surface in v1. The orchestration
//! is:
//!
//! 1. Parse `--kind` and resolve `--head` / `--body` source.
//! 2. Detect provider context (`--provider` override or remote URL).
//! 3. Run the shared validation chain from [`crate::validations`].
//! 4. Materialize the body into a temp file so backend argv can use a
//!    canonical `--body-file` form (gh) or the verbatim string (glab).
//! 5. Build the create call, render `--dry-run` plan, or invoke the runner.
//! 6. Parse the create call's stdout for the new PR/MR URL → number/iid.
//! 7. Re-fetch the resulting PR/MR via the backend's view command and emit
//!    the normalized envelope payload.

use std::ffi::OsString;
use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};

use nils_common::cli_contract::{OutputFormat, schema_version_for};
use serde::Serialize;
use tempfile::NamedTempFile;

use crate::backend::{BackendCall, BackendProgram, BackendRunner, BackendSuccess, DryRunPayload};
use crate::cli::{BINARY, GlobalFlags, PrCreateArgs};
use crate::config::ForgeConfig;
use crate::envelope::emit_success;
use crate::error::ForgeError;
use crate::ops::label::{LabelTarget, validate_label_inputs};
use crate::ops::repo_view;
use crate::provider::{Provider, ProviderContext, detect, git_remote_url};
use crate::rate_limit::default_runner;
use crate::validations::{
    BodyHeadings, PrKind, body_sections, branch_kind_matches, branch_name, branch_pushed,
    git_branch_state, git_status_porcelain, no_local_path, title_length, worktree_clean,
};

const SCHEMA: &str = "pr.create";
const SCHEMA_VERSION: u32 = 1;

type RemoteUrlFn<'a> = Box<dyn Fn(&str) -> Option<String> + 'a>;
type CurrentBranchFn<'a> = Box<dyn Fn() -> Result<String, ForgeError> + 'a>;
type DefaultBranchFn<'a> =
    Box<dyn Fn(&dyn BackendRunner, &ProviderContext) -> Result<String, ForgeError> + 'a>;
type GitStatusFn<'a> = Box<dyn Fn(&Path) -> Result<String, ForgeError> + 'a>;
type HeadStateFn<'a> =
    Box<dyn Fn(&Path, &str) -> Result<crate::validations::HeadState, ForgeError> + 'a>;

/// Envelope payload for `cli.forge-cli.pr.create.v1`.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PrCreatePayload {
    pub provider: &'static str,
    pub number: u64,
    pub url: String,
    pub head: String,
    pub base: String,
    pub draft: bool,
    pub title: String,
    pub kind: &'static str,
}

/// Production entrypoint. Pulls every external dependency from the real
/// process / filesystem / git environment.
pub fn run(
    global: &GlobalFlags,
    args: PrCreateArgs,
    format: OutputFormat,
) -> Result<i32, ForgeError> {
    let runner = default_runner();
    let env = Environment::production();
    run_with(&runner, global, args, format, &env)
}

/// Resolver bundle injected by tests. Each closure stubs one external
/// dependency: provider detection, git porcelain, head state, default base
/// branch, and current branch name.
pub struct Environment<'a> {
    pub remote_url: RemoteUrlFn<'a>,
    pub current_branch: CurrentBranchFn<'a>,
    pub default_branch: DefaultBranchFn<'a>,
    pub git_status: GitStatusFn<'a>,
    pub head_state: HeadStateFn<'a>,
    pub workdir: PathBuf,
    pub headings: BodyHeadings,
    /// Resolved `[test_first].require`: when true, feature/bug PRs must carry
    /// verified test-first evidence. Injected by tests; loaded from layered
    /// config in [`Environment::production`].
    pub test_first_required: bool,
}

impl<'a> Environment<'a> {
    /// Real implementation that talks to git + the backend.
    pub fn production() -> Self {
        let workdir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let cfg = ForgeConfig::load_layered(&workdir, find_git_toplevel(&workdir).as_deref());
        Self {
            remote_url: Box::new(git_remote_url),
            current_branch: Box::new(|| {
                let out = std::process::Command::new("git")
                    .args(["rev-parse", "--abbrev-ref", "HEAD"])
                    .output()
                    .map_err(|e| {
                        ForgeError::software(
                            schema_err(),
                            "git rev-parse failed to spawn",
                            Some(e.to_string()),
                        )
                    })?;
                if !out.status.success() {
                    return Err(ForgeError::software(
                        schema_err(),
                        "git rev-parse --abbrev-ref HEAD exited non-zero",
                        Some(String::from_utf8_lossy(&out.stderr).into_owned()),
                    ));
                }
                Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
            }),
            default_branch: Box::new(|runner, ctx| {
                let call = repo_view::build_call_for_default_branch(ctx);
                let output = runner.run(&call)?;
                let payload = repo_view::parse_backend_output(ctx, &output)?;
                Ok(payload.default_branch)
            }),
            git_status: Box::new(git_status_porcelain),
            head_state: Box::new(git_branch_state),
            workdir,
            headings: BodyHeadings::default(),
            test_first_required: cfg.resolve_test_first_required(None),
        }
    }
}

fn schema_err() -> String {
    schema_version_for(BINARY, "error", 1)
}

fn schema_ok() -> String {
    schema_version_for(BINARY, SCHEMA, SCHEMA_VERSION)
}

/// Resolve the git toplevel for layered-config discovery. Returns `None`
/// outside a work tree (the layered loader then walks to the filesystem root).
pub(crate) fn find_git_toplevel(start: &Path) -> Option<PathBuf> {
    let output = std::process::Command::new("git")
        .current_dir(start)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!path.is_empty()).then(|| PathBuf::from(path))
}

/// Test-first gate. When `required` is true (resolved from
/// `[test_first].require`) and the PR is a feature/bug change, the
/// `--test-first-evidence` directory must hold a verified `test-first-evidence`
/// record — a failing test or explicit waiver plus a passing final validation.
/// Other kinds (docs, chore, ci, refactor) are exempt.
pub(crate) fn test_first_gate(
    kind: PrKind,
    required: bool,
    evidence_dir: Option<&str>,
) -> Result<(), ForgeError> {
    if !required || !matches!(kind, PrKind::Feature | PrKind::Bug) {
        return Ok(());
    }
    let Some(dir) = evidence_dir else {
        return Err(ForgeError::validation(
            schema_err(),
            "test_first_evidence_required",
            "this repo requires test-first evidence for feature/bug PRs; pass \
             --test-first-evidence <dir> pointing at a verified test-first-evidence \
             record (a failing test or explicit waiver plus a passing final validation)",
            None,
        ));
    };
    match agent_workflow_primitives::test_first_evidence::verify_dir(Path::new(dir)) {
        Ok(result) if result.complete => Ok(()),
        Ok(result) if result.record.schema_version == "test-first-evidence.record.v1" => {
            Err(ForgeError::validation(
                schema_err(),
                "test_first_evidence_legacy",
                format!(
                    "test-first evidence at '{dir}' uses record v1; re-record the change with test-first-evidence v2"
                ),
                Some(format!("record_file={}", result.record_file)),
            ))
        }
        Ok(result) => Err(ForgeError::validation(
            schema_err(),
            "test_first_evidence_incomplete",
            format!(
                "test-first evidence at '{dir}' is incomplete: missing {missing}",
                missing = result.missing.join(", ")
            ),
            Some(format!("record_file={}", result.record_file)),
        )),
        Err(message) => Err(ForgeError::validation(
            schema_err(),
            "test_first_evidence_unreadable",
            format!("could not read test-first evidence at '{dir}'"),
            Some(message),
        )),
    }
}

/// Test-friendly entrypoint: caller injects the runner + environment.
pub fn run_with<R: BackendRunner>(
    runner: &R,
    global: &GlobalFlags,
    args: PrCreateArgs,
    format: OutputFormat,
    env: &Environment<'_>,
) -> Result<i32, ForgeError> {
    let ctx = detect(
        global.provider_hint(),
        &global.remote,
        global.repo.as_deref(),
        |r| (env.remote_url)(r),
    )?;

    // 1. Resolve head + base.
    let head = match args.head.clone() {
        Some(h) => h,
        None => (env.current_branch)()?,
    };
    let base = match args.base.clone() {
        Some(b) => b,
        None => (env.default_branch)(runner as &dyn BackendRunner, &ctx)?,
    };

    // 2. Resolve body content. `--body` and `--body-file` cannot both be
    //    set — clap rejects that at parse time with USAGE 64. If neither is
    //    provided, the body is empty and the body_summary check will fail
    //    with DATA 65 (body_missing_summary), which matches the spec.
    let body = read_body(args.body.as_deref(), args.body_file.as_deref())?;

    // 3. Validation chain — order matches spec §"pr create" so the most
    //    obvious failure (bad branch name) is reported first.
    let prefix = branch_name(&head)?;
    let kind: PrKind = args.kind.into_kind();
    branch_kind_matches(prefix, kind)?;
    title_length(&args.title)?;
    no_local_path(&args.title, "title")?;
    body_sections(&body, &env.headings)?;
    no_local_path(&body, "body")?;
    let label_target = match ctx.provider {
        Provider::GitHub | Provider::Local => LabelTarget::Pr,
        Provider::GitLab => LabelTarget::Mr,
    };
    validate_label_inputs(
        &args.labels,
        args.label_catalog.as_deref(),
        args.strict_labels,
        label_target,
    )?;
    worktree_clean(&env.workdir, |w| (env.git_status)(w))?;
    branch_pushed(&env.workdir, &head, |w, branch| (env.head_state)(w, branch))?;
    test_first_gate(
        kind,
        env.test_first_required,
        args.test_first_evidence.as_deref(),
    )?;

    let draft = !args.no_draft;

    // 4. Materialize body into a temp file for argv handoff. The temp file
    //    lives until the end of run_with(); the backend reads it before we
    //    drop it.
    let body_tempfile = write_body_tempfile(&body)?;
    let body_path = body_tempfile.path().to_path_buf();

    let create_call = build_create_call(
        &ctx,
        &head,
        &base,
        &args.title,
        &body,
        &body_path,
        draft,
        &args.reviewers,
        &args.labels,
    );

    if global.dry_run {
        let payload = DryRunPayload::new(ctx.provider, &create_call);
        return Ok(emit_success(schema_ok(), payload, format, |p| {
            println!("would run: {plan}", plan = p.plan.join(" "));
        }));
    }

    let payload = create_and_fetch(runner, &ctx, &create_call, kind)?;
    Ok(emit_success(schema_ok(), payload, format, render_text))
}

/// Macro-facing entry point: run the full validation chain + backend create +
/// view re-fetch and return the typed payload. The `env` argument supplies
/// the same hooks that `run_with` uses so the macro can inject test stubs.
pub fn compute<R: BackendRunner>(
    runner: &R,
    global: &GlobalFlags,
    args: &PrCreateArgs,
    env: &Environment<'_>,
) -> Result<PrCreatePayload, ForgeError> {
    let ctx = detect(
        global.provider_hint(),
        &global.remote,
        global.repo.as_deref(),
        |r| (env.remote_url)(r),
    )?;
    let head = match args.head.clone() {
        Some(h) => h,
        None => (env.current_branch)()?,
    };
    let base = match args.base.clone() {
        Some(b) => b,
        None => (env.default_branch)(runner as &dyn BackendRunner, &ctx)?,
    };
    let body = read_body(args.body.as_deref(), args.body_file.as_deref())?;
    let prefix = branch_name(&head)?;
    let kind: PrKind = args.kind.into_kind();
    branch_kind_matches(prefix, kind)?;
    title_length(&args.title)?;
    no_local_path(&args.title, "title")?;
    body_sections(&body, &env.headings)?;
    no_local_path(&body, "body")?;
    let label_target = match ctx.provider {
        Provider::GitHub | Provider::Local => LabelTarget::Pr,
        Provider::GitLab => LabelTarget::Mr,
    };
    validate_label_inputs(
        &args.labels,
        args.label_catalog.as_deref(),
        args.strict_labels,
        label_target,
    )?;
    worktree_clean(&env.workdir, |w| (env.git_status)(w))?;
    branch_pushed(&env.workdir, &head, |w, branch| (env.head_state)(w, branch))?;
    test_first_gate(
        kind,
        env.test_first_required,
        args.test_first_evidence.as_deref(),
    )?;

    let draft = !args.no_draft;
    let body_tempfile = write_body_tempfile(&body)?;
    let body_path = body_tempfile.path().to_path_buf();
    let create_call = build_create_call(
        &ctx,
        &head,
        &base,
        &args.title,
        &body,
        &body_path,
        draft,
        &args.reviewers,
        &args.labels,
    );
    create_and_fetch(runner, &ctx, &create_call, kind)
}

fn create_and_fetch<R: BackendRunner>(
    runner: &R,
    ctx: &ProviderContext,
    create_call: &BackendCall,
    kind: PrKind,
) -> Result<PrCreatePayload, ForgeError> {
    let create_output = runner.run(create_call)?;
    let (number, url) = parse_create_output(ctx, &create_output)?;
    let view_call = build_view_call(ctx, number);
    let view_output = runner.run(&view_call)?;
    parse_view_output(ctx, &view_output, number, url, kind)
}

fn render_text(payload: &PrCreatePayload) {
    println!(
        "opened {provider} #{number} ({state}): {url}",
        provider = payload.provider,
        number = payload.number,
        state = if payload.draft { "draft" } else { "ready" },
        url = payload.url,
    );
}

/// Read body content from `--body`, `--body-file`, or `--body-file -`
/// (stdin). Returns an empty string when neither flag is set (the body
/// validators then reject with `body_missing_summary`).
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
                "failed to read PR body from stdin",
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

/// Persist body content into a tempfile so backend argv has a stable
/// `--body-file` path. The returned handle owns the file; dropping it
/// deletes the temp.
fn write_body_tempfile(body: &str) -> Result<NamedTempFile, ForgeError> {
    let tmp = tempfile::Builder::new()
        .prefix("forge-cli-body-")
        .suffix(".md")
        .tempfile()
        .map_err(|e| {
            ForgeError::software(
                schema_err(),
                "failed to create PR body tempfile",
                Some(e.to_string()),
            )
        })?;
    fs::write(tmp.path(), body).map_err(|e| {
        ForgeError::software(
            schema_err(),
            "failed to write PR body tempfile",
            Some(e.to_string()),
        )
    })?;
    Ok(tmp)
}

#[allow(clippy::too_many_arguments)]
fn build_create_call(
    ctx: &ProviderContext,
    head: &str,
    base: &str,
    title: &str,
    body: &str,
    body_path: &Path,
    draft: bool,
    reviewers: &[String],
    labels: &[String],
) -> BackendCall {
    let program = BackendProgram::for_provider(ctx.provider);
    let mut argv: Vec<OsString> = Vec::new();
    match ctx.provider {
        Provider::GitHub | Provider::Local => {
            argv.push(OsString::from("pr"));
            argv.push(OsString::from("create"));
            if draft {
                argv.push(OsString::from("--draft"));
            }
            argv.push(OsString::from("--head"));
            argv.push(OsString::from(head));
            argv.push(OsString::from("--base"));
            argv.push(OsString::from(base));
            argv.push(OsString::from("--title"));
            argv.push(OsString::from(title));
            argv.push(OsString::from("--body-file"));
            argv.push(OsString::from(body_path));
            for r in reviewers {
                argv.push(OsString::from("--reviewer"));
                argv.push(OsString::from(r));
            }
            for l in labels {
                argv.push(OsString::from("--label"));
                argv.push(OsString::from(l));
            }
        }
        Provider::GitLab => {
            argv.push(OsString::from("mr"));
            argv.push(OsString::from("create"));
            if draft {
                argv.push(OsString::from("--draft"));
            }
            argv.push(OsString::from("--source-branch"));
            argv.push(OsString::from(head));
            argv.push(OsString::from("--target-branch"));
            argv.push(OsString::from(base));
            argv.push(OsString::from("--title"));
            argv.push(OsString::from(title));
            argv.push(OsString::from("--description"));
            argv.push(OsString::from(body));
            if !reviewers.is_empty() {
                argv.push(OsString::from("--reviewer"));
                argv.push(OsString::from(reviewers.join(",")));
            }
            if !labels.is_empty() {
                argv.push(OsString::from("--label"));
                argv.push(OsString::from(labels.join(",")));
            }
        }
    }
    ctx.push_repo_override(&mut argv);
    BackendCall::new(program, argv)
}

fn build_view_call(ctx: &ProviderContext, number: u64) -> BackendCall {
    let program = BackendProgram::for_provider(ctx.provider);
    let mut argv: Vec<OsString> = match ctx.provider {
        Provider::GitHub | Provider::Local => vec![
            OsString::from("pr"),
            OsString::from("view"),
            OsString::from(number.to_string()),
            OsString::from("--json"),
            OsString::from("number,url,headRefName,baseRefName,isDraft,title"),
        ],
        Provider::GitLab => vec![
            OsString::from("mr"),
            OsString::from("view"),
            OsString::from(number.to_string()),
            OsString::from("-F"),
            OsString::from("json"),
        ],
    };
    ctx.push_repo_override(&mut argv);
    BackendCall::new(program, argv)
}

/// Extract `(number, url)` from `gh pr create` / `glab mr create` stdout.
/// Both backends print the URL on the last non-blank line. The PR/MR id is
/// the last path segment.
pub fn parse_create_output(
    ctx: &ProviderContext,
    output: &BackendSuccess,
) -> Result<(u64, String), ForgeError> {
    let combined = format!("{}\n{}", output.stdout, output.stderr);
    let url = combined
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .find_map(extract_pr_url)
        .ok_or_else(|| {
            ForgeError::software(
                schema_err(),
                "backend did not emit a recognizable PR/MR URL",
                Some(format!("stdout={:?}", output.stdout)),
            )
        })?;
    let number = parse_url_number(&url, ctx.provider).ok_or_else(|| {
        ForgeError::software(
            schema_err(),
            "failed to parse PR/MR number from backend URL",
            Some(format!("url={url}")),
        )
    })?;
    Ok((number, url))
}

fn extract_pr_url(line: &str) -> Option<String> {
    line.split_whitespace()
        .find(|tok| {
            (tok.starts_with("http://") || tok.starts_with("https://"))
                && (tok.contains("/pull/") || tok.contains("/merge_requests/"))
        })
        .map(|s| s.trim_matches(|c: char| c == '.' || c == ',').to_string())
}

fn parse_url_number(url: &str, provider: Provider) -> Option<u64> {
    let needle = match provider {
        Provider::GitHub | Provider::Local => "/pull/",
        Provider::GitLab => "/merge_requests/",
    };
    let idx = url.find(needle)? + needle.len();
    let rest = &url[idx..];
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

/// Parse `gh pr view --json …` / `glab mr view -F json` into the canonical
/// envelope payload.
pub fn parse_view_output(
    ctx: &ProviderContext,
    output: &BackendSuccess,
    number: u64,
    url_fallback: String,
    kind: PrKind,
) -> Result<PrCreatePayload, ForgeError> {
    let value: serde_json::Value = serde_json::from_str(output.stdout.trim()).map_err(|e| {
        ForgeError::software(
            schema_err(),
            format!(
                "{program} {sub} view JSON is invalid",
                program = match ctx.provider {
                    Provider::GitHub | Provider::Local => "gh pr",
                    Provider::GitLab => "glab mr",
                },
                sub = "post-create"
            ),
            Some(e.to_string()),
        )
    })?;

    match ctx.provider {
        Provider::GitHub | Provider::Local => Ok(PrCreatePayload {
            provider: ctx.provider.as_str(),
            number,
            url: value
                .get("url")
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .unwrap_or(url_fallback),
            head: required_str(&value, "headRefName")?,
            base: required_str(&value, "baseRefName")?,
            draft: value
                .get("isDraft")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            title: required_str(&value, "title")?,
            kind: kind.as_str(),
        }),
        Provider::GitLab => Ok(PrCreatePayload {
            provider: ctx.provider.as_str(),
            number,
            url: value
                .get("web_url")
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .unwrap_or(url_fallback),
            head: required_str(&value, "source_branch")?,
            base: required_str(&value, "target_branch")?,
            draft: gitlab_is_draft(&value),
            title: required_str(&value, "title")?,
            kind: kind.as_str(),
        }),
    }
}

fn required_str(value: &serde_json::Value, key: &str) -> Result<String, ForgeError> {
    value
        .get(key)
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| {
            ForgeError::software(
                schema_err(),
                format!("missing required field '{key}' in view JSON"),
                None,
            )
        })
}

fn gitlab_is_draft(value: &serde_json::Value) -> bool {
    // `glab mr view -F json` exposes either `draft: bool` (current glab)
    // or `work_in_progress: bool` (older glab). We accept both.
    if let Some(b) = value.get("draft").and_then(|v| v.as_bool()) {
        return b;
    }
    if let Some(b) = value.get("work_in_progress").and_then(|v| v.as_bool()) {
        return b;
    }
    if let Some(title) = value.get("title").and_then(|v| v.as_str()) {
        let lower = title.to_ascii_lowercase();
        return lower.starts_with("draft:") || lower.starts_with("wip:");
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::PrKindFlag;
    use crate::provider::{DetectionSource, Provider};
    use pretty_assertions::assert_eq;

    fn github_ctx() -> ProviderContext {
        ProviderContext {
            provider: Provider::GitHub,
            host: "github.com".into(),
            source: DetectionSource::Flag,
            repo: None,
        }
    }

    fn gitlab_ctx() -> ProviderContext {
        ProviderContext {
            provider: Provider::GitLab,
            host: "gitlab.com".into(),
            source: DetectionSource::Flag,
            repo: None,
        }
    }

    #[test]
    fn build_create_call_github_default_argv() {
        let ctx = github_ctx();
        let call = build_create_call(
            &ctx,
            "feat/x",
            "main",
            "demo",
            "body",
            Path::new("/tmp/body.md"),
            true,
            &[],
            &[],
        );
        let plan = call.plan_argv();
        assert_eq!(plan[1], "pr");
        assert_eq!(plan[2], "create");
        assert!(plan.contains(&"--draft".to_string()));
        let head_idx = plan.iter().position(|s| s == "--head").unwrap();
        assert_eq!(plan[head_idx + 1], "feat/x");
        let body_idx = plan.iter().position(|s| s == "--body-file").unwrap();
        assert_eq!(plan[body_idx + 1], "/tmp/body.md");
    }

    #[test]
    fn build_create_call_skips_draft_when_off() {
        let ctx = github_ctx();
        let call = build_create_call(
            &ctx,
            "feat/x",
            "main",
            "demo",
            "body",
            Path::new("/tmp/body.md"),
            false,
            &[],
            &[],
        );
        let plan = call.plan_argv();
        assert!(!plan.contains(&"--draft".to_string()));
    }

    #[test]
    fn build_create_call_gitlab_passes_description_inline() {
        let ctx = gitlab_ctx();
        let call = build_create_call(
            &ctx,
            "feat/x",
            "main",
            "demo",
            "body-content",
            Path::new("/tmp/body.md"),
            true,
            &["alice".into(), "bob".into()],
            &["needs-review".into()],
        );
        let plan = call.plan_argv();
        assert_eq!(plan[1], "mr");
        assert_eq!(plan[2], "create");
        let desc_idx = plan.iter().position(|s| s == "--description").unwrap();
        assert_eq!(plan[desc_idx + 1], "body-content");
        let reviewer_idx = plan.iter().position(|s| s == "--reviewer").unwrap();
        assert_eq!(plan[reviewer_idx + 1], "alice,bob");
    }

    #[test]
    fn parse_url_number_handles_github_and_gitlab() {
        assert_eq!(
            parse_url_number("https://github.com/o/r/pull/42", Provider::GitHub),
            Some(42)
        );
        assert_eq!(
            parse_url_number(
                "https://gitlab.com/o/r/-/merge_requests/77",
                Provider::GitLab
            ),
            Some(77)
        );
        assert_eq!(parse_url_number("not a url", Provider::GitHub), None);
    }

    #[test]
    fn parse_create_output_extracts_url_from_stdout() {
        let ctx = github_ctx();
        let output = BackendSuccess {
            stdout: "Creating draft pull request for feat/x into main in o/r\n\nhttps://github.com/o/r/pull/5\n".into(),
            stderr: String::new(),
        };
        let (n, url) = parse_create_output(&ctx, &output).unwrap();
        assert_eq!(n, 5);
        assert_eq!(url, "https://github.com/o/r/pull/5");
    }

    #[test]
    fn parse_view_output_github_shape() {
        let ctx = github_ctx();
        let output = BackendSuccess {
            stdout: r#"{"number":5,"url":"https://github.com/o/r/pull/5","headRefName":"feat/x","baseRefName":"main","isDraft":true,"title":"demo"}"#.into(),
            stderr: String::new(),
        };
        let payload = parse_view_output(
            &ctx,
            &output,
            5,
            "https://github.com/o/r/pull/5".into(),
            PrKind::Feature,
        )
        .unwrap();
        assert_eq!(payload.provider, "github");
        assert_eq!(payload.number, 5);
        assert_eq!(payload.head, "feat/x");
        assert_eq!(payload.base, "main");
        assert!(payload.draft);
        assert_eq!(payload.kind, "feature");
    }

    #[test]
    fn parse_view_output_gitlab_shape_with_draft_field() {
        let ctx = gitlab_ctx();
        let output = BackendSuccess {
            stdout: r#"{"iid":7,"web_url":"https://gitlab.com/o/r/-/merge_requests/7","source_branch":"feat/y","target_branch":"main","draft":true,"title":"demo"}"#.into(),
            stderr: String::new(),
        };
        let payload = parse_view_output(
            &ctx,
            &output,
            7,
            "https://gitlab.com/o/r/-/merge_requests/7".into(),
            PrKind::Feature,
        )
        .unwrap();
        assert!(payload.draft);
        assert_eq!(payload.url, "https://gitlab.com/o/r/-/merge_requests/7");
        assert_eq!(payload.base, "main");
    }

    #[test]
    fn parse_view_output_gitlab_legacy_wip_field() {
        let ctx = gitlab_ctx();
        let output = BackendSuccess {
            stdout: r#"{"iid":7,"web_url":"u","source_branch":"feat/y","target_branch":"main","work_in_progress":true,"title":"Draft: demo"}"#.into(),
            stderr: String::new(),
        };
        let payload = parse_view_output(&ctx, &output, 7, "u".into(), PrKind::Feature).unwrap();
        assert!(payload.draft);
    }

    #[test]
    fn read_body_prefers_inline_over_file() {
        let s = read_body(Some("inline"), Some("/dev/null")).unwrap();
        assert_eq!(s, "inline");
    }

    #[test]
    fn read_body_returns_empty_when_neither_set() {
        assert_eq!(read_body(None, None).unwrap(), "");
    }

    // Helper so tests don't depend on PrKindFlag layout reordering.
    fn args(title: &str, kind: PrKindFlag, body: Option<&str>) -> PrCreateArgs {
        PrCreateArgs {
            head: Some("feat/sample".into()),
            base: Some("main".into()),
            title: title.into(),
            body: body.map(str::to_string),
            body_file: None,
            kind,
            no_draft: false,
            reviewers: Vec::new(),
            labels: Vec::new(),
            label_catalog: None,
            strict_labels: false,
            test_first_evidence: None,
        }
    }

    struct StubRunner;
    impl BackendRunner for StubRunner {
        fn run(&self, _: &BackendCall) -> Result<BackendSuccess, ForgeError> {
            unreachable!("dry-run path must not invoke the runner");
        }
    }

    fn passthrough_env<'a>() -> Environment<'a> {
        Environment {
            remote_url: Box::new(|_| Some("git@github.com:o/r.git".into())),
            current_branch: Box::new(|| Ok("feat/sample".into())),
            default_branch: Box::new(|_, _| Ok("main".into())),
            git_status: Box::new(|_| Ok(String::new())),
            head_state: Box::new(|_, _| {
                Ok(crate::validations::HeadState {
                    head_sha: "abc".into(),
                    upstream_sha: Some("abc".into()),
                })
            }),
            workdir: PathBuf::from("."),
            headings: BodyHeadings::default(),
            test_first_required: false,
        }
    }

    #[test]
    fn run_with_dry_run_returns_plan_envelope() {
        let runner = StubRunner;
        let env = passthrough_env();
        let global = GlobalFlags {
            format: Some(OutputFormat::Json),
            remote: "origin".into(),
            provider: None,
            repo: None,
            store_root: None,
            dry_run: true,
        };
        let body = "## Summary\n\nyes.\n\n## Test plan\n\nverified.\n";
        let code = run_with(
            &runner,
            &global,
            args("demo: ok", PrKindFlag::Feature, Some(body)),
            OutputFormat::Json,
            &env,
        )
        .expect("dry-run");
        assert_eq!(code, nils_common::cli_contract::exit::SUCCESS);
    }

    #[test]
    fn run_with_rejects_dirty_worktree() {
        let runner = StubRunner;
        let env = Environment {
            git_status: Box::new(|_| Ok(" M src/lib.rs\n".into())),
            ..passthrough_env()
        };
        let global = GlobalFlags {
            format: None,
            remote: "origin".into(),
            provider: None,
            repo: None,
            store_root: None,
            dry_run: true,
        };
        let body = "## Summary\n\nyes.\n\n## Test plan\n\nverified.\n";
        let err = run_with(
            &runner,
            &global,
            args("demo: ok", PrKindFlag::Feature, Some(body)),
            OutputFormat::Json,
            &env,
        )
        .expect_err("dirty");
        assert_eq!(err.kind(), "dirty_worktree");
    }

    #[test]
    fn run_with_rejects_branch_kind_mismatch() {
        let runner = StubRunner;
        let env = passthrough_env();
        let global = GlobalFlags {
            format: None,
            remote: "origin".into(),
            provider: None,
            repo: None,
            store_root: None,
            dry_run: true,
        };
        let body = "## Summary\n\nyes.\n\n## Test plan\n\nverified.\n";
        let err = run_with(
            &runner,
            &global,
            args("demo: ok", PrKindFlag::Bug, Some(body)),
            OutputFormat::Json,
            &env,
        )
        .expect_err("mismatch");
        assert_eq!(err.kind(), "branch_kind_mismatch");
    }

    #[test]
    fn run_with_rejects_missing_summary() {
        let runner = StubRunner;
        let env = passthrough_env();
        let global = GlobalFlags {
            format: None,
            remote: "origin".into(),
            provider: None,
            repo: None,
            store_root: None,
            dry_run: true,
        };
        let body = "## Test plan\n\nyes.\n";
        let err = run_with(
            &runner,
            &global,
            args("demo: ok", PrKindFlag::Feature, Some(body)),
            OutputFormat::Json,
            &env,
        )
        .expect_err("no summary");
        assert_eq!(err.kind(), "body_missing_summary");
    }

    fn write_evidence(dir: &Path, json: &str) {
        std::fs::write(dir.join("test-first-evidence.json"), json).unwrap();
    }

    #[test]
    fn test_first_gate_passes_when_not_required() {
        assert!(test_first_gate(PrKind::Feature, false, None).is_ok());
    }

    #[test]
    fn test_first_gate_exempts_non_behavior_kinds() {
        for kind in [PrKind::Docs, PrKind::Chore, PrKind::Ci, PrKind::Refactor] {
            assert!(
                test_first_gate(kind, true, None).is_ok(),
                "{kind:?} should be exempt",
                kind = kind,
            );
        }
    }

    #[test]
    fn test_first_gate_requires_evidence_for_feature_and_bug() {
        for kind in [PrKind::Feature, PrKind::Bug] {
            let err = test_first_gate(kind, true, None).expect_err("required");
            assert_eq!(err.kind(), "test_first_evidence_required");
        }
    }

    #[test]
    fn test_first_gate_accepts_complete_record() {
        let dir = tempfile::tempdir().unwrap();
        write_evidence(
            dir.path(),
            r#"{"schema_version":"test-first-evidence.record.v2","change_classification":"behavior-change","contract_delta":{"changed_behaviors":["durable gate"]},"no_existing_tests_reason":"fixture has no existing tests","waiver":{"reason":"fixture","kind":"non-testable","why_no_red":"fixture path","substitute_validation":["cargo test"]},"final_validations":[{"command":"cargo test","status":"pass","scope":"focused"}],"no_residual_gaps":true}"#,
        );
        assert!(test_first_gate(PrKind::Feature, true, dir.path().to_str()).is_ok());
    }

    #[test]
    fn test_first_gate_rejects_incomplete_record() {
        let dir = tempfile::tempdir().unwrap();
        write_evidence(
            dir.path(),
            r#"{"schema_version":"test-first-evidence.record.v2","change_classification":"behavior-change","contract_delta":{"changed_behaviors":["durable gate"]},"no_existing_tests_reason":"fixture has no existing tests","waiver":{"reason":"fixture","kind":"non-testable","why_no_red":"fixture path","substitute_validation":["cargo test"]},"no_residual_gaps":true}"#,
        );
        let err =
            test_first_gate(PrKind::Feature, true, dir.path().to_str()).expect_err("incomplete");
        assert_eq!(err.kind(), "test_first_evidence_incomplete");
    }

    #[test]
    fn test_first_gate_rejects_legacy_record_with_rerecord_error() {
        let dir = tempfile::tempdir().unwrap();
        write_evidence(
            dir.path(),
            r#"{"schema_version":"test-first-evidence.record.v1","change_classification":"behavior-change","waiver":{"reason":"fixture"},"final_validation":{"command":"cargo test","status":"pass"}}"#,
        );
        let err =
            test_first_gate(PrKind::Feature, true, dir.path().to_str()).expect_err("v1 record");
        assert_eq!(err.kind(), "test_first_evidence_legacy");
        assert!(err.to_string().contains("re-record"));
    }

    #[test]
    fn test_first_gate_rejects_unreadable_record() {
        let dir = tempfile::tempdir().unwrap();
        let err =
            test_first_gate(PrKind::Feature, true, dir.path().to_str()).expect_err("unreadable");
        assert_eq!(err.kind(), "test_first_evidence_unreadable");
    }

    #[test]
    fn run_with_blocks_feature_when_required_without_evidence() {
        let runner = StubRunner;
        let env = Environment {
            test_first_required: true,
            ..passthrough_env()
        };
        let global = GlobalFlags {
            format: None,
            remote: "origin".into(),
            provider: None,
            repo: None,
            store_root: None,
            dry_run: true,
        };
        let body = "## Summary\n\nyes.\n\n## Test plan\n\nverified.\n";
        let err = run_with(
            &runner,
            &global,
            args("demo: ok", PrKindFlag::Feature, Some(body)),
            OutputFormat::Json,
            &env,
        )
        .expect_err("gate");
        assert_eq!(err.kind(), "test_first_evidence_required");
    }
}
