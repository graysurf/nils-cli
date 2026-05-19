//! Shared lock-down validations consumed by every mutating PR / MR op.
//!
//! Spec: `crates/forge-cli/docs/specs/forge-cli-spec-v1.md` §"Lock-down
//! policy" plus `crates/forge-cli/docs/specs/forge-cli-ops-v1.yaml`
//! `validations_catalog`. Each rule below maps 1:1 to a row in the catalog
//! and returns a [`ForgeError::Validation`] with the rule's documented
//! `error.kind` literal. The numeric exit class (`DATA 65`) lives in
//! [`crate::error::ForgeError::exit_code`] — never inlined here.
//!
//! Body parsing rule (spec §"Lock-down policy" item 2): "non-empty H2
//! `## Summary` / `## Test plan` section" means the H2 heading line itself
//! does not count as content; only non-blank lines below the heading and
//! above the next H2 (or end-of-body) count. Both "section absent" and
//! "section present but empty" produce the same `error.kind` because the
//! user-visible failure is identical.

use std::path::Path;
use std::process::Command;

use nils_common::cli_contract::schema_version_for;

use crate::cli::BINARY;
use crate::error::ForgeError;

/// PR/MR kind declared by the caller via `--kind`. Drives the
/// `branch_kind_matches` rule plus the macro in Sprint 6.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrKind {
    Feature,
    Bug,
}

impl PrKind {
    /// Render the kind to the lower-case enum literal used in envelopes and
    /// argv (`feature`, `bug`).
    pub fn as_str(self) -> &'static str {
        match self {
            PrKind::Feature => "feature",
            PrKind::Bug => "bug",
        }
    }

    /// Parse the `--kind` flag value. Anything outside the spec's enum is a
    /// usage error and is handled at the clap layer; this helper exists for
    /// internal callers that already hold the string.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "feature" => Some(PrKind::Feature),
            "bug" => Some(PrKind::Bug),
            _ => None,
        }
    }
}

/// Branch prefix recovered from a branch name that matches the
/// `branch_name` rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BranchPrefix {
    Feat,
    Fix,
}

impl BranchPrefix {
    pub fn as_str(self) -> &'static str {
        match self {
            BranchPrefix::Feat => "feat",
            BranchPrefix::Fix => "fix",
        }
    }
}

/// Configurable body H2 headings (set via `.forge-cli.toml` in later
/// sprints). Defaults match the spec §"Lock-down policy" item 2.
#[derive(Debug, Clone)]
pub struct BodyHeadings {
    pub summary: String,
    pub test_plan: String,
}

impl Default for BodyHeadings {
    fn default() -> Self {
        Self {
            summary: "## Summary".to_string(),
            test_plan: "## Test plan".to_string(),
        }
    }
}

/// Hard cap on title length per spec §"Lock-down policy" item 3.
pub const TITLE_MAX_LEN: usize = 70;

fn schema() -> String {
    schema_version_for(BINARY, "error", 1)
}

/// Rule 1a — branch name matches `^(feat|fix)/[a-z0-9][a-z0-9-]{1,63}$`.
///
/// Returns the matched prefix so callers can chain into
/// [`branch_kind_matches`] without re-parsing.
pub fn branch_name(branch: &str) -> Result<BranchPrefix, ForgeError> {
    let (prefix, rest) = match branch.split_once('/') {
        Some((p, r)) => (p, r),
        None => return Err(branch_name_err(branch, "missing 'feat/' or 'fix/' prefix")),
    };

    let prefix = match prefix {
        "feat" => BranchPrefix::Feat,
        "fix" => BranchPrefix::Fix,
        other => {
            return Err(branch_name_err(
                branch,
                &format!("unknown prefix '{other}' (expected 'feat' or 'fix')"),
            ));
        }
    };

    if rest.is_empty() {
        return Err(branch_name_err(branch, "slug is empty"));
    }
    if rest.len() > 64 {
        return Err(branch_name_err(
            branch,
            &format!("slug is {len} chars; max 64", len = rest.len()),
        ));
    }
    let bytes = rest.as_bytes();
    let first = bytes[0];
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return Err(branch_name_err(
            branch,
            "slug must start with a lowercase letter or digit",
        ));
    }
    for &b in &bytes[1..] {
        if !(b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') {
            return Err(branch_name_err(
                branch,
                "slug must be lowercase [a-z0-9-] only",
            ));
        }
    }
    Ok(prefix)
}

fn branch_name_err(branch: &str, why: &str) -> ForgeError {
    ForgeError::validation(
        schema(),
        "branch_name_invalid",
        format!("branch '{branch}' is invalid: {why}"),
        Some("rule=^(feat|fix)/[a-z0-9][a-z0-9-]{1,63}$".to_string()),
    )
}

/// Rule 1b — declared `--kind` matches the branch prefix
/// (`feature` ↔ `feat/*`, `bug` ↔ `fix/*`).
pub fn branch_kind_matches(prefix: BranchPrefix, kind: PrKind) -> Result<(), ForgeError> {
    let ok = matches!(
        (prefix, kind),
        (BranchPrefix::Feat, PrKind::Feature) | (BranchPrefix::Fix, PrKind::Bug)
    );
    if ok {
        Ok(())
    } else {
        Err(ForgeError::validation(
            schema(),
            "branch_kind_mismatch",
            format!(
                "branch prefix '{prefix}/' does not match --kind '{kind}'",
                prefix = prefix.as_str(),
                kind = kind.as_str(),
            ),
            Some(format!(
                "feature -> feat/*, bug -> fix/* (branch_prefix={p}, kind={k})",
                p = prefix.as_str(),
                k = kind.as_str(),
            )),
        ))
    }
}

/// Rule 3 — `len(title) <= 70` (codepoint count) and no trailing whitespace.
pub fn title_length(title: &str) -> Result<(), ForgeError> {
    if title.is_empty() {
        return Err(ForgeError::validation(
            schema(),
            "title_too_long",
            "title is empty",
            Some("rule=len(title) in 1..=70".to_string()),
        ));
    }
    let last = title.chars().next_back().expect("non-empty");
    if last.is_whitespace() {
        return Err(ForgeError::validation(
            schema(),
            "title_too_long",
            "title has trailing whitespace",
            Some("rule=len(title) <= 70 and no trailing whitespace".to_string()),
        ));
    }
    let count = title.chars().count();
    if count > TITLE_MAX_LEN {
        return Err(ForgeError::validation(
            schema(),
            "title_too_long",
            format!("title length {count} exceeds maximum {TITLE_MAX_LEN}"),
            Some(format!("rule=len(title) <= {TITLE_MAX_LEN}")),
        ));
    }
    Ok(())
}

/// Rule 2a — body contains a non-empty H2 `## Summary` section.
pub fn body_summary(body: &str, headings: &BodyHeadings) -> Result<(), ForgeError> {
    if has_non_empty_section(body, &headings.summary) {
        Ok(())
    } else {
        Err(ForgeError::validation(
            schema(),
            "body_missing_summary",
            format!(
                "body is missing a non-empty '{heading}' section",
                heading = headings.summary
            ),
            Some(format!("rule=non-empty H2 '{}' section", headings.summary)),
        ))
    }
}

/// Rule 2b — body contains a non-empty H2 `## Test plan` section.
pub fn body_test_plan(body: &str, headings: &BodyHeadings) -> Result<(), ForgeError> {
    if has_non_empty_section(body, &headings.test_plan) {
        Ok(())
    } else {
        Err(ForgeError::validation(
            schema(),
            "body_missing_test_plan",
            format!(
                "body is missing a non-empty '{heading}' section",
                heading = headings.test_plan
            ),
            Some(format!(
                "rule=non-empty H2 '{}' section",
                headings.test_plan
            )),
        ))
    }
}

/// Walk `body` line-by-line. Find the configured H2 heading; collect
/// non-blank lines beneath it until either the next H2 or end of input.
/// Returns true iff at least one non-blank content line was found.
fn has_non_empty_section(body: &str, heading: &str) -> bool {
    let mut in_section = false;
    let mut saw_content = false;
    for line in body.lines() {
        let trimmed = line.trim();
        if is_h2_heading(trimmed) {
            if in_section {
                return saw_content;
            }
            if trimmed == heading {
                in_section = true;
                continue;
            }
        }
        if in_section && !trimmed.is_empty() {
            saw_content = true;
        }
    }
    in_section && saw_content
}

fn is_h2_heading(line: &str) -> bool {
    // `## …` exactly — three or more `#` would be H3+ and must not collide
    // with `## Summary`.
    line.starts_with("## ") && !line.starts_with("### ")
}

/// Rule 4 — `git status --porcelain` is empty (no staged, unstaged, or
/// untracked changes).
///
/// `git_status_fn` is injected so tests can stub the porcelain output
/// without spawning git.
pub fn worktree_clean<F>(workdir: &Path, git_status_fn: F) -> Result<(), ForgeError>
where
    F: FnOnce(&Path) -> Result<String, ForgeError>,
{
    let porcelain = git_status_fn(workdir)?;
    if porcelain.lines().all(|l| l.trim().is_empty()) {
        Ok(())
    } else {
        let preview: Vec<&str> = porcelain.lines().filter(|l| !l.trim().is_empty()).collect();
        let detail = preview.join("\n");
        Err(ForgeError::validation(
            schema(),
            "dirty_worktree",
            "worktree is dirty (commit, stash, or discard local changes first)",
            Some(detail),
        ))
    }
}

/// Rule 5 — HEAD has an upstream and matches the upstream's SHA. The
/// `head_state_fn` returns `(head_sha, upstream_sha)` — `Ok(None)` for
/// upstream means "no upstream configured".
pub fn head_pushed<F>(workdir: &Path, head_state_fn: F) -> Result<(), ForgeError>
where
    F: FnOnce(&Path) -> Result<HeadState, ForgeError>,
{
    let state = head_state_fn(workdir)?;
    match state.upstream_sha {
        None => Err(ForgeError::validation(
            schema(),
            "head_not_pushed",
            "HEAD has no upstream tracking branch (push the branch first)",
            None,
        )),
        Some(upstream) if upstream == state.head_sha => Ok(()),
        Some(upstream) => Err(ForgeError::validation(
            schema(),
            "head_not_pushed",
            "HEAD differs from its upstream (push the branch first)",
            Some(format!(
                "head={head}\nupstream={upstream}",
                head = state.head_sha
            )),
        )),
    }
}

/// State pair consumed by [`head_pushed`]. `upstream_sha` is `None` when
/// the branch has no `@{upstream}` tracking ref configured.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeadState {
    pub head_sha: String,
    pub upstream_sha: Option<String>,
}

/// Default porcelain reader used in production. Spawns `git -C <workdir>
/// status --porcelain=v1`. Maps git failures to `SOFTWARE 70` because a
/// missing or broken git binary is an environment invariant, not a
/// lock-down violation.
pub fn git_status_porcelain(workdir: &Path) -> Result<String, ForgeError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(workdir)
        .args(["status", "--porcelain=v1"])
        .output()
        .map_err(|e| {
            ForgeError::software(
                schema(),
                "git status --porcelain failed to spawn",
                Some(e.to_string()),
            )
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        return Err(ForgeError::software(
            schema(),
            "git status --porcelain exited non-zero",
            Some(stderr),
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Default head-state resolver. Reads `HEAD` and `@{upstream}` via git.
pub fn git_head_state(workdir: &Path) -> Result<HeadState, ForgeError> {
    let head_sha = run_git_capture(workdir, &["rev-parse", "HEAD"])?;
    let head_sha = head_sha.trim().to_string();

    let upstream = run_git_capture(workdir, &["rev-parse", "--abbrev-ref", "@{upstream}"]);
    let upstream_sha = match upstream {
        Ok(_) => Some(
            run_git_capture(workdir, &["rev-parse", "@{upstream}"])?
                .trim()
                .to_string(),
        ),
        Err(_) => None,
    };
    Ok(HeadState {
        head_sha,
        upstream_sha,
    })
}

fn run_git_capture(workdir: &Path, args: &[&str]) -> Result<String, ForgeError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(workdir)
        .args(args)
        .output()
        .map_err(|e| {
            ForgeError::software(
                schema(),
                format!("git {} failed to spawn", args.join(" ")),
                Some(e.to_string()),
            )
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        return Err(ForgeError::software(
            schema(),
            format!("git {} exited non-zero", args.join(" ")),
            Some(stderr),
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use std::path::PathBuf;

    fn ok_branch(name: &str) -> BranchPrefix {
        branch_name(name).unwrap_or_else(|e| panic!("expected ok branch '{name}', got {e:?}"))
    }

    fn err_kind(err: ForgeError) -> &'static str {
        err.kind()
    }

    #[test]
    fn branch_name_accepts_feat_and_fix() {
        assert_eq!(ok_branch("feat/forge-cli-v1"), BranchPrefix::Feat);
        assert_eq!(ok_branch("fix/abc-123-mr-body"), BranchPrefix::Fix);
        assert_eq!(ok_branch("feat/a"), BranchPrefix::Feat);
    }

    #[test]
    fn branch_name_rejects_uppercase_slug() {
        let err = branch_name("feat/Mixed-Case").expect_err("uppercase");
        assert_eq!(err_kind(err), "branch_name_invalid");
    }

    #[test]
    fn branch_name_rejects_missing_prefix() {
        let err = branch_name("main").expect_err("no prefix");
        assert_eq!(err_kind(err), "branch_name_invalid");
    }

    #[test]
    fn branch_name_rejects_unknown_prefix() {
        let err = branch_name("docs/release-notes").expect_err("docs/");
        assert_eq!(err_kind(err), "branch_name_invalid");
    }

    #[test]
    fn branch_name_rejects_leading_hyphen() {
        let err = branch_name("feat/-leading-hyphen").expect_err("leading hyphen");
        assert_eq!(err_kind(err), "branch_name_invalid");
    }

    #[test]
    fn branch_name_rejects_empty_slug() {
        let err = branch_name("feat/").expect_err("empty slug");
        assert_eq!(err_kind(err), "branch_name_invalid");
    }

    #[test]
    fn branch_name_rejects_oversized_slug() {
        let slug = "a".repeat(65);
        let err = branch_name(&format!("feat/{slug}")).expect_err("oversized");
        assert_eq!(err_kind(err), "branch_name_invalid");
    }

    #[test]
    fn branch_kind_matches_happy_paths() {
        branch_kind_matches(BranchPrefix::Feat, PrKind::Feature).expect("feat+feature");
        branch_kind_matches(BranchPrefix::Fix, PrKind::Bug).expect("fix+bug");
    }

    #[test]
    fn branch_kind_matches_rejects_crossed_pair() {
        let err = branch_kind_matches(BranchPrefix::Feat, PrKind::Bug).expect_err("feat+bug");
        assert_eq!(err_kind(err), "branch_kind_mismatch");
        let err = branch_kind_matches(BranchPrefix::Fix, PrKind::Feature).expect_err("fix+feat");
        assert_eq!(err_kind(err), "branch_kind_mismatch");
    }

    #[test]
    fn title_length_accepts_short_title() {
        title_length("short and sweet").expect("ok");
    }

    #[test]
    fn title_length_rejects_over_70_chars() {
        let title: String = "a".repeat(71);
        let err = title_length(&title).expect_err("too long");
        assert_eq!(err_kind(err), "title_too_long");
    }

    #[test]
    fn title_length_rejects_empty() {
        let err = title_length("").expect_err("empty");
        assert_eq!(err_kind(err), "title_too_long");
    }

    #[test]
    fn title_length_rejects_trailing_whitespace() {
        let err = title_length("hello ").expect_err("trailing space");
        assert_eq!(err_kind(err), "title_too_long");
    }

    #[test]
    fn title_length_counts_codepoints_not_bytes() {
        // 70 CJK codepoints (each 3 UTF-8 bytes) — under the codepoint cap.
        let title: String = "文".repeat(70);
        title_length(&title).expect("ok at 70 codepoints");
        // 71 codepoints → rejected.
        let title: String = "文".repeat(71);
        let err = title_length(&title).expect_err("71 codepoints");
        assert_eq!(err_kind(err), "title_too_long");
    }

    #[test]
    fn body_summary_accepts_well_formed_body() {
        let body = "## Summary\n\nWhat this PR does.\n\n## Test plan\n\nHow it was verified.\n";
        body_summary(body, &BodyHeadings::default()).expect("summary present");
        body_test_plan(body, &BodyHeadings::default()).expect("test plan present");
    }

    #[test]
    fn body_summary_rejects_when_section_absent() {
        let body = "## Test plan\n\nOnly the test plan here.\n";
        let err = body_summary(body, &BodyHeadings::default()).expect_err("no summary");
        assert_eq!(err_kind(err), "body_missing_summary");
    }

    #[test]
    fn body_summary_rejects_when_section_empty() {
        // Heading present but no content before the next H2.
        let body = "## Summary\n\n## Test plan\n\nthings.\n";
        let err = body_summary(body, &BodyHeadings::default()).expect_err("empty section");
        assert_eq!(err_kind(err), "body_missing_summary");
    }

    #[test]
    fn body_test_plan_rejects_when_section_absent() {
        let body = "## Summary\n\nDescribed it.\n";
        let err = body_test_plan(body, &BodyHeadings::default()).expect_err("no test plan");
        assert_eq!(err_kind(err), "body_missing_test_plan");
    }

    #[test]
    fn body_test_plan_rejects_when_section_empty() {
        let body = "## Summary\n\nDescribed it.\n\n## Test plan\n";
        let err = body_test_plan(body, &BodyHeadings::default()).expect_err("empty test plan");
        assert_eq!(err_kind(err), "body_missing_test_plan");
    }

    #[test]
    fn body_summary_ignores_h3_headings() {
        // `### Summary` must not satisfy the H2 rule.
        let body = "### Summary\n\nNot an H2.\n\n## Test plan\n\nyes.\n";
        let err = body_summary(body, &BodyHeadings::default()).expect_err("h3 not H2");
        assert_eq!(err_kind(err), "body_missing_summary");
    }

    #[test]
    fn body_headings_respect_custom_overrides() {
        let custom = BodyHeadings {
            summary: "## 摘要".to_string(),
            test_plan: "## 驗證計畫".to_string(),
        };
        let body = "## 摘要\n\n做了什麼。\n\n## 驗證計畫\n\n怎麼驗。\n";
        body_summary(body, &custom).expect("zh summary");
        body_test_plan(body, &custom).expect("zh test plan");
    }

    #[test]
    fn worktree_clean_accepts_empty_porcelain() {
        worktree_clean(&PathBuf::from("."), |_| Ok(String::new())).expect("clean");
        worktree_clean(&PathBuf::from("."), |_| Ok("\n  \n".to_string())).expect("clean+ws");
    }

    #[test]
    fn worktree_clean_rejects_dirty_porcelain() {
        let err = worktree_clean(&PathBuf::from("."), |_| {
            Ok(" M src/lib.rs\n?? tmp/note.txt\n".to_string())
        })
        .expect_err("dirty");
        assert_eq!(err_kind(err), "dirty_worktree");
    }

    #[test]
    fn head_pushed_accepts_matching_shas() {
        head_pushed(&PathBuf::from("."), |_| {
            Ok(HeadState {
                head_sha: "deadbeef".into(),
                upstream_sha: Some("deadbeef".into()),
            })
        })
        .expect("clean");
    }

    #[test]
    fn head_pushed_rejects_missing_upstream() {
        let err = head_pushed(&PathBuf::from("."), |_| {
            Ok(HeadState {
                head_sha: "deadbeef".into(),
                upstream_sha: None,
            })
        })
        .expect_err("no upstream");
        assert_eq!(err_kind(err), "head_not_pushed");
    }

    #[test]
    fn head_pushed_rejects_divergent_shas() {
        let err = head_pushed(&PathBuf::from("."), |_| {
            Ok(HeadState {
                head_sha: "aaaaaaaa".into(),
                upstream_sha: Some("bbbbbbbb".into()),
            })
        })
        .expect_err("divergent");
        assert_eq!(err_kind(err), "head_not_pushed");
    }

    #[test]
    fn pr_kind_round_trips_strings() {
        assert_eq!(PrKind::parse("feature"), Some(PrKind::Feature));
        assert_eq!(PrKind::parse("bug"), Some(PrKind::Bug));
        assert_eq!(PrKind::parse("nope"), None);
        assert_eq!(PrKind::Feature.as_str(), "feature");
        assert_eq!(PrKind::Bug.as_str(), "bug");
    }
}
