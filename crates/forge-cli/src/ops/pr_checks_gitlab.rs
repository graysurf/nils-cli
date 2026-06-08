//! GitLab branch of `pr checks`.
//!
//! Numeric MR checks prefer structured GitLab API data through `glab api`:
//! `mr view -F json` provides the MR URL + head pipeline, then the jobs API
//! provides individual job rows. Branch-only calls without a repo/project path
//! keep the older `glab ci status -b <branch>` text fallback. The fallback is
//! still version-pinned because it parses human output.

use std::ffi::OsString;

use crate::backend::{BackendCall, BackendProgram, BackendRunner, BackendSuccess};
use crate::error::ForgeError;
use crate::glab_version::{ensure_supported, parse_version_line};
use crate::ops::gitlab_api;
use crate::ops::pr_checks::{
    CheckItem, CheckState, PrChecksPayload, aggregate, missing, schema_err,
};
use crate::provider::ProviderContext;

/// Build the `glab ci status -b <branch>` call. The id parameter is the
/// branch name to query (the resolver upstream of this function converts a
/// numeric MR id to its source branch).
pub fn build_status_call(ctx: &ProviderContext, branch: &str) -> BackendCall {
    let mut argv: Vec<OsString> = vec![
        OsString::from("ci"),
        OsString::from("status"),
        OsString::from("-b"),
        OsString::from(branch),
    ];
    ctx.push_repo_override(&mut argv);
    BackendCall::new(BackendProgram::Glab, argv)
}

/// Build the `glab --version` probe call.
pub fn build_version_call() -> BackendCall {
    BackendCall::new(BackendProgram::Glab, [OsString::from("--version")])
}

/// Snapshot path for `pr checks` against GitLab. Probes version first,
/// then resolves `--head <branch>` from the id (numeric MRs are looked up via
/// `mr view`), then parses `glab ci status` text.
///
/// `glab` exits non-zero when there is no pipeline at all (vs. an empty
/// pipeline). We surface that case as an empty-success payload to match the
/// "empty pipeline" semantic the rest of the gate already expects — a repo
/// without `.gitlab-ci.yml` should not look like a backend failure to the
/// merge gate.
pub fn snapshot<R: BackendRunner>(
    runner: &R,
    ctx: &ProviderContext,
    args: &crate::cli::PrChecksArgs,
) -> Result<PrChecksPayload, ForgeError> {
    if let Some(payload) = snapshot_via_api(runner, ctx, args)? {
        return Ok(payload);
    }
    snapshot_via_text(runner, ctx, args)
}

fn snapshot_via_text<R: BackendRunner>(
    runner: &R,
    ctx: &ProviderContext,
    args: &crate::cli::PrChecksArgs,
) -> Result<PrChecksPayload, ForgeError> {
    probe_version(runner)?;
    let branch = resolve_branch(runner, ctx, &args.id)?;
    let call = build_status_call(ctx, &branch);
    let raw = runner.run_raw(&call)?;
    if !raw.status_success {
        let stderr_lower = raw.stderr.to_ascii_lowercase();
        let stdout_lower = raw.stdout.to_ascii_lowercase();
        if stderr_lower.contains("no pipeline") || stdout_lower.contains("no pipeline") {
            return Ok(aggregate(ctx, Vec::new(), args.required_only, None));
        }
        return Err(ForgeError::backend_error(
            schema_err(),
            format!(
                "{exe} exited with status {exit_code}",
                exe = BackendProgram::Glab.executable().to_string_lossy(),
                exit_code = raw.exit_code,
            ),
            Some(raw.stderr),
        ));
    }
    let output = BackendSuccess {
        stdout: raw.stdout,
        stderr: raw.stderr,
    };
    parse_status_text(ctx, &output, args.required_only)
}

fn snapshot_via_api<R: BackendRunner>(
    runner: &R,
    ctx: &ProviderContext,
    args: &crate::cli::PrChecksArgs,
) -> Result<Option<PrChecksPayload>, ForgeError> {
    if args.id.chars().all(|c| c.is_ascii_digit()) {
        let mr = fetch_mr_view_json(runner, ctx, &args.id)?;
        return Ok(snapshot_from_mr_json(runner, ctx, &mr, args.required_only));
    }

    let Some(project) = gitlab_api::project_path_from_ctx(ctx) else {
        return Ok(None);
    };
    let encoded_project = gitlab_api::encode_project_path(project);
    let branch = gitlab_api::encode_query_value(&args.id);
    let path = format!(
        "projects/{encoded_project}/merge_requests?state=opened&source_branch={branch}&per_page=1"
    );
    let out = runner.run(&gitlab_api::api_call(&ctx.host, path))?;
    let value: serde_json::Value = serde_json::from_str(out.stdout.trim()).map_err(|e| {
        ForgeError::software(
            schema_err(),
            "GitLab merge request API JSON is invalid",
            Some(e.to_string()),
        )
    })?;
    let Some(iid) = value
        .as_array()
        .and_then(|arr| arr.first())
        .and_then(|mr| mr.get("iid"))
        .and_then(|v| v.as_u64())
    else {
        return Ok(None);
    };
    let path = format!("projects/{encoded_project}/merge_requests/{iid}");
    let out = runner.run(&gitlab_api::api_call(&ctx.host, path))?;
    let mr: serde_json::Value = serde_json::from_str(out.stdout.trim()).map_err(|e| {
        ForgeError::software(
            schema_err(),
            "GitLab merge request API JSON is invalid",
            Some(e.to_string()),
        )
    })?;
    Ok(snapshot_from_mr_json(runner, ctx, &mr, args.required_only))
}

fn fetch_mr_view_json<R: BackendRunner>(
    runner: &R,
    ctx: &ProviderContext,
    id: &str,
) -> Result<serde_json::Value, ForgeError> {
    let mut argv: Vec<OsString> = vec![
        OsString::from("mr"),
        OsString::from("view"),
        OsString::from(id),
        OsString::from("-F"),
        OsString::from("json"),
    ];
    ctx.push_repo_override(&mut argv);
    let out = runner.run(&BackendCall::new(BackendProgram::Glab, argv))?;
    serde_json::from_str(out.stdout.trim()).map_err(|e| {
        ForgeError::software(
            schema_err(),
            "glab mr view JSON is invalid",
            Some(e.to_string()),
        )
    })
}

fn snapshot_from_mr_json<R: BackendRunner>(
    runner: &R,
    ctx: &ProviderContext,
    mr: &serde_json::Value,
    required_only: bool,
) -> Option<PrChecksPayload> {
    let (host, project) = gitlab_target(ctx, mr)?;
    let pipeline = mr
        .get("head_pipeline")
        .filter(|v| !v.is_null())
        .or_else(|| mr.get("pipeline").filter(|v| !v.is_null()));
    let Some(pipeline) = pipeline else {
        return Some(aggregate(ctx, Vec::new(), required_only, None));
    };

    if let Some(pipeline_id) = pipeline.get("id").and_then(|v| v.as_u64()) {
        let encoded_project = gitlab_api::encode_project_path(&project);
        let path = format!("projects/{encoded_project}/pipelines/{pipeline_id}/jobs?per_page=100");
        match runner.run(&gitlab_api::api_call(&host, path)) {
            Ok(out) => match parse_jobs_json(&out) {
                Ok(jobs) if !jobs.is_empty() => {
                    return Some(aggregate(ctx, jobs, required_only, None));
                }
                Ok(_) => {}
                Err(_) => {}
            },
            Err(ForgeError::BackendUnavailable { .. }) => return None,
            Err(_) => {}
        }
    }

    let checks = pipeline_check_item(pipeline).into_iter().collect();
    Some(aggregate(ctx, checks, required_only, None))
}

fn gitlab_target(ctx: &ProviderContext, mr: &serde_json::Value) -> Option<(String, String)> {
    if let Some(url) = mr.get("web_url").and_then(|v| v.as_str()) {
        let host = gitlab_api::host_from_url(url)?;
        let project = gitlab_api::project_path_from_mr_url(url)?;
        return Some((host, project));
    }
    let project = gitlab_api::project_path_from_ctx(ctx)?.to_string();
    Some((ctx.host.clone(), project))
}

fn parse_jobs_json(output: &BackendSuccess) -> Result<Vec<CheckItem>, ForgeError> {
    let value: serde_json::Value = serde_json::from_str(output.stdout.trim()).map_err(|e| {
        ForgeError::software(
            schema_err(),
            "GitLab jobs API JSON is invalid",
            Some(e.to_string()),
        )
    })?;
    let arr = value.as_array().ok_or_else(|| {
        ForgeError::software(schema_err(), "GitLab jobs API JSON is not an array", None)
    })?;
    let mut checks = Vec::new();
    for job in arr {
        let Some(status) = job.get("status").and_then(|v| v.as_str()) else {
            continue;
        };
        let Some(state) = match_status(status) else {
            continue;
        };
        let name = job
            .get("name")
            .and_then(|v| v.as_str())
            .or_else(|| job.get("stage").and_then(|v| v.as_str()))
            .unwrap_or("job")
            .to_string();
        let required = !job
            .get("allow_failure")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        checks.push(CheckItem {
            name,
            state: state.as_str(),
            url: job
                .get("web_url")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            conclusion: Some(canonical_status(state)),
            workflow: job
                .get("stage")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            required,
            started_at: job
                .get("started_at")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            completed_at: job
                .get("finished_at")
                .and_then(|v| v.as_str())
                .map(str::to_string),
        });
    }
    Ok(checks)
}

fn pipeline_check_item(pipeline: &serde_json::Value) -> Option<CheckItem> {
    let status = pipeline.get("status").and_then(|v| v.as_str())?;
    let state = match_status(status)?;
    Some(CheckItem {
        name: pipeline
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("pipeline")
            .to_string(),
        state: state.as_str(),
        url: pipeline
            .get("web_url")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        conclusion: Some(canonical_status(state)),
        workflow: Some("pipeline".to_string()),
        required: true,
        started_at: pipeline
            .get("created_at")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        completed_at: pipeline
            .get("updated_at")
            .and_then(|v| v.as_str())
            .map(str::to_string),
    })
}

/// Verify the installed `glab` is inside the pinned support range. Returns
/// the parsed minor on success; on out-of-range, returns the typed
/// `glab_version_unsupported` error.
pub fn probe_version<R: BackendRunner>(runner: &R) -> Result<(u32, u32, u32), ForgeError> {
    let call = build_version_call();
    let output = runner.run(&call)?;
    let line = output.stdout.lines().next().unwrap_or("").trim();
    let parsed = parse_version_line(line).ok_or_else(|| {
        ForgeError::software(
            schema_err(),
            "could not parse glab --version output",
            Some(format!("stdout={line:?}")),
        )
    })?;
    ensure_supported(parsed).map_err(|hint| {
        ForgeError::unavailable(
            schema_err(),
            "glab_version_unsupported",
            "installed glab is outside the supported minor range for the branch-only GitLab CI text parser",
            Some(format!(
                "{hint} Numeric MR checks use structured GitLab API data when project context is available."
            )),
        )
    })?;
    Ok(parsed)
}

/// Resolve a PR id to a source branch. Numeric ids hit `glab mr view <id>`;
/// branch-shaped ids pass through.
fn resolve_branch<R: BackendRunner>(
    runner: &R,
    ctx: &ProviderContext,
    id: &str,
) -> Result<String, ForgeError> {
    if !id.chars().all(|c| c.is_ascii_digit()) {
        return Ok(id.to_string());
    }
    let mut argv: Vec<OsString> = vec![
        OsString::from("mr"),
        OsString::from("view"),
        OsString::from(id),
        OsString::from("-F"),
        OsString::from("json"),
    ];
    ctx.push_repo_override(&mut argv);
    let call = BackendCall::new(BackendProgram::Glab, argv);
    let out = runner.run(&call)?;
    let value: serde_json::Value = serde_json::from_str(out.stdout.trim()).map_err(|e| {
        ForgeError::software(
            schema_err(),
            "glab mr view JSON is invalid",
            Some(e.to_string()),
        )
    })?;
    value
        .get("source_branch")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| missing("source_branch"))
}

/// Parse `glab ci status -b <branch>` text output into the canonical payload.
///
/// `glab` prints pipeline jobs one per line. The expected format (across the
/// supported minor) is roughly:
///
/// ```text
/// stage  job_name  status (duration)
/// ```
///
/// We tolerate ANSI colour codes and leading whitespace; we accept the empty
/// "no pipeline found" output as "no checks". Unrecognised non-empty lines
/// are skipped to keep the parser strict-but-tolerant; if there are zero
/// recognised jobs we treat that as no checks present.
pub fn parse_status_text(
    ctx: &ProviderContext,
    output: &BackendSuccess,
    required_only: bool,
) -> Result<PrChecksPayload, ForgeError> {
    let stripped = strip_ansi(&output.stdout);
    let mut checks = Vec::new();
    let mut saw_marker = false;
    for raw in stripped.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        // Header / footer lines we recognise but skip.
        let lower = line.to_ascii_lowercase();
        if lower.starts_with("showing")
            || lower.starts_with("getting")
            || lower.starts_with("pipeline")
            || lower.starts_with("no pipeline")
            || lower.starts_with("checking")
            || lower.starts_with("status:")
            || lower.starts_with("ref:")
            || lower.starts_with("sha:")
            || lower.starts_with("url:")
            || lower.contains("press enter")
        {
            saw_marker = true;
            continue;
        }
        if let Some(item) = parse_job_line(line) {
            checks.push(item);
        }
    }
    let _ = saw_marker;
    Ok(aggregate(ctx, checks, required_only, None))
}

/// Parse a single job line. Accepts the `glab` formats:
///
/// - `<bullet> <name>  <status>` (compact form)
/// - `<stage>: <name>  (<status>)` (verbose form)
/// - `<name> <status>` (minimal)
///
/// Where status is one of: `success`, `failed`, `running`, `pending`,
/// `created`, `manual`, `skipped`, `canceled`/`cancelled`, `scheduled`.
fn parse_job_line(line: &str) -> Option<CheckItem> {
    // The status token is the most reliable anchor; everything before it is
    // the name (after stripping a leading bullet/stage prefix). We scan for
    // a known status token from the right so trailing duration / link
    // suffixes don't confuse the parser.
    let tokens: Vec<&str> = line.split_whitespace().collect();
    if tokens.len() < 2 {
        return None;
    }
    let mut status_idx: Option<usize> = None;
    let mut state: Option<CheckState> = None;
    for (idx, tok) in tokens.iter().enumerate() {
        let trimmed = tok.trim_matches(|c: char| !c.is_ascii_alphanumeric());
        if let Some(s) = match_status(trimmed) {
            status_idx = Some(idx);
            state = Some(s);
        }
    }
    let status_idx = status_idx?;
    let state = state?;
    // Name = tokens[0..status_idx], joined. Strip trailing punctuation (e.g.
    // the colon in `test:`) per-token and drop pure-punctuation tokens.
    let mut name_tokens: Vec<String> = Vec::new();
    for tok in &tokens[..status_idx] {
        let trimmed: String = tok
            .trim_matches(|c: char| matches!(c, ':' | ',' | ';' | '·' | '|' | '*'))
            .to_string();
        if trimmed.is_empty() || trimmed.chars().all(|c| !c.is_alphanumeric()) {
            continue;
        }
        name_tokens.push(trimmed);
    }
    if name_tokens.is_empty() {
        return None;
    }
    let name = name_tokens.join(" ");
    if name.is_empty() {
        return None;
    }
    // `glab` doesn't surface a per-job link in text mode; leave url None.
    // Treat every parsed pipeline job as required because GitLab does not
    // expose an "is required" flag in text output — gating thus matches
    // GitHub's `isRequired=true` semantics for the whole pipeline.
    Some(CheckItem {
        name,
        state: state.as_str(),
        url: None,
        conclusion: Some(canonical_status(state)),
        workflow: None,
        required: true,
        started_at: None,
        completed_at: None,
    })
}

fn match_status(tok: &str) -> Option<CheckState> {
    match tok.to_ascii_lowercase().as_str() {
        "success" | "succeeded" | "passed" => Some(CheckState::Success),
        "failed" | "failure" => Some(CheckState::Failure),
        "canceled" | "cancelled" => Some(CheckState::Cancelled),
        "skipped" => Some(CheckState::Skipped),
        "manual" => Some(CheckState::Neutral),
        "running" | "pending" | "created" | "scheduled" | "preparing" | "waiting_for_resource" => {
            Some(CheckState::Pending)
        }
        _ => None,
    }
}

fn canonical_status(s: CheckState) -> String {
    s.as_str().to_string()
}

/// Strip ANSI colour escapes (CSI / SGR) from `s`. Cheap state machine; we
/// never write user-provided ANSI back out so the stripping is
/// production-safe.
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            // Consume CSI: ESC [ … <final-byte>. Non-CSI escapes (e.g. ESC c
            // reset) drop their next char silently — we never echo ANSI back
            // out.
            if chars.next() == Some('[') {
                for next in chars.by_ref() {
                    if ('@'..='~').contains(&next) {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::BackendSuccess;
    use crate::provider::{DetectionSource, Provider};
    use pretty_assertions::assert_eq;

    fn ctx() -> ProviderContext {
        ProviderContext {
            provider: Provider::GitLab,
            host: "gitlab.com".into(),
            source: DetectionSource::Flag,
            repo: None,
        }
    }

    #[test]
    fn build_status_call_uses_branch_flag() {
        let call = build_status_call(&ctx(), "feat/sample");
        let plan = call.plan_argv();
        assert_eq!(
            plan[1..],
            [
                "ci".to_string(),
                "status".to_string(),
                "-b".to_string(),
                "feat/sample".to_string()
            ]
        );
    }

    #[test]
    fn build_status_call_includes_repo_override_when_set() {
        let mut ctx = ctx();
        ctx.repo = Some("owner/name".into());
        let plan = build_status_call(&ctx, "feat/sample").plan_argv();
        let pos = plan
            .iter()
            .position(|s| s == "--repo")
            .expect("--repo present");
        assert_eq!(plan[pos + 1], "owner/name");
    }

    #[test]
    fn strip_ansi_removes_colour_codes() {
        let s = "\u{1b}[32msuccess\u{1b}[0m";
        assert_eq!(strip_ansi(s), "success");
    }

    #[test]
    fn parse_job_line_compact_form() {
        let item = parse_job_line("build  success (2m 12s)").expect("parse");
        assert_eq!(item.name, "build");
        assert_eq!(item.state, "success");
        assert!(item.required);
    }

    #[test]
    fn parse_job_line_verbose_form() {
        let item = parse_job_line("test: rspec failed").expect("parse");
        assert_eq!(item.name, "test rspec");
        assert_eq!(item.state, "failure");
    }

    #[test]
    fn parse_job_line_canonicalises_canceled_to_cancelled() {
        let item = parse_job_line("deploy canceled").expect("parse");
        assert_eq!(item.state, "cancelled");
    }

    #[test]
    fn parse_status_text_all_success() {
        let stdout = "\
Showing status for branch feat/sample
build  success (2m)
lint   success (40s)
";
        let out = BackendSuccess {
            stdout: stdout.into(),
            stderr: String::new(),
        };
        let p = parse_status_text(&ctx(), &out, true).unwrap();
        assert_eq!(p.state, "success");
        assert_eq!(p.required_count, 2);
        assert_eq!(p.success_count, 2);
    }

    #[test]
    fn parse_status_text_one_failure() {
        let stdout = "\
build  success (2m)
test   failed (4m)
";
        let out = BackendSuccess {
            stdout: stdout.into(),
            stderr: String::new(),
        };
        let p = parse_status_text(&ctx(), &out, true).unwrap();
        assert_eq!(p.state, "failure");
        assert_eq!(p.required_count, 2);
        assert_eq!(p.failed.len(), 1);
        assert_eq!(p.failed[0].name, "test");
    }

    #[test]
    fn parse_status_text_pending_only() {
        let stdout = "\
build  running
lint   pending
";
        let out = BackendSuccess {
            stdout: stdout.into(),
            stderr: String::new(),
        };
        let p = parse_status_text(&ctx(), &out, true).unwrap();
        assert_eq!(p.state, "pending");
        assert_eq!(p.pending.len(), 2);
    }

    #[test]
    fn parse_status_text_manual_only_is_neutral_success() {
        let stdout = "deploy  manual\n";
        let out = BackendSuccess {
            stdout: stdout.into(),
            stderr: String::new(),
        };
        let p = parse_status_text(&ctx(), &out, true).unwrap();
        // Manual maps to neutral (terminal non-failing); aggregate to success.
        assert_eq!(p.state, "success");
        assert_eq!(p.required_count, 1);
    }

    #[test]
    fn parse_status_text_mixed_states_promotes_failure() {
        let stdout = "\
build  success (2m)
test   failed (4m)
deploy manual
e2e    skipped
";
        let out = BackendSuccess {
            stdout: stdout.into(),
            stderr: String::new(),
        };
        let p = parse_status_text(&ctx(), &out, true).unwrap();
        assert_eq!(p.state, "failure");
        assert_eq!(p.required_count, 4);
        assert_eq!(p.failed.len(), 1);
    }

    #[test]
    fn parse_status_text_empty_pipeline_is_success() {
        let stdout = "No pipeline found for branch feat/sample\n";
        let out = BackendSuccess {
            stdout: stdout.into(),
            stderr: String::new(),
        };
        let p = parse_status_text(&ctx(), &out, true).unwrap();
        assert_eq!(p.state, "success");
        assert_eq!(p.required_count, 0);
        assert!(p.checks.is_empty());
    }

    #[test]
    fn parse_status_text_strips_ansi_codes() {
        let stdout = "build  \u{1b}[32msuccess\u{1b}[0m (1m)\n";
        let out = BackendSuccess {
            stdout: stdout.into(),
            stderr: String::new(),
        };
        let p = parse_status_text(&ctx(), &out, true).unwrap();
        assert_eq!(p.state, "success");
    }
}
