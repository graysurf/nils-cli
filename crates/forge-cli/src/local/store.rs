//! File-backed JSON store for `Provider::Local`.
//!
//! Implements the on-disk contract frozen in
//! `crates/plan-issue-cli/docs/specs/local-provider-contract-v1.md`:
//!
//! ```text
//! <store-root>/
//!   repo.json          RepoFile  — slug, provider, allocation counters
//!   issues/<n>.json    IssueRecord — REAL (the store is the source of truth)
//!   prs/<n>.json       PrRecord    — STUB (seeded by the driver, read-only here)
//! ```
//!
//! The issue half is authoritative: `forge-cli` reads and writes these
//! records. The PR half is seeded out-of-band (the e2e driver writes
//! `prs/<n>.json` directly) and only read here. Timestamps are synthesized
//! deterministically from a monotonic clock counter persisted in `repo.json`
//! (never the wall clock — `nils-common`'s determinism policy forbids
//! `SystemTime::now`), so golden / conformance tests are reproducible.

use std::path::{Path, PathBuf};

use nils_common::cli_contract::schema_version_for;
use serde::{Deserialize, Serialize};

use crate::cli::BINARY;
use crate::error::ForgeError;

/// `repo.json` — store metadata plus monotonic allocation / clock counters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoFile {
    pub slug: String,
    pub provider: String,
    pub next_issue: u64,
    pub next_pr: u64,
    /// Monotonic clock counter (seconds past the deterministic base). Consumed
    /// one tick per synthesized timestamp so issue / comment `created_at`
    /// values are stable and strictly increasing without a wall clock.
    #[serde(default)]
    pub clock: u64,
}

impl RepoFile {
    fn new(slug: String) -> Self {
        Self {
            slug,
            provider: "local".to_string(),
            next_issue: 1,
            next_pr: 1,
            clock: 0,
        }
    }
}

/// One issue comment inside an [`IssueRecord`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssueComment {
    pub id: u64,
    pub body: String,
    pub author: String,
    pub created_at: String,
    pub url: String,
}

/// `issues/<n>.json` — REAL issue record (the store owns this).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssueRecord {
    pub number: u64,
    pub title: String,
    pub body: String,
    pub labels: Vec<String>,
    /// `"open"` | `"closed"`.
    pub state: String,
    /// `null` while open; `"completed"` | `"not-planned"` once closed.
    pub close_reason: Option<String>,
    #[serde(default)]
    pub comments: Vec<IssueComment>,
}

/// One PR comment inside a [`PrRecord`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrComment {
    pub body: String,
    pub html_url: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub created_at: String,
}

/// `prs/<n>.json` — STUB PR record. Every field is seeded by the test /
/// driver; mirrors `plan-issue-cli`'s `PrMergeSummary` plus a comment stream.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrRecord {
    pub number: u64,
    /// Raw GitHub-style state: `MERGED` | `OPEN` | `CLOSED`.
    pub state: String,
    pub merged: bool,
    pub merge_sha: Option<String>,
    /// `success` | `failure` | `pending` | `error` | null.
    #[serde(default)]
    pub checks: Option<String>,
    #[serde(default)]
    pub required_state: Option<String>,
    #[serde(default)]
    pub required_count: Option<u32>,
    #[serde(default)]
    pub non_required_failures: Vec<String>,
    #[serde(default)]
    pub comments: Vec<PrComment>,
}

/// Handle to a file-backed local store rooted at a single directory. One
/// store root holds exactly one repository (its slug lives in `repo.json`).
#[derive(Debug, Clone)]
pub struct Store {
    root: PathBuf,
}

impl Store {
    /// Open (creating if absent) the store at `root`. `slug` seeds a fresh
    /// `repo.json` when one does not already exist; an existing store keeps
    /// its recorded slug.
    pub fn open(root: impl Into<PathBuf>, slug: &str) -> Result<Self, ForgeError> {
        let store = Self { root: root.into() };
        std::fs::create_dir_all(store.root.join("issues"))
            .map_err(|e| store.io_err("issues", &e))?;
        std::fs::create_dir_all(store.root.join("prs")).map_err(|e| store.io_err("prs", &e))?;
        if !store.repo_path().exists() {
            store.save_repo(&RepoFile::new(slug.to_string()))?;
        }
        Ok(store)
    }

    fn repo_path(&self) -> PathBuf {
        self.root.join("repo.json")
    }

    fn issue_path(&self, number: u64) -> PathBuf {
        self.root.join("issues").join(format!("{number}.json"))
    }

    fn pr_path(&self, number: u64) -> PathBuf {
        self.root.join("prs").join(format!("{number}.json"))
    }

    pub fn load_repo(&self) -> Result<RepoFile, ForgeError> {
        let raw =
            std::fs::read_to_string(self.repo_path()).map_err(|e| self.io_err("repo.json", &e))?;
        serde_json::from_str(&raw).map_err(|e| self.parse_err("repo.json", &e))
    }

    pub fn save_repo(&self, repo: &RepoFile) -> Result<(), ForgeError> {
        self.write_json(&self.repo_path(), repo)
    }

    pub fn read_issue(&self, number: u64) -> Result<IssueRecord, ForgeError> {
        let path = self.issue_path(number);
        let raw = std::fs::read_to_string(&path).map_err(|_| self.not_found("issue", number))?;
        serde_json::from_str(&raw).map_err(|e| self.parse_err(&format!("issues/{number}.json"), &e))
    }

    pub fn write_issue(&self, issue: &IssueRecord) -> Result<(), ForgeError> {
        self.write_json(&self.issue_path(issue.number), issue)
    }

    /// Every issue number present in the store, ascending.
    pub fn list_issue_numbers(&self) -> Result<Vec<u64>, ForgeError> {
        let dir = self.root.join("issues");
        let mut numbers = Vec::new();
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(_) => return Ok(numbers),
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if let Some(stem) = name.strip_suffix(".json")
                && let Ok(n) = stem.parse::<u64>()
            {
                numbers.push(n);
            }
        }
        numbers.sort_unstable();
        Ok(numbers)
    }

    pub fn read_pr(&self, number: u64) -> Result<PrRecord, ForgeError> {
        let path = self.pr_path(number);
        let raw = std::fs::read_to_string(&path).map_err(|_| self.not_found("pr", number))?;
        serde_json::from_str(&raw).map_err(|e| self.parse_err(&format!("prs/{number}.json"), &e))
    }

    /// Allocate the next issue number, advancing `repo.json`'s counter.
    pub fn alloc_issue_number(&self, repo: &mut RepoFile) -> u64 {
        let number = repo.next_issue;
        repo.next_issue += 1;
        number
    }

    /// Consume one clock tick and return the synthesized RFC-3339 timestamp.
    pub fn tick_clock(&self, repo: &mut RepoFile) -> String {
        let ts = format_timestamp(repo.clock);
        repo.clock += 1;
        ts
    }

    /// Synthetic comment URL for the local scheme (round-trips through
    /// resolve-approval scans).
    pub fn issue_comment_url(&self, slug: &str, issue: u64, comment_id: u64) -> String {
        format!("local://{slug}/issues/{issue}#comment-{comment_id}")
    }

    pub fn issue_url(&self, slug: &str, issue: u64) -> String {
        format!("local://{slug}/issues/{issue}")
    }

    pub fn pr_url(&self, slug: &str, pr: u64) -> String {
        format!("local://{slug}/pull/{pr}")
    }

    fn write_json<T: Serialize>(&self, path: &Path, value: &T) -> Result<(), ForgeError> {
        let body = serde_json::to_string_pretty(value)
            .map_err(|e| self.parse_err(&path.display().to_string(), &e))?;
        std::fs::write(path, body).map_err(|e| self.io_err(&path.display().to_string(), &e))
    }

    fn io_err(&self, what: &str, err: &std::io::Error) -> ForgeError {
        ForgeError::software(
            schema_err(),
            format!(
                "local store I/O failed for {what} under {}",
                self.root.display()
            ),
            Some(err.to_string()),
        )
    }

    fn parse_err(&self, what: &str, err: &serde_json::Error) -> ForgeError {
        ForgeError::software(
            schema_err(),
            format!("local store JSON error in {what}"),
            Some(err.to_string()),
        )
    }

    fn not_found(&self, kind: &str, number: u64) -> ForgeError {
        // Mirror the real backends: gh / glab exit non-zero for a missing
        // resource, which forge-cli surfaces as a `backend_error` (RUNTIME 1).
        ForgeError::backend_error(
            schema_err(),
            format!(
                "local {kind} #{number} not found under {}",
                self.root.display()
            ),
            None,
        )
    }
}

fn schema_err() -> String {
    schema_version_for(BINARY, "error", 1)
}

/// Epoch day for the deterministic base date `2026-01-01` (days since
/// 1970-01-01, computed via `days_from_civil`).
const BASE_EPOCH_DAY: i64 = 20454;

/// Format `base + secs` as an RFC-3339 UTC timestamp (`YYYY-MM-DDTHH:MM:SSZ`),
/// where `base` is `2026-01-01T00:00:00Z`. Pure integer arithmetic, no wall
/// clock — see the module determinism note.
pub fn format_timestamp(secs: u64) -> String {
    let days = BASE_EPOCH_DAY + (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (year, month, day) = civil_from_days(days);
    let hour = rem / 3_600;
    let minute = (rem % 3_600) / 60;
    let second = rem % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Convert days-since-Unix-epoch to a `(year, month, day)` civil date using
/// Howard Hinnant's `civil_from_days` algorithm.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn base_timestamp_is_2026_01_01() {
        assert_eq!(format_timestamp(0), "2026-01-01T00:00:00Z");
    }

    #[test]
    fn timestamp_advances_seconds_minutes_hours() {
        assert_eq!(format_timestamp(1), "2026-01-01T00:00:01Z");
        assert_eq!(format_timestamp(61), "2026-01-01T00:01:01Z");
        assert_eq!(format_timestamp(3_661), "2026-01-01T01:01:01Z");
    }

    #[test]
    fn timestamp_rolls_over_days_and_months() {
        // 31 days -> 2026-02-01 (January has 31 days).
        assert_eq!(format_timestamp(31 * 86_400), "2026-02-01T00:00:00Z");
        // 365 days -> 2027-01-01 (2026 is not a leap year).
        assert_eq!(format_timestamp(365 * 86_400), "2027-01-01T00:00:00Z");
    }

    #[test]
    fn open_creates_repo_and_dirs() {
        let dir = tempfile::TempDir::new().unwrap();
        let store = Store::open(dir.path(), "demo/local").unwrap();
        let repo = store.load_repo().unwrap();
        assert_eq!(repo.slug, "demo/local");
        assert_eq!(repo.provider, "local");
        assert_eq!(repo.next_issue, 1);
        assert!(dir.path().join("issues").is_dir());
        assert!(dir.path().join("prs").is_dir());
    }

    #[test]
    fn issue_round_trips_through_store() {
        let dir = tempfile::TempDir::new().unwrap();
        let store = Store::open(dir.path(), "demo/local").unwrap();
        let mut repo = store.load_repo().unwrap();
        let number = store.alloc_issue_number(&mut repo);
        store.save_repo(&repo).unwrap();
        let issue = IssueRecord {
            number,
            title: "Plan: x".into(),
            body: "body".into(),
            labels: vec!["plan".into()],
            state: "open".into(),
            close_reason: None,
            comments: vec![],
        };
        store.write_issue(&issue).unwrap();
        let read = store.read_issue(number).unwrap();
        assert_eq!(read.number, 1);
        assert_eq!(read.title, "Plan: x");
        assert_eq!(store.load_repo().unwrap().next_issue, 2);
        assert_eq!(store.list_issue_numbers().unwrap(), vec![1]);
    }

    #[test]
    fn read_missing_issue_is_not_found() {
        let dir = tempfile::TempDir::new().unwrap();
        let store = Store::open(dir.path(), "demo/local").unwrap();
        let err = store.read_issue(99).expect_err("missing");
        assert_eq!(err.kind(), "backend_error");
    }

    #[test]
    fn seeded_pr_record_reads_back() {
        let dir = tempfile::TempDir::new().unwrap();
        let store = Store::open(dir.path(), "demo/local").unwrap();
        let seeded = r#"{"number":7,"state":"MERGED","merged":true,"merge_sha":"0000","checks":"success","required_state":"success","required_count":0,"non_required_failures":[],"comments":[{"body":"lgtm","html_url":"local://demo/local/pull/7#comment-1"}]}"#;
        std::fs::write(dir.path().join("prs").join("7.json"), seeded).unwrap();
        let pr = store.read_pr(7).unwrap();
        assert_eq!(pr.number, 7);
        assert!(pr.merged);
        assert_eq!(pr.merge_sha.as_deref(), Some("0000"));
        assert_eq!(pr.comments.len(), 1);
        assert_eq!(pr.comments[0].body, "lgtm");
    }

    #[test]
    fn clock_ticks_are_monotonic_and_persist() {
        let dir = tempfile::TempDir::new().unwrap();
        let store = Store::open(dir.path(), "demo/local").unwrap();
        let mut repo = store.load_repo().unwrap();
        assert_eq!(store.tick_clock(&mut repo), "2026-01-01T00:00:00Z");
        assert_eq!(store.tick_clock(&mut repo), "2026-01-01T00:00:01Z");
        store.save_repo(&repo).unwrap();
        assert_eq!(store.load_repo().unwrap().clock, 2);
    }
}
