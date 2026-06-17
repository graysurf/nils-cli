use std::fmt;

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
pub const LOCAL_PATH_MAX_HITS: usize = 20;

/// Env var that disables the local-path scan after a verified false positive.
/// Deliberately distinct from the file-write hook's `SKIP_PORTABLE_PATH_SCAN`
/// so bypassing one egress layer never silently disables the other.
pub const ALLOW_LOCAL_PATH_ENV: &str = "FORGE_CLI_ALLOW_LOCAL_PATH";

/// Stable machine-readable error kind for provider-bound local path failures.
pub const LOCAL_PATH_ERROR_KIND: &str = "local_path_present";

/// One machine-local home path found in provider-bound text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalPathHit {
    /// 1-based line number within the scanned text.
    pub line: usize,
    /// The offending path with trailing sentence punctuation stripped. This is
    /// kept for tests and tooling; provider-facing diagnostics must not render it.
    pub sample: String,
    /// The `$HOME`-relative replacement suggested in diagnostics.
    pub suggestion: String,
}

/// Provider-bound payload violation report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalPathError {
    source: String,
    hits: Vec<LocalPathHit>,
}

impl LocalPathError {
    pub fn new(source: impl Into<String>, hits: Vec<LocalPathHit>) -> Self {
        Self {
            source: source.into(),
            hits,
        }
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn hits(&self) -> &[LocalPathHit] {
        &self.hits
    }

    pub fn message(&self) -> String {
        format!(
            "{source} contains {n} machine-local home path(s); use $HOME-relative paths",
            source = self.source,
            n = self.hits.len()
        )
    }

    pub fn detail(&self) -> String {
        render_local_path_detail(&self.hits)
    }

    pub fn full_message(&self) -> String {
        format!("{}.\n{}", self.message(), self.detail())
    }
}

impl fmt::Display for LocalPathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.full_message())
    }
}

impl std::error::Error for LocalPathError {}

/// Scan `text` for machine-local home paths (`/Users/<owner>/...`,
/// `/home/<owner>/...`). Pure, with no env gate and no I/O, so every detection
/// branch is unit-testable. The literal allowlist still applies here.
pub fn scan_local_paths(text: &str) -> Vec<LocalPathHit> {
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
                    continue;
                }

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

    found.sort_by_key(|(line, start, _)| (*line, *start));
    let mut seen: Vec<(usize, String)> = Vec::new();
    let mut deduped = Vec::new();
    for (_, _, hit) in found {
        let key = (hit.line, hit.sample.clone());
        if seen.iter().any(|existing| existing == &key) {
            continue;
        }
        seen.push(key);
        deduped.push(hit);
    }
    deduped
}

pub fn validate_no_local_paths(text: &str, source: &str) -> Result<(), LocalPathError> {
    if local_path_scan_disabled() {
        return Ok(());
    }
    let hits = scan_local_paths(text);
    if hits.is_empty() {
        Ok(())
    } else {
        Err(LocalPathError::new(source, hits))
    }
}

pub fn local_path_scan_disabled() -> bool {
    matches!(std::env::var(ALLOW_LOCAL_PATH_ENV), Ok(v) if v == "1")
}

pub fn render_local_path_detail(hits: &[LocalPathHit]) -> String {
    let mut lines: Vec<String> = hits
        .iter()
        .take(LOCAL_PATH_MAX_HITS)
        .map(|hit| {
            format!(
                "line {line}: use {suggestion}",
                line = hit.line,
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

fn is_allowed_local_path(sample: &str) -> bool {
    LOCAL_PATH_ALLOWLIST
        .iter()
        .any(|prefix| sample == *prefix || sample.starts_with(&format!("{prefix}/")))
}

#[cfg(test)]
mod tests {
    use super::{
        ALLOW_LOCAL_PATH_ENV, LOCAL_PATH_MAX_HITS, LocalPathHit, render_local_path_detail,
        scan_local_paths, validate_no_local_paths,
    };

    #[test]
    fn scan_local_paths_allowlists_container_and_runner_roots() {
        assert!(scan_local_paths("/home/agent/run and /home/linuxbrew/.linuxbrew/bin").is_empty());
        assert!(scan_local_paths("CI artifact at /home/runner/work/repo").is_empty());
        assert_eq!(scan_local_paths("/home/runners/x").len(), 1);
    }

    #[test]
    fn scan_local_paths_strips_trailing_sentence_punctuation() {
        let hits = scan_local_paths("the path is /Users/example/notes.md.");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].sample, "/Users/example/notes.md");
        assert_eq!(hits[0].suggestion, "$HOME/notes.md");
    }

    #[test]
    fn scan_local_paths_stops_tail_at_delimiters() {
        let hits = scan_local_paths("run `/Users/example/bin/tool` now");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].sample, "/Users/example/bin/tool");
    }

    #[test]
    fn scan_local_paths_owner_only_without_tail() {
        let hits = scan_local_paths("home is /Users/example");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].sample, "/Users/example");
        assert_eq!(hits[0].suggestion, "$HOME");
    }

    #[test]
    fn scan_local_paths_ignores_bare_roots_without_owner() {
        assert!(scan_local_paths("the /Users/ directory or /home/ mount").is_empty());
    }

    #[test]
    fn scan_local_paths_reports_line_numbers_and_dedups_per_line() {
        let text =
            "line one is clean\nsee /Users/example/a and /Users/example/a again\n/home/bob/c";
        let hits = scan_local_paths(text);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].line, 2);
        assert_eq!(hits[0].sample, "/Users/example/a");
        assert_eq!(hits[1].line, 3);
        assert_eq!(hits[1].sample, "/home/bob/c");
    }

    #[test]
    fn render_local_path_detail_suggests_home_without_echoing_personal_path() {
        let hits = scan_local_paths("see /Users/example/Project/private");
        let detail = render_local_path_detail(&hits);
        assert!(
            detail.contains("line 1: use $HOME/Project/private"),
            "{detail}"
        );
        assert!(!detail.contains("/Users/example"), "{detail}");
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
        assert!(detail.contains(ALLOW_LOCAL_PATH_ENV), "{detail}");
    }

    #[test]
    fn validation_error_full_message_names_source_without_raw_path() {
        let err = validate_no_local_paths("logs under /home/alice/notes", "comment")
            .expect_err("local path");
        let full = err.full_message();
        assert!(
            full.starts_with("comment contains 1 machine-local"),
            "{full}"
        );
        assert!(full.contains("$HOME/notes"), "{full}");
        assert!(!full.contains("/home/alice"), "{full}");
    }
}
