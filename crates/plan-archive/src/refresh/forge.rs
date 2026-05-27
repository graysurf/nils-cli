//! forge-cli integration for `plan-archive refresh`.
//!
//! All provider API access is delegated to the released `forge-cli`
//! binary; no auth or host endpoint config lives here. The
//! [`ForgeFetcher`] trait is the injection seam that lets tests stub
//! the provider without a network.

use std::process::Command;

use super::refparse::{RefKind, RefTarget, parse_ref_url};

/// Fetch provider payloads. Implemented by [`RealForge`] in
/// production and by stubs in tests.
pub trait ForgeFetcher {
    /// Fetch the raw JSON payload for a single ref.
    fn fetch_payload(&self, target: &RefTarget) -> Result<String, String>;

    /// List open refs for `host/org/repo`, optionally limited to
    /// those updated on or after `since` (`YYYY-MM-DD`).
    fn list_open_refs(
        &self,
        host: &str,
        org: &str,
        repo: &str,
        since: Option<&str>,
    ) -> Result<Vec<RefTarget>, String>;
}

/// Real backend that shells out to `forge-cli`.
pub struct RealForge {
    binary: String,
}

impl RealForge {
    /// Build from the environment. Honours `PLAN_ARCHIVE_FORGE_CLI`
    /// so a wrapper or test harness can redirect the binary.
    pub fn from_env() -> Self {
        let binary =
            std::env::var("PLAN_ARCHIVE_FORGE_CLI").unwrap_or_else(|_| "forge-cli".to_string());
        Self { binary }
    }

    fn provider_for(host: &str) -> &'static str {
        if host == "github.com" {
            "github"
        } else {
            // Every non-github host is treated as a GitLab instance;
            // forge-cli owns the actual endpoint resolution.
            "gitlab"
        }
    }

    /// Run one `forge-cli` invocation and return stdout, mapping a
    /// spawn failure or non-zero exit into an error string.
    fn run(&self, args: &[&str]) -> Result<String, String> {
        let out = Command::new(&self.binary)
            .args(args)
            .output()
            .map_err(|e| format!("spawn {}: {e}", self.binary))?;
        if !out.status.success() {
            return Err(format!(
                "forge-cli exited {:?}: {}",
                out.status.code(),
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    }
}

impl ForgeFetcher for RealForge {
    fn fetch_payload(&self, target: &RefTarget) -> Result<String, String> {
        let provider = Self::provider_for(&target.host);
        let repo_slug = format!("{}/{}", target.org_or_group_path, target.repo);
        let number = target.number.to_string();
        let base = || vec!["--provider", provider, "--repo", &repo_slug, "--format", "json"];
        match target.kind {
            RefKind::Issue => {
                let mut args = base();
                args.extend(["issue", "view", number.as_str(), "--with-comments"]);
                self.run(&args)
            }
            // `forge-cli pr view` has no `--with-comments` flag (unlike
            // `issue view`); the PR/MR comment stream is a separate `pr
            // comments` call. Fetch both and merge the comments into the
            // view payload so PR/MR snapshots carry the same `data.comments`
            // shape that issue snapshots do.
            RefKind::Pull | RefKind::MergeRequest => {
                let mut view_args = base();
                view_args.extend(["pr", "view", number.as_str()]);
                let view = self.run(&view_args)?;
                let mut comment_args = base();
                comment_args.extend(["pr", "comments", number.as_str()]);
                let comments = self.run(&comment_args)?;
                merge_pr_comments(&view, &comments)
            }
        }
    }

    fn list_open_refs(
        &self,
        host: &str,
        org: &str,
        repo: &str,
        since: Option<&str>,
    ) -> Result<Vec<RefTarget>, String> {
        let provider = Self::provider_for(host);
        let repo_slug = format!("{org}/{repo}");
        let mut targets = Vec::new();
        for (subcommand, _kind) in [("issue", RefKind::Issue), ("pr", RefKind::Pull)] {
            let mut args = vec![
                "--provider".to_string(),
                provider.to_string(),
                "--repo".to_string(),
                repo_slug.clone(),
                "--format".to_string(),
                "json".to_string(),
                subcommand.to_string(),
                "list".to_string(),
                "--state".to_string(),
                "open".to_string(),
            ];
            if let Some(since) = since {
                args.push("--since".to_string());
                args.push(since.to_string());
            }
            let out = Command::new(&self.binary)
                .args(&args)
                .output()
                .map_err(|e| format!("spawn {}: {e}", self.binary))?;
            if !out.status.success() {
                return Err(format!(
                    "forge-cli {subcommand} list exited {:?}: {}",
                    out.status.code(),
                    String::from_utf8_lossy(&out.stderr).trim()
                ));
            }
            let body = String::from_utf8_lossy(&out.stdout);
            targets.extend(parse_listing(&body, host, org, repo, subcommand));
        }
        Ok(targets)
    }
}

/// Merge a `forge-cli pr comments` payload's `data.comments` array into a
/// `forge-cli pr view` payload under `data.comments`, returning the combined
/// JSON string. This mirrors the embedded `comments` field that `issue view
/// --with-comments` produces, so PR/MR snapshots share the issue shape. A
/// missing comment stream becomes an empty array.
fn merge_pr_comments(view_json: &str, comments_json: &str) -> Result<String, String> {
    let mut view: serde_json::Value =
        serde_json::from_str(view_json).map_err(|e| format!("parse pr view: {e}"))?;
    let comments: serde_json::Value =
        serde_json::from_str(comments_json).map_err(|e| format!("parse pr comments: {e}"))?;
    let stream = comments
        .get("data")
        .and_then(|d| d.get("comments"))
        .cloned()
        .unwrap_or_else(|| serde_json::Value::Array(Vec::new()));
    if let Some(data) = view.get_mut("data").and_then(|d| d.as_object_mut()) {
        data.insert("comments".to_string(), stream);
    }
    serde_json::to_string(&view).map_err(|e| format!("serialize merged pr payload: {e}"))
}

/// Parse a `forge-cli … list` JSON envelope into ref targets. The
/// envelope shape is `{ data: { items: [{ number, url, … }] } }`;
/// we read `url` when present (authoritative) and fall back to
/// reconstructing from `number`.
fn parse_listing(
    body: &str,
    host: &str,
    org: &str,
    repo: &str,
    subcommand: &str,
) -> Vec<RefTarget> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
        return Vec::new();
    };
    let items = value
        .get("data")
        .and_then(|d| {
            d.get("items")
                .or_else(|| d.get("issues"))
                .or_else(|| d.get("pulls"))
        })
        .or_else(|| value.get("items"))
        .and_then(|i| i.as_array())
        .cloned()
        .unwrap_or_default();

    let mut out = Vec::new();
    for item in items {
        if let Some(url) = item.get("url").and_then(|u| u.as_str())
            && let Some(target) = parse_ref_url(url)
        {
            out.push(target);
            continue;
        }
        if let Some(number) = item.get("number").and_then(|n| n.as_u64()) {
            let kind = if subcommand == "issue" {
                RefKind::Issue
            } else if host == "github.com" {
                RefKind::Pull
            } else {
                RefKind::MergeRequest
            };
            out.push(RefTarget {
                host: host.to_string(),
                org_or_group_path: org.to_string(),
                repo: repo.to_string(),
                kind,
                number,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_mapping() {
        assert_eq!(RealForge::provider_for("github.com"), "github");
        assert_eq!(RealForge::provider_for("gitlab.com"), "gitlab");
        assert_eq!(RealForge::provider_for("gitlab.example.com"), "gitlab");
    }

    #[test]
    fn listing_reads_url_field() {
        let body = r#"{"data":{"items":[
            {"number":1,"url":"https://github.com/org/repo/issues/1"},
            {"number":2,"url":"https://github.com/org/repo/issues/2"}
        ]}}"#;
        let targets = parse_listing(body, "github.com", "org", "repo", "issue");
        assert_eq!(targets.len(), 2);
        assert_eq!(targets[0].number, 1);
        assert_eq!(targets[0].kind, RefKind::Issue);
    }

    #[test]
    fn listing_falls_back_to_number() {
        let body = r#"{"data":{"items":[{"number":9}]}}"#;
        let targets = parse_listing(body, "gitlab.example.com", "g/p", "ingest", "pr");
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].kind, RefKind::MergeRequest);
        assert_eq!(targets[0].number, 9);
        assert_eq!(targets[0].org_or_group_path, "g/p");
    }

    #[test]
    fn listing_empty_on_garbage() {
        assert!(parse_listing("not json", "github.com", "o", "r", "issue").is_empty());
    }

    #[test]
    fn merge_pr_comments_embeds_stream() {
        let view = r#"{"schema_version":"v1","ok":true,"data":{"number":542,"title":"x"}}"#;
        let comments =
            r#"{"ok":true,"data":{"number":542,"comments":[{"author":"a","body":"hi"}]}}"#;
        let merged = merge_pr_comments(view, comments).unwrap();
        let v: serde_json::Value = serde_json::from_str(&merged).unwrap();
        assert_eq!(v["data"]["comments"].as_array().unwrap().len(), 1);
        assert_eq!(v["data"]["title"], "x");
    }

    #[test]
    fn merge_pr_comments_defaults_empty_when_absent() {
        let view = r#"{"data":{"number":1}}"#;
        let comments = r#"{"data":{"number":1}}"#;
        let merged = merge_pr_comments(view, comments).unwrap();
        let v: serde_json::Value = serde_json::from_str(&merged).unwrap();
        assert!(v["data"]["comments"].as_array().unwrap().is_empty());
    }

    #[test]
    fn merge_pr_comments_errors_on_garbage() {
        assert!(merge_pr_comments("not json", "{}").is_err());
    }
}
