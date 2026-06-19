//! `ProviderAdapter` that routes every provider op through `forge-cli`'s
//! provider-neutral surface.
//!
//! This is the single live adapter for plan-issue: after the
//! plan-issue → forge-cli consolidation
//! (`docs/plans/2026-06-19-plan-issue-forge-cli-consolidation`) every provider
//! — GitHub, GitLab, and the in-process Local backend — routes through this
//! adapter, retiring the in-crate `gh` client. `forge-cli` becomes the single
//! provider gateway and identity chokepoint.
//!
//! Subprocess details:
//!
//! - The adapter shells out to `forge-cli` (overridable via `FORGE_CLI_BIN`).
//! - Every call passes `--format json --provider <provider> --repo <slug>`
//!   (`github`, `gitlab`, or `local` for the in-process file-backed backend)
//!   so the target is unambiguous, even when the cwd's git remote points
//!   elsewhere.
//! - The v1 envelope (`{ok, schema_version, data}` for success or
//!   `{ok:false, error:{code,message}}`) is parsed into a typed error message
//!   carrying both the forge-cli error code and the argv that failed.
//!
//! Comment-stream shape: `forge-cli issue view --with-comments` returns
//! comments as `[{url, author, created_at, body}, ...]`. `issue_evidence`
//! re-wraps them as `{"comments": [...]}` so the existing
//! [`crate::lifecycle_record::audit_record`] parser (which reads `body`,
//! `url`, `created_at`) accepts them unchanged.

use std::path::Path;

use nils_common::process as common_process;
use serde_json::Value;
use serde_json::json;

use crate::adapter::{PrMergeSummary, ProviderAdapter};
use crate::commands::plan::CloseReason;

const TRACKER_ISSUE_SCAN_LIMIT: &str = "200";

/// Runner abstraction so unit tests can inject scripted forge-cli responses.
pub trait ForgeCliRunner {
    fn run(&self, args: &[&str]) -> Result<String, String>;
}

/// Default runner: spawns `forge-cli` (or `FORGE_CLI_BIN`) and returns its
/// stdout. Non-zero exits surface as `Err`.
#[derive(Debug, Default, Clone, Copy)]
pub struct ProcessForgeCliRunner;

impl ForgeCliRunner for ProcessForgeCliRunner {
    fn run(&self, args: &[&str]) -> Result<String, String> {
        let bin = std::env::var("FORGE_CLI_BIN").unwrap_or_else(|_| "forge-cli".to_string());

        let output = common_process::run_output(&bin, args)
            .map(|out| out.into_std_output())
            .map_err(|err| format!("failed to execute {bin}: {err}"))?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        if !output.status.success() {
            let detail = if !stderr.trim().is_empty() {
                stderr.trim().to_string()
            } else {
                stdout.trim().to_string()
            };
            return Err(format!("{bin} {} failed: {detail}", args.join(" ")));
        }
        Ok(stdout)
    }
}

/// Forge-routed `ProviderAdapter`. Carries the `--provider` string it emits so
/// the one adapter serves GitHub, GitLab, and the in-process `Provider::Local`
/// file-backed backend (`forge-cli --provider github|gitlab|local`).
pub struct ForgeCliAdapter {
    /// `--provider` value forwarded to every forge-cli call (`github`,
    /// `gitlab`, or `local`). Local rides the same forge-cli rail; the store
    /// root is read by forge-cli from `FORGE_CLI_LOCAL_STORE`.
    provider: &'static str,
    /// Force flag. Trait methods do not consult it because forge-cli's own
    /// markdown / local-path validation gates already enforce the same policy;
    /// this field is retained so the adapter can grow a `--force` pass-through
    /// later without an API change.
    #[allow(dead_code)]
    force: bool,
    runner: Box<dyn ForgeCliRunner + Send + Sync>,
}

impl ForgeCliAdapter {
    /// GitLab-backed adapter (emits `--provider gitlab`).
    pub fn new(force: bool) -> Self {
        Self::with_provider("gitlab", force, Box::new(ProcessForgeCliRunner))
    }

    /// GitHub-backed adapter (emits `--provider github`). Selected for GitHub
    /// repos after the plan-issue → forge-cli consolidation retired the
    /// in-crate `gh` client; identity is the inherited ambient token, exactly
    /// as the prior `GhCliAdapter` behaved.
    pub fn new_github(force: bool) -> Self {
        Self::with_provider("github", force, Box::new(ProcessForgeCliRunner))
    }

    /// Local-backend adapter (emits `--provider local`). forge-cli reads the
    /// file store root from `FORGE_CLI_LOCAL_STORE`; the e2e driver sets it.
    pub fn new_local(force: bool) -> Self {
        Self::with_provider("local", force, Box::new(ProcessForgeCliRunner))
    }

    fn with_provider(
        provider: &'static str,
        force: bool,
        runner: Box<dyn ForgeCliRunner + Send + Sync>,
    ) -> Self {
        Self {
            provider,
            force,
            runner,
        }
    }

    /// Test-only constructor that swaps in a scripted runner (GitLab provider).
    #[cfg(test)]
    pub fn with_runner(force: bool, runner: Box<dyn ForgeCliRunner + Send + Sync>) -> Self {
        Self::with_provider("gitlab", force, runner)
    }

    /// Test-only constructor for the local provider with a scripted runner.
    #[cfg(test)]
    pub fn with_runner_local(force: bool, runner: Box<dyn ForgeCliRunner + Send + Sync>) -> Self {
        Self::with_provider("local", force, runner)
    }

    /// Test-only constructor for the GitHub provider with a scripted runner.
    /// Exercises the GitHub-specific argv branches (`--reason` on close,
    /// `--required-only` on the merge-gate `pr checks` call).
    #[cfg(test)]
    pub fn with_runner_github(force: bool, runner: Box<dyn ForgeCliRunner + Send + Sync>) -> Self {
        Self::with_provider("github", force, runner)
    }

    /// Run forge-cli with the given args, parse the v1 envelope, and return
    /// the `data` field. Surfaces forge-cli error envelopes verbatim.
    fn run_envelope(&self, args: &[&str]) -> Result<Value, String> {
        let stdout = self.runner.run(args)?;
        let trimmed = stdout.trim();
        if trimmed.is_empty() {
            return Err(format!(
                "forge-cli {} produced empty stdout",
                args.join(" ")
            ));
        }
        let value: Value = serde_json::from_str(trimmed)
            .map_err(|err| format!("forge-cli {} output is not JSON: {err}", args.join(" ")))?;
        if value.get("ok") == Some(&Value::Bool(false)) {
            let code = value
                .pointer("/error/code")
                .and_then(Value::as_str)
                .unwrap_or("?");
            let message = value
                .pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or("?");
            return Err(format!(
                "forge-cli {} failed: {code}: {message}",
                args.join(" ")
            ));
        }
        value
            .get("data")
            .cloned()
            .ok_or_else(|| format!("forge-cli {} envelope missing `data`", args.join(" ")))
    }

    /// Common argv prefix: `--format json --provider <provider> --repo <slug>`.
    fn base_args<'a>(&self, repo: &'a str) -> Vec<&'a str> {
        vec![
            "--format",
            "json",
            "--provider",
            self.provider,
            "--repo",
            repo,
        ]
    }

    fn body_file_str(path: &Path) -> Result<&str, String> {
        path.to_str()
            .ok_or_else(|| format!("body file path is not valid UTF-8: {}", path.display()))
    }
}

impl ProviderAdapter for ForgeCliAdapter {
    fn issue_body(&self, repo: &str, issue: u64) -> Result<String, String> {
        let issue_str = issue.to_string();
        let mut args = self.base_args(repo);
        args.extend(["issue", "view", &issue_str]);
        let data = self.run_envelope(&args)?;
        data.get("body")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| "forge-cli issue view data missing `body`".to_string())
    }

    fn issue_evidence(&self, repo: &str, issue: u64) -> Result<(String, String), String> {
        let issue_str = issue.to_string();
        let mut args = self.base_args(repo);
        args.extend(["issue", "view", &issue_str, "--with-comments"]);
        let data = self.run_envelope(&args)?;
        let body = data
            .get("body")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let comments = data
            .get("comments")
            .cloned()
            .unwrap_or_else(|| Value::Array(Vec::new()));
        // Re-shape into the `{"comments": [...]}` envelope expected by
        // `lifecycle_record::audit_record`. The inner fields (`body`, `url`,
        // `created_at`) already match what the audit parser reads.
        let envelope = json!({ "comments": comments });
        let comments_json = serde_json::to_string(&envelope)
            .map_err(|err| format!("failed to serialize issue evidence comments: {err}"))?;
        Ok((body, comments_json))
    }

    fn list_open_tracker_issues(&self, repo: &str, labels: &[String]) -> Result<Vec<u64>, String> {
        let trimmed_labels: Vec<&str> = labels
            .iter()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect();
        let mut args = self.base_args(repo);
        args.extend([
            "issue",
            "list",
            "--state",
            "open",
            "--limit",
            TRACKER_ISSUE_SCAN_LIMIT,
        ]);
        for label in &trimmed_labels {
            args.push("--label");
            args.push(label);
        }
        let data = self.run_envelope(&args)?;
        let items = data
            .get("items")
            .and_then(Value::as_array)
            .ok_or_else(|| "forge-cli issue list data missing `items`".to_string())?;
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
        let body_file_str = Self::body_file_str(body_file)?;
        let mut args = self.base_args(repo);
        args.extend([
            "issue",
            "create",
            "--title",
            title,
            "--body-file",
            body_file_str,
        ]);
        let trimmed_labels: Vec<&str> = labels
            .iter()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect();
        for label in &trimmed_labels {
            args.push("--label");
            args.push(label);
        }
        let data = self.run_envelope(&args)?;
        let number = data
            .get("number")
            .and_then(Value::as_u64)
            .ok_or_else(|| "forge-cli issue create data missing `number`".to_string())?;
        let url = data
            .get("url")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| "forge-cli issue create data missing `url`".to_string())?;
        Ok((number, url))
    }

    fn edit_issue_body(&self, repo: &str, issue: u64, body_file: &Path) -> Result<(), String> {
        let issue_str = issue.to_string();
        let body_file_str = Self::body_file_str(body_file)?;
        let mut args = self.base_args(repo);
        args.extend(["issue", "edit", &issue_str, "--body-file", body_file_str]);
        self.run_envelope(&args).map(|_| ())
    }

    fn comment_issue(&self, repo: &str, issue: u64, body_file: &Path) -> Result<String, String> {
        let issue_str = issue.to_string();
        let body_file_str = Self::body_file_str(body_file)?;
        let mut args = self.base_args(repo);
        args.extend(["issue", "comment", &issue_str, "--body-file", body_file_str]);
        let data = self.run_envelope(&args)?;
        data.get("url")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| "forge-cli issue comment data missing `url`".to_string())
    }

    fn edit_issue_labels(
        &self,
        repo: &str,
        issue: u64,
        add: &[String],
        remove: &[String],
    ) -> Result<(), String> {
        let issue_str = issue.to_string();
        let trimmed_add: Vec<&str> = add
            .iter()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect();
        let trimmed_remove: Vec<&str> = remove
            .iter()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect();
        if trimmed_add.is_empty() && trimmed_remove.is_empty() {
            return Ok(());
        }
        let mut args = self.base_args(repo);
        args.extend(["issue", "edit", &issue_str]);
        for label in &trimmed_add {
            args.push("--add-label");
            args.push(label);
        }
        for label in &trimmed_remove {
            args.push("--remove-label");
            args.push(label);
        }
        self.run_envelope(&args).map(|_| ())
    }

    fn close_issue(
        &self,
        repo: &str,
        issue: u64,
        reason: CloseReason,
        close_comment: Option<&str>,
    ) -> Result<(), String> {
        // To preserve the "post a final comment + close" semantic that
        // callers rely on, decompose into two atomic calls: `issue comment
        // --body <c>` (only when a comment is supplied) then `issue close`.
        //
        // `CloseReason` handling is provider-specific:
        // - GitHub: forge-cli's `issue close --reason completed|"not planned"`
        //   accepts the reason, so pass it through to keep the
        //   completed/not-planned distinction.
        // - GitLab / Local: `glab issue close` has no `--reason` concept and
        //   forge-cli silently ignores the flag on those providers, so the
        //   reason resolves to the same "closed" state regardless.
        let issue_str = issue.to_string();
        if let Some(comment) = close_comment
            && !comment.trim().is_empty()
        {
            let mut args = self.base_args(repo);
            args.extend(["issue", "comment", &issue_str, "--body", comment]);
            self.run_envelope(&args)?;
        }
        let mut args = self.base_args(repo);
        args.extend(["issue", "close", &issue_str]);
        if self.provider == "github" {
            args.push("--reason");
            args.push(match reason {
                CloseReason::Completed => "completed",
                CloseReason::NotPlanned => "not planned",
            });
        }
        self.run_envelope(&args).map(|_| ())
    }

    fn pr_is_merged(&self, repo: &str, pr: u64) -> Result<bool, String> {
        let pr_str = pr.to_string();
        let mut args = self.base_args(repo);
        args.extend(["pr", "view", &pr_str]);
        let data = self.run_envelope(&args)?;
        let state = data
            .get("state")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let merged_at_present = data.get("merged_at").map(|v| !v.is_null()).unwrap_or(false);
        Ok(state.eq_ignore_ascii_case("merged") || merged_at_present)
    }

    fn pr_merge_summary(&self, repo: &str, pr: u64) -> Result<PrMergeSummary, String> {
        // Compose `pr view` (state + merge_commit_sha) + `pr checks`
        // (rolled-up status) — `pr view` payload exposes both fields after
        // sympoies/nils-cli#495 (G3); `pr checks` was already there.
        let pr_str = pr.to_string();

        let mut view_args = self.base_args(repo);
        view_args.extend(["pr", "view", &pr_str]);
        let view = self.run_envelope(&view_args)?;
        let state = view
            .get("state")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let merged_at_present = view.get("merged_at").map(|v| !v.is_null()).unwrap_or(false);
        let merged = state.eq_ignore_ascii_case("merged") || merged_at_present;
        let merge_sha = view
            .get("merge_commit_sha")
            .and_then(Value::as_str)
            .map(str::to_string)
            .filter(|s| !s.is_empty());

        let mut checks_args = self.base_args(repo);
        checks_args.extend(["pr", "checks", &pr_str]);
        // GitHub gates on the REQUIRED-check subset: ask forge-cli for the
        // required-only snapshot so the gating `state` / `required_count` /
        // failing-check data reflect only required checks. GitLab / Local
        // have no required-check concept, so they use the plain aggregate
        // rollup and report zero required checks (see below).
        let github = self.provider == "github";
        if github {
            checks_args.push("--required-only");
        }
        // A red required check still returns a successful forge-cli envelope
        // with `state=failure`. An error envelope here means the required-check
        // snapshot could not be read at all, so GitHub closeout must fail
        // closed instead of proceeding with unknown required-check evidence.
        let checks_data = match self.run_envelope(&checks_args) {
            Ok(data) => Some(data),
            Err(err) if github => return Err(format!("forge-cli pr checks read failed: {err}")),
            // GitLab / Local have no required-check concept. Keep their
            // historical degraded path so a missing optional pipeline snapshot
            // does not block closeout after the view-side merge data is read.
            Err(_) => None,
        };

        // Aggregate rollup state used by `checks` (informational) and the
        // GitLab fallback. Under `--required-only` on GitHub this `state` is
        // already the required-gating rollup.
        let checks = checks_data
            .as_ref()
            .and_then(|d| d.get("state"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .filter(|s| !s.is_empty());

        let (required_state, required_count, non_required_failures) = if github {
            let data = checks_data.as_ref().ok_or_else(|| {
                "forge-cli pr checks read failed: missing required-check snapshot".to_string()
            })?;
            // forge-cli's `pr checks --required-only` returns the gating
            // `state` (success/failure/pending/...) and the number of
            // required checks. `data.checks` carries every check with its
            // `required` flag, from which we recover the non-required
            // failure list (informational evidence; never blocks).
            let required_state = data
                .get("state")
                .and_then(Value::as_str)
                .map(str::to_string)
                .filter(|s| !s.is_empty());
            let required_count = data
                .get("required_count")
                .and_then(Value::as_u64)
                .and_then(|n| u32::try_from(n).ok());
            let non_required_failures = non_required_failure_names(data);
            (required_state, required_count, non_required_failures)
        } else {
            // GitLab / Local have no first-class required-check concept:
            // pipeline jobs are reported as a single rolled-up status, or not
            // at all. Report zero required checks — the same shape a GitHub
            // branch without a required-check rule yields — so the
            // closeout-comment `Required` column renders `none required` (via
            // `required_check_label`) instead of `unknown`, and the close gate
            // treats it as a clean resolve per the #502 "non-required failures
            // never block close" contract. Tracked at sympoies/nils-cli#557.
            (Some("success".to_string()), Some(0), Vec::new())
        };

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
        // forge-cli `pr comments` returns `{provider, number, url, comments:[
        // {url, author, created_at, body}, ...]}`. The plan-issue
        // `resolve-approval` consumer reads `body`, `html_url`, `created_at`
        // off each entry — rename `url` → `html_url` to keep that consumer
        // unchanged (the GitHub adapter returns gh's raw payload which uses
        // `html_url`).
        let pr_str = pr.to_string();
        let mut args = self.base_args(repo);
        args.extend(["pr", "comments", &pr_str]);
        let data = self.run_envelope(&args)?;
        let arr = data
            .get("comments")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let reshaped = arr
            .into_iter()
            .map(|mut item| {
                if let Value::Object(ref mut obj) = item
                    && let Some(url) = obj.remove("url")
                {
                    obj.insert("html_url".to_string(), url);
                }
                item
            })
            .collect();
        Ok(reshaped)
    }
}

/// Recover the failing non-required check names from a forge-cli
/// `pr checks` payload's `data.checks` array. `data.checks` carries every
/// check (required or not) with its `required` flag and normalized `state`,
/// even under `--required-only` (which only narrows the gating counters), so
/// the non-required failures the closeout comment surfaces can be derived
/// without a second backend call. Sorted + deduped for stable rendering.
fn non_required_failure_names(checks_data: &Value) -> Vec<String> {
    let Some(items) = checks_data.get("checks").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut names: Vec<String> = items
        .iter()
        .filter(|item| {
            // Non-required checks only.
            !item
                .get("required")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        })
        .filter(|item| {
            let state = item
                .get("state")
                .and_then(Value::as_str)
                .map(str::to_ascii_lowercase);
            matches!(
                state.as_deref(),
                Some("failure" | "cancelled" | "timed_out" | "error")
            )
        })
        .filter_map(|item| item.get("name").and_then(Value::as_str).map(str::to_string))
        .collect();
    names.sort();
    names.dedup();
    names
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::path::PathBuf;

    use super::*;

    /// One scripted forge-cli response. The runner panics if the next call
    /// doesn't match the expected argv prefix; that makes regressions in the
    /// argv shape obvious.
    struct ScriptedRunner {
        responses: RefCell<Vec<String>>,
        calls: RefCell<Vec<Vec<String>>>,
    }

    impl ScriptedRunner {
        fn new(responses: Vec<&str>) -> Self {
            Self {
                responses: RefCell::new(responses.into_iter().map(String::from).collect()),
                calls: RefCell::new(Vec::new()),
            }
        }

        fn calls(&self) -> Vec<Vec<String>> {
            self.calls.borrow().clone()
        }
    }

    impl ForgeCliRunner for ScriptedRunner {
        fn run(&self, args: &[&str]) -> Result<String, String> {
            self.calls
                .borrow_mut()
                .push(args.iter().map(|s| s.to_string()).collect());
            let mut pending = self.responses.borrow_mut();
            if pending.is_empty() {
                return Err(format!(
                    "ScriptedRunner exhausted: no canned response for argv {:?}",
                    args
                ));
            }
            Ok(pending.remove(0))
        }
    }

    fn adapter_with(responses: Vec<&str>) -> (ForgeCliAdapter, std::sync::Arc<RunnerHandle>) {
        let runner = std::sync::Arc::new(RunnerHandle::new(responses));
        let proxy = RunnerProxy {
            inner: runner.clone(),
        };
        (ForgeCliAdapter::with_runner(false, Box::new(proxy)), runner)
    }

    fn adapter_with_local(responses: Vec<&str>) -> (ForgeCliAdapter, std::sync::Arc<RunnerHandle>) {
        let runner = std::sync::Arc::new(RunnerHandle::new(responses));
        let proxy = RunnerProxy {
            inner: runner.clone(),
        };
        (
            ForgeCliAdapter::with_runner_local(false, Box::new(proxy)),
            runner,
        )
    }

    fn adapter_with_github(
        responses: Vec<&str>,
    ) -> (ForgeCliAdapter, std::sync::Arc<RunnerHandle>) {
        let runner = std::sync::Arc::new(RunnerHandle::new(responses));
        let proxy = RunnerProxy {
            inner: runner.clone(),
        };
        (
            ForgeCliAdapter::with_runner_github(false, Box::new(proxy)),
            runner,
        )
    }

    /// Thread-safe wrapper around `ScriptedRunner` so the adapter trait
    /// bound (`Send + Sync`) is satisfied while keeping the test API tiny.
    struct RunnerHandle {
        inner: std::sync::Mutex<ScriptedRunner>,
    }

    impl RunnerHandle {
        fn new(responses: Vec<&str>) -> Self {
            Self {
                inner: std::sync::Mutex::new(ScriptedRunner::new(responses)),
            }
        }

        fn calls(&self) -> Vec<Vec<String>> {
            self.inner.lock().unwrap().calls()
        }
    }

    struct RunnerProxy {
        inner: std::sync::Arc<RunnerHandle>,
    }

    impl ForgeCliRunner for RunnerProxy {
        fn run(&self, args: &[&str]) -> Result<String, String> {
            self.inner.inner.lock().unwrap().run(args)
        }
    }

    #[test]
    fn list_open_tracker_issues_uses_broad_resume_scan_limit() {
        let (adapter, handle) = adapter_with(vec![
            r#"{"ok":true,"schema_version":"cli.forge-cli.issue.list.v1","data":{"provider":"github","items":[]}}"#,
        ]);
        let items = adapter
            .list_open_tracker_issues("g/p", &["plan".into(), "tracking".into()])
            .expect("list open trackers");
        assert!(items.is_empty());

        let calls = handle.calls();
        assert_eq!(calls.len(), 1);
        let argv = &calls[0];
        let limit_idx = argv
            .iter()
            .position(|s| s == "--limit")
            .expect("record-open resume scan must pass an explicit limit");
        assert_eq!(
            argv[limit_idx + 1],
            "200",
            "record-open resume scan must preserve the previous broad scan ceiling"
        );
    }

    #[test]
    fn create_issue_passes_provider_and_repo_and_parses_envelope() {
        let (adapter, handle) = adapter_with(vec![
            r#"{
            "schema_version": "cli.forge-cli.issue.create.v1",
            "ok": true,
            "data": {
                "provider": "gitlab",
                "number": 7,
                "url": "https://gitlab.example.com/grp/proj/-/issues/7"
            }
        }"#,
        ]);
        let body = PathBuf::from("/tmp/body.md");
        let (n, url) = adapter
            .create_issue(
                "grp/proj",
                "title",
                &body,
                &["type::feature".into(), "  ".into(), "area::cli".into()],
            )
            .expect("create");
        assert_eq!(n, 7);
        assert_eq!(url, "https://gitlab.example.com/grp/proj/-/issues/7");

        let calls = handle.calls();
        assert_eq!(calls.len(), 1);
        let argv = &calls[0];
        assert!(argv.iter().any(|s| s == "--provider"), "{argv:?}");
        assert_eq!(
            argv[argv.iter().position(|s| s == "--provider").unwrap() + 1],
            "gitlab"
        );
        assert_eq!(
            argv[argv.iter().position(|s| s == "--repo").unwrap() + 1],
            "grp/proj"
        );
        assert!(argv.windows(2).any(|w| w[0] == "issue" && w[1] == "create"));
        // Blank labels are skipped.
        let label_idxs: Vec<usize> = argv
            .iter()
            .enumerate()
            .filter(|(_, s)| s.as_str() == "--label")
            .map(|(i, _)| i)
            .collect();
        assert_eq!(label_idxs.len(), 2);
        assert_eq!(argv[label_idxs[0] + 1], "type::feature");
        assert_eq!(argv[label_idxs[1] + 1], "area::cli");
    }

    #[test]
    fn local_adapter_emits_provider_local() {
        let (adapter, handle) = adapter_with_local(vec![
            r#"{"ok":true,"schema_version":"cli.forge-cli.issue.create.v1","data":{"provider":"local","number":1,"url":"local://demo/issues/1"}}"#,
        ]);
        let body = PathBuf::from("/tmp/body.md");
        let (n, url) = adapter
            .create_issue("demo", "title", &body, &[])
            .expect("create");
        assert_eq!(n, 1);
        assert_eq!(url, "local://demo/issues/1");

        let calls = handle.calls();
        assert_eq!(calls.len(), 1);
        let argv = &calls[0];
        assert_eq!(
            argv[argv.iter().position(|s| s == "--provider").unwrap() + 1],
            "local",
            "local adapter must route through forge-cli --provider local: {argv:?}"
        );
        assert_eq!(
            argv[argv.iter().position(|s| s == "--repo").unwrap() + 1],
            "demo"
        );
    }

    #[test]
    fn comment_issue_returns_data_url_from_envelope() {
        let (adapter, _) = adapter_with(vec![
            r#"{
            "ok": true,
            "schema_version": "cli.forge-cli.issue.comment.v1",
            "data": {"provider":"gitlab","number":7,"url":"https://x.com/g/p/-/issues/7#note_42"}
        }"#,
        ]);
        let url = adapter
            .comment_issue("g/p", 7, Path::new("/tmp/body.md"))
            .expect("comment");
        assert_eq!(url, "https://x.com/g/p/-/issues/7#note_42");
    }

    #[test]
    fn github_tracker_scan_preserves_legacy_200_issue_limit() {
        let (adapter, handle) = adapter_with_github(vec![
            r#"{
            "ok": true,
            "schema_version": "cli.forge-cli.issue.list.v1",
            "data": {
                "provider": "github",
                "items": [
                    {"number": 42, "url": "https://github.com/o/r/issues/42", "state": "open", "title": "tracker", "labels": [], "author": null, "assignees": []}
                ]
            }
        }"#,
        ]);
        let issues = adapter
            .list_open_tracker_issues("o/r", &["type::plan".into(), "state::open".into()])
            .expect("list issues");
        assert_eq!(issues, vec![42]);

        let calls = handle.calls();
        assert_eq!(calls.len(), 1);
        let argv = &calls[0];
        let idx = argv
            .iter()
            .position(|s| s == "--limit")
            .expect("github tracker scan must set --limit");
        assert_eq!(argv[idx + 1], "200");
    }

    #[test]
    fn edit_issue_body_passes_body_file_flag() {
        let (adapter, handle) = adapter_with(vec![
            r#"{
            "ok": true,
            "schema_version": "cli.forge-cli.issue.edit.v1",
            "data": {"provider":"gitlab","number":7,"url":"u","state":"open","title":"t","labels":[],"assignees":[]}
        }"#,
        ]);
        adapter
            .edit_issue_body("g/p", 7, Path::new("/tmp/new-body.md"))
            .expect("edit");
        let argv = &handle.calls()[0];
        let body_file_idx = argv.iter().position(|s| s == "--body-file").unwrap();
        assert_eq!(argv[body_file_idx + 1], "/tmp/new-body.md");
        assert!(argv.windows(2).any(|w| w[0] == "issue" && w[1] == "edit"));
    }

    #[test]
    fn issue_evidence_reshapes_comments_into_gh_compatible_envelope() {
        let (adapter, _) = adapter_with(vec![
            r#"{
            "ok": true,
            "schema_version": "cli.forge-cli.issue.view.v1",
            "data": {
                "provider": "gitlab",
                "number": 7,
                "url": "https://x.com/g/p/-/issues/7",
                "state": "open",
                "title": "t",
                "body": "issue body here",
                "labels": [],
                "assignees": [],
                "comments": [
                    {"url":"https://x.com/g/p/-/issues/7#note_1","author":"alice","created_at":"2025-01-01T00:00:00Z","body":"first"},
                    {"url":"https://x.com/g/p/-/issues/7#note_2","author":"bob","created_at":"2025-01-02T00:00:00Z","body":"second"}
                ]
            }
        }"#,
        ]);
        let (body, comments_json) = adapter.issue_evidence("g/p", 7).expect("evidence");
        assert_eq!(body, "issue body here");
        let reparsed: Value = serde_json::from_str(&comments_json).unwrap();
        let arr = reparsed.get("comments").and_then(Value::as_array).unwrap();
        assert_eq!(arr.len(), 2);
        // The audit parser reads `body`, `url`, and `created_at` (already
        // present in the forge-cli normalized shape) — no further reshape.
        assert_eq!(arr[0].get("body").and_then(Value::as_str), Some("first"));
        assert_eq!(
            arr[0].get("url").and_then(Value::as_str),
            Some("https://x.com/g/p/-/issues/7#note_1")
        );
        assert_eq!(
            arr[0].get("created_at").and_then(Value::as_str),
            Some("2025-01-01T00:00:00Z")
        );
    }

    #[test]
    fn edit_issue_labels_skips_blank_entries_and_omits_call_when_both_empty() {
        let (adapter, handle) = adapter_with(vec![
            r#"{
            "ok": true,
            "schema_version": "cli.forge-cli.issue.edit.v1",
            "data": {"provider":"gitlab","number":7,"url":"u","state":"open","title":"t","labels":[],"assignees":[]}
        }"#,
        ]);
        // No labels — no subprocess call.
        adapter
            .edit_issue_labels("g/p", 7, &[], &[])
            .expect("noop labels");
        assert!(
            handle.calls().is_empty(),
            "edit_issue_labels with no labels must not shell out"
        );

        // One label each, plus blank entries that should be skipped.
        adapter
            .edit_issue_labels(
                "g/p",
                7,
                &["type::test".into(), "  ".into()],
                &["state::stale".into()],
            )
            .expect("labels");
        let argv = &handle.calls()[0];
        let adds: Vec<usize> = argv
            .iter()
            .enumerate()
            .filter(|(_, s)| s.as_str() == "--add-label")
            .map(|(i, _)| i)
            .collect();
        assert_eq!(adds.len(), 1);
        assert_eq!(argv[adds[0] + 1], "type::test");
        let rms: Vec<usize> = argv
            .iter()
            .enumerate()
            .filter(|(_, s)| s.as_str() == "--remove-label")
            .map(|(i, _)| i)
            .collect();
        assert_eq!(rms.len(), 1);
        assert_eq!(argv[rms[0] + 1], "state::stale");
    }

    #[test]
    fn forge_cli_error_envelope_is_propagated() {
        let (adapter, _) = adapter_with(vec![
            r#"{
            "ok": false,
            "schema_version": "cli.forge-cli.error.v1",
            "error": {"code":"some-code","message":"some message"}
        }"#,
        ]);
        let err = adapter
            .create_issue("g/p", "t", Path::new("/tmp/b.md"), &[])
            .expect_err("envelope error");
        assert!(err.contains("some-code"), "{err}");
        assert!(err.contains("some message"), "{err}");
    }

    #[test]
    fn close_issue_without_comment_invokes_only_issue_close() {
        let (adapter, handle) = adapter_with(vec![
            r#"{
            "ok": true,
            "schema_version": "cli.forge-cli.issue.close.v1",
            "data": {"provider":"gitlab","number":7,"url":"u","state":"closed"}
        }"#,
        ]);
        adapter
            .close_issue("g/p", 7, CloseReason::Completed, None)
            .expect("close without comment");
        let calls = handle.calls();
        assert_eq!(calls.len(), 1, "expected only the close call");
        assert!(
            calls[0]
                .windows(2)
                .any(|w| w[0] == "issue" && w[1] == "close")
        );
        // Reason is dropped — GitLab has no equivalent.
        assert!(!calls[0].iter().any(|s| s.contains("--reason")));
    }

    #[test]
    fn close_issue_with_comment_posts_then_closes() {
        let (adapter, handle) = adapter_with(vec![
            r#"{"ok":true,"schema_version":"cli.forge-cli.issue.comment.v1","data":{"provider":"gitlab","number":7,"url":"https://x.com/g/p/-/issues/7#note_42"}}"#,
            r#"{"ok":true,"schema_version":"cli.forge-cli.issue.close.v1","data":{"provider":"gitlab","number":7,"url":"u","state":"closed"}}"#,
        ]);
        adapter
            .close_issue(
                "g/p",
                7,
                CloseReason::NotPlanned,
                Some("closing because of X"),
            )
            .expect("close with comment");
        let calls = handle.calls();
        assert_eq!(calls.len(), 2, "expected comment then close");
        assert!(
            calls[0]
                .windows(2)
                .any(|w| w[0] == "issue" && w[1] == "comment")
        );
        let body_idx = calls[0].iter().position(|s| s == "--body").unwrap();
        assert_eq!(calls[0][body_idx + 1], "closing because of X");
        assert!(
            calls[1]
                .windows(2)
                .any(|w| w[0] == "issue" && w[1] == "close")
        );
    }

    #[test]
    fn close_issue_passes_reason_on_github_only() {
        // GitHub: the close call carries `--reason completed` so the
        // completed/not-planned distinction survives the forge-cli flip.
        let (adapter, handle) = adapter_with_github(vec![
            r#"{"ok":true,"schema_version":"cli.forge-cli.issue.close.v1","data":{"provider":"github","number":7,"url":"u","state":"closed"}}"#,
        ]);
        adapter
            .close_issue("o/r", 7, CloseReason::Completed, None)
            .expect("github close completed");
        let calls = handle.calls();
        assert_eq!(calls.len(), 1, "expected only the close call");
        let close = &calls[0];
        assert!(close.windows(2).any(|w| w[0] == "issue" && w[1] == "close"));
        let idx = close
            .iter()
            .position(|s| s == "--reason")
            .expect("github close must carry --reason");
        assert_eq!(close[idx + 1], "completed");

        // GitHub NotPlanned maps to `not planned`.
        let (adapter, handle) = adapter_with_github(vec![
            r#"{"ok":true,"schema_version":"cli.forge-cli.issue.close.v1","data":{"provider":"github","number":7,"url":"u","state":"closed"}}"#,
        ]);
        adapter
            .close_issue("o/r", 7, CloseReason::NotPlanned, None)
            .expect("github close not planned");
        let calls = handle.calls();
        let idx = calls[0]
            .iter()
            .position(|s| s == "--reason")
            .expect("github close must carry --reason");
        assert_eq!(calls[0][idx + 1], "not planned");

        // GitLab: the reason is still dropped (degrade unchanged).
        let (adapter, handle) = adapter_with(vec![
            r#"{"ok":true,"schema_version":"cli.forge-cli.issue.close.v1","data":{"provider":"gitlab","number":7,"url":"u","state":"closed"}}"#,
        ]);
        adapter
            .close_issue("g/p", 7, CloseReason::NotPlanned, None)
            .expect("gitlab close");
        let calls = handle.calls();
        assert!(
            !calls[0].iter().any(|s| s == "--reason"),
            "gitlab close must not carry --reason: {:?}",
            calls[0]
        );
    }

    #[test]
    fn close_issue_blank_comment_skips_the_comment_call() {
        let (adapter, handle) = adapter_with(vec![
            r#"{
            "ok": true,
            "schema_version": "cli.forge-cli.issue.close.v1",
            "data": {"provider":"gitlab","number":7,"url":"u","state":"closed"}
        }"#,
        ]);
        adapter
            .close_issue("g/p", 7, CloseReason::Completed, Some("   "))
            .expect("close with blank comment");
        assert_eq!(handle.calls().len(), 1, "blank comment must be skipped");
    }

    #[test]
    fn pr_is_merged_returns_true_for_merged_state() {
        let (adapter, _) = adapter_with(vec![
            r#"{
            "ok": true,
            "schema_version": "cli.forge-cli.pr.view.v1",
            "data": {"provider":"gitlab","number":7,"url":"u","state":"merged","draft":false,"title":"t","head":"x","base":"main","mergeable":"yes","merged_at":"2025-01-01T00:00:00Z","merge_commit_sha":"abc","labels":[]}
        }"#,
        ]);
        assert!(adapter.pr_is_merged("g/p", 7).expect("merged state"));
    }

    #[test]
    fn pr_is_merged_returns_false_for_open_state() {
        let (adapter, _) = adapter_with(vec![
            r#"{
            "ok": true,
            "schema_version": "cli.forge-cli.pr.view.v1",
            "data": {"provider":"gitlab","number":7,"url":"u","state":"open","draft":false,"title":"t","head":"x","base":"main","mergeable":"yes","merged_at":null,"merge_commit_sha":null,"labels":[]}
        }"#,
        ]);
        assert!(!adapter.pr_is_merged("g/p", 7).expect("open state"));
    }

    #[test]
    fn pr_merge_summary_composes_view_and_checks() {
        let (adapter, handle) = adapter_with(vec![
            r#"{"ok":true,"schema_version":"cli.forge-cli.pr.view.v1","data":{"provider":"gitlab","number":7,"url":"u","state":"merged","draft":false,"title":"t","head":"x","base":"main","mergeable":"yes","merged_at":"2025-01-01T00:00:00Z","merge_commit_sha":"deadbeef","labels":[]}}"#,
            r#"{"ok":true,"schema_version":"cli.forge-cli.pr.checks.v1","data":{"provider":"gitlab","state":"success","required_count":1,"success_count":1,"failed":[],"pending":[],"checks":[]}}"#,
        ]);
        let summary = adapter.pr_merge_summary("g/p", 7).expect("merge summary");
        assert_eq!(summary.state, "merged");
        assert!(summary.merged);
        assert_eq!(summary.merge_sha.as_deref(), Some("deadbeef"));
        assert_eq!(summary.checks.as_deref(), Some("success"));
        // GitLab parity (sympoies/nils-cli#557): the adapter reports
        // zero required checks so closeout rendering lands on the
        // `none required` label instead of `unknown`.
        assert_eq!(summary.required_state.as_deref(), Some("success"));
        assert_eq!(summary.required_count, Some(0));
        assert!(summary.non_required_failures.is_empty());
        let calls = handle.calls();
        assert_eq!(calls.len(), 2);
        assert!(calls[0].windows(2).any(|w| w[0] == "pr" && w[1] == "view"));
        assert!(
            calls[1]
                .windows(2)
                .any(|w| w[0] == "pr" && w[1] == "checks")
        );
    }

    #[test]
    fn pr_merge_summary_github_fails_when_required_checks_cannot_be_read() {
        // GitHub closeout must fail closed when the required-check read itself
        // fails. Treating this as `None` hides auth/rate-limit/backend
        // failures from `record close`.
        let (adapter, _) = adapter_with_github(vec![
            r#"{"ok":true,"schema_version":"cli.forge-cli.pr.view.v1","data":{"provider":"github","number":7,"url":"u","state":"merged","draft":false,"title":"t","head":"x","base":"main","mergeable":"yes","merged_at":"2025-01-01T00:00:00Z","merge_commit_sha":"deadbeef","labels":[]}}"#,
            r#"{"ok":false,"schema_version":"cli.forge-cli.error.v1","error":{"code":"backend_error","message":"checks rollup failure"}}"#,
        ]);
        let err = adapter
            .pr_merge_summary("o/r", 7)
            .expect_err("required-check read failure must block closeout");
        assert!(
            err.contains("pr checks"),
            "error should name the failed required-check read: {err}"
        );
    }

    #[test]
    fn pr_merge_summary_github_blocks_on_failing_required_check() {
        // HIGHEST-PRIORITY correctness case: a GitHub PR whose REQUIRED check
        // failed must report `required_state=failure` with a non-zero
        // `required_count` so the `record close` merge gate refuses to pass.
        // Before the consolidation fix the adapter hard-coded
        // `required_state=success, required_count=0` and would have let this
        // through as a false pass.
        let (adapter, handle) = adapter_with_github(vec![
            r#"{"ok":true,"schema_version":"cli.forge-cli.pr.view.v1","data":{"provider":"github","number":7,"url":"u","state":"merged","draft":false,"title":"t","head":"x","base":"main","mergeable":"yes","merged_at":"2025-01-01T00:00:00Z","merge_commit_sha":"deadbeef","labels":[]}}"#,
            r#"{"ok":true,"schema_version":"cli.forge-cli.pr.checks.v1","data":{"provider":"github","state":"failure","required_count":1,"success_count":0,"failed":[{"name":"ci-required"}],"pending":[],"checks":[{"name":"ci-required","state":"failure","required":true}]}}"#,
        ]);
        let summary = adapter.pr_merge_summary("o/r", 7).expect("merge summary");
        assert_eq!(
            summary.required_state.as_deref(),
            Some("failure"),
            "failing required check must surface as required_state=failure"
        );
        assert_eq!(summary.required_count, Some(1));
        // The `pr checks` call must request the required-only gating subset.
        let calls = handle.calls();
        assert_eq!(calls.len(), 2);
        let checks_call = &calls[1];
        assert!(
            checks_call
                .windows(2)
                .any(|w| w[0] == "pr" && w[1] == "checks"),
            "{checks_call:?}"
        );
        assert!(
            checks_call.iter().any(|s| s == "--required-only"),
            "github merge-gate must pass --required-only: {checks_call:?}"
        );
    }

    #[test]
    fn pr_merge_summary_github_fails_closed_when_required_checks_cannot_be_read() {
        let (adapter, _) = adapter_with_github(vec![
            r#"{"ok":true,"schema_version":"cli.forge-cli.pr.view.v1","data":{"provider":"github","number":7,"url":"u","state":"merged","draft":false,"title":"t","head":"x","base":"main","mergeable":"yes","merged_at":"2025-01-01T00:00:00Z","merge_commit_sha":"deadbeef","labels":[]}}"#,
            r#"{"ok":false,"schema_version":"cli.forge-cli.error.v1","error":{"code":"backend_error","message":"rate limited while reading required checks"}}"#,
        ]);
        let err = adapter
            .pr_merge_summary("o/r", 7)
            .expect_err("github required-check read errors must fail closed");
        assert!(err.contains("required checks"), "{err}");
        assert!(err.contains("rate limited"), "{err}");
    }

    #[test]
    fn pr_merge_summary_github_passes_with_clean_required_checks() {
        let (adapter, _) = adapter_with_github(vec![
            r#"{"ok":true,"schema_version":"cli.forge-cli.pr.view.v1","data":{"provider":"github","number":7,"url":"u","state":"merged","draft":false,"title":"t","head":"x","base":"main","mergeable":"yes","merged_at":"2025-01-01T00:00:00Z","merge_commit_sha":"deadbeef","labels":[]}}"#,
            r#"{"ok":true,"schema_version":"cli.forge-cli.pr.checks.v1","data":{"provider":"github","state":"success","required_count":2,"success_count":2,"failed":[],"pending":[],"checks":[{"name":"build","state":"success","required":true},{"name":"lint","state":"success","required":true}]}}"#,
        ]);
        let summary = adapter.pr_merge_summary("o/r", 7).expect("merge summary");
        assert_eq!(summary.required_state.as_deref(), Some("success"));
        assert_eq!(summary.required_count, Some(2));
        assert!(summary.non_required_failures.is_empty());
    }

    #[test]
    fn pr_merge_summary_github_reports_non_required_failures_without_blocking() {
        // A non-required check failing is informational only: required_state
        // stays success (gate passes) but the non-required failure name is
        // surfaced for the closeout comment.
        let (adapter, _) = adapter_with_github(vec![
            r#"{"ok":true,"schema_version":"cli.forge-cli.pr.view.v1","data":{"provider":"github","number":7,"url":"u","state":"merged","draft":false,"title":"t","head":"x","base":"main","mergeable":"yes","merged_at":"2025-01-01T00:00:00Z","merge_commit_sha":"deadbeef","labels":[]}}"#,
            r#"{"ok":true,"schema_version":"cli.forge-cli.pr.checks.v1","data":{"provider":"github","state":"success","required_count":1,"success_count":1,"failed":[],"pending":[],"checks":[{"name":"build","state":"success","required":true},{"name":"optional-lint","state":"failure","required":false}]}}"#,
        ]);
        let summary = adapter.pr_merge_summary("o/r", 7).expect("merge summary");
        assert_eq!(summary.required_state.as_deref(), Some("success"));
        assert_eq!(summary.required_count, Some(1));
        assert_eq!(
            summary.non_required_failures,
            vec!["optional-lint".to_string()]
        );
    }

    #[test]
    fn pr_merge_summary_github_zero_required_checks_passes() {
        // A branch with no required-check rule reports required_count=0 and a
        // success gating state — the `none required` closeout label and a clean
        // gate, matching the GitLab parity behaviour.
        let (adapter, _) = adapter_with_github(vec![
            r#"{"ok":true,"schema_version":"cli.forge-cli.pr.view.v1","data":{"provider":"github","number":7,"url":"u","state":"merged","draft":false,"title":"t","head":"x","base":"main","mergeable":"yes","merged_at":"2025-01-01T00:00:00Z","merge_commit_sha":"deadbeef","labels":[]}}"#,
            r#"{"ok":true,"schema_version":"cli.forge-cli.pr.checks.v1","data":{"provider":"github","state":"success","required_count":0,"success_count":0,"failed":[],"pending":[],"checks":[]}}"#,
        ]);
        let summary = adapter.pr_merge_summary("o/r", 7).expect("merge summary");
        assert_eq!(summary.required_state.as_deref(), Some("success"));
        assert_eq!(summary.required_count, Some(0));
        assert!(summary.non_required_failures.is_empty());
    }

    #[test]
    fn pr_merge_summary_gitlab_keeps_zero_required_path() {
        // GitLab has no required-check concept: required_state stays success
        // with required_count=0 regardless of the pipeline rollup, so the
        // closeout renders `none required` and the gate treats it as clean.
        let (adapter, handle) = adapter_with(vec![
            r#"{"ok":true,"schema_version":"cli.forge-cli.pr.view.v1","data":{"provider":"gitlab","number":7,"url":"u","state":"merged","draft":false,"title":"t","head":"x","base":"main","mergeable":"yes","merged_at":"2025-01-01T00:00:00Z","merge_commit_sha":"deadbeef","labels":[]}}"#,
            r#"{"ok":true,"schema_version":"cli.forge-cli.pr.checks.v1","data":{"provider":"gitlab","state":"failure","required_count":1,"success_count":0,"failed":[{"name":"pipeline"}],"pending":[],"checks":[]}}"#,
        ]);
        let summary = adapter.pr_merge_summary("g/p", 7).expect("merge summary");
        assert_eq!(summary.required_state.as_deref(), Some("success"));
        assert_eq!(summary.required_count, Some(0));
        assert!(summary.non_required_failures.is_empty());
        // GitLab must NOT request --required-only (no required-check concept).
        let calls = handle.calls();
        assert!(
            !calls[1].iter().any(|s| s == "--required-only"),
            "gitlab merge-gate must not pass --required-only: {:?}",
            calls[1]
        );
    }

    #[test]
    fn pr_comments_reshapes_url_to_html_url_for_resolve_approval() {
        let (adapter, _) = adapter_with(vec![
            r#"{
            "ok": true,
            "schema_version": "cli.forge-cli.pr.comments.v1",
            "data": {
                "provider": "gitlab",
                "number": 7,
                "url": "https://x.com/g/p/-/merge_requests/7",
                "comments": [
                    {"url":"https://x.com/g/p/-/merge_requests/7#note_1","author":"alice","created_at":"2025-01-01T00:00:00Z","body":"- Decision: merge — approved"},
                    {"url":"https://x.com/g/p/-/merge_requests/7#note_2","author":"bob","created_at":"2025-01-02T00:00:00Z","body":"some other note"}
                ]
            }
        }"#,
        ]);
        let comments = adapter.pr_comments("g/p", 7).expect("comments");
        assert_eq!(comments.len(), 2);
        // resolve-approval reads `body`, `html_url`, `created_at`.
        assert_eq!(
            comments[0].get("html_url").and_then(Value::as_str),
            Some("https://x.com/g/p/-/merge_requests/7#note_1")
        );
        assert!(
            comments[0]
                .get("body")
                .and_then(Value::as_str)
                .unwrap()
                .contains("Decision: merge")
        );
        assert_eq!(
            comments[0].get("created_at").and_then(Value::as_str),
            Some("2025-01-01T00:00:00Z")
        );
        // The forge-cli-native `url` key must be removed after the rename so
        // consumers do not see two competing fields.
        assert!(comments[0].get("url").is_none());
    }
}
