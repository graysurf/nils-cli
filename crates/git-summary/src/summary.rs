use anyhow::Result;
use nils_common::git as common_git;
use serde::Serialize;
use std::collections::BTreeMap;

use crate::dates::{build_range_args, validate_date};
use crate::git::run_git;

const SEPARATOR: &str = "----------------------------------------------------------------------------------------------------------------------------------------";
const RECORD_SEPARATOR: char = '\u{1e}';
const FIELD_SEPARATOR: char = '\u{1f}';

#[derive(Debug, Clone, Serialize)]
pub struct SummaryPayload {
    pub range: SummaryRange,
    pub mailmap: bool,
    pub authors: Vec<AuthorSummary>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SummaryRange {
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AuthorSummary {
    pub name: String,
    pub email: String,
    pub added: i64,
    pub deleted: i64,
    pub net: i64,
    pub commits: i64,
    pub first: String,
    pub last: String,
}

#[derive(Debug)]
pub struct SummaryFailure {
    pub code: &'static str,
    pub message: String,
    pub exit_code: i32,
}

impl SummaryFailure {
    fn data(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            exit_code: nils_common::cli_contract::exit::DATA,
        }
    }

    fn runtime(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            exit_code: nils_common::cli_contract::exit::RUNTIME,
        }
    }
}

pub fn collect_summary(
    label: impl Into<String>,
    since: Option<&str>,
    until: Option<&str>,
    use_mailmap: bool,
) -> std::result::Result<SummaryPayload, SummaryFailure> {
    if (since.is_some() && until.is_none()) || (since.is_none() && until.is_some()) {
        return Err(SummaryFailure::data(
            "invalid-range",
            "❌ Please provide both start and end dates (YYYY-MM-DD).",
        ));
    }

    if let Some(value) = since
        && let Err(msg) = validate_date(value)
    {
        return Err(SummaryFailure::data("invalid-date", msg));
    }
    if let Some(value) = until
        && let Err(msg) = validate_date(value)
    {
        return Err(SummaryFailure::data("invalid-date", msg));
    }

    if let (Some(start), Some(end)) = (since, until)
        && start > end
    {
        return Err(SummaryFailure::data(
            "invalid-range",
            "❌ Start date must be on or before end date.",
        ));
    }

    let log_args = match (since, until) {
        (Some(start), Some(end)) => build_range_args(start, end),
        _ => vec!["--no-merges".to_string()],
    };

    let authors = collect_author_rows(&log_args, use_mailmap)
        .map_err(|err| SummaryFailure::runtime("git-log-failed", format!("{err:#}")))?;

    Ok(SummaryPayload {
        range: SummaryRange {
            label: label.into(),
            from: since.map(str::to_string),
            to: until.map(str::to_string),
        },
        mailmap: use_mailmap,
        authors,
    })
}

pub fn render_text(payload: &SummaryPayload) {
    println!(
        "{:<25} {:<40} {:>8} {:>8} {:>8} {:>8} {:>12} {:>12}",
        "Name", "Email", "Added", "Deleted", "Net", "Commits", "First", "Last"
    );
    println!("{SEPARATOR}");

    for author in &payload.authors {
        println!(
            "{:<25} {:<40} {:>8} {:>8} {:>8} {:>8} {:>12} {:>12}",
            author.name,
            truncate_email(&author.email),
            author.added,
            author.deleted,
            author.net,
            author.commits,
            author.first,
            author.last
        );
    }
}

fn collect_author_rows(log_args: &[String], use_mailmap: bool) -> Result<Vec<AuthorSummary>> {
    let mut args = vec!["log".to_string()];
    args.extend(log_args.iter().cloned());
    let (name_placeholder, email_placeholder) = if use_mailmap {
        ("%aN", "%aE")
    } else {
        ("%an", "%ae")
    };
    args.push(format!(
        "--pretty=tformat:%x1e{name_placeholder}%x1f{email_placeholder}%x1f%cs"
    ));
    args.push("--numstat".to_string());

    let output = run_git(&args)?;
    let mut aggregates = BTreeMap::<(String, String), AuthorAggregate>::new();

    for record in output.split(RECORD_SEPARATOR).skip(1) {
        let record = record.trim_start_matches('\n');
        let Some((header, numstat)) = record.split_once('\n') else {
            continue;
        };
        let mut fields = header.splitn(3, FIELD_SEPARATOR);
        let Some(name) = fields.next() else {
            continue;
        };
        let Some(email) = fields.next() else {
            continue;
        };
        let Some(date) = fields.next() else {
            continue;
        };

        let (added, deleted) = parse_numstat_totals(numstat);
        let key = (name.to_string(), email.to_string());
        let aggregate = aggregates.entry(key).or_default();
        aggregate.added += added;
        aggregate.deleted += deleted;
        aggregate.commits += 1;
        if aggregate.first.is_empty() || date < aggregate.first.as_str() {
            aggregate.first = date.to_string();
        }
        if aggregate.last.is_empty() || date > aggregate.last.as_str() {
            aggregate.last = date.to_string();
        }
    }

    let mut authors = aggregates
        .into_iter()
        .filter(|(_, aggregate)| aggregate.added != 0 || aggregate.deleted != 0)
        .map(|((name, email), aggregate)| AuthorSummary {
            name,
            email,
            added: aggregate.added,
            deleted: aggregate.deleted,
            net: aggregate.added - aggregate.deleted,
            commits: aggregate.commits,
            first: aggregate.first,
            last: aggregate.last,
        })
        .collect::<Vec<_>>();
    authors.sort_by(|a, b| {
        b.net
            .cmp(&a.net)
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.email.cmp(&b.email))
    });
    Ok(authors)
}

fn truncate_email(email: &str) -> String {
    email.chars().take(40).collect::<String>()
}

fn parse_numstat_totals(log: &str) -> (i64, i64) {
    let mut added = 0i64;
    let mut deleted = 0i64;

    for line in log.lines() {
        let mut parts = line.splitn(3, '\t');
        let added_part = match parts.next() {
            Some(part) => part,
            None => continue,
        };
        let deleted_part = match parts.next() {
            Some(part) => part,
            None => continue,
        };
        let path = match parts.next() {
            Some(part) => part,
            None => continue,
        };

        if is_lockfile_line(path) {
            continue;
        }

        added += added_part.parse::<i64>().unwrap_or(0);
        deleted += deleted_part.parse::<i64>().unwrap_or(0);
    }

    (added, deleted)
}

fn is_lockfile_line(line: &str) -> bool {
    let trimmed = line.trim_end();
    if common_git::is_lockfile_path(trimmed) {
        return true;
    }
    std::path::Path::new(trimmed)
        .file_name()
        .and_then(|name| name.to_str())
        .map(|name| name.ends_with(".lock"))
        .unwrap_or(false)
}

#[derive(Default)]
struct AuthorAggregate {
    added: i64,
    deleted: i64,
    commits: i64,
    first: String,
    last: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn truncate_email_limits_length() {
        let email = "a".repeat(45);
        let truncated = truncate_email(&email);
        assert_eq!(truncated.len(), 40);
    }

    #[test]
    fn parse_numstat_totals_counts_paths_with_spaces() {
        let log = "\
2024-01-01
1\t2\tpath/with space.txt
3\t4\tpath/with space/another file.md
";
        let (added, deleted) = parse_numstat_totals(log);
        assert_eq!((added, deleted), (4, 6));
    }

    #[test]
    fn parse_numstat_totals_skips_lockfiles_with_spaces() {
        let log = "\
1\t1\tpath/with space/yarn.lock
2\t3\tpath/with space/src/lib.rs
";
        let (added, deleted) = parse_numstat_totals(log);
        assert_eq!((added, deleted), (2, 3));
    }

    #[test]
    fn parse_numstat_totals_treats_binary_as_zero() {
        let log = "\
2024-01-01
-\t-\tbin.dat
";
        let (added, deleted) = parse_numstat_totals(log);
        assert_eq!((added, deleted), (0, 0));
    }

    #[test]
    fn lockfile_detection_catches_known_patterns() {
        assert!(is_lockfile_line("yarn.lock"));
        assert!(is_lockfile_line("nested/package-lock.json"));
        assert!(is_lockfile_line("nested/pnpm-lock.yaml"));
        assert!(is_lockfile_line("nested/other.lock"));
        assert!(!is_lockfile_line("nested/lockfile.txt"));
    }

    #[test]
    fn lockfile_detection_catches_bun_and_npm_shrinkwrap() {
        assert!(is_lockfile_line("bun.lockb"));
        assert!(is_lockfile_line("nested/bun.lockb"));
        assert!(is_lockfile_line("bun.lock"));
        assert!(is_lockfile_line("npm-shrinkwrap.json"));
        assert!(is_lockfile_line("frontend/npm-shrinkwrap.json"));
    }

    #[test]
    fn lockfile_detection_uses_basename_for_known_names() {
        assert!(!is_lockfile_line("fake-yarn.lock.txt"));
        assert!(!is_lockfile_line("notyarn.lockb"));
        assert!(!is_lockfile_line("package-lock.json.bak"));
    }

    #[test]
    fn lockfile_detection_handles_trailing_whitespace() {
        assert!(is_lockfile_line("yarn.lock\n"));
        assert!(is_lockfile_line("nested/bun.lockb\n"));
    }

    #[test]
    fn parse_numstat_totals_skips_bun_lockb_and_npm_shrinkwrap() {
        let log = "\
2024-01-01
100\t50\tbun.lockb
200\t75\tnpm-shrinkwrap.json
5\t3\tsrc/lib.rs
";
        let (added, deleted) = parse_numstat_totals(log);
        assert_eq!((added, deleted), (5, 3));
    }
}
