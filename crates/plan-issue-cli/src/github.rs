use std::fs;
use std::path::Path;

use nils_common::git as common_git;
use nils_common::markdown;
use nils_common::process as common_process;
use serde_json::Value;

use crate::commands::plan::CloseReason;

pub trait GitHubAdapter {
    fn issue_body(&self, repo: &str, issue: u64) -> Result<String, String>;

    /// Fetch the issue body plus comments JSON, suitable for `audit_record`
    /// fixture parsing. Returns `(body, comments_json)` where
    /// `comments_json` is the raw JSON string from
    /// `gh issue view --json comments`.
    fn issue_evidence(&self, repo: &str, issue: u64) -> Result<(String, String), String>;

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
}

#[derive(Debug, Clone, Copy, Default)]
pub struct GhCliAdapter {
    force: bool,
}

impl GhCliAdapter {
    pub const fn new(force: bool) -> Self {
        Self { force }
    }

    fn run(args: &[String]) -> Result<String, String> {
        let arg_refs = args.iter().map(String::as_str).collect::<Vec<_>>();
        let output = common_process::run_output("gh", &arg_refs)
            .map(|output| output.into_std_output())
            .map_err(|err| format!("failed to execute gh: {err}"))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let detail = if stderr.is_empty() { stdout } else { stderr };
            return Err(format!("gh {} failed: {detail}", args.join(" ")));
        }

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
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

impl GitHubAdapter for GhCliAdapter {
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
        let stdout = Self::run(&args)?;
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
        let stdout = Self::run(&args)?;
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

        let stdout = Self::run(&args)?;
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
        Self::run(&args).map(|_| ())
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
        let stdout = Self::run(&args)?;
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

        Self::run(&args).map(|_| ())
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

        Self::run(&args).map(|_| ())
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
        let stdout = Self::run(&args)?;
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
        let stdout = Self::run(&args)?;
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
        let checks = json
            .get("statusCheckRollup")
            .and_then(rollup_status)
            .filter(|status| !status.is_empty());
        Ok(PrMergeSummary {
            state,
            merged,
            merge_sha,
            checks,
        })
    }

    fn pr_comments(&self, repo: &str, pr: u64) -> Result<Vec<Value>, String> {
        // Use `--paginate` so PRs with > 100 comments still return the full
        // list. Endpoint mirrors REST `/repos/{owner}/{repo}/issues/{n}/comments`
        // which is the issue-style stream used for review-evidence comments.
        let endpoint = format!("repos/{repo}/issues/{pr}/comments");
        let args = vec!["api".to_string(), "--paginate".to_string(), endpoint];
        let stdout = Self::run(&args)?;

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

pub fn resolve_repo(repo_override: Option<&str>) -> Result<String, String> {
    if let Some(repo) = repo_override {
        return normalize_repo_slug(repo).ok_or_else(|| format!("invalid --repo value: {repo}"));
    }

    let output = common_git::run_output(&["remote", "get-url", "origin"])
        .map_err(|err| format!("failed to run `git remote get-url origin`: {err}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(format!(
            "failed to resolve repository from git remote: {}",
            if stderr.is_empty() {
                "unknown error"
            } else {
                &stderr
            }
        ));
    }

    let remote = String::from_utf8_lossy(&output.stdout).trim().to_string();
    normalize_repo_slug(&remote).ok_or_else(|| {
        format!(
            "unable to derive owner/repo from origin remote `{remote}`; pass --repo <owner/repo>"
        )
    })
}

fn normalize_repo_slug(raw: &str) -> Option<String> {
    let trimmed = raw.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return None;
    }

    let candidate = trimmed
        .strip_prefix("git@github.com:")
        .or_else(|| trimmed.strip_prefix("https://github.com/"))
        .or_else(|| trimmed.strip_prefix("http://github.com/"))
        .or_else(|| trimmed.strip_prefix("ssh://git@github.com/"));

    if let Some(candidate) = candidate {
        let normalized = candidate.trim_end_matches(".git").trim_end_matches('/');
        if is_owner_repo(normalized) {
            return Some(normalized.to_string());
        }
    }

    if is_owner_repo(trimmed) {
        Some(trimmed.to_string())
    } else {
        None
    }
}

fn is_owner_repo(value: &str) -> bool {
    if value.contains(':') || value.contains("://") || value.ends_with(".git") {
        return false;
    }

    let mut parts = value.split('/');
    let owner = parts.next().unwrap_or_default().trim();
    let repo = parts.next().unwrap_or_default().trim();
    parts.next().is_none() && !owner.is_empty() && !repo.is_empty()
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
        extract_issue_comment_url, is_issue_comment_url, issue_number_from_url,
        normalize_repo_slug, rollup_status,
    };
    use crate::commands::plan::CloseReason;
    use crate::github::{GhCliAdapter, GitHubAdapter, resolve_repo};
    use nils_test_support::git::{InitRepoOptions, git, init_repo_with};
    use nils_test_support::{CwdGuard, EnvGuard, GlobalStateLock, StubBinDir, prepend_path};
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
    fn normalize_repo_slug_accepts_common_remote_forms() {
        let samples = [
            ("sympoies/nils-cli", "sympoies/nils-cli"),
            ("git@github.com:sympoies/nils-cli.git", "sympoies/nils-cli"),
            (
                "https://github.com/sympoies/nils-cli.git",
                "sympoies/nils-cli",
            ),
            (
                "ssh://git@github.com/sympoies/nils-cli.git",
                "sympoies/nils-cli",
            ),
        ];

        for (raw, expected) in samples {
            assert_eq!(normalize_repo_slug(raw).as_deref(), Some(expected));
        }
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

    #[test]
    fn resolve_repo_supports_override_and_origin_remote_detection() {
        assert_eq!(
            resolve_repo(Some("sympoies/nils-cli")).expect("override"),
            "sympoies/nils-cli"
        );
        assert!(resolve_repo(Some("https://example.com/repo")).is_err());

        let lock = GlobalStateLock::new();
        let repo = init_repo_with(InitRepoOptions::new().with_branch("main"));
        git(
            repo.path(),
            &[
                "remote",
                "add",
                "origin",
                "git@github.com:sympoies/nils-cli.git",
            ],
        );
        let _cwd = CwdGuard::set(&lock, repo.path()).expect("set cwd");
        assert_eq!(
            resolve_repo(None).expect("resolve from origin"),
            "sympoies/nils-cli"
        );
    }

    #[test]
    fn resolve_repo_reports_missing_or_unparseable_origin() {
        let lock = GlobalStateLock::new();

        let missing = init_repo_with(InitRepoOptions::new().with_branch("main"));
        let _cwd_missing = CwdGuard::set(&lock, missing.path()).expect("set cwd missing");
        let err_missing = resolve_repo(None).expect_err("missing origin should fail");
        assert!(
            err_missing.contains("failed to resolve repository from git remote"),
            "{err_missing}"
        );
        drop(_cwd_missing);

        let unparseable = init_repo_with(InitRepoOptions::new().with_branch("main"));
        git(
            unparseable.path(),
            &["remote", "add", "origin", "ssh://example.com/project.git"],
        );
        let _cwd_unparseable = CwdGuard::set(&lock, unparseable.path()).expect("set cwd parse");
        let err_unparseable = resolve_repo(None).expect_err("unparseable origin should fail");
        assert!(
            err_unparseable.contains("unable to derive owner/repo"),
            "{err_unparseable}"
        );
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
}
