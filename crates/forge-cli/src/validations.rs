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
use serde::Serialize;

use crate::cli::BINARY;
use crate::error::ForgeError;

/// Hint appended to the `body_missing_*` validation `details` pointing the
/// operator at the body scaffold so a missing section is one command away
/// from fixed.
pub const BODY_SCAFFOLD_HINT: &str =
    "scaffold a valid body with `agent-runtime pr-body render --kind <kind>`";

/// PR/MR kind declared by the caller via `--kind`. Drives the
/// `branch_kind_matches` rule plus the macro in Sprint 6. The set tracks
/// the Conventional Commits type whitelist (`feature`, `bug`, `chore`,
/// `docs`, `ci`, `refactor`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrKind {
    Feature,
    Bug,
    Chore,
    Docs,
    Ci,
    Refactor,
}

impl PrKind {
    /// Render the kind to the lower-case enum literal used in envelopes and
    /// argv.
    pub fn as_str(self) -> &'static str {
        match self {
            PrKind::Feature => "feature",
            PrKind::Bug => "bug",
            PrKind::Chore => "chore",
            PrKind::Docs => "docs",
            PrKind::Ci => "ci",
            PrKind::Refactor => "refactor",
        }
    }

    /// Parse the `--kind` flag value. Anything outside the spec's enum is a
    /// usage error and is handled at the clap layer; this helper exists for
    /// internal callers that already hold the string.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "feature" => Some(PrKind::Feature),
            "bug" => Some(PrKind::Bug),
            "chore" => Some(PrKind::Chore),
            "docs" => Some(PrKind::Docs),
            "ci" => Some(PrKind::Ci),
            "refactor" => Some(PrKind::Refactor),
            _ => None,
        }
    }
}

/// Branch prefix recovered from a branch name that matches the
/// `branch_name` rule. The set tracks the Conventional Commits type
/// whitelist (`feat`, `fix`, `chore`, `docs`, `ci`, `refactor`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BranchPrefix {
    Feat,
    Fix,
    Chore,
    Docs,
    Ci,
    Refactor,
}

impl BranchPrefix {
    pub fn as_str(self) -> &'static str {
        match self {
            BranchPrefix::Feat => "feat",
            BranchPrefix::Fix => "fix",
            BranchPrefix::Chore => "chore",
            BranchPrefix::Docs => "docs",
            BranchPrefix::Ci => "ci",
            BranchPrefix::Refactor => "refactor",
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

/// Rule 1a — branch name matches
/// `^(feat|fix|chore|docs|ci|refactor)/[a-z0-9][a-z0-9.-]{1,63}$`.
///
/// The slug character class permits `.` so release-style branches such as
/// `chore/release-0.22.1` validate without forcing kebab-case versions on
/// callers. Returns the matched prefix so callers can chain into
/// [`branch_kind_matches`] without re-parsing.
pub fn branch_name(branch: &str) -> Result<BranchPrefix, ForgeError> {
    let (prefix, rest) = match branch.split_once('/') {
        Some((p, r)) => (p, r),
        None => {
            return Err(branch_name_err(
                branch,
                "missing one of feat|fix|chore|docs|ci|refactor prefix",
            ));
        }
    };

    let prefix = match prefix {
        "feat" => BranchPrefix::Feat,
        "fix" => BranchPrefix::Fix,
        "chore" => BranchPrefix::Chore,
        "docs" => BranchPrefix::Docs,
        "ci" => BranchPrefix::Ci,
        "refactor" => BranchPrefix::Refactor,
        other => {
            return Err(branch_name_err(
                branch,
                &format!(
                    "unknown prefix '{other}' (expected one of feat|fix|chore|docs|ci|refactor)"
                ),
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
        if !(b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'.') {
            return Err(branch_name_err(
                branch,
                "slug must be lowercase [a-z0-9.-] only",
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
        Some("rule=^(feat|fix|chore|docs|ci|refactor)/[a-z0-9][a-z0-9.-]{1,63}$".to_string()),
    )
}

/// Rule 1b — declared `--kind` matches the branch prefix one-for-one
/// (`feature` ↔ `feat/*`, `bug` ↔ `fix/*`, `chore` ↔ `chore/*`,
/// `docs` ↔ `docs/*`, `ci` ↔ `ci/*`, `refactor` ↔ `refactor/*`).
pub fn branch_kind_matches(prefix: BranchPrefix, kind: PrKind) -> Result<(), ForgeError> {
    let ok = matches!(
        (prefix, kind),
        (BranchPrefix::Feat, PrKind::Feature)
            | (BranchPrefix::Fix, PrKind::Bug)
            | (BranchPrefix::Chore, PrKind::Chore)
            | (BranchPrefix::Docs, PrKind::Docs)
            | (BranchPrefix::Ci, PrKind::Ci)
            | (BranchPrefix::Refactor, PrKind::Refactor)
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
                "feature -> feat/*, bug -> fix/*, chore -> chore/*, docs -> docs/*, ci -> ci/*, refactor -> refactor/* (branch_prefix={p}, kind={k})",
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
            Some(format!(
                "rule=non-empty H2 '{}' section; {BODY_SCAFFOLD_HINT}",
                headings.summary
            )),
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
                "rule=non-empty H2 '{}' section; {BODY_SCAFFOLD_HINT}",
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

/// Rule 2 (aggregate) — body must contain non-empty `## Summary` AND
/// `## Test plan` sections.
///
/// When exactly one section is missing, this returns that section's canonical
/// error (`body_missing_summary` / `body_missing_test_plan`) so existing
/// single-section consumers keep matching on the same `error.kind`. When both
/// are missing, it returns a single `body_missing_sections` error enumerating
/// every missing section, with the per-section codes preserved in `details`
/// so the additive aggregation never hides which sections failed.
pub fn body_sections(body: &str, headings: &BodyHeadings) -> Result<(), ForgeError> {
    let summary = body_summary(body, headings);
    let test_plan = body_test_plan(body, headings);
    match (summary, test_plan) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(err), Ok(())) | (Ok(()), Err(err)) => Err(err),
        (Err(summary_err), Err(test_plan_err)) => Err(ForgeError::validation(
            schema(),
            "body_missing_sections",
            format!(
                "body is missing required sections: '{}' and '{}'",
                headings.summary, headings.test_plan
            ),
            Some(format!(
                "missing={},{}; {BODY_SCAFFOLD_HINT}",
                summary_err.kind(),
                test_plan_err.kind()
            )),
        )),
    }
}

/// Literal path prefixes that are portable container / CI-runner roots, not
/// user-specific home paths. Mirrors the allowlist in the repo-side
/// `portable-paths-scan.py` hook, plus `/home/runner` for pasted CI logs.
const LOCAL_PATH_ALLOWLIST: &[&str] = &["/home/agent", "/home/linuxbrew", "/home/runner"];

/// The two machine-local home roots the rule scans for. ASCII-only, so byte
/// offsets from `str::match_indices` always land on char boundaries.
const LOCAL_PATH_ROOTS: &[&str] = &["/Users/", "/home/"];

/// Closing delimiters (whitespace is handled separately) that terminate a path
/// tail. Mirrors the hook's `[^\s`'"<>)\]}]` tail exclusion set.
const LOCAL_PATH_DELIMITERS: &[char] = &['`', '\'', '"', '<', '>', ')', ']', '}'];

/// Trailing punctuation stripped from a matched path so a path ending a
/// sentence does not capture the period. Mirrors the hook's
/// `TRAILING_PUNCTUATION`.
const LOCAL_PATH_TRAILING_PUNCT: &[char] = &['.', ',', ';', ':', ')', ']', '}', '\'', '"', '`'];

/// Cap on enumerated hits in the error `detail`, matching the hook's
/// `MAX_FORMATTED_HITS` so a pathological body cannot produce an unbounded
/// message.
const LOCAL_PATH_MAX_HITS: usize = 20;

/// Env var that disables the local-path scan after a verified false positive.
/// Deliberately distinct from the file-write hook's `SKIP_PORTABLE_PATH_SCAN`
/// so bypassing one egress layer never silently disables the other.
pub const ALLOW_LOCAL_PATH_ENV: &str = "FORGE_CLI_ALLOW_LOCAL_PATH";

/// One machine-local home path found in posted text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalPathHit {
    /// 1-based line number within the scanned text.
    pub line: usize,
    /// The offending path with trailing sentence punctuation stripped.
    pub sample: String,
    /// The `$HOME`-relative replacement suggested in the error detail.
    pub suggestion: String,
}

/// Scan `text` for machine-local home paths (`/Users/<owner>/…`,
/// `/home/<owner>/…`). Pure — no env gate and no I/O — so every detection branch
/// is unit-testable. The literal allowlist still applies here: an allowlisted
/// container path is never a hit regardless of the escape hatch.
fn scan_local_paths(text: &str) -> Vec<LocalPathHit> {
    let mut found: Vec<(usize, usize, LocalPathHit)> = Vec::new();
    for (idx, line) in text.lines().enumerate() {
        let line_no = idx + 1;
        for root in LOCAL_PATH_ROOTS {
            for (start, _) in line.match_indices(root) {
                let owner_start = start + root.len();
                let owner_len: usize = line[owner_start..]
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
                    .map(char::len_utf8)
                    .sum();
                if owner_len == 0 {
                    // A bare `/Users/` or `/home/` with no owner segment is not
                    // a user home path.
                    continue;
                }
                // The tail is only part of the path when an actual `/` follows
                // the owner segment, matching the hook's optional `/…` group.
                let tail_start = owner_start + owner_len;
                let after_owner = &line[tail_start..];
                let tail_len: usize = if after_owner.starts_with('/') {
                    after_owner
                        .chars()
                        .take_while(|c| !c.is_whitespace() && !LOCAL_PATH_DELIMITERS.contains(c))
                        .map(char::len_utf8)
                        .sum()
                } else {
                    0
                };
                let matched = &line[start..tail_start + tail_len];
                let sample = matched.trim_end_matches(LOCAL_PATH_TRAILING_PUNCT);
                if sample.is_empty() || is_allowed_local_path(sample) {
                    continue;
                }
                let prefix_len = root.len() + owner_len;
                let tail = sample.get(prefix_len..).unwrap_or("");
                found.push((
                    line_no,
                    start,
                    LocalPathHit {
                        line: line_no,
                        sample: sample.to_string(),
                        suggestion: format!("$HOME{tail}"),
                    },
                ));
            }
        }
    }
    // Report left-to-right, top-to-bottom; collapse a path repeated on one line
    // to a single hit so the detail stays signal-dense.
    found.sort_by_key(|(line, start, _)| (*line, *start));
    let mut seen = std::collections::HashSet::new();
    found
        .into_iter()
        .filter(|(_, _, hit)| seen.insert((hit.line, hit.sample.clone())))
        .map(|(_, _, hit)| hit)
        .collect()
}

fn is_allowed_local_path(sample: &str) -> bool {
    LOCAL_PATH_ALLOWLIST
        .iter()
        .any(|prefix| sample == *prefix || sample.starts_with(&format!("{prefix}/")))
}

fn local_path_scan_disabled() -> bool {
    matches!(std::env::var(ALLOW_LOCAL_PATH_ENV), Ok(v) if v == "1")
}

fn render_local_path_detail(hits: &[LocalPathHit]) -> String {
    let mut lines: Vec<String> = hits
        .iter()
        .take(LOCAL_PATH_MAX_HITS)
        .map(|hit| {
            format!(
                "line {line}: {sample} -> use {suggestion}",
                line = hit.line,
                sample = hit.sample,
                suggestion = hit.suggestion,
            )
        })
        .collect();
    let extra = hits.len().saturating_sub(LOCAL_PATH_MAX_HITS);
    if extra > 0 {
        lines.push(format!("... {extra} more local path(s) omitted"));
    }
    lines.push(format!(
        "set {ALLOW_LOCAL_PATH_ENV}=1 to bypass after verifying a false positive"
    ));
    lines.join("\n")
}

/// Rule 11 — posted text (title / body / comment) MUST NOT embed a machine-local
/// home path (`/Users/<owner>/…`, `/home/<owner>/…`). This mirrors the repo-side
/// `portable-paths-scan.py` file-write hook so the forge egress path enforces
/// the same portability rule the hook already enforces on disk. `field` names
/// the offending input (`title` / `body` / `comment`) in the message; the
/// `detail` enumerates each offending line plus its `$HOME`-relative fix. Set
/// `FORGE_CLI_ALLOW_LOCAL_PATH=1` to bypass a verified false positive.
pub fn no_local_path(text: &str, field: &str) -> Result<(), ForgeError> {
    if local_path_scan_disabled() {
        return Ok(());
    }
    let hits = scan_local_paths(text);
    if hits.is_empty() {
        return Ok(());
    }
    Err(ForgeError::validation(
        schema(),
        "local_path_present",
        format!(
            "{field} contains {n} machine-local home path(s); use $HOME-relative paths",
            n = hits.len()
        ),
        Some(render_local_path_detail(&hits)),
    ))
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

/// One rule's verdict in a non-short-circuiting local preflight. `code` and
/// `message` are populated only on failure (mirroring the rule's
/// [`ForgeError`] `kind` and message). Serialized additively into the
/// `pr deliver --dry-run` envelope's `local_preflight` block.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RuleVerdict {
    pub rule: &'static str,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl RuleVerdict {
    fn from_result(rule: &'static str, result: Result<(), ForgeError>) -> Self {
        match result {
            Ok(()) => Self {
                rule,
                ok: true,
                code: None,
                message: None,
            },
            Err(err) => Self {
                rule,
                ok: false,
                code: Some(err.kind().to_string()),
                message: Some(err.to_string()),
            },
        }
    }

    fn not_evaluated(rule: &'static str, why: &str) -> Self {
        Self {
            rule,
            ok: false,
            code: None,
            message: Some(format!("not evaluated: {why}")),
        }
    }
}

/// Resolved inputs for the local preflight. The caller resolves the head
/// branch and body up front so the runner stays a pure string/`git`
/// evaluation with no provider calls.
#[derive(Debug, Clone)]
pub struct PreflightInputs<'a> {
    pub branch: &'a str,
    pub kind: PrKind,
    pub title: &'a str,
    pub body: &'a str,
    pub headings: &'a BodyHeadings,
}

/// Evaluate the non-mutating lock-down rules (1a, 1b, 3, 2a, 2b, 4, 5)
/// without returning early on the first failure, collecting a per-rule
/// verdict for each. This is the faithful-preflight runner behind
/// `pr deliver --dry-run`: it never invokes a provider backend, only local
/// string checks plus the injected local `git` readers. A `git` reader that
/// errors (e.g. not a repo) surfaces as that rule's failing verdict rather
/// than aborting the sweep.
pub fn run_local_preflight<FS, FH>(
    inputs: &PreflightInputs<'_>,
    workdir: &Path,
    git_status_fn: FS,
    head_state_fn: FH,
) -> Vec<RuleVerdict>
where
    FS: FnOnce(&Path) -> Result<String, ForgeError>,
    FH: FnOnce(&Path) -> Result<HeadState, ForgeError>,
{
    let mut verdicts = Vec::with_capacity(9);

    // Rule 1a — branch name. Capture the prefix for Rule 1b.
    let branch_result = branch_name(inputs.branch);
    let prefix = branch_result.as_ref().ok().copied();
    verdicts.push(RuleVerdict::from_result(
        "branch_name",
        branch_result.map(|_| ()),
    ));

    // Rule 1b — kind matches branch prefix. Only checkable once 1a resolves a
    // prefix; otherwise reported as not-evaluated so the sweep stays complete.
    verdicts.push(match prefix {
        Some(prefix) => {
            RuleVerdict::from_result("branch_kind", branch_kind_matches(prefix, inputs.kind))
        }
        None => RuleVerdict::not_evaluated("branch_kind", "branch name is invalid"),
    });

    // Rule 3 — title length.
    verdicts.push(RuleVerdict::from_result(
        "title_length",
        title_length(inputs.title),
    ));

    // Rule 11 (title) — no machine-local home path in the title.
    verdicts.push(RuleVerdict::from_result(
        "title_local_path",
        no_local_path(inputs.title, "title"),
    ));

    // Rules 2a / 2b — body sections, reported individually so the preflight
    // surfaces every missing section at once.
    verdicts.push(RuleVerdict::from_result(
        "body_summary",
        body_summary(inputs.body, inputs.headings),
    ));
    verdicts.push(RuleVerdict::from_result(
        "body_test_plan",
        body_test_plan(inputs.body, inputs.headings),
    ));

    // Rule 11 (body) — no machine-local home path in the body.
    verdicts.push(RuleVerdict::from_result(
        "body_local_path",
        no_local_path(inputs.body, "body"),
    ));

    // Rule 4 — clean worktree (local git read).
    verdicts.push(RuleVerdict::from_result(
        "worktree_clean",
        worktree_clean(workdir, git_status_fn),
    ));

    // Rule 5 — head pushed / matches upstream (local git read).
    verdicts.push(RuleVerdict::from_result(
        "head_pushed",
        head_pushed(workdir, head_state_fn),
    ));

    verdicts
}

/// Resolve the current branch via `git -C <workdir> rev-parse --abbrev-ref
/// HEAD`. Used by `pr deliver --dry-run` to feed the preflight when no
/// explicit `--head` is given.
pub fn git_current_branch(workdir: &Path) -> Result<String, ForgeError> {
    run_git_capture(workdir, &["rev-parse", "--abbrev-ref", "HEAD"]).map(|s| s.trim().to_string())
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
    fn branch_name_accepts_full_conventional_commits_set() {
        assert_eq!(ok_branch("feat/forge-cli-v1"), BranchPrefix::Feat);
        assert_eq!(ok_branch("fix/abc-123-mr-body"), BranchPrefix::Fix);
        assert_eq!(ok_branch("feat/a"), BranchPrefix::Feat);
        assert_eq!(ok_branch("chore/release-0.22.1"), BranchPrefix::Chore);
        assert_eq!(ok_branch("docs/release-notes"), BranchPrefix::Docs);
        assert_eq!(ok_branch("ci/upgrade-runners"), BranchPrefix::Ci);
        assert_eq!(
            ok_branch("refactor/forge-cli-validations"),
            BranchPrefix::Refactor,
        );
    }

    #[test]
    fn branch_name_accepts_dot_in_slug() {
        // SemVer-shaped release branches must validate without forcing the
        // bump skill to kebab-case the version segment.
        assert_eq!(ok_branch("chore/release-1.2.3"), BranchPrefix::Chore);
        assert_eq!(ok_branch("fix/2.0.0-hotfix"), BranchPrefix::Fix);
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
        let err = branch_name("hotfix/something").expect_err("hotfix/");
        assert_eq!(err_kind(err), "branch_name_invalid");
        let err = branch_name("issue/s1-t1-foo").expect_err("issue/");
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
        branch_kind_matches(BranchPrefix::Chore, PrKind::Chore).expect("chore+chore");
        branch_kind_matches(BranchPrefix::Docs, PrKind::Docs).expect("docs+docs");
        branch_kind_matches(BranchPrefix::Ci, PrKind::Ci).expect("ci+ci");
        branch_kind_matches(BranchPrefix::Refactor, PrKind::Refactor).expect("refactor+refactor");
    }

    #[test]
    fn branch_kind_matches_rejects_crossed_pair() {
        let err = branch_kind_matches(BranchPrefix::Feat, PrKind::Bug).expect_err("feat+bug");
        assert_eq!(err_kind(err), "branch_kind_mismatch");
        let err = branch_kind_matches(BranchPrefix::Fix, PrKind::Feature).expect_err("fix+feat");
        assert_eq!(err_kind(err), "branch_kind_mismatch");
        let err =
            branch_kind_matches(BranchPrefix::Chore, PrKind::Feature).expect_err("chore+feat");
        assert_eq!(err_kind(err), "branch_kind_mismatch");
        let err =
            branch_kind_matches(BranchPrefix::Docs, PrKind::Refactor).expect_err("docs+refactor");
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

    fn clean_status(_: &Path) -> Result<String, ForgeError> {
        Ok(String::new())
    }

    fn pushed_head(_: &Path) -> Result<HeadState, ForgeError> {
        Ok(HeadState {
            head_sha: "deadbeef".into(),
            upstream_sha: Some("deadbeef".into()),
        })
    }

    fn unpushed_head(_: &Path) -> Result<HeadState, ForgeError> {
        Ok(HeadState {
            head_sha: "deadbeef".into(),
            upstream_sha: None,
        })
    }

    #[test]
    fn body_sections_accepts_complete_body() {
        let body = "## Summary\n\nWhat.\n\n## Test plan\n\nHow.\n";
        body_sections(body, &BodyHeadings::default()).expect("both present");
    }

    #[test]
    fn body_sections_returns_canonical_code_when_only_one_missing() {
        // Existing single-section consumers keep matching the canonical codes.
        let only_test_plan = "## Test plan\n\nHow.\n";
        let err = body_sections(only_test_plan, &BodyHeadings::default()).expect_err("no summary");
        assert_eq!(err_kind(err), "body_missing_summary");

        let only_summary = "## Summary\n\nWhat.\n";
        let err = body_sections(only_summary, &BodyHeadings::default()).expect_err("no test plan");
        assert_eq!(err_kind(err), "body_missing_test_plan");
    }

    #[test]
    fn body_sections_aggregates_when_both_missing() {
        let body = "no required sections here\n";
        let err = body_sections(body, &BodyHeadings::default()).expect_err("both missing");
        assert_eq!(err.kind(), "body_missing_sections");
        // Message enumerates both headings; details preserve the per-section
        // codes so the aggregation never hides which sections failed.
        assert!(err.message().contains("## Summary"), "{}", err.message());
        assert!(err.message().contains("## Test plan"), "{}", err.message());
        let detail = err.detail().expect("detail present");
        assert!(detail.contains("body_missing_summary"), "{detail}");
        assert!(detail.contains("body_missing_test_plan"), "{detail}");
    }

    fn verdict<'a>(verdicts: &'a [RuleVerdict], rule: &str) -> &'a RuleVerdict {
        verdicts
            .iter()
            .find(|v| v.rule == rule)
            .unwrap_or_else(|| panic!("missing verdict for {rule}"))
    }

    #[test]
    fn run_local_preflight_all_green_for_valid_inputs() {
        let headings = BodyHeadings::default();
        let inputs = PreflightInputs {
            branch: "feat/demo",
            kind: PrKind::Feature,
            title: "demo",
            body: "## Summary\n\nx\n\n## Test plan\n\ny\n",
            headings: &headings,
        };
        let verdicts = run_local_preflight(&inputs, Path::new("."), clean_status, pushed_head);
        assert_eq!(verdicts.len(), 9);
        assert!(verdicts.iter().all(|v| v.ok), "{verdicts:?}");
    }

    #[test]
    fn run_local_preflight_reports_every_failure_without_short_circuit() {
        // Empty body + unpushed head must both surface in one sweep.
        let headings = BodyHeadings::default();
        let inputs = PreflightInputs {
            branch: "feat/demo",
            kind: PrKind::Feature,
            title: "demo",
            body: "",
            headings: &headings,
        };
        let verdicts = run_local_preflight(&inputs, Path::new("."), clean_status, unpushed_head);
        assert!(verdict(&verdicts, "branch_name").ok);
        assert!(verdict(&verdicts, "branch_kind").ok);
        assert!(verdict(&verdicts, "title_length").ok);
        assert_eq!(
            verdict(&verdicts, "body_summary").code.as_deref(),
            Some("body_missing_summary")
        );
        assert_eq!(
            verdict(&verdicts, "body_test_plan").code.as_deref(),
            Some("body_missing_test_plan")
        );
        assert!(verdict(&verdicts, "worktree_clean").ok);
        assert_eq!(
            verdict(&verdicts, "head_pushed").code.as_deref(),
            Some("head_not_pushed")
        );
    }

    #[test]
    fn run_local_preflight_marks_branch_kind_not_evaluated_on_invalid_branch() {
        let headings = BodyHeadings::default();
        let inputs = PreflightInputs {
            branch: "not-a-valid-branch",
            kind: PrKind::Feature,
            title: "demo",
            body: "## Summary\n\nx\n\n## Test plan\n\ny\n",
            headings: &headings,
        };
        let verdicts = run_local_preflight(&inputs, Path::new("."), clean_status, pushed_head);
        let branch = verdict(&verdicts, "branch_name");
        assert!(!branch.ok);
        assert_eq!(branch.code.as_deref(), Some("branch_name_invalid"));
        let kind = verdict(&verdicts, "branch_kind");
        assert!(!kind.ok);
        assert!(kind.code.is_none(), "kind not evaluated -> no code");
        assert!(
            kind.message
                .as_deref()
                .unwrap_or("")
                .contains("not evaluated"),
            "{kind:?}"
        );
    }

    #[test]
    fn pr_kind_round_trips_strings() {
        assert_eq!(PrKind::parse("feature"), Some(PrKind::Feature));
        assert_eq!(PrKind::parse("bug"), Some(PrKind::Bug));
        assert_eq!(PrKind::parse("chore"), Some(PrKind::Chore));
        assert_eq!(PrKind::parse("docs"), Some(PrKind::Docs));
        assert_eq!(PrKind::parse("ci"), Some(PrKind::Ci));
        assert_eq!(PrKind::parse("refactor"), Some(PrKind::Refactor));
        assert_eq!(PrKind::parse("nope"), None);
        assert_eq!(PrKind::Feature.as_str(), "feature");
        assert_eq!(PrKind::Bug.as_str(), "bug");
        assert_eq!(PrKind::Chore.as_str(), "chore");
        assert_eq!(PrKind::Docs.as_str(), "docs");
        assert_eq!(PrKind::Ci.as_str(), "ci");
        assert_eq!(PrKind::Refactor.as_str(), "refactor");
    }

    #[test]
    fn no_local_path_accepts_portable_text() {
        no_local_path("see $HOME/Project/foo and ./relative/path", "body").expect("portable");
        no_local_path("no paths here at all", "title").expect("no paths");
        no_local_path("", "body").expect("empty");
    }

    #[test]
    fn no_local_path_rejects_macos_home_path() {
        let err = no_local_path("clone into /Users/terry/Project/x", "body").expect_err("macos");
        assert_eq!(err.kind(), "local_path_present");
        let detail = err.detail().expect("detail present");
        assert!(detail.contains("/Users/terry/Project/x"), "{detail}");
        assert!(detail.contains("use $HOME/Project/x"), "{detail}");
    }

    #[test]
    fn no_local_path_rejects_linux_home_path() {
        let err = no_local_path("logs under /home/alice/notes", "comment").expect_err("linux");
        assert_eq!(err.kind(), "local_path_present");
        let detail = err.detail().expect("detail present");
        assert!(detail.contains("use $HOME/notes"), "{detail}");
    }

    #[test]
    fn no_local_path_message_names_the_field() {
        let err = no_local_path("/Users/terry", "title").expect_err("title field");
        assert!(
            err.message().starts_with("title contains"),
            "{}",
            err.message()
        );
    }

    #[test]
    fn scan_local_paths_allowlists_container_and_runner_roots() {
        // Allowlisted literal roots and their children never hit.
        assert!(scan_local_paths("/home/agent/run and /home/linuxbrew/.linuxbrew/bin").is_empty());
        assert!(scan_local_paths("CI artifact at /home/runner/work/repo").is_empty());
        // A non-allowlisted owner under /home still hits.
        assert_eq!(scan_local_paths("/home/runners/x").len(), 1);
    }

    #[test]
    fn scan_local_paths_strips_trailing_sentence_punctuation() {
        let hits = scan_local_paths("the path is /Users/terry/notes.md.");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].sample, "/Users/terry/notes.md");
        assert_eq!(hits[0].suggestion, "$HOME/notes.md");
    }

    #[test]
    fn scan_local_paths_stops_tail_at_delimiters() {
        // A backtick-fenced path terminates at the closing delimiter.
        let hits = scan_local_paths("run `/Users/terry/bin/tool` now");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].sample, "/Users/terry/bin/tool");
    }

    #[test]
    fn scan_local_paths_owner_only_without_tail() {
        let hits = scan_local_paths("home is /Users/terry");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].sample, "/Users/terry");
        assert_eq!(hits[0].suggestion, "$HOME");
    }

    #[test]
    fn scan_local_paths_ignores_bare_roots_without_owner() {
        assert!(scan_local_paths("the /Users/ directory or /home/ mount").is_empty());
    }

    #[test]
    fn scan_local_paths_reports_line_numbers_and_dedups_per_line() {
        let text = "line one is clean\nsee /Users/terry/a and /Users/terry/a again\n/home/bob/c";
        let hits = scan_local_paths(text);
        // Repeated identical path on line 2 collapses to one; line 3 adds another.
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].line, 2);
        assert_eq!(hits[0].sample, "/Users/terry/a");
        assert_eq!(hits[1].line, 3);
        assert_eq!(hits[1].sample, "/home/bob/c");
    }

    #[test]
    fn render_local_path_detail_caps_and_appends_escape_hatch() {
        let hits: Vec<LocalPathHit> = (1..=LOCAL_PATH_MAX_HITS + 5)
            .map(|n| LocalPathHit {
                line: n,
                sample: format!("/Users/u/p{n}"),
                suggestion: format!("$HOME/p{n}"),
            })
            .collect();
        let detail = render_local_path_detail(&hits);
        assert!(
            detail.contains("... 5 more local path(s) omitted"),
            "{detail}"
        );
        assert!(detail.contains("FORGE_CLI_ALLOW_LOCAL_PATH=1"), "{detail}");
    }
}
