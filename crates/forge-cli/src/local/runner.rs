//! In-process [`BackendRunner`] for `Provider::Local`.
//!
//! The `Provider::Local` ops build the *same* gh-style [`BackendCall`]s as the
//! GitHub branch (see each op's `Provider::GitHub | Provider::Local` arm) and
//! parse the *same* gh-shaped JSON. The only thing that differs is the runner:
//! instead of spawning `gh`, [`LocalRunner`] interprets the planned argv and
//! reads / writes the file-backed [`Store`], synthesizing gh-shaped JSON on
//! stdout. This keeps the op layer provider-agnostic while all local behaviour
//! lives here plus [`crate::local::store`].
//!
//! Coverage: the REAL issue half (`issue create / view / comment / edit /
//! close / list`), the seeded-read `pr view`, and the `gh api …/comments`
//! call behind `pr comments`. `pr checks` does NOT route through here — it
//! reads the seeded rollup directly via `pr_checks::snapshot`'s
//! `Provider::Local` branch.

use nils_common::cli_contract::schema_version_for;
use serde_json::{Value, json};

use crate::backend::{BackendCall, BackendRunner, BackendSuccess};
use crate::cli::{BINARY, GlobalFlags};
use crate::error::ForgeError;
use crate::local::store::{IssueComment, IssueRecord, Store};

/// Runner that serves `Provider::Local` calls from a file-backed store.
pub struct LocalRunner {
    root: std::path::PathBuf,
    slug: String,
}

impl LocalRunner {
    pub fn new(root: impl Into<std::path::PathBuf>, slug: String) -> Self {
        Self {
            root: root.into(),
            slug,
        }
    }

    /// Resolve a runner from the global flags: the store root comes from
    /// `--store-root` or `$FORGE_CLI_LOCAL_STORE`; the slug comes from
    /// `--repo` (a leading `local:` is stripped) and defaults to `local`.
    pub fn from_global(global: &GlobalFlags) -> Result<Self, ForgeError> {
        let root = super::resolve_store_root(global)?;
        let slug = super::resolve_slug(global.repo.as_deref());
        Ok(Self::new(root, slug))
    }

    fn store(&self) -> Result<Store, ForgeError> {
        Store::open(&self.root, &self.slug)
    }

    fn dispatch(&self, argv: &[String]) -> Result<String, ForgeError> {
        match (
            argv.first().map(String::as_str),
            argv.get(1).map(String::as_str),
        ) {
            (Some("issue"), Some("create")) => self.issue_create(argv),
            (Some("issue"), Some("view")) => self.issue_view(argv),
            (Some("issue"), Some("comment")) => self.issue_comment(argv),
            (Some("issue"), Some("edit")) => self.issue_edit(argv),
            (Some("issue"), Some("close")) => self.issue_close(argv),
            (Some("issue"), Some("list")) => self.issue_list(argv),
            (Some("pr"), Some("view")) => self.pr_view(argv),
            (Some("api"), _) => self.api_comments(argv),
            _ => Err(unsupported(argv)),
        }
    }

    fn issue_create(&self, argv: &[String]) -> Result<String, ForgeError> {
        let title = flag_value(argv, "--title").unwrap_or_default().to_string();
        let body = match flag_value(argv, "--body-file") {
            Some(path) => std::fs::read_to_string(path).map_err(|e| {
                ForgeError::software(
                    schema_err(),
                    format!("local issue create: failed to read --body-file '{path}'"),
                    Some(e.to_string()),
                )
            })?,
            None => flag_value(argv, "--body").unwrap_or_default().to_string(),
        };
        let labels = flag_values(argv, "--label");
        let store = self.store()?;
        let mut repo = store.load_repo()?;
        let number = store.alloc_issue_number(&mut repo);
        let issue = IssueRecord {
            number,
            title,
            body,
            labels,
            state: "open".into(),
            close_reason: None,
            comments: Vec::new(),
        };
        store.write_issue(&issue)?;
        store.save_repo(&repo)?;
        // gh prints the new issue URL on stdout; the op extracts the trailing
        // number from it (parse accepts the `local://` scheme).
        Ok(format!("{}\n", store.issue_url(&repo.slug, number)))
    }

    fn issue_view(&self, argv: &[String]) -> Result<String, ForgeError> {
        let id = positional_id(argv, 2)?;
        let with_comments = flag_value(argv, "--json")
            .map(|f| f.contains("comments"))
            .unwrap_or(false);
        let store = self.store()?;
        let repo = store.load_repo()?;
        let issue = store.read_issue(id)?;
        Ok(issue_view_json(&store, &repo.slug, &issue, with_comments).to_string())
    }

    fn issue_comment(&self, argv: &[String]) -> Result<String, ForgeError> {
        let id = positional_id(argv, 2)?;
        let body = flag_value(argv, "--body").unwrap_or_default().to_string();
        let store = self.store()?;
        let mut repo = store.load_repo()?;
        let mut issue = store.read_issue(id)?;
        let comment_id = issue.comments.len() as u64 + 1;
        let created_at = store.tick_clock(&mut repo);
        let url = store.issue_comment_url(&repo.slug, id, comment_id);
        issue.comments.push(IssueComment {
            id: comment_id,
            body,
            author: "local".into(),
            created_at,
            url: url.clone(),
        });
        store.write_issue(&issue)?;
        store.save_repo(&repo)?;
        // The op ignores this stdout and re-fetches the view; return the
        // comment URL anyway for parity with gh.
        Ok(format!("{url}\n"))
    }

    fn issue_edit(&self, argv: &[String]) -> Result<String, ForgeError> {
        let id = positional_id(argv, 2)?;
        let store = self.store()?;
        let mut issue = store.read_issue(id)?;
        if let Some(title) = flag_value(argv, "--title") {
            issue.title = title.to_string();
        }
        if let Some(body) = flag_value(argv, "--body") {
            issue.body = body.to_string();
        }
        for add in flag_values(argv, "--add-label") {
            if !issue.labels.contains(&add) {
                issue.labels.push(add);
            }
        }
        let remove = flag_values(argv, "--remove-label");
        if !remove.is_empty() {
            issue.labels.retain(|l| !remove.contains(l));
        }
        store.write_issue(&issue)?;
        Ok(String::new())
    }

    fn issue_close(&self, argv: &[String]) -> Result<String, ForgeError> {
        let id = positional_id(argv, 2)?;
        let store = self.store()?;
        let mut issue = store.read_issue(id)?;
        issue.state = "closed".into();
        store.write_issue(&issue)?;
        Ok(String::new())
    }

    fn issue_list(&self, argv: &[String]) -> Result<String, ForgeError> {
        let state_filter = flag_value(argv, "--state").unwrap_or("open").to_string();
        // gh joins repeated `--label` selectors into one comma list; AND
        // semantics (an issue must carry every requested label).
        let wanted_labels: Vec<String> = flag_value(argv, "--label")
            .map(|csv| {
                csv.split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        let limit: usize = flag_value(argv, "--limit")
            .and_then(|l| l.parse().ok())
            .unwrap_or(usize::MAX);
        let store = self.store()?;
        let repo = store.load_repo()?;
        let mut items: Vec<Value> = Vec::new();
        for number in store.list_issue_numbers()? {
            let issue = store.read_issue(number)?;
            if !state_matches(&state_filter, &issue.state) {
                continue;
            }
            if !wanted_labels.iter().all(|w| issue.labels.contains(w)) {
                continue;
            }
            items.push(issue_list_item_json(&store, &repo.slug, &issue));
            if items.len() >= limit {
                break;
            }
        }
        Ok(Value::Array(items).to_string())
    }

    fn pr_view(&self, argv: &[String]) -> Result<String, ForgeError> {
        let id = positional_id(argv, 2)?;
        let store = self.store()?;
        let repo = store.load_repo()?;
        let pr = store.read_pr(id)?;
        let url = store.pr_url(&repo.slug, id);
        Ok(pr_view_json(&pr, &url).to_string())
    }

    /// Serve the `gh api … repos/<slug>/issues/<n>/comments` call behind
    /// `pr comments`. Only the PR number is needed; it is parsed from the path.
    fn api_comments(&self, argv: &[String]) -> Result<String, ForgeError> {
        let path = argv
            .iter()
            .find(|a| a.contains("/comments") || a.contains("/issues/"))
            .ok_or_else(|| unsupported(argv))?;
        let number = parse_api_pr_number(path).ok_or_else(|| {
            ForgeError::software(
                schema_err(),
                "local api: could not parse PR number from comments path",
                Some(format!("path={path}")),
            )
        })?;
        let store = self.store()?;
        let pr = store.read_pr(number)?;
        let comments: Vec<Value> = pr
            .comments
            .iter()
            .map(|c| {
                json!({
                    "user": {"login": c.author},
                    "body": c.body,
                    "html_url": c.html_url,
                    "created_at": c.created_at,
                })
            })
            .collect();
        Ok(Value::Array(comments).to_string())
    }
}

impl BackendRunner for LocalRunner {
    fn run(&self, call: &BackendCall) -> Result<BackendSuccess, ForgeError> {
        let argv: Vec<String> = call
            .argv
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let stdout = self.dispatch(&argv)?;
        Ok(BackendSuccess {
            stdout,
            stderr: String::new(),
        })
    }
}

fn issue_view_json(store: &Store, slug: &str, issue: &IssueRecord, with_comments: bool) -> Value {
    let labels: Vec<Value> = issue.labels.iter().map(|l| json!({"name": l})).collect();
    let mut obj = json!({
        "number": issue.number,
        "url": store.issue_url(slug, issue.number),
        "state": gh_issue_state(&issue.state),
        "title": issue.title,
        "body": issue.body,
        "labels": labels,
        "assignees": [],
    });
    if with_comments {
        let comments: Vec<Value> = issue
            .comments
            .iter()
            .map(|c| {
                json!({
                    "author": {"login": c.author},
                    "body": c.body,
                    "url": c.url,
                    "createdAt": c.created_at,
                })
            })
            .collect();
        obj["comments"] = Value::Array(comments);
    }
    obj
}

fn issue_list_item_json(store: &Store, slug: &str, issue: &IssueRecord) -> Value {
    let labels: Vec<Value> = issue.labels.iter().map(|l| json!({"name": l})).collect();
    json!({
        "number": issue.number,
        "url": store.issue_url(slug, issue.number),
        "state": gh_issue_state(&issue.state),
        "title": issue.title,
        "labels": labels,
        "author": {"login": "local"},
        "assignees": [],
    })
}

fn pr_view_json(pr: &crate::local::store::PrRecord, url: &str) -> Value {
    // `pr view` derives `state=merged` from a non-empty `mergedAt`, so set it
    // only when the seeded record is merged; otherwise pass the raw state
    // (`OPEN` / `CLOSED`) through the normalizer.
    let merged_at = if pr.merged {
        json!("2026-01-01T00:00:00Z")
    } else {
        Value::Null
    };
    let merge_commit = match (pr.merged, pr.merge_sha.as_deref()) {
        (true, Some(sha)) => json!({"oid": sha}),
        _ => Value::Null,
    };
    json!({
        "number": pr.number,
        "url": url,
        "state": pr.state,
        "isDraft": false,
        "title": "",
        "headRefName": "",
        "baseRefName": "",
        "mergeable": "UNKNOWN",
        "mergedAt": merged_at,
        "mergeCommit": merge_commit,
        "labels": [],
    })
}

/// Map the stored lowercase issue state to gh's uppercase wire form (the
/// normalizer lowercases again, so this is cosmetic parity with `gh`).
fn gh_issue_state(state: &str) -> &'static str {
    match state {
        "closed" => "CLOSED",
        _ => "OPEN",
    }
}

/// `--state open|closed|all` filter against a stored `open|closed` state.
fn state_matches(filter: &str, state: &str) -> bool {
    match filter {
        "all" => true,
        "closed" => state == "closed",
        _ => state == "open",
    }
}

/// Parse the PR number from a comments API path such as
/// `repos/<owner>/<repo>/issues/<n>/comments?per_page=100`.
fn parse_api_pr_number(path: &str) -> Option<u64> {
    let after = path.split("/issues/").nth(1)?;
    let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

/// First value following `name` in a flat argv (`["--title","x"]`).
fn flag_value<'a>(argv: &'a [String], name: &str) -> Option<&'a str> {
    argv.iter()
        .position(|a| a == name)
        .and_then(|i| argv.get(i + 1))
        .map(String::as_str)
}

/// Every value following a repeated `name` flag.
fn flag_values(argv: &[String], name: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < argv.len() {
        if argv[i] == name
            && let Some(v) = argv.get(i + 1)
        {
            out.push(v.clone());
            i += 2;
        } else {
            i += 1;
        }
    }
    out
}

fn positional_id(argv: &[String], index: usize) -> Result<u64, ForgeError> {
    argv.get(index)
        .and_then(|s| s.parse::<u64>().ok())
        .ok_or_else(|| {
            ForgeError::software(
                schema_err(),
                "local backend: expected a numeric id argument",
                Some(format!("argv={argv:?}")),
            )
        })
}

fn unsupported(argv: &[String]) -> ForgeError {
    ForgeError::software(
        schema_err(),
        "local backend does not support this call",
        Some(format!("argv={argv:?}")),
    )
}

fn schema_err() -> String {
    schema_version_for(BINARY, "error", 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn runner() -> (tempfile::TempDir, LocalRunner) {
        let dir = tempfile::TempDir::new().unwrap();
        let runner = LocalRunner::new(dir.path(), "demo/local".to_string());
        (dir, runner)
    }

    fn call(args: &[&str]) -> BackendCall {
        BackendCall::new(crate::backend::BackendProgram::Local, args.iter().copied())
    }

    #[test]
    fn create_then_view_round_trips() {
        let dir = tempfile::TempDir::new().unwrap();
        let body = dir.path().join("body.md");
        std::fs::write(&body, "issue body").unwrap();
        let runner = LocalRunner::new(dir.path().join("store"), "demo/local".to_string());

        let create = runner
            .run(&call(&[
                "issue",
                "create",
                "--title",
                "Plan: x",
                "--body-file",
                body.to_str().unwrap(),
                "--label",
                "plan",
            ]))
            .unwrap();
        assert!(create.stdout.contains("local://demo/local/issues/1"));

        let view = runner
            .run(&call(&[
                "issue",
                "view",
                "1",
                "--json",
                "number,url,state,title,body,labels,assignees",
            ]))
            .unwrap();
        let v: Value = serde_json::from_str(&view.stdout).unwrap();
        assert_eq!(v["number"], 1);
        assert_eq!(v["state"], "OPEN");
        assert_eq!(v["title"], "Plan: x");
        assert_eq!(v["body"], "issue body");
        assert_eq!(v["labels"][0]["name"], "plan");
    }

    #[test]
    fn comment_edit_close_mutate_the_record() {
        let (_dir, runner) = runner();
        runner
            .run(&call(&["issue", "create", "--title", "t", "--body", "b"]))
            .unwrap();
        runner
            .run(&call(&["issue", "comment", "1", "--body", "first note"]))
            .unwrap();
        runner
            .run(&call(&[
                "issue",
                "edit",
                "1",
                "--title",
                "t2",
                "--add-label",
                "x",
            ]))
            .unwrap();
        let view = runner
            .run(&call(&[
                "issue",
                "view",
                "1",
                "--json",
                "number,url,state,title,body,labels,assignees,comments",
            ]))
            .unwrap();
        let v: Value = serde_json::from_str(&view.stdout).unwrap();
        assert_eq!(v["title"], "t2");
        assert_eq!(v["labels"][0]["name"], "x");
        assert_eq!(v["comments"][0]["body"], "first note");
        assert_eq!(v["comments"][0]["author"]["login"], "local");
        assert_eq!(v["comments"][0]["createdAt"], "2026-01-01T00:00:00Z");

        runner.run(&call(&["issue", "close", "1"])).unwrap();
        let after = runner
            .run(&call(&["issue", "view", "1", "--json", "state"]))
            .unwrap();
        let a: Value = serde_json::from_str(&after.stdout).unwrap();
        assert_eq!(a["state"], "CLOSED");
    }

    #[test]
    fn list_filters_by_state_and_labels_with_and_semantics() {
        let (_dir, runner) = runner();
        runner
            .run(&call(&[
                "issue", "create", "--title", "a", "--body", "", "--label", "plan", "--label", "p1",
            ]))
            .unwrap();
        runner
            .run(&call(&[
                "issue", "create", "--title", "b", "--body", "", "--label", "plan",
            ]))
            .unwrap();
        runner.run(&call(&["issue", "close", "2"])).unwrap();

        // open + label plan,p1 -> only issue 1.
        let open = runner
            .run(&call(&[
                "issue", "list", "--state", "open", "--limit", "30", "--label", "plan,p1",
                "--json", "x",
            ]))
            .unwrap();
        let items: Value = serde_json::from_str(&open.stdout).unwrap();
        assert_eq!(items.as_array().unwrap().len(), 1);
        assert_eq!(items[0]["number"], 1);

        // all + label plan -> issues 1 and 2.
        let all = runner
            .run(&call(&[
                "issue", "list", "--state", "all", "--limit", "30", "--label", "plan", "--json",
                "x",
            ]))
            .unwrap();
        let all_items: Value = serde_json::from_str(&all.stdout).unwrap();
        assert_eq!(all_items.as_array().unwrap().len(), 2);
    }

    #[test]
    fn pr_view_reads_seeded_record() {
        let (dir, runner) = runner();
        let seeded = r#"{"number":7,"state":"MERGED","merged":true,"merge_sha":"abc123","checks":"success","required_state":"success","required_count":2,"non_required_failures":[]}"#;
        std::fs::create_dir_all(dir.path().join("prs")).unwrap();
        std::fs::write(dir.path().join("prs").join("7.json"), seeded).unwrap();
        let out = runner
            .run(&call(&["pr", "view", "7", "--json", "x"]))
            .unwrap();
        let v: Value = serde_json::from_str(&out.stdout).unwrap();
        assert_eq!(v["number"], 7);
        assert_eq!(v["state"], "MERGED");
        assert_eq!(v["mergeCommit"]["oid"], "abc123");
        assert_eq!(v["mergedAt"], "2026-01-01T00:00:00Z");
        assert_eq!(v["url"], "local://demo/local/pull/7");
    }

    #[test]
    fn api_comments_returns_seeded_pr_comments() {
        let (dir, runner) = runner();
        let seeded = r#"{"number":7,"state":"OPEN","merged":false,"merge_sha":null,"comments":[{"body":"lgtm","html_url":"local://demo/local/pull/7#comment-1","author":"reviewer","created_at":"2026-01-01T00:00:05Z"}]}"#;
        std::fs::create_dir_all(dir.path().join("prs")).unwrap();
        std::fs::write(dir.path().join("prs").join("7.json"), seeded).unwrap();
        let out = runner
            .run(&call(&[
                "api",
                "--paginate",
                "repos/demo/local/issues/7/comments?per_page=100",
            ]))
            .unwrap();
        let v: Value = serde_json::from_str(&out.stdout).unwrap();
        assert_eq!(v[0]["user"]["login"], "reviewer");
        assert_eq!(v[0]["body"], "lgtm");
        assert_eq!(v[0]["html_url"], "local://demo/local/pull/7#comment-1");
    }

    #[test]
    fn parse_api_pr_number_from_comments_path() {
        assert_eq!(
            parse_api_pr_number("repos/o/r/issues/42/comments?per_page=100"),
            Some(42)
        );
        assert_eq!(parse_api_pr_number("no-number-here"), None);
    }

    #[test]
    fn unsupported_call_is_software_error() {
        let (_dir, runner) = runner();
        let err = runner
            .run(&call(&["repo", "view"]))
            .expect_err("unsupported");
        assert_eq!(err.kind(), "software_error");
    }
}
