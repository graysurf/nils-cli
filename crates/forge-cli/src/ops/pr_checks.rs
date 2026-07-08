//! `pr checks` atom — one-shot snapshot of PR/MR check state.
//!
//! Spec / ops: `cli.forge-cli.pr.checks.v1`. GitHub uses
//! `gh pr checks <id> --json name,state,bucket,workflow,link,startedAt,
//! completedAt,description` plus a separate `--required` call for required-only
//! gating. GitLab is delegated to
//! [`crate::ops::pr_checks_gitlab`] which parses `glab ci status` text after
//! probing `glab --version`.
//!
//! Per spec, `--required-only=true` (the default) ignores non-required checks
//! for the gating decision but still reports them under `data.checks`. The
//! aggregate `data.state` derives from the gating subset.

use std::collections::HashSet;
use std::ffi::OsString;

use nils_common::cli_contract::{OutputFormat, schema_version_for};
use serde::Serialize;

use crate::backend::{BackendCall, BackendProgram, BackendRunner, BackendSuccess, DryRunPayload};
use crate::cli::{BINARY, GlobalFlags, PrChecksArgs};
use crate::envelope::emit_success;
use crate::error::ForgeError;
use crate::ops::pr_checks_gitlab;
use crate::provider::{Provider, ProviderContext, detect, git_remote_url};
use crate::rate_limit::default_runner;

pub const SCHEMA: &str = "pr.checks";
pub const SCHEMA_VERSION: u32 = 1;

pub const GH_JSON_FIELDS: &str =
    "name,state,bucket,workflow,link,startedAt,completedAt,description";

/// Canonical normalized check state. Spec enum:
/// `success | failure | pending | cancelled | neutral | skipped | timed_out`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckState {
    Success,
    Failure,
    Pending,
    Cancelled,
    Neutral,
    Skipped,
    TimedOut,
}

impl CheckState {
    pub const fn as_str(self) -> &'static str {
        match self {
            CheckState::Success => "success",
            CheckState::Failure => "failure",
            CheckState::Pending => "pending",
            CheckState::Cancelled => "cancelled",
            CheckState::Neutral => "neutral",
            CheckState::Skipped => "skipped",
            CheckState::TimedOut => "timed_out",
        }
    }

    /// True iff this state is a terminal (non-pending) state.
    pub const fn is_terminal(self) -> bool {
        !matches!(self, CheckState::Pending)
    }

    /// True iff this state should fail the gate.
    pub const fn is_failing(self) -> bool {
        matches!(
            self,
            CheckState::Failure | CheckState::Cancelled | CheckState::TimedOut
        )
    }

    /// True iff this state should pass the gate (terminal + non-failing).
    pub const fn is_passing(self) -> bool {
        matches!(
            self,
            CheckState::Success | CheckState::Skipped | CheckState::Neutral
        )
    }
}

/// One normalized check entry. `url`/`workflow`/`description` are best-effort
/// passthroughs from the backend.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CheckItem {
    pub name: String,
    pub state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conclusion: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workflow: Option<String>,
    pub required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
}

/// Failing-check summary (entry of `data.failed`).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct FailedCheck {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conclusion: Option<String>,
}

/// Pending-check summary (entry of `data.pending`).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PendingCheck {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// Envelope payload for `cli.forge-cli.pr.checks.v1` (one-shot snapshot;
/// `pr wait-checks` reuses the same struct with `duration_ms` set).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PrChecksPayload {
    pub provider: &'static str,
    pub state: &'static str,
    pub required_count: u32,
    pub success_count: u32,
    pub failed: Vec<FailedCheck>,
    pub pending: Vec<PendingCheck>,
    pub checks: Vec<CheckItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

pub fn run(
    global: &GlobalFlags,
    args: PrChecksArgs,
    format: OutputFormat,
) -> Result<i32, ForgeError> {
    let runner = default_runner();
    run_with(&runner, global, &args, format, git_remote_url)
}

pub fn run_with<R: BackendRunner, F: Fn(&str) -> Option<String>>(
    runner: &R,
    global: &GlobalFlags,
    args: &PrChecksArgs,
    format: OutputFormat,
    remote_url_lookup: F,
) -> Result<i32, ForgeError> {
    let ctx = detect(
        global.provider_hint(),
        &global.remote,
        global.repo.as_deref(),
        remote_url_lookup,
    )?;
    let payload = snapshot(runner, global, &ctx, args)?;
    Ok(emit_success(
        schema_version_for(BINARY, SCHEMA, SCHEMA_VERSION),
        payload,
        format,
        render_text,
    ))
}

/// Snapshot helper used by both `pr checks` and (via reuse) `pr wait-checks`.
/// Returns either a dry-run plan envelope-flagged path or a populated payload.
/// When `global.dry_run` is set, this emits the dry-run envelope itself and
/// returns `None` (i.e. caller MUST short-circuit). For non-dry-run callers
/// this never returns the dry-run shape.
pub fn snapshot<R: BackendRunner>(
    runner: &R,
    global: &GlobalFlags,
    ctx: &ProviderContext,
    args: &PrChecksArgs,
) -> Result<PrChecksPayload, ForgeError> {
    if global.dry_run {
        // The atom emits its own dry-run envelope upstream; snapshot is only
        // used by handlers that have already filtered out dry-run. Treat this
        // as a software-error invariant if somehow called with dry_run.
        return Err(ForgeError::software(
            schema_err(),
            "snapshot() invoked with global.dry_run; caller should short-circuit",
            None,
        ));
    }
    match ctx.provider {
        Provider::GitHub => snapshot_github(runner, ctx, args),
        Provider::GitLab => pr_checks_gitlab::snapshot(runner, ctx, args),
        Provider::Local => snapshot_local(global, ctx, args),
    }
}

/// Build a checks snapshot for `Provider::Local` from the seeded `PrRecord`
/// rollup. The local store records the aggregate (`required_state`,
/// `required_count`, `non_required_failures`), not individual checks, so we
/// synthesize gating entries that reproduce that rollup and reuse the shared
/// [`aggregate`] so the payload shape matches the real backends.
fn snapshot_local(
    global: &GlobalFlags,
    ctx: &ProviderContext,
    args: &PrChecksArgs,
) -> Result<PrChecksPayload, ForgeError> {
    let root = crate::local::resolve_store_root(global)?;
    let slug = crate::local::resolve_slug(global.repo.as_deref());
    let store = crate::local::store::Store::open(root, &slug)?;
    let id: u64 = args.id.parse().map_err(|_| {
        ForgeError::software(
            schema_err(),
            "local pr checks: id must be numeric",
            Some(format!("id={}", args.id)),
        )
    })?;
    let pr = store.read_pr(id)?;
    let required_count = pr.required_count.unwrap_or(0);
    let required_state = pr
        .required_state
        .as_deref()
        .or(pr.checks.as_deref())
        .unwrap_or("success");
    let mut checks: Vec<CheckItem> = Vec::new();
    for i in 0..required_count {
        // The first required check carries the rollup state; the rest pass so
        // the aggregate reproduces the seeded `required_state`.
        let state = if i == 0 {
            map_local_check_state(required_state)
        } else {
            CheckState::Success
        };
        checks.push(local_check_item(format!("required-{}", i + 1), state, true));
    }
    // Zero declared required checks but a non-success rollup: surface one
    // required entry so the gate reflects the seed rather than reporting green.
    if required_count == 0 && !matches!(map_local_check_state(required_state), CheckState::Success)
    {
        checks.push(local_check_item(
            "required".to_string(),
            map_local_check_state(required_state),
            true,
        ));
    }
    for name in &pr.non_required_failures {
        checks.push(local_check_item(name.clone(), CheckState::Failure, false));
    }
    Ok(aggregate(ctx, checks, args.required_only, None))
}

fn local_check_item(name: String, state: CheckState, required: bool) -> CheckItem {
    CheckItem {
        name,
        state: state.as_str(),
        url: None,
        conclusion: None,
        workflow: None,
        required,
        started_at: None,
        completed_at: None,
    }
}

fn map_local_check_state(raw: &str) -> CheckState {
    match raw.to_ascii_lowercase().as_str() {
        "success" => CheckState::Success,
        "failure" | "error" => CheckState::Failure,
        "pending" => CheckState::Pending,
        "cancelled" | "canceled" => CheckState::Cancelled,
        "skipped" => CheckState::Skipped,
        "neutral" => CheckState::Neutral,
        "timed_out" => CheckState::TimedOut,
        _ => CheckState::Pending,
    }
}

fn snapshot_github<R: BackendRunner>(
    runner: &R,
    ctx: &ProviderContext,
    args: &PrChecksArgs,
) -> Result<PrChecksPayload, ForgeError> {
    let call = build_github_call(ctx, &args.id);
    let output = match run_github_checks_call(runner, &call) {
        Ok(output) => output,
        Err(err) if is_status_rollup_permission_error(&err) => {
            return snapshot_github_rest_fallback(runner, ctx, args);
        }
        Err(err) => return Err(err),
    };
    if args.required_only {
        let required_call = build_github_required_call(ctx, &args.id);
        let required_output = match run_github_checks_call(runner, &required_call) {
            Ok(output) => output,
            Err(err) if is_status_rollup_permission_error(&err) => {
                return snapshot_github_rest_fallback(runner, ctx, args);
            }
            Err(err) => return Err(err),
        };
        return parse_github_snapshot_with_required_output(ctx, &output, &required_output);
    }
    parse_github_snapshot(ctx, &output, false)
}

fn snapshot_github_rest_fallback<R: BackendRunner>(
    runner: &R,
    ctx: &ProviderContext,
    args: &PrChecksArgs,
) -> Result<PrChecksPayload, ForgeError> {
    let repo = github_repo_slug(ctx)?;
    let head_call = build_github_head_view_call(ctx, &args.id);
    let head_output = runner.run(&head_call)?;
    let head_ref_oid = parse_github_head_ref(&head_output)?;

    let check_runs_call = build_github_check_runs_call(ctx, repo, &head_ref_oid);
    let check_runs_output = runner.run(&check_runs_call)?;
    let mut checks = parse_github_rest_check_runs(&check_runs_output, args.required_only)?;

    let statuses_call = build_github_statuses_call(ctx, repo, &head_ref_oid);
    let statuses_output = runner.run(&statuses_call)?;
    checks.extend(parse_github_rest_statuses(
        &statuses_output,
        args.required_only,
    )?);

    Ok(aggregate_github_unknown_requiredness(
        ctx,
        checks,
        args.required_only,
    ))
}

fn aggregate_github_unknown_requiredness(
    ctx: &ProviderContext,
    mut checks: Vec<CheckItem>,
    required_only: bool,
) -> PrChecksPayload {
    if required_only && checks.is_empty() {
        checks.push(CheckItem {
            name: "github-status-rollup-requiredness-unknown".to_string(),
            state: CheckState::Pending.as_str(),
            url: None,
            conclusion: None,
            workflow: None,
            required: true,
            started_at: None,
            completed_at: None,
        });
    }
    let mut payload = aggregate(ctx, checks, required_only, None);
    if required_only {
        payload
            .warnings
            .push("github_status_rollup_requiredness_unknown_all_rows_gated".to_string());
    }
    payload
}

fn github_repo_slug(ctx: &ProviderContext) -> Result<&str, ForgeError> {
    let Some(repo) = ctx.repo.as_deref() else {
        return Err(ForgeError::validation(
            schema_err(),
            "repo_required",
            "github checks REST fallback requires --repo owner/name or a recognised GitHub remote",
            None,
        ));
    };
    let mut parts = repo.split('/');
    let owner = parts.next().filter(|part| !part.is_empty());
    let name = parts.next().filter(|part| !part.is_empty());
    if owner.is_none() || name.is_none() || parts.next().is_some() {
        return Err(ForgeError::validation(
            schema_err(),
            "repo_required",
            "github checks REST fallback requires a repo slug shaped as owner/name",
            Some(format!("repo={repo}")),
        ));
    }
    Ok(repo)
}

/// Build the `gh pr checks <id> --json …` call. Public so the dry-run helper
/// can render the plan.
pub fn build_github_call(ctx: &ProviderContext, id: &str) -> BackendCall {
    let mut argv: Vec<OsString> = vec![
        OsString::from("pr"),
        OsString::from("checks"),
        OsString::from(id),
        OsString::from("--json"),
        OsString::from(GH_JSON_FIELDS),
    ];
    ctx.push_repo_override(&mut argv);
    BackendCall::new(BackendProgram::Gh, argv)
}

/// Build the `gh pr checks <id> --required --json …` call used for GitHub
/// required-check gating because `gh 2.92.0` no longer exposes `isRequired`
/// through the JSON field set.
pub fn build_github_required_call(ctx: &ProviderContext, id: &str) -> BackendCall {
    let mut argv: Vec<OsString> = vec![
        OsString::from("pr"),
        OsString::from("checks"),
        OsString::from(id),
        OsString::from("--required"),
        OsString::from("--json"),
        OsString::from(GH_JSON_FIELDS),
    ];
    ctx.push_repo_override(&mut argv);
    BackendCall::new(BackendProgram::Gh, argv)
}

/// Build the narrow fallback call used when `gh pr checks` hits a
/// permission-sensitive statusCheckRollup traversal internally.
pub fn build_github_head_view_call(ctx: &ProviderContext, id: &str) -> BackendCall {
    let mut argv: Vec<OsString> = vec![
        OsString::from("pr"),
        OsString::from("view"),
        OsString::from(id),
        OsString::from("--json"),
        OsString::from("headRefOid"),
    ];
    ctx.push_repo_override(&mut argv);
    BackendCall::new(BackendProgram::Gh, argv)
}

/// Build the REST check-runs fallback call for a PR head commit.
pub fn build_github_check_runs_call(
    ctx: &ProviderContext,
    repo: &str,
    head_ref_oid: &str,
) -> BackendCall {
    let endpoint = format!("repos/{repo}/commits/{head_ref_oid}/check-runs?per_page=100");
    let mut argv = vec![OsString::from("api"), OsString::from(endpoint)];
    ctx.push_github_api_hostname(&mut argv);
    BackendCall::new(BackendProgram::Gh, argv)
}

/// Build the REST combined-status fallback call for commit status contexts.
pub fn build_github_statuses_call(
    ctx: &ProviderContext,
    repo: &str,
    head_ref_oid: &str,
) -> BackendCall {
    let endpoint = format!("repos/{repo}/commits/{head_ref_oid}/status?per_page=100");
    let mut argv = vec![OsString::from("api"), OsString::from(endpoint)];
    ctx.push_github_api_hostname(&mut argv);
    BackendCall::new(BackendProgram::Gh, argv)
}

/// Build the dry-run preview call for the current provider — public so
/// downstream atoms (e.g. `pr wait-checks` dry-run) can reuse it.
pub fn build_dry_run_call(ctx: &ProviderContext, args: &PrChecksArgs) -> BackendCall {
    match ctx.provider {
        Provider::GitHub | Provider::Local if args.required_only => {
            build_github_required_call(ctx, &args.id)
        }
        Provider::GitHub | Provider::Local => build_github_call(ctx, &args.id),
        Provider::GitLab => pr_checks_gitlab::build_status_call(ctx, &args.id),
    }
}

fn run_github_checks_call<R: BackendRunner>(
    runner: &R,
    call: &BackendCall,
) -> Result<BackendSuccess, ForgeError> {
    let output = runner.run_raw(call)?;
    if output.status_success || !output.stdout.trim().is_empty() {
        return Ok(BackendSuccess {
            stdout: output.stdout,
            stderr: output.stderr,
        });
    }
    if is_no_checks_reported(Some(output.stderr.as_str())) {
        return Ok(BackendSuccess {
            stdout: "[]".into(),
            stderr: output.stderr,
        });
    }
    let exe = call.program.executable();
    Err(ForgeError::backend_error(
        schema_err(),
        format!(
            "{exe} exited with status {exit_code}",
            exe = exe.to_string_lossy(),
            exit_code = output.exit_code
        ),
        Some(output.stderr),
    ))
}

fn is_no_checks_reported(detail: Option<&str>) -> bool {
    detail
        .map(|d| {
            let lower = d.to_ascii_lowercase();
            lower.contains("no required checks reported") || lower.contains("no checks reported")
        })
        .unwrap_or(false)
}

fn is_status_rollup_permission_error(err: &ForgeError) -> bool {
    let detail = match err {
        ForgeError::BackendError {
            detail: Some(detail),
            ..
        } => detail,
        _ => return false,
    };
    let lower = detail.to_ascii_lowercase();
    let has_rollup = lower.contains("statuscheckrollup") || lower.contains("status check rollup");
    has_rollup
        && (lower.contains("resource not accessible")
            || lower.contains("not accessible")
            || lower.contains("permission"))
}

fn parse_github_snapshot(
    ctx: &ProviderContext,
    output: &BackendSuccess,
    required_only: bool,
) -> Result<PrChecksPayload, ForgeError> {
    let checks = parse_github_checks(output, None, false)?;
    Ok(aggregate(ctx, checks, required_only, None))
}

fn parse_github_snapshot_with_required_output(
    ctx: &ProviderContext,
    all_output: &BackendSuccess,
    required_output: &BackendSuccess,
) -> Result<PrChecksPayload, ForgeError> {
    let required_entries = parse_github_checks(required_output, None, true)?;
    let required_names = required_entries
        .iter()
        .map(|c| c.name.clone())
        .collect::<HashSet<_>>();
    let mut checks = parse_github_checks(all_output, Some(&required_names), false)?;

    if checks.is_empty() {
        checks = required_entries;
    } else if !required_names.is_empty() {
        let present_required = checks
            .iter()
            .filter(|c| c.required)
            .map(|c| c.name.clone())
            .collect::<HashSet<_>>();
        for required in required_entries {
            if !present_required.contains(required.name.as_str()) {
                checks.push(required);
            }
        }
    }

    Ok(aggregate(ctx, checks, true, None))
}

fn parse_github_checks(
    output: &BackendSuccess,
    required_names: Option<&HashSet<String>>,
    force_required: bool,
) -> Result<Vec<CheckItem>, ForgeError> {
    let trimmed = output.stdout.trim();
    // Empty stdout: GitHub historically prints nothing when there are no
    // checks; tolerate the empty case as "no checks".
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    let value: serde_json::Value = serde_json::from_str(trimmed).map_err(|e| {
        ForgeError::software(
            schema_err(),
            "pr checks JSON is invalid",
            Some(e.to_string()),
        )
    })?;
    let arr = value.as_array().ok_or_else(|| {
        ForgeError::software(
            schema_err(),
            "pr checks JSON root is not an array",
            Some(format!("got {value:?}")),
        )
    })?;
    let mut checks = Vec::with_capacity(arr.len());
    for entry in arr {
        checks.push(parse_github_entry(entry, required_names, force_required)?);
    }
    Ok(checks)
}

fn parse_github_head_ref(output: &BackendSuccess) -> Result<String, ForgeError> {
    let trimmed = output.stdout.trim();
    if trimmed.is_empty() {
        return Err(ForgeError::software(
            schema_err(),
            "pr view headRefOid JSON is empty",
            None,
        ));
    }
    let value: serde_json::Value = serde_json::from_str(trimmed).map_err(|e| {
        ForgeError::software(
            schema_err(),
            "pr view headRefOid JSON is invalid",
            Some(e.to_string()),
        )
    })?;
    value
        .get("headRefOid")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            ForgeError::software(
                schema_err(),
                "missing required field 'headRefOid' in pr view JSON",
                None,
            )
        })
}

fn parse_github_rest_check_runs(
    output: &BackendSuccess,
    force_required: bool,
) -> Result<Vec<CheckItem>, ForgeError> {
    let trimmed = output.stdout.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    let value: serde_json::Value = serde_json::from_str(trimmed).map_err(|e| {
        ForgeError::software(
            schema_err(),
            "GitHub check-runs JSON is invalid",
            Some(e.to_string()),
        )
    })?;
    let runs = value
        .get("check_runs")
        .and_then(|v| v.as_array())
        .ok_or_else(|| {
            ForgeError::software(
                schema_err(),
                "missing required field 'check_runs' in GitHub check-runs JSON",
                None,
            )
        })?;
    let truncated = json_total_count_exceeds_len(&value, runs.len());
    let mut checks = Vec::with_capacity(runs.len() + usize::from(truncated));
    for run in runs {
        checks.push(parse_github_rest_check_run(run, force_required)?);
    }
    if truncated {
        checks.push(CheckItem {
            name: "github-check-runs-pagination-truncated".to_string(),
            state: CheckState::Pending.as_str(),
            url: None,
            conclusion: None,
            workflow: None,
            required: force_required,
            started_at: None,
            completed_at: None,
        });
    }
    Ok(checks)
}

fn parse_github_rest_check_run(
    value: &serde_json::Value,
    force_required: bool,
) -> Result<CheckItem, ForgeError> {
    let name = json_string(value, &["name"]).ok_or_else(|| missing("name"))?;
    let conclusion = json_string(value, &["conclusion"]);
    let raw_state = json_string(value, &["status"]);
    let state = normalize_github_rollup_state(
        Some("CheckRun"),
        conclusion.as_deref(),
        raw_state.as_deref(),
    );
    let url = json_string(value, &["details_url", "detailsUrl", "html_url", "htmlUrl"]);
    let workflow = nested_json_string(value, &["check_suite", "workflow_run", "name"])
        .or_else(|| json_string(value, &["workflow_name", "workflowName", "workflow"]))
        .or_else(|| nested_json_string(value, &["app", "name"]));
    let started_at = json_string(value, &["started_at", "startedAt"]);
    let completed_at = json_string(value, &["completed_at", "completedAt"]);
    Ok(CheckItem {
        name,
        state: state.as_str(),
        url,
        conclusion,
        workflow,
        required: force_required,
        started_at,
        completed_at,
    })
}

fn parse_github_rest_statuses(
    output: &BackendSuccess,
    force_required: bool,
) -> Result<Vec<CheckItem>, ForgeError> {
    let trimmed = output.stdout.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    let value: serde_json::Value = serde_json::from_str(trimmed).map_err(|e| {
        ForgeError::software(
            schema_err(),
            "GitHub combined status JSON is invalid",
            Some(e.to_string()),
        )
    })?;
    let statuses = value
        .get("statuses")
        .and_then(|v| v.as_array())
        .ok_or_else(|| {
            ForgeError::software(
                schema_err(),
                "missing required field 'statuses' in GitHub combined status JSON",
                None,
            )
        })?;
    let truncated = json_total_count_exceeds_len(&value, statuses.len());
    let mut checks = Vec::with_capacity(statuses.len() + usize::from(truncated));
    for status in statuses {
        checks.push(parse_github_rest_status(status, force_required)?);
    }
    if truncated {
        checks.push(CheckItem {
            name: "github-statuses-pagination-truncated".to_string(),
            state: CheckState::Pending.as_str(),
            url: None,
            conclusion: None,
            workflow: None,
            required: force_required,
            started_at: None,
            completed_at: None,
        });
    }
    Ok(checks)
}

fn json_total_count_exceeds_len(value: &serde_json::Value, len: usize) -> bool {
    value
        .get("total_count")
        .and_then(|v| v.as_u64())
        .and_then(|count| usize::try_from(count).ok())
        .is_some_and(|count| count > len)
}

fn parse_github_rest_status(
    value: &serde_json::Value,
    force_required: bool,
) -> Result<CheckItem, ForgeError> {
    let name = json_string(value, &["context"]).ok_or_else(|| missing("context"))?;
    let raw_state = json_string(value, &["state"]);
    let state = normalize_github_state(None, None, raw_state.as_deref());
    let url = json_string(value, &["target_url", "targetUrl"]);
    let started_at = json_string(value, &["created_at", "createdAt"]);
    let completed_at = json_string(value, &["updated_at", "updatedAt"]);
    Ok(CheckItem {
        name,
        state: state.as_str(),
        url,
        conclusion: None,
        workflow: None,
        required: force_required,
        started_at,
        completed_at,
    })
}

fn json_string(value: &serde_json::Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        value
            .get(*key)
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    })
}

fn nested_json_string(value: &serde_json::Value, path: &[&str]) -> Option<String> {
    let mut current = value;
    for key in path {
        current = current.get(*key)?;
    }
    current
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn normalize_github_rollup_state(
    kind: Option<&str>,
    conclusion: Option<&str>,
    raw_state: Option<&str>,
) -> CheckState {
    if matches!(kind, Some("CheckRun"))
        && matches!(
            raw_state.map(str::to_ascii_lowercase).as_deref(),
            Some("completed")
        )
        && !is_known_github_conclusion(conclusion)
    {
        return CheckState::Pending;
    }
    normalize_github_state(None, conclusion, raw_state)
}

fn is_known_github_conclusion(conclusion: Option<&str>) -> bool {
    matches!(
        conclusion.map(str::to_ascii_lowercase).as_deref(),
        Some(
            "success"
                | "failure"
                | "action_required"
                | "stale"
                | "startup_failure"
                | "cancelled"
                | "skipped"
                | "neutral"
                | "timed_out"
        )
    )
}

fn parse_github_entry(
    value: &serde_json::Value,
    required_names: Option<&HashSet<String>>,
    force_required: bool,
) -> Result<CheckItem, ForgeError> {
    let name = value
        .get("name")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| missing("name"))?;
    let bucket = value
        .get("bucket")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let conclusion = value
        .get("conclusion")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let raw_state = value
        .get("state")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    let state = normalize_github_state(bucket.as_deref(), conclusion.as_deref(), raw_state);
    let url = value
        .get("link")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let workflow = value
        .get("workflow")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let required = if force_required {
        true
    } else if let Some(names) = required_names {
        names.contains(&name)
    } else {
        value
            .get("isRequired")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    };
    let started_at = value
        .get("startedAt")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let completed_at = value
        .get("completedAt")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    Ok(CheckItem {
        name,
        state: state.as_str(),
        url,
        conclusion,
        workflow,
        required,
        started_at,
        completed_at,
    })
}

/// Normalize a GitHub `(bucket, conclusion, state)` triple to a canonical
/// [`CheckState`]. `conclusion` is accepted for compatibility with older
/// fixture/output shapes, but the `gh 2.92.0` request path relies on `bucket`
/// and `state`.
pub fn normalize_github_state(
    bucket: Option<&str>,
    conclusion: Option<&str>,
    raw_state: Option<&str>,
) -> CheckState {
    if let Some(c) = conclusion {
        match c.to_ascii_lowercase().as_str() {
            "success" => return CheckState::Success,
            "failure" | "action_required" | "stale" | "startup_failure" => {
                return CheckState::Failure;
            }
            "cancelled" => return CheckState::Cancelled,
            "skipped" => return CheckState::Skipped,
            "neutral" => return CheckState::Neutral,
            "timed_out" => return CheckState::TimedOut,
            _ => {}
        }
    }
    let bucket = bucket.map(str::to_ascii_lowercase);
    match bucket.as_deref() {
        Some("pass") => CheckState::Success,
        Some("fail") => CheckState::Failure,
        Some("pending") => CheckState::Pending,
        Some("skipping") => CheckState::Skipped,
        Some("cancel") => CheckState::Cancelled,
        _ => match raw_state.map(str::to_ascii_lowercase).as_deref() {
            Some("success" | "pass" | "completed") => CheckState::Success,
            Some(
                "failure" | "failed" | "error" | "action_required" | "stale" | "startup_failure",
            ) => CheckState::Failure,
            Some("pending" | "queued" | "in_progress" | "waiting" | "requested" | "expected") => {
                CheckState::Pending
            }
            Some("cancelled" | "canceled") => CheckState::Cancelled,
            Some("skipped" | "skipping") => CheckState::Skipped,
            Some("neutral") => CheckState::Neutral,
            Some("timed_out" | "timeout") => CheckState::TimedOut,
            _ => CheckState::Pending,
        },
    }
}

/// Aggregate a list of checks into the payload, derived per spec's
/// terminal-state mapping.
pub fn aggregate(
    ctx: &ProviderContext,
    checks: Vec<CheckItem>,
    required_only: bool,
    duration_ms: Option<u64>,
) -> PrChecksPayload {
    let gating: Vec<&CheckItem> = if required_only {
        checks.iter().filter(|c| c.required).collect()
    } else {
        checks.iter().collect()
    };
    let mut required_count = 0u32;
    let mut success_count = 0u32;
    let mut failed: Vec<FailedCheck> = Vec::new();
    let mut pending: Vec<PendingCheck> = Vec::new();
    let mut has_failure = false;
    let mut has_cancelled = false;
    let mut has_timed_out = false;
    let mut has_pending = false;
    for c in &gating {
        required_count += 1;
        match c.state {
            "success" => success_count += 1,
            "failure" => {
                has_failure = true;
                failed.push(FailedCheck {
                    name: c.name.clone(),
                    url: c.url.clone(),
                    conclusion: c.conclusion.clone(),
                });
            }
            "cancelled" => {
                has_cancelled = true;
                failed.push(FailedCheck {
                    name: c.name.clone(),
                    url: c.url.clone(),
                    conclusion: c.conclusion.clone(),
                });
            }
            "timed_out" => {
                has_timed_out = true;
                failed.push(FailedCheck {
                    name: c.name.clone(),
                    url: c.url.clone(),
                    conclusion: c.conclusion.clone(),
                });
            }
            "pending" => {
                has_pending = true;
                pending.push(PendingCheck {
                    name: c.name.clone(),
                    url: c.url.clone(),
                });
            }
            // "skipped" / "neutral" are terminal non-failing; they count
            // against required_count but not toward success_count and they
            // don't pollute failed/pending.
            _ => {}
        }
    }
    let state = if has_failure {
        CheckState::Failure
    } else if has_timed_out {
        CheckState::TimedOut
    } else if has_cancelled {
        CheckState::Cancelled
    } else if has_pending {
        CheckState::Pending
    } else {
        // No failures, no pending, no cancelled, no timeouts.
        CheckState::Success
    };
    PrChecksPayload {
        provider: ctx.provider.as_str(),
        state: state.as_str(),
        required_count,
        success_count,
        failed,
        pending,
        checks,
        duration_ms,
        warnings: Vec::new(),
    }
}

pub(super) fn missing(key: &str) -> ForgeError {
    ForgeError::software(
        schema_err(),
        format!("missing required field '{key}' in pr checks JSON"),
        None,
    )
}

pub(super) fn schema_err() -> String {
    schema_version_for(BINARY, "error", 1)
}

/// Render the dry-run plan and emit it as the standard dry-run envelope.
pub fn emit_dry_run<R: BackendRunner>(
    _runner: &R,
    ctx: &ProviderContext,
    args: &PrChecksArgs,
    format: OutputFormat,
) -> i32 {
    let call = build_dry_run_call(ctx, args);
    let payload = DryRunPayload::new(ctx.provider, &call);
    emit_success(
        schema_version_for(BINARY, SCHEMA, SCHEMA_VERSION),
        payload,
        format,
        |p| println!("would run: {plan}", plan = p.plan.join(" ")),
    )
}

fn render_text(payload: &PrChecksPayload) {
    println!(
        "{state} [{provider}] required={required} success={success} failed={fcount} pending={pcount}",
        state = payload.state,
        provider = payload.provider,
        required = payload.required_count,
        success = payload.success_count,
        fcount = payload.failed.len(),
        pcount = payload.pending.len(),
    );
    for f in &payload.failed {
        println!(
            "  fail: {name}{url}",
            name = f.name,
            url = f
                .url
                .as_deref()
                .map(|u| format!(" ({u})"))
                .unwrap_or_default(),
        );
    }
    for p in &payload.pending {
        println!(
            "  pending: {name}{url}",
            name = p.name,
            url = p
                .url
                .as_deref()
                .map(|u| format!(" ({u})"))
                .unwrap_or_default(),
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

    fn item(name: &str, state: CheckState, required: bool) -> CheckItem {
        CheckItem {
            name: name.into(),
            state: state.as_str(),
            url: Some(format!("https://ci/{name}")),
            conclusion: None,
            workflow: None,
            required,
            started_at: None,
            completed_at: None,
        }
    }

    #[test]
    fn check_state_terminal_and_failing_classifications() {
        assert!(!CheckState::Pending.is_terminal());
        assert!(CheckState::Success.is_terminal());
        assert!(CheckState::Failure.is_failing());
        assert!(CheckState::Cancelled.is_failing());
        assert!(CheckState::TimedOut.is_failing());
        assert!(CheckState::Success.is_passing());
        assert!(CheckState::Skipped.is_passing());
        assert!(CheckState::Neutral.is_passing());
        assert!(!CheckState::Pending.is_passing());
    }

    #[test]
    fn normalize_prefers_conclusion_over_bucket() {
        assert_eq!(
            normalize_github_state(Some("pass"), Some("success"), None),
            CheckState::Success
        );
        assert_eq!(
            normalize_github_state(Some("fail"), Some("timed_out"), None),
            CheckState::TimedOut
        );
        assert_eq!(
            normalize_github_state(Some("pass"), Some("neutral"), None),
            CheckState::Neutral
        );
        assert_eq!(
            normalize_github_state(Some("pass"), Some("skipped"), None),
            CheckState::Skipped
        );
        assert_eq!(
            normalize_github_state(Some("fail"), Some("action_required"), None),
            CheckState::Failure
        );
    }

    #[test]
    fn normalize_falls_back_to_bucket_when_no_conclusion() {
        assert_eq!(
            normalize_github_state(Some("pending"), None, None),
            CheckState::Pending
        );
        assert_eq!(
            normalize_github_state(Some("cancel"), None, None),
            CheckState::Cancelled
        );
        assert_eq!(
            normalize_github_state(None, None, None),
            CheckState::Pending
        );
    }

    #[test]
    fn normalize_falls_back_to_state_when_bucket_is_unknown() {
        assert_eq!(
            normalize_github_state(None, None, Some("SUCCESS")),
            CheckState::Success
        );
        assert_eq!(
            normalize_github_state(None, None, Some("FAILURE")),
            CheckState::Failure
        );
        assert_eq!(
            normalize_github_state(None, None, Some("IN_PROGRESS")),
            CheckState::Pending
        );
        assert_eq!(
            normalize_github_state(None, None, Some("CANCELLED")),
            CheckState::Cancelled
        );
    }

    #[test]
    fn aggregate_all_required_success_is_success() {
        let checks = vec![
            item("a", CheckState::Success, true),
            item("b", CheckState::Success, true),
            // Optional pending check stays out of the gating set.
            item("opt", CheckState::Pending, false),
        ];
        let p = aggregate(&ctx(Provider::GitHub), checks, true, None);
        assert_eq!(p.state, "success");
        assert_eq!(p.required_count, 2);
        assert_eq!(p.success_count, 2);
        assert!(p.failed.is_empty());
        assert!(p.pending.is_empty());
        assert_eq!(p.checks.len(), 3); // all reported in data.checks
    }

    #[test]
    fn aggregate_any_required_failure_is_failure() {
        let checks = vec![
            item("a", CheckState::Success, true),
            item("b", CheckState::Failure, true),
            item("c", CheckState::Pending, true),
        ];
        let p = aggregate(&ctx(Provider::GitHub), checks, true, None);
        assert_eq!(p.state, "failure");
        assert_eq!(p.required_count, 3);
        assert_eq!(p.success_count, 1);
        assert_eq!(p.failed.len(), 1);
        assert_eq!(p.failed[0].name, "b");
        // pending list still populated when failure also present, so callers
        // see the whole snapshot.
        assert_eq!(p.pending.len(), 1);
        assert_eq!(p.pending[0].name, "c");
    }

    #[test]
    fn aggregate_skipped_and_neutral_count_required_but_not_success() {
        let checks = vec![
            item("a", CheckState::Success, true),
            item("b", CheckState::Skipped, true),
            item("c", CheckState::Neutral, true),
        ];
        let p = aggregate(&ctx(Provider::GitHub), checks, true, None);
        // All terminal, none failing -> success.
        assert_eq!(p.state, "success");
        assert_eq!(p.required_count, 3);
        assert_eq!(p.success_count, 1);
        assert!(p.failed.is_empty());
        assert!(p.pending.is_empty());
    }

    #[test]
    fn aggregate_pending_only_required_is_pending() {
        let checks = vec![
            item("a", CheckState::Pending, true),
            item("b", CheckState::Pending, true),
        ];
        let p = aggregate(&ctx(Provider::GitHub), checks, true, None);
        assert_eq!(p.state, "pending");
        assert_eq!(p.required_count, 2);
        assert_eq!(p.success_count, 0);
        assert_eq!(p.pending.len(), 2);
    }

    #[test]
    fn aggregate_cancelled_outranks_pending_but_failure_outranks_cancelled() {
        let mixed = vec![
            item("a", CheckState::Cancelled, true),
            item("b", CheckState::Pending, true),
        ];
        let p = aggregate(&ctx(Provider::GitHub), mixed, true, None);
        assert_eq!(p.state, "cancelled");
        let mixed2 = vec![
            item("a", CheckState::Failure, true),
            item("b", CheckState::Cancelled, true),
        ];
        let p2 = aggregate(&ctx(Provider::GitHub), mixed2, true, None);
        assert_eq!(p2.state, "failure");
    }

    #[test]
    fn aggregate_required_only_false_includes_optional_checks_in_gating() {
        let checks = vec![
            item("req", CheckState::Success, true),
            item("opt-fail", CheckState::Failure, false),
        ];
        // required-only=true treats optional failure as non-gating; state = success.
        let p_true = aggregate(&ctx(Provider::GitHub), checks.clone(), true, None);
        assert_eq!(p_true.state, "success");
        assert_eq!(p_true.required_count, 1);
        assert!(p_true.failed.is_empty());
        // required-only=false promotes optional checks into the gating set.
        let p_false = aggregate(&ctx(Provider::GitHub), checks, false, None);
        assert_eq!(p_false.state, "failure");
        assert_eq!(p_false.required_count, 2);
        assert_eq!(p_false.failed.len(), 1);
    }

    #[test]
    fn aggregate_empty_checks_is_success() {
        let p = aggregate(&ctx(Provider::GitHub), Vec::new(), true, None);
        assert_eq!(p.state, "success");
        assert_eq!(p.required_count, 0);
        assert_eq!(p.success_count, 0);
        assert!(p.failed.is_empty());
        assert!(p.pending.is_empty());
        assert!(p.checks.is_empty());
    }

    #[test]
    fn aggregate_records_duration_ms() {
        let p = aggregate(&ctx(Provider::GitHub), Vec::new(), true, Some(1234));
        assert_eq!(p.duration_ms, Some(1234));
    }

    #[test]
    fn build_github_call_carries_id_and_json_fields() {
        let call = build_github_call(&ctx(Provider::GitHub), "42");
        let plan = call.plan_argv();
        assert_eq!(
            plan[1..4],
            ["pr".to_string(), "checks".to_string(), "42".to_string()]
        );
        let json_idx = plan.iter().position(|s| s == "--json").expect("--json arg");
        assert!(plan[json_idx + 1].contains("bucket"));
        assert!(!plan[json_idx + 1].contains("isRequired"));
        assert!(!plan[json_idx + 1].contains("conclusion"));
    }

    #[test]
    fn build_github_required_call_uses_required_flag_and_supported_json_fields() {
        let call = build_github_required_call(&ctx(Provider::GitHub), "42");
        let plan = call.plan_argv();
        assert!(plan.iter().any(|s| s == "--required"), "{plan:?}");
        let json_idx = plan.iter().position(|s| s == "--json").expect("--json arg");
        assert_eq!(plan[json_idx + 1], GH_JSON_FIELDS);
        assert!(!plan[json_idx + 1].contains("isRequired"));
        assert!(!plan[json_idx + 1].contains("conclusion"));
    }

    #[test]
    fn parse_github_snapshot_empty_array_is_success() {
        let out = BackendSuccess {
            stdout: "[]".into(),
            stderr: String::new(),
        };
        let p = parse_github_snapshot(&ctx(Provider::GitHub), &out, true).unwrap();
        assert_eq!(p.state, "success");
        assert!(p.checks.is_empty());
    }

    #[test]
    fn parse_github_snapshot_blank_stdout_is_success() {
        let out = BackendSuccess {
            stdout: String::new(),
            stderr: String::new(),
        };
        let p = parse_github_snapshot(&ctx(Provider::GitHub), &out, true).unwrap();
        assert_eq!(p.state, "success");
    }

    #[test]
    fn parse_github_snapshot_invalid_json_is_software_error() {
        let out = BackendSuccess {
            stdout: "not json".into(),
            stderr: String::new(),
        };
        let err = parse_github_snapshot(&ctx(Provider::GitHub), &out, true).expect_err("invalid");
        assert_eq!(err.kind(), "software_error");
    }

    #[test]
    fn parse_github_snapshot_full_success_fixture() {
        let stdout = r#"[
            {"name":"build","bucket":"pass","state":"COMPLETED","link":"https://ci/1","workflow":"CI","startedAt":"2026-05-19T10:00:00Z","completedAt":"2026-05-19T10:05:00Z","description":""},
            {"name":"lint","bucket":"pass","state":"COMPLETED","link":"https://ci/2","workflow":"CI","startedAt":"2026-05-19T10:00:00Z","completedAt":"2026-05-19T10:02:00Z","description":""}
        ]"#;
        let out = BackendSuccess {
            stdout: stdout.into(),
            stderr: String::new(),
        };
        let p = parse_github_snapshot(&ctx(Provider::GitHub), &out, false).unwrap();
        assert_eq!(p.state, "success");
        assert_eq!(p.required_count, 2);
        assert_eq!(p.success_count, 2);
        assert_eq!(p.checks.len(), 2);
        assert_eq!(p.checks[0].url.as_deref(), Some("https://ci/1"));
        assert_eq!(p.checks[0].workflow.as_deref(), Some("CI"));
    }

    #[test]
    fn parse_github_snapshot_with_required_output_marks_only_required_names() {
        let all_stdout = r#"[
            {"name":"build","bucket":"pass","state":"COMPLETED","link":"https://ci/1"},
            {"name":"test","bucket":"fail","state":"COMPLETED","link":"https://ci/2"},
            {"name":"flaky","bucket":"pending","state":"IN_PROGRESS","link":"https://ci/3"}
        ]"#;
        let required_stdout = r#"[
            {"name":"build","bucket":"pass","state":"COMPLETED","link":"https://ci/1"},
            {"name":"test","bucket":"fail","state":"COMPLETED","link":"https://ci/2"}
        ]"#;
        let all = BackendSuccess {
            stdout: all_stdout.into(),
            stderr: String::new(),
        };
        let required = BackendSuccess {
            stdout: required_stdout.into(),
            stderr: String::new(),
        };
        let p = parse_github_snapshot_with_required_output(&ctx(Provider::GitHub), &all, &required)
            .unwrap();
        assert_eq!(p.state, "failure");
        assert_eq!(p.required_count, 2);
        assert_eq!(p.failed.len(), 1);
        assert_eq!(p.failed[0].name, "test");
        // Optional pending check is included in data.checks but not in gating
        // pending list when --required-only=true.
        assert_eq!(p.checks.len(), 3);
        assert!(p.pending.iter().all(|p| p.name != "flaky"));
    }

    #[test]
    fn parse_github_snapshot_handles_missing_name_as_software_error() {
        let stdout = r#"[{"bucket":"pass","isRequired":true}]"#;
        let out = BackendSuccess {
            stdout: stdout.into(),
            stderr: String::new(),
        };
        let err = parse_github_snapshot(&ctx(Provider::GitHub), &out, true).expect_err("missing");
        assert_eq!(err.kind(), "software_error");
    }
}
