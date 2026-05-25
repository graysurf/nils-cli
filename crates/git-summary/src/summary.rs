use anyhow::Result;
use nils_common::git as common_git;
use nils_term::progress::{Progress, ProgressFinish, ProgressOptions};

use crate::dates::{build_range_args, validate_date};
use crate::git::run_git;

const SEPARATOR: &str = "----------------------------------------------------------------------------------------------------------------------------------------";

pub fn summary(since: Option<&str>, until: Option<&str>) -> i32 {
    if (since.is_some() && until.is_none()) || (since.is_none() && until.is_some()) {
        println!("❌ Please provide both start and end dates (YYYY-MM-DD).");
        return 1;
    }

    if let Some(value) = since
        && let Err(msg) = validate_date(value)
    {
        println!("{msg}");
        return 1;
    }
    if let Some(value) = until
        && let Err(msg) = validate_date(value)
    {
        println!("{msg}");
        return 1;
    }

    if let (Some(start), Some(end)) = (since, until)
        && start > end
    {
        println!("❌ Start date must be on or before end date.");
        return 1;
    }

    let log_args = match (since, until) {
        (Some(start), Some(end)) => build_range_args(start, end),
        _ => vec!["--no-merges".to_string()],
    };

    match render_summary(&log_args) {
        Ok(()) => 0,
        Err(err) => {
            eprintln!("{err:#}");
            1
        }
    }
}

fn render_summary(log_args: &[String]) -> Result<()> {
    let authors = collect_authors(log_args)?;
    let progress = if !authors.is_empty() {
        Some(Progress::new(
            authors.len() as u64,
            ProgressOptions::default().with_finish(ProgressFinish::Clear),
        ))
    } else {
        None
    };

    let mut rows = Vec::new();
    for (idx, author) in authors.iter().enumerate() {
        if let Some(p) = &progress {
            p.set_message(author.to_string());
        }

        rows.push(collect_author_row(author, log_args)?);

        if let Some(p) = &progress {
            p.set_position((idx + 1) as u64);
        }
    }

    rows.sort_by(|a, b| b.net.cmp(&a.net).then_with(|| a.line.cmp(&b.line)));

    if let Some(p) = progress {
        p.finish_and_clear();
    }

    println!(
        "{:<25} {:<40} {:>8} {:>8} {:>8} {:>8} {:>12} {:>12}",
        "Name", "Email", "Added", "Deleted", "Net", "Commits", "First", "Last"
    );
    println!("{SEPARATOR}");

    for row in rows {
        println!("{}", row.line);
    }

    Ok(())
}

fn collect_authors(log_args: &[String]) -> Result<Vec<String>> {
    let mut args = vec!["log".to_string()];
    args.extend(log_args.iter().cloned());
    args.push("--pretty=format:%an <%ae>".to_string());

    let output = run_git(&args)?;
    let mut authors = output
        .lines()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty())
        .map(|line| line.to_string())
        .collect::<Vec<_>>();
    authors.sort();
    authors.dedup();
    Ok(authors)
}

fn collect_author_row(author: &str, log_args: &[String]) -> Result<Row> {
    let (name, email) = split_author(author);
    let short_email = truncate_email(&email);

    let mut args = vec!["log".to_string()];
    args.extend(log_args.iter().cloned());
    if !email.is_empty() {
        args.push(format!("--author={email}"));
    }
    args.push("--pretty=format:%cd".to_string());
    args.push("--date=short".to_string());
    args.push("--numstat".to_string());

    let log = run_git(&args)?;

    let (commits, first_commit, last_commit) = parse_commit_dates(&log);
    let (added, deleted) = parse_numstat_totals(&log);
    let net = added - deleted;

    let line = format!(
        "{:<25} {:<40} {:>8} {:>8} {:>8} {:>8} {:>12} {:>12}",
        name, short_email, added, deleted, net, commits, first_commit, last_commit
    );

    Ok(Row { net, line })
}

fn split_author(author: &str) -> (String, String) {
    if let (Some(start), Some(end)) = (author.find('<'), author.rfind('>'))
        && start < end
    {
        let name = author[..start].trim().to_string();
        let email = author[start + 1..end].trim().to_string();
        return (name, email);
    }
    (author.trim().to_string(), String::new())
}

fn truncate_email(email: &str) -> String {
    email.chars().take(40).collect::<String>()
}

fn parse_commit_dates(log: &str) -> (i64, String, String) {
    let mut commits = 0i64;
    let mut first_commit = String::new();
    let mut last_commit = String::new();

    for line in log.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() == 1 {
            commits += 1;
            if last_commit.is_empty() {
                last_commit = parts[0].to_string();
            }
            first_commit = parts[0].to_string();
        }
    }

    (commits, first_commit, last_commit)
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

struct Row {
    net: i64,
    line: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn split_author_parses_name_and_email() {
        let (name, email) = split_author("Jane Doe <jane@example.com>");
        assert_eq!(name, "Jane Doe");
        assert_eq!(email, "jane@example.com");
    }

    #[test]
    fn split_author_handles_missing_brackets() {
        let (name, email) = split_author("nobody");
        assert_eq!(name, "nobody");
        assert_eq!(email, "");
    }

    #[test]
    fn truncate_email_limits_length() {
        let email = "a".repeat(45);
        let truncated = truncate_email(&email);
        assert_eq!(truncated.len(), 40);
    }

    #[test]
    fn parse_commit_dates_tracks_first_and_last() {
        let log = "\
2024-01-05
1\t2\ta.txt
2024-01-03
3\t4\tb.txt
";
        let (commits, first_commit, last_commit) = parse_commit_dates(log);
        assert_eq!(commits, 2);
        assert_eq!(first_commit, "2024-01-03");
        assert_eq!(last_commit, "2024-01-05");
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
