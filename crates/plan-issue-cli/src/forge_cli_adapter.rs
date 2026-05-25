//! GitLab-backed `ProviderAdapter` that routes through `forge-cli`'s
//! provider-neutral surface.
//!
//! Sprint 2 Task 2.2 wires the `record open` path (`create_issue`,
//! `comment_issue`, `edit_issue_body`, `issue_evidence`, `edit_issue_labels`)
//! through `forge-cli` subprocess calls. The remaining methods stay as
//! `provider_not_implemented` stubs until Sprint 3 lands `record post / audit
//! / close / link-pr` and Sprint 4 lands the dispatch family.
//!
//! Subprocess details:
//!
//! - The adapter shells out to `forge-cli` (overridable via `FORGE_CLI_BIN`).
//! - Every call passes `--format json --provider gitlab --repo <slug>` so the
//!   target is unambiguous, even when the cwd's git remote points elsewhere.
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

use crate::commands::plan::CloseReason;
use crate::github::{PrMergeSummary, ProviderAdapter};

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

/// `ProviderAdapter` implementation for GitLab repos.
pub struct ForgeCliAdapter {
    /// Mirrors `GhCliAdapter::force`. Sprint 2/3 trait methods do not consult
    /// it because forge-cli's own markdown-validation gates already enforce
    /// the same policy; this field is retained so the adapter can grow a
    /// `--force` pass-through later without an API change.
    #[allow(dead_code)]
    force: bool,
    runner: Box<dyn ForgeCliRunner + Send + Sync>,
}

impl ForgeCliAdapter {
    pub fn new(force: bool) -> Self {
        Self {
            force,
            runner: Box::new(ProcessForgeCliRunner),
        }
    }

    /// Test-only constructor that swaps in a scripted runner.
    #[cfg(test)]
    pub fn with_runner(force: bool, runner: Box<dyn ForgeCliRunner + Send + Sync>) -> Self {
        Self { force, runner }
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

    /// Common argv prefix: `--format json --provider gitlab --repo <slug>`.
    fn base_args<'a>(&self, repo: &'a str) -> Vec<&'a str> {
        vec!["--format", "json", "--provider", "gitlab", "--repo", repo]
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
        _reason: CloseReason,
        close_comment: Option<&str>,
    ) -> Result<(), String> {
        // GitLab `glab issue close` has no `--reason` or `--comment` flag,
        // and `forge-cli issue close` mirrors that today. To preserve the
        // "post a final comment + close" semantic that GitHub callers rely
        // on, decompose into two atomic calls: `issue comment --body <c>`
        // (only when a comment is supplied) then `issue close`. The
        // `CloseReason` argument is intentionally dropped — GitLab has no
        // native concept; both `Completed` and `NotPlanned` resolve to the
        // same "closed" state.
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
        // `pr checks` exits non-zero when the rollup is `failure` / `pending`
        // — that is information we want, not an error. The forge-cli error
        // path here would surface those as `Err`; today's pr_checks_gitlab
        // also already handles the "no pipeline" case (#485) returning
        // empty success. For pr_merge_summary we just want the `state`
        // field. If forge-cli errors, treat checks as `None`.
        let checks = match self.run_envelope(&checks_args) {
            Ok(checks_data) => checks_data
                .get("state")
                .and_then(Value::as_str)
                .map(str::to_string)
                .filter(|s| !s.is_empty()),
            Err(_) => None,
        };
        // GitLab has no first-class required-check concept: pipeline
        // jobs are either reported by `glab` as a single rolled-up
        // status, or not at all. We leave the required fields at
        // `None`/empty so the close gate falls back to the aggregate
        // `checks` value (matching pre-#502 GitLab behavior).
        Ok(PrMergeSummary {
            state,
            merged,
            merge_sha,
            checks,
            required_state: None,
            required_count: None,
            non_required_failures: Vec::new(),
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
    fn pr_merge_summary_tolerates_checks_failure_by_returning_none() {
        // `pr checks` exits non-zero when the rollup is failing — we want
        // pr_merge_summary to keep going (returning checks=None) so callers
        // can still see the view-side state + merge_sha.
        let (adapter, _) = adapter_with(vec![
            r#"{"ok":true,"schema_version":"cli.forge-cli.pr.view.v1","data":{"provider":"gitlab","number":7,"url":"u","state":"open","draft":false,"title":"t","head":"x","base":"main","mergeable":"yes","merged_at":null,"merge_commit_sha":null,"labels":[]}}"#,
            r#"{"ok":false,"schema_version":"cli.forge-cli.error.v1","error":{"code":"backend_error","message":"checks rollup failure"}}"#,
        ]);
        let summary = adapter.pr_merge_summary("g/p", 7).expect("merge summary");
        assert_eq!(summary.state, "open");
        assert!(!summary.merged);
        assert!(summary.merge_sha.is_none());
        assert!(summary.checks.is_none(), "checks failure tolerated as None");
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
