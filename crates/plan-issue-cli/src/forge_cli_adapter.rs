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

fn not_implemented(op: &str) -> String {
    format!(
        "provider_not_implemented: GitLab `{op}` is wired up to the routing layer but not yet implemented (Sprint 2.2 only ships `record open`). Track sympoies/nils-cli#490."
    )
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
        _repo: &str,
        _issue: u64,
        _reason: CloseReason,
        _close_comment: Option<&str>,
    ) -> Result<(), String> {
        // Sprint 3 lands `record close`; today close_issue stays a stub so
        // any caller that hits it learns the gap with a typed message.
        let _ = self.force;
        Err(not_implemented("issue close"))
    }

    fn pr_is_merged(&self, _repo: &str, _pr: u64) -> Result<bool, String> {
        Err(not_implemented("pr is-merged"))
    }

    fn pr_merge_summary(&self, _repo: &str, _pr: u64) -> Result<PrMergeSummary, String> {
        Err(not_implemented("pr merge-summary"))
    }

    fn pr_comments(&self, _repo: &str, _pr: u64) -> Result<Vec<Value>, String> {
        Err(not_implemented("pr comments"))
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
    fn unimplemented_methods_still_return_provider_not_implemented() {
        let (adapter, _) = adapter_with(vec![]);
        assert!(
            adapter
                .close_issue("g/p", 1, CloseReason::Completed, None)
                .unwrap_err()
                .contains("provider_not_implemented")
        );
        assert!(
            adapter
                .pr_is_merged("g/p", 1)
                .unwrap_err()
                .contains("provider_not_implemented")
        );
        assert!(
            adapter
                .pr_merge_summary("g/p", 1)
                .unwrap_err()
                .contains("provider_not_implemented")
        );
        assert!(
            adapter
                .pr_comments("g/p", 1)
                .unwrap_err()
                .contains("provider_not_implemented")
        );
    }
}
