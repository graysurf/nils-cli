use std::fs;
use std::path::Path;

use nils_common::markdown;
use nils_common::process as common_process;
use serde_json::Value;

use crate::commands::plan::CloseReason;

pub trait ProviderAdapter {
    fn issue_body(&self, repo: &str, issue: u64) -> Result<String, String>;

    /// Fetch the issue body plus comments JSON, suitable for `audit_record`
    /// fixture parsing. Returns `(body, comments_json)` where
    /// `comments_json` is the raw JSON string from
    /// `gh issue view --json comments`.
    fn issue_evidence(&self, repo: &str, issue: u64) -> Result<(String, String), String>;

    /// Enumerate open tracker issues to consider for `record open` resume,
    /// scoped by `labels` (AND semantics; an empty slice lists every open
    /// issue). Returns the issue numbers; the caller reads each one's
    /// lifecycle evidence to match the bundle's source snapshot identity.
    fn list_open_tracker_issues(&self, repo: &str, labels: &[String]) -> Result<Vec<u64>, String>;

    fn create_issue(
        &self,
        repo: &str,
        title: &str,
        body_file: &Path,
        labels: &[String],
    ) -> Result<(u64, String), String>;

    fn edit_issue_body(&self, repo: &str, issue: u64, body_file: &Path) -> Result<(), String>;

    /// Post an issue comment. Returns the URL of the created comment as
    /// printed by `gh issue comment` on stdout.
    fn comment_issue(&self, repo: &str, issue: u64, body_file: &Path) -> Result<String, String>;

    fn edit_issue_labels(
        &self,
        repo: &str,
        issue: u64,
        add_labels: &[String],
        remove_labels: &[String],
    ) -> Result<(), String>;

    fn close_issue(
        &self,
        repo: &str,
        issue: u64,
        reason: CloseReason,
        close_comment: Option<&str>,
    ) -> Result<(), String>;

    fn pr_is_merged(&self, repo: &str, pr: u64) -> Result<bool, String>;

    /// Provider-verified PR summary used by `record close` strict gating.
    /// Returns merge state, optional merge commit SHA, and rolled-up check
    /// status when available.
    fn pr_merge_summary(&self, repo: &str, pr: u64) -> Result<PrMergeSummary, String>;

    /// List the PR's issue-style comments via `gh api`. Returns an array of
    /// objects with at least `body` and `html_url` keys (passed through
    /// from the GitHub REST response). Used by `resolve-approval`.
    fn pr_comments(&self, repo: &str, pr: u64) -> Result<Vec<Value>, String>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrMergeSummary {
    /// Raw `state` field from the GitHub PR view (`MERGED`, `OPEN`, `CLOSED`).
    pub state: String,
    pub merged: bool,
    pub merge_sha: Option<String>,
    /// Rolled-up status check state when known
    /// (`success`, `failure`, `pending`, `error`, ...).
    pub checks: Option<String>,
    /// Required-check rollup state when the adapter can resolve the
    /// required/non-required distinction
    /// (`success`, `failure`, `pending`, ...). `None` means the
    /// adapter could not classify (e.g. GitLab today, or a degraded
    /// `gh` call); the close gate falls back to `checks` in that case.
    pub required_state: Option<String>,
    /// Number of required checks reported by the provider. `None` when
    /// classification is unavailable; `Some(0)` means zero required
    /// checks were declared.
    pub required_count: Option<u32>,
    /// Names of non-required checks that ended in a failure-class
    /// state. Used as informational evidence in the closeout comment;
    /// the close gate never blocks on this alone.
    pub non_required_failures: Vec<String>,
}

/// Output captured from a single `gh` invocation. Exposes stdout, stderr,
/// and exit status so callers that need to branch on stderr text (e.g.
/// `pr_required_summary`'s "no required checks reported" recognition) can
/// inspect every channel without re-shelling out.
#[derive(Debug, Clone)]
pub struct GhRunOutput {
    pub success: bool,
    /// Raw exit code surfaced from the runner. Production paths only
    /// branch on `success`; the field is kept for tests that need to
    /// assert a specific non-zero code, and for future probes.
    #[allow(dead_code)]
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

/// Test-friendly indirection over the `gh` shellout. Production code uses
/// [`default_gh_runner`]; tests inject a fake that returns canned outputs
/// via [`GhCliAdapter::with_runner`].
pub type GhRunner = fn(&[&str]) -> Result<GhRunOutput, String>;

/// Default runner backing [`GhCliAdapter`]. Spawns the real `gh` binary
/// and forwards stdout / stderr / exit status into [`GhRunOutput`].
pub fn default_gh_runner(args: &[&str]) -> Result<GhRunOutput, String> {
    let output = common_process::run_output("gh", args)
        .map(|output| output.into_std_output())
        .map_err(|err| format!("failed to execute gh: {err}"))?;
    Ok(GhRunOutput {
        success: output.status.success(),
        exit_code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
    })
}

#[derive(Debug, Clone, Copy)]
pub struct GhCliAdapter {
    force: bool,
    runner: GhRunner,
}

impl Default for GhCliAdapter {
    fn default() -> Self {
        Self {
            force: false,
            runner: default_gh_runner,
        }
    }
}

impl GhCliAdapter {
    pub const fn new(force: bool) -> Self {
        Self {
            force,
            runner: default_gh_runner,
        }
    }

    /// Build an adapter with a custom runner. Used by unit tests to drive
    /// `gh` call sites deterministically; production callers should keep
    /// using [`GhCliAdapter::new`].
    #[cfg(test)]
    pub const fn with_runner(force: bool, runner: GhRunner) -> Self {
        Self { force, runner }
    }

    fn run(&self, args: &[String]) -> Result<String, String> {
        let arg_refs = args.iter().map(String::as_str).collect::<Vec<_>>();
        let output = (self.runner)(&arg_refs)?;
        if !output.success {
            let stderr_trim = output.stderr.trim();
            let stdout_trim = output.stdout.trim();
            let detail = if stderr_trim.is_empty() {
                stdout_trim
            } else {
                stderr_trim
            };
            return Err(format!("gh {} failed: {detail}", args.join(" ")));
        }
        Ok(output.stdout)
    }

    /// Like [`Self::run`] but surfaces the full [`GhRunOutput`] regardless
    /// of exit status. Used by `pr_required_summary` so the "no required
    /// checks reported" stderr branch can be recognised without losing
    /// other failure paths.
    fn run_full(&self, args: &[String]) -> Result<GhRunOutput, String> {
        let arg_refs = args.iter().map(String::as_str).collect::<Vec<_>>();
        (self.runner)(&arg_refs)
    }

    fn parse_json(stdout: &str, context: &str) -> Result<Value, String> {
        serde_json::from_str(stdout.trim())
            .map_err(|err| format!("failed to parse gh JSON for {context}: {err}"))
    }

    fn guard_markdown_payload(&self, payload: &str, context: &str) -> Result<(), String> {
        if self.force {
            return Ok(());
        }

        markdown::validate_markdown_payload(payload).map_err(|err| {
            format!("{context}: {err}. Replace escaped controls or re-run with --force.")
        })
    }

    /// Resolve the required-check rollup for `repo#pr` via
    /// `gh pr checks <pr> --required --json bucket,state,conclusion,name`.
    ///
    /// Returns `(required_state, required_count, non_required_failures)`.
    /// All three are `None`/empty when `gh` rejects the call or the
    /// JSON does not parse — the close gate then falls back to the
    /// aggregate rollup carried in `checks`. The `rollup_value`
    /// argument is the `statusCheckRollup` field harvested from
    /// `gh pr view` and is used to compute the non-required failure
    /// list (rollup names not in the required set).
    fn pr_required_summary(
        &self,
        repo: &str,
        pr: u64,
        rollup_value: Option<&Value>,
    ) -> (Option<String>, Option<u32>, Vec<String>) {
        let args = vec![
            "pr".to_string(),
            "checks".to_string(),
            pr.to_string(),
            "--repo".to_string(),
            repo.to_string(),
            "--required".to_string(),
            "--json".to_string(),
            // Only fields this function actually reads. `conclusion` was
            // listed historically but the function never indexes
            // `required_array[].conclusion`; current `gh` (cli >=2.x)
            // also rejects `conclusion` outright with
            // `Unknown JSON field: "conclusion"` (exit 5), which would
            // otherwise force every callsite into the catch-all `unknown`
            // render branch.
            "name,state".to_string(),
        ];
        let output = match self.run_full(&args) {
            Ok(out) => out,
            // Spawn failures (network / `gh` not installed) — fall back
            // to the catch-all `unknown` render branch.
            Err(_) => return (None, None, Vec::new()),
        };
        if !output.success {
            // upstream contract: `gh pr checks --required` exits non-zero
            // and writes one of two stderr messages for a branch with no
            // required-check rule:
            //   - `no required checks reported on the '<branch>' branch`
            //     when the branch has checks but none are required, and
            //   - `no checks reported on the '<branch>' branch` when the
            //     branch has no checks at all (note: no "required" word).
            // Both mean "zero required checks": treat them as the canonical
            // zero-required success rollup so non-required failures do not
            // block the close gate and the renderer can show `none required`
            // rather than `unknown`. Other non-zero exits remain failures and
            // propagate as `(None, None, [])`.
            if output.stderr.contains("no required checks reported")
                || output.stderr.contains("no checks reported")
            {
                return (Some("success".to_string()), Some(0), Vec::new());
            }
            return (None, None, Vec::new());
        }
        let raw = output.stdout;
        let required_array: Vec<Value> = match serde_json::from_str(raw.trim()) {
            Ok(Value::Array(items)) => items,
            Ok(_) => return (None, None, Vec::new()),
            // Defensive secondary: if a future `gh` version starts
            // exiting 0 with empty stdout for the same condition (e.g.
            // `--json` mode flips to "empty array"), still treat it as
            // zero-required success.
            Err(_) if raw.trim().is_empty() => {
                return (Some("success".to_string()), Some(0), Vec::new());
            }
            Err(_) => return (None, None, Vec::new()),
        };
        let required_count = u32::try_from(required_array.len()).unwrap_or(u32::MAX);
        // Empty array means "no required checks defined" — same logical
        // state as the stderr-recognition branch above and the
        // defensive empty-stdout branch. Map directly to "success" so
        // the renderer can pick the `none required` label rather than
        // bouncing through `rollup_status`, which would return "none"
        // for an empty array and downgrade to `CheckStatus::None`.
        if required_array.is_empty() {
            return (Some("success".to_string()), Some(0), Vec::new());
        }
        let required_state = rollup_status(&Value::Array(required_array.clone()))
            .unwrap_or_else(|| "unknown".to_string());

        let required_names: std::collections::HashSet<String> = required_array
            .iter()
            .filter_map(|item| item.get("name").and_then(Value::as_str).map(str::to_string))
            .collect();

        let mut non_required_failures: Vec<String> = Vec::new();
        if let Some(Value::Array(items)) = rollup_value {
            for item in items {
                let name = item
                    .get("name")
                    .or_else(|| item.get("context"))
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let state_str = item
                    .get("conclusion")
                    .or_else(|| item.get("state"))
                    .and_then(Value::as_str)
                    .map(|s| s.to_ascii_lowercase());
                let failed = matches!(
                    state_str.as_deref(),
                    Some(
                        "failure"
                            | "cancelled"
                            | "timed_out"
                            | "action_required"
                            | "error"
                            | "stale"
                            | "startup_failure"
                    )
                );
                if let Some(name) = name
                    && failed
                    && !required_names.contains(&name)
                {
                    non_required_failures.push(name);
                }
            }
        }
        non_required_failures.sort();
        non_required_failures.dedup();

        (
            Some(required_state),
            Some(required_count),
            non_required_failures,
        )
    }

    fn guard_markdown_file(&self, path: &Path, context: &str) -> Result<(), String> {
        if self.force {
            return Ok(());
        }

        let payload = fs::read_to_string(path).map_err(|err| {
            format!(
                "{context}: failed to read markdown payload {}: {err}",
                path.display()
            )
        })?;

        self.guard_markdown_payload(&payload, context)
    }
}

impl ProviderAdapter for GhCliAdapter {
    fn issue_body(&self, repo: &str, issue: u64) -> Result<String, String> {
        let args = vec![
            "issue".to_string(),
            "view".to_string(),
            issue.to_string(),
            "--repo".to_string(),
            repo.to_string(),
            "--json".to_string(),
            "body".to_string(),
        ];
        let stdout = self.run(&args)?;
        let json = Self::parse_json(&stdout, "issue view")?;
        let body = json
            .get("body")
            .and_then(Value::as_str)
            .ok_or_else(|| "gh issue view JSON missing `body`".to_string())?;
        Ok(body.to_string())
    }

    fn issue_evidence(&self, repo: &str, issue: u64) -> Result<(String, String), String> {
        let args = vec![
            "issue".to_string(),
            "view".to_string(),
            issue.to_string(),
            "--repo".to_string(),
            repo.to_string(),
            "--json".to_string(),
            "body,comments".to_string(),
        ];
        let stdout = self.run(&args)?;
        let json = Self::parse_json(&stdout, "issue view (body+comments)")?;
        let body = json
            .get("body")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let comments = json
            .get("comments")
            .cloned()
            .unwrap_or(Value::Array(Vec::new()));
        let envelope = serde_json::json!({ "comments": comments });
        let comments_json = serde_json::to_string(&envelope)
            .map_err(|err| format!("failed to serialize issue evidence comments: {err}"))?;
        Ok((body, comments_json))
    }

    fn list_open_tracker_issues(&self, repo: &str, labels: &[String]) -> Result<Vec<u64>, String> {
        let mut args = vec![
            "issue".to_string(),
            "list".to_string(),
            "--repo".to_string(),
            repo.to_string(),
            "--state".to_string(),
            "open".to_string(),
            "--limit".to_string(),
            // gh defaults to 30; raise the ceiling so detection still sees a
            // bundle's tracker on a busy repo. A bundle should resolve to one
            // tracker, so the list is only ever a candidate set to scan.
            "200".to_string(),
            "--json".to_string(),
            "number".to_string(),
        ];
        for label in labels {
            let trimmed = label.trim();
            if !trimmed.is_empty() {
                args.push("--label".to_string());
                args.push(trimmed.to_string());
            }
        }
        let stdout = self.run(&args)?;
        let json = Self::parse_json(&stdout, "issue list")?;
        let items = json
            .as_array()
            .ok_or_else(|| "gh issue list JSON is not an array".to_string())?;
        Ok(items
            .iter()
            .filter_map(|item| item.get("number").and_then(Value::as_u64))
            .collect())
    }

    fn create_issue(
        &self,
        repo: &str,
        title: &str,
        body_file: &Path,
        labels: &[String],
    ) -> Result<(u64, String), String> {
        self.guard_markdown_file(body_file, "github issue create body write rejected")?;

        let mut args = vec![
            "issue".to_string(),
            "create".to_string(),
            "--repo".to_string(),
            repo.to_string(),
            "--title".to_string(),
            title.to_string(),
            "--body-file".to_string(),
            body_file.to_string_lossy().to_string(),
        ];

        for label in labels {
            let trimmed = label.trim();
            if !trimmed.is_empty() {
                args.push("--label".to_string());
                args.push(trimmed.to_string());
            }
        }

        let stdout = self.run(&args)?;
        let url = stdout.trim().to_string();
        let issue_number = issue_number_from_url(&url)
            .ok_or_else(|| format!("unable to parse issue number from gh output: {url}"))?;
        Ok((issue_number, url))
    }

    fn edit_issue_body(&self, repo: &str, issue: u64, body_file: &Path) -> Result<(), String> {
        self.guard_markdown_file(body_file, "github issue body update rejected")?;

        let args = vec![
            "issue".to_string(),
            "edit".to_string(),
            issue.to_string(),
            "--repo".to_string(),
            repo.to_string(),
            "--body-file".to_string(),
            body_file.to_string_lossy().to_string(),
        ];
        self.run(&args).map(|_| ())
    }

    fn comment_issue(&self, repo: &str, issue: u64, body_file: &Path) -> Result<String, String> {
        self.guard_markdown_file(body_file, "github issue comment write rejected")?;

        let args = vec![
            "issue".to_string(),
            "comment".to_string(),
            issue.to_string(),
            "--repo".to_string(),
            repo.to_string(),
            "--body-file".to_string(),
            body_file.to_string_lossy().to_string(),
        ];
        let stdout = self.run(&args)?;
        extract_issue_comment_url(&stdout).ok_or_else(|| {
            format!(
                "gh issue comment did not print a recognisable comment URL; got: {:?}",
                stdout.trim()
            )
        })
    }

    fn edit_issue_labels(
        &self,
        repo: &str,
        issue: u64,
        add_labels: &[String],
        remove_labels: &[String],
    ) -> Result<(), String> {
        let mut args = vec![
            "issue".to_string(),
            "edit".to_string(),
            issue.to_string(),
            "--repo".to_string(),
            repo.to_string(),
        ];

        let add_csv = add_labels
            .iter()
            .map(|label| label.trim())
            .filter(|label| !label.is_empty())
            .collect::<Vec<_>>()
            .join(",");
        if !add_csv.is_empty() {
            args.push("--add-label".to_string());
            args.push(add_csv);
        }

        let remove_csv = remove_labels
            .iter()
            .map(|label| label.trim())
            .filter(|label| !label.is_empty())
            .collect::<Vec<_>>()
            .join(",");
        if !remove_csv.is_empty() {
            args.push("--remove-label".to_string());
            args.push(remove_csv);
        }

        if args.len() == 5 {
            return Ok(());
        }

        self.run(&args).map(|_| ())
    }

    fn close_issue(
        &self,
        repo: &str,
        issue: u64,
        reason: CloseReason,
        close_comment: Option<&str>,
    ) -> Result<(), String> {
        let mut args = vec![
            "issue".to_string(),
            "close".to_string(),
            issue.to_string(),
            "--repo".to_string(),
            repo.to_string(),
            "--reason".to_string(),
            match reason {
                CloseReason::Completed => "completed",
                CloseReason::NotPlanned => "not planned",
            }
            .to_string(),
        ];

        if let Some(comment) = close_comment {
            let trimmed = comment.trim();
            if !trimmed.is_empty() {
                self.guard_markdown_payload(trimmed, "github issue close comment write rejected")?;
                args.push("--comment".to_string());
                args.push(trimmed.to_string());
            }
        }

        self.run(&args).map(|_| ())
    }

    fn pr_is_merged(&self, repo: &str, pr: u64) -> Result<bool, String> {
        let args = vec![
            "pr".to_string(),
            "view".to_string(),
            pr.to_string(),
            "--repo".to_string(),
            repo.to_string(),
            "--json".to_string(),
            "state,mergedAt".to_string(),
        ];
        let stdout = self.run(&args)?;
        let json = Self::parse_json(&stdout, "pr view")?;

        let merged_at_present = !json.get("mergedAt").is_some_and(Value::is_null);
        let merged_state = json
            .get("state")
            .and_then(Value::as_str)
            .is_some_and(|state| state.eq_ignore_ascii_case("merged"));

        Ok(merged_at_present || merged_state)
    }

    fn pr_merge_summary(&self, repo: &str, pr: u64) -> Result<PrMergeSummary, String> {
        let args = vec![
            "pr".to_string(),
            "view".to_string(),
            pr.to_string(),
            "--repo".to_string(),
            repo.to_string(),
            "--json".to_string(),
            "state,mergeCommit,statusCheckRollup".to_string(),
        ];
        let stdout = self.run(&args)?;
        let json = Self::parse_json(&stdout, "pr view (merge summary)")?;

        let state = json
            .get("state")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let merged = state.eq_ignore_ascii_case("merged");
        let merge_sha = json
            .get("mergeCommit")
            .and_then(|value| value.get("oid"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .filter(|sha| !sha.is_empty());
        let rollup_value = json.get("statusCheckRollup");
        let checks = rollup_value
            .and_then(rollup_status)
            .filter(|status| !status.is_empty());
        let (required_state, required_count, non_required_failures) =
            self.pr_required_summary(repo, pr, rollup_value);
        Ok(PrMergeSummary {
            state,
            merged,
            merge_sha,
            checks,
            required_state,
            required_count,
            non_required_failures,
        })
    }

    fn pr_comments(&self, repo: &str, pr: u64) -> Result<Vec<Value>, String> {
        // Use `--paginate` so PRs with > 100 comments still return the full
        // list. Endpoint mirrors REST `/repos/{owner}/{repo}/issues/{n}/comments`
        // which is the issue-style stream used for review-evidence comments.
        let endpoint = format!("repos/{repo}/issues/{pr}/comments");
        let args = vec!["api".to_string(), "--paginate".to_string(), endpoint];
        let stdout = self.run(&args)?;

        // `gh api --paginate` concatenates JSON arrays back-to-back without
        // a wrapping comma, so we parse each top-level array on its own
        // line first; falling back to a single-array parse keeps the simple
        // case fast.
        let trimmed = stdout.trim();
        if trimmed.is_empty() {
            return Ok(Vec::new());
        }
        if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
            return Ok(match value {
                Value::Array(items) => items,
                other => vec![other],
            });
        }

        // Fall back to splitting concatenated arrays.
        let mut combined: Vec<Value> = Vec::new();
        for chunk in trimmed.split("][") {
            let chunk = chunk.trim();
            if chunk.is_empty() {
                continue;
            }
            let normalized = if !chunk.starts_with('[') {
                format!("[{chunk}")
            } else {
                chunk.to_string()
            };
            let normalized = if !normalized.ends_with(']') {
                format!("{normalized}]")
            } else {
                normalized
            };
            let value: Value = serde_json::from_str(&normalized)
                .map_err(|err| format!("failed to parse pr comments page: {err}"))?;
            if let Value::Array(items) = value {
                combined.extend(items);
            } else {
                combined.push(value);
            }
        }
        Ok(combined)
    }
}

/// Pick the first line in `stdout` that looks like a GitHub issue/PR
/// comment URL (`https://github.com/<owner>/<repo>/(issues|pull)/<n>#issuecomment-<m>`).
/// Returns the trimmed URL or `None` if no line matches.
///
/// Used by `GhCliAdapter::comment_issue` so banner / warning lines from `gh`
/// do not corrupt the returned URL. Address PR-454 specialist review finding
/// (security/0.80) — `stdout.lines().last()` returned an empty string when
/// `gh` emitted a trailing newline.
fn extract_issue_comment_url(stdout: &str) -> Option<String> {
    for line in stdout.lines() {
        let trimmed = line.trim();
        if is_issue_comment_url(trimmed) {
            return Some(trimmed.to_string());
        }
    }
    None
}

fn is_issue_comment_url(value: &str) -> bool {
    let Some(rest) = value.strip_prefix("https://github.com/") else {
        return false;
    };
    let Some((base, suffix)) = rest.split_once("#issuecomment-") else {
        return false;
    };
    if !suffix.chars().all(|ch| ch.is_ascii_digit()) || suffix.is_empty() {
        return false;
    }
    let mut parts = base.splitn(4, '/');
    let owner = parts.next().unwrap_or("");
    let repo = parts.next().unwrap_or("");
    let kind = parts.next().unwrap_or("");
    let number = parts.next().unwrap_or("");
    !owner.is_empty()
        && !repo.is_empty()
        && (kind == "issues" || kind == "pull")
        && number.chars().all(|ch| ch.is_ascii_digit())
        && !number.is_empty()
}

fn rollup_status(rollup: &Value) -> Option<String> {
    let extract_state = |item: &Value| -> Option<String> {
        item.get("state")
            .and_then(Value::as_str)
            .map(|status| status.to_ascii_lowercase())
            .or_else(|| {
                item.get("conclusion")
                    .and_then(Value::as_str)
                    .map(|status| status.to_ascii_lowercase())
            })
    };

    match rollup {
        Value::Array(items) if items.is_empty() => Some("none".to_string()),
        Value::Array(items) => {
            let mut has_failure = false;
            let mut has_pending = false;
            let mut has_success = false;
            for item in items {
                match extract_state(item).as_deref() {
                    Some("success" | "neutral" | "skipped") => has_success = true,
                    Some("failure" | "cancelled" | "timed_out" | "action_required" | "error") => {
                        has_failure = true
                    }
                    Some("pending" | "in_progress" | "queued" | "expected") => has_pending = true,
                    _ => {}
                }
            }
            if has_failure {
                Some("failure".to_string())
            } else if has_pending {
                Some("pending".to_string())
            } else if has_success {
                Some("success".to_string())
            } else {
                Some("unknown".to_string())
            }
        }
        Value::Object(_) => extract_state(rollup),
        _ => None,
    }
}

fn issue_number_from_url(url: &str) -> Option<u64> {
    let trimmed = url.trim().trim_end_matches('/');
    let tail = trimmed.rsplit('/').next()?;
    tail.parse::<u64>().ok()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{
        extract_issue_comment_url, is_issue_comment_url, issue_number_from_url, rollup_status,
    };
    use crate::commands::plan::CloseReason;
    use crate::github::{GhCliAdapter, ProviderAdapter};
    use nils_test_support::{EnvGuard, GlobalStateLock, StubBinDir, prepend_path};
    use tempfile::TempDir;

    fn gh_stub_script() -> &'static str {
        r#"#!/usr/bin/env bash
set -euo pipefail

if [[ -n "${GH_STUB_LOG:-}" ]]; then
  printf '%s\n' "$*" >> "$GH_STUB_LOG"
fi

if [[ -n "${GH_STUB_FORCE_FAIL:-}" ]]; then
  echo "${GH_STUB_FORCE_FAIL}" >&2
  exit 1
fi

cmd="${1:-}"
sub="${2:-}"
case "$cmd $sub" in
  "issue view")
    if [[ -n "${GH_STUB_ISSUE_VIEW_JSON:-}" ]]; then
      printf '%s\n' "$GH_STUB_ISSUE_VIEW_JSON"
    else
      printf '%s\n' '{"body":"from-stub-body"}'
    fi
    ;;
  "issue create")
    if [[ -n "${GH_STUB_ISSUE_CREATE_URL:-}" ]]; then
      printf '%s\n' "$GH_STUB_ISSUE_CREATE_URL"
    else
      printf '%s\n' 'https://github.com/sympoies/nils-cli/issues/217'
    fi
    ;;
  "issue edit")
    ;;
  "issue comment")
    if [[ -n "${GH_STUB_ISSUE_COMMENT_URL:-}" ]]; then
      printf '%s\n' "$GH_STUB_ISSUE_COMMENT_URL"
    else
      printf '%s\n' 'https://github.com/sympoies/nils-cli/issues/217#issuecomment-1'
    fi
    ;;
  "issue close")
    ;;
  "pr view")
    if [[ -n "${GH_STUB_PR_VIEW_JSON:-}" ]]; then
      printf '%s\n' "$GH_STUB_PR_VIEW_JSON"
    else
      printf '%s\n' '{"state":"MERGED","mergedAt":null}'
    fi
    ;;
  *)
    echo "unsupported gh call: $*" >&2
    exit 1
    ;;
esac
"#
    }

    #[test]
    fn issue_number_from_url_extracts_tail_numeric_segment() {
        assert_eq!(
            issue_number_from_url("https://github.com/sympoies/nils-cli/issues/217"),
            Some(217)
        );
        assert_eq!(
            issue_number_from_url("https://github.com/sympoies/nils-cli/pull/221"),
            Some(221)
        );
    }

    #[test]
    fn gh_adapter_live_methods_work_with_stubbed_gh() {
        let lock = GlobalStateLock::new();
        let stubs = StubBinDir::new();
        stubs.write_exe("gh", gh_stub_script());
        let _path = prepend_path(&lock, stubs.path());

        let tmp = TempDir::new().expect("tempdir");
        let body_file = tmp.path().join("body.md");
        fs::write(&body_file, "normal markdown body").expect("write body");

        let adapter = GhCliAdapter::new(false);
        let body = adapter
            .issue_body("sympoies/nils-cli", 217)
            .expect("issue body");
        assert_eq!(body, "from-stub-body");

        let (issue_no, issue_url) = adapter
            .create_issue(
                "sympoies/nils-cli",
                "title",
                &body_file,
                &["triage".to_string(), " ".to_string(), "plan".to_string()],
            )
            .expect("create issue");
        assert_eq!(issue_no, 217);
        assert_eq!(issue_url, "https://github.com/sympoies/nils-cli/issues/217");

        adapter
            .edit_issue_body("sympoies/nils-cli", 217, &body_file)
            .expect("edit body");
        adapter
            .comment_issue("sympoies/nils-cli", 217, &body_file)
            .expect("comment");
        adapter
            .edit_issue_labels(
                "sympoies/nils-cli",
                217,
                &["needs-review".to_string()],
                &["blocked".to_string()],
            )
            .expect("edit labels");
        adapter
            .close_issue(
                "sympoies/nils-cli",
                217,
                CloseReason::Completed,
                Some("closing comment"),
            )
            .expect("close issue");

        assert!(
            adapter
                .pr_is_merged("sympoies/nils-cli", 221)
                .expect("merged check")
        );
    }

    #[test]
    fn gh_adapter_guard_rejects_escaped_payload_without_force() {
        let lock = GlobalStateLock::new();
        let stubs = StubBinDir::new();
        stubs.write_exe("gh", gh_stub_script());
        let _path = prepend_path(&lock, stubs.path());

        let tmp = TempDir::new().expect("tempdir");
        let escaped_file = tmp.path().join("escaped.md");
        fs::write(&escaped_file, "line one\\nline two").expect("write escaped payload");

        let strict = GhCliAdapter::new(false);
        let strict_err = strict
            .create_issue("sympoies/nils-cli", "title", &escaped_file, &[])
            .expect_err("escaped payload should fail");
        assert!(strict_err.contains("write rejected"), "{strict_err}");

        let force = GhCliAdapter::new(true);
        let forced = force
            .create_issue("sympoies/nils-cli", "title", &escaped_file, &[])
            .expect("force mode bypasses markdown guard");
        assert_eq!(forced.0, 217);
    }

    #[test]
    fn gh_adapter_pr_merge_logic_and_error_paths_are_covered() {
        let lock = GlobalStateLock::new();
        let stubs = StubBinDir::new();
        stubs.write_exe("gh", gh_stub_script());
        let _path = prepend_path(&lock, stubs.path());

        let adapter = GhCliAdapter::new(false);
        let _open_state = EnvGuard::set(
            &lock,
            "GH_STUB_PR_VIEW_JSON",
            r#"{"state":"OPEN","mergedAt":null}"#,
        );
        assert!(
            !adapter
                .pr_is_merged("sympoies/nils-cli", 221)
                .expect("open pr")
        );
        drop(_open_state);

        let _merged_at = EnvGuard::set(
            &lock,
            "GH_STUB_PR_VIEW_JSON",
            r#"{"state":"OPEN","mergedAt":"2026-02-25T00:00:00Z"}"#,
        );
        assert!(
            adapter
                .pr_is_merged("sympoies/nils-cli", 221)
                .expect("mergedAt present")
        );
        drop(_merged_at);

        let _bad_json = EnvGuard::set(&lock, "GH_STUB_ISSUE_VIEW_JSON", "not-json");
        let parse_err = adapter
            .issue_body("sympoies/nils-cli", 217)
            .expect_err("invalid json should fail");
        assert!(parse_err.contains("failed to parse gh JSON"), "{parse_err}");
        drop(_bad_json);

        let _missing_body = EnvGuard::set(&lock, "GH_STUB_ISSUE_VIEW_JSON", r#"{"id":217}"#);
        let missing_body = adapter
            .issue_body("sympoies/nils-cli", 217)
            .expect_err("missing body should fail");
        assert!(
            missing_body.contains("JSON missing `body`"),
            "{missing_body}"
        );
        drop(_missing_body);

        let _force_fail = EnvGuard::set(&lock, "GH_STUB_FORCE_FAIL", "forced failure");
        let run_err = adapter
            .pr_is_merged("sympoies/nils-cli", 221)
            .expect_err("gh failure should surface");
        assert!(run_err.contains("gh pr view"), "{run_err}");
    }

    // Sprint 3 PR-454 specialist review follow-ups (testing + security).

    #[test]
    fn extract_issue_comment_url_picks_recognisable_line_and_rejects_noise() {
        let url = "https://github.com/sympoies/nils-cli/issues/448#issuecomment-12345";
        assert_eq!(extract_issue_comment_url(url).as_deref(), Some(url));
        let with_banner = format!("warning: deprecated\n{url}\n");
        assert_eq!(
            extract_issue_comment_url(&with_banner).as_deref(),
            Some(url)
        );
        let pr_url = "https://github.com/sympoies/nils-cli/pull/454#issuecomment-99";
        assert_eq!(extract_issue_comment_url(pr_url).as_deref(), Some(pr_url));

        assert_eq!(extract_issue_comment_url(""), None);
        assert_eq!(extract_issue_comment_url("\n\n"), None);
        assert_eq!(
            extract_issue_comment_url("https://example.com/owner/repo/issues/1#issuecomment-1"),
            None
        );
        assert_eq!(
            extract_issue_comment_url("https://github.com/owner/repo/issues/1#comment-1"),
            None
        );
        assert!(!is_issue_comment_url("not a url"));
    }

    #[test]
    fn rollup_status_normalizes_array_and_object_shapes() {
        use serde_json::json;
        assert_eq!(rollup_status(&json!([])), Some("none".to_string()));
        assert_eq!(
            rollup_status(&json!([{"state": "SUCCESS"}, {"state": "success"}])),
            Some("success".to_string())
        );
        assert_eq!(
            rollup_status(&json!([{"state": "success"}, {"state": "failure"}])),
            Some("failure".to_string())
        );
        assert_eq!(
            rollup_status(&json!([{"state": "success"}, {"state": "pending"}])),
            Some("pending".to_string())
        );
        assert_eq!(
            rollup_status(&json!({"state": "success"})),
            Some("success".to_string())
        );
        assert_eq!(
            rollup_status(&json!({"conclusion": "failure"})),
            Some("failure".to_string())
        );
        assert_eq!(rollup_status(&json!(null)), None);
    }

    fn gh_pr_merge_summary_stub_script() -> &'static str {
        r#"#!/usr/bin/env bash
set -euo pipefail
cmd="${1:-}"; sub="${2:-}"
case "$cmd $sub" in
  "pr view")
    if [[ -n "${GH_STUB_PR_VIEW_FULL_JSON:-}" ]]; then
      printf '%s\n' "$GH_STUB_PR_VIEW_FULL_JSON"
    else
      printf '%s\n' '{"state":"MERGED","mergeCommit":{"oid":"abcd"},"statusCheckRollup":{"state":"success"}}'
    fi
    ;;
  *)
    echo "unsupported gh call: $*" >&2
    exit 1
    ;;
esac
"#
    }

    #[test]
    fn gh_adapter_pr_merge_summary_parses_state_merge_sha_and_checks() {
        let lock = GlobalStateLock::new();
        let stubs = StubBinDir::new();
        stubs.write_exe("gh", gh_pr_merge_summary_stub_script());
        let _path = prepend_path(&lock, stubs.path());

        let adapter = GhCliAdapter::new(false);
        let summary = adapter
            .pr_merge_summary("sympoies/nils-cli", 454)
            .expect("merge summary");
        assert_eq!(summary.state, "MERGED");
        assert!(summary.merged);
        assert_eq!(summary.merge_sha.as_deref(), Some("abcd"));
        assert_eq!(summary.checks.as_deref(), Some("success"));

        let _open = EnvGuard::set(
            &lock,
            "GH_STUB_PR_VIEW_FULL_JSON",
            r#"{"state":"OPEN","mergeCommit":null,"statusCheckRollup":{"state":"pending"}}"#,
        );
        let summary_open = adapter
            .pr_merge_summary("sympoies/nils-cli", 454)
            .expect("open pr summary");
        assert!(!summary_open.merged);
        assert!(summary_open.merge_sha.is_none());
        assert_eq!(summary_open.checks.as_deref(), Some("pending"));
        drop(_open);

        let _empty = EnvGuard::set(
            &lock,
            "GH_STUB_PR_VIEW_FULL_JSON",
            r#"{"state":"MERGED","mergeCommit":{"oid":""},"statusCheckRollup":[]}"#,
        );
        let summary_empty = adapter
            .pr_merge_summary("sympoies/nils-cli", 454)
            .expect("empty rollup summary");
        assert!(summary_empty.merged);
        assert!(
            summary_empty.merge_sha.is_none(),
            "empty oid should be filtered out"
        );
        assert_eq!(summary_empty.checks.as_deref(), Some("none"));
    }

    // Sprint 1 of the closeout required-check rendering plan
    // (sympoies/nils-cli#561): exercise pr_required_summary's
    // "no required checks reported" stderr recognition deterministically
    // through an injected `GhRunner`, plus the success + failure paths.

    use super::{GhRunOutput, default_gh_runner};

    fn ok_runner(stdout: &str) -> super::GhRunner {
        // Each branch returns a fresh static fn so we can use them as
        // `GhRunner` (fn pointer) values without capturing locals.
        // The injected runner is keyed by the `--required` flag; a smarter
        // dispatcher per-test would be overkill for unit coverage.
        match stdout {
            "empty_array" => |_| {
                Ok(GhRunOutput {
                    success: true,
                    exit_code: Some(0),
                    stdout: "[]".to_string(),
                    stderr: String::new(),
                })
            },
            "one_required_pass" => |_| {
                Ok(GhRunOutput {
                    success: true,
                    exit_code: Some(0),
                    stdout: r#"[{"name":"ci","state":"SUCCESS"}]"#.to_string(),
                    stderr: String::new(),
                })
            },
            _ => |_| {
                Ok(GhRunOutput {
                    success: true,
                    exit_code: Some(0),
                    stdout: String::new(),
                    stderr: String::new(),
                })
            },
        }
    }

    fn stderr_runner(message: &'static str) -> super::GhRunner {
        // Map the canonical stderr strings to static-only branches the
        // `GhRunner` (fn pointer) type permits.
        match message {
            "no_required" => |_| {
                Ok(GhRunOutput {
                    success: false,
                    exit_code: Some(1),
                    stdout: String::new(),
                    stderr: "no required checks reported on the 'feature/x' branch\n".to_string(),
                })
            },
            "no_checks" => |_| {
                Ok(GhRunOutput {
                    success: false,
                    exit_code: Some(1),
                    stdout: String::new(),
                    stderr: "no checks reported on the 'feature/x' branch\n".to_string(),
                })
            },
            "auth" => |_| {
                Ok(GhRunOutput {
                    success: false,
                    exit_code: Some(4),
                    stdout: String::new(),
                    stderr: "gh: GraphQL error: Could not resolve to a PR\n".to_string(),
                })
            },
            _ => |_| Err("simulated spawn failure".to_string()),
        }
    }

    #[test]
    fn pr_required_summary_recognises_no_required_checks_reported_stderr() {
        // The canonical exit-1 + "no required checks reported on the
        // '<branch>' branch" stderr message must be classified as the
        // zero-required success case, not as the catch-all `(None, None, [])`.
        let adapter = GhCliAdapter::with_runner(false, stderr_runner("no_required"));
        let (state, count, non_required) =
            adapter.pr_required_summary("sympoies/nils-cli", 553, None);
        assert_eq!(state.as_deref(), Some("success"));
        assert_eq!(count, Some(0));
        assert!(non_required.is_empty());
    }

    #[test]
    fn pr_required_summary_recognises_no_checks_reported_stderr() {
        // A branch with no checks at all makes `gh pr checks --required` exit 1
        // with "no checks reported on the '<branch>' branch" (no "required"
        // word). This must also classify as the zero-required success case so
        // the closeout `Required` column renders `none required`, not the
        // catch-all `unknown`. Regression for graysurf/plan-tracking-testbed#17
        // (the sibling case sympoies/nils-cli#557 missed).
        let adapter = GhCliAdapter::with_runner(false, stderr_runner("no_checks"));
        let (state, count, non_required) =
            adapter.pr_required_summary("sympoies/nils-cli", 8, None);
        assert_eq!(state.as_deref(), Some("success"));
        assert_eq!(count, Some(0));
        assert!(non_required.is_empty());
    }

    #[test]
    fn pr_required_summary_returns_zero_required_on_empty_array_stdout() {
        let adapter = GhCliAdapter::with_runner(false, ok_runner("empty_array"));
        let (state, count, non_required) =
            adapter.pr_required_summary("sympoies/nils-cli", 553, None);
        assert_eq!(state.as_deref(), Some("success"));
        assert_eq!(count, Some(0));
        assert!(non_required.is_empty());
    }

    #[test]
    fn pr_required_summary_returns_pass_with_count_for_one_required_check() {
        let adapter = GhCliAdapter::with_runner(false, ok_runner("one_required_pass"));
        let (state, count, non_required) =
            adapter.pr_required_summary("sympoies/nils-cli", 553, None);
        assert_eq!(state.as_deref(), Some("success"));
        assert_eq!(count, Some(1));
        assert!(non_required.is_empty());
    }

    #[test]
    fn pr_required_summary_returns_none_on_unrecognised_failure_stderr() {
        // Any non-zero exit whose stderr does NOT include
        // "no required checks reported" stays in the catch-all
        // `(None, None, [])` branch so the renderer can emit `unknown`
        // and a future regression remains visible.
        let adapter = GhCliAdapter::with_runner(false, stderr_runner("auth"));
        let (state, count, non_required) =
            adapter.pr_required_summary("sympoies/nils-cli", 553, None);
        assert!(state.is_none());
        assert!(count.is_none());
        assert!(non_required.is_empty());
    }

    #[test]
    fn pr_required_summary_returns_none_on_runner_spawn_failure() {
        let adapter = GhCliAdapter::with_runner(false, stderr_runner("spawn"));
        let (state, count, non_required) =
            adapter.pr_required_summary("sympoies/nils-cli", 553, None);
        assert!(state.is_none());
        assert!(count.is_none());
        assert!(non_required.is_empty());
    }

    #[test]
    fn default_gh_runner_is_callable_via_function_pointer() {
        // Smoke that the production runner satisfies the `GhRunner` type
        // (fn pointer) bound without indirection at the call site.
        let _: super::GhRunner = default_gh_runner;
    }
}
