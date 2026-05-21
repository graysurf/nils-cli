use anyhow::Result;
use nils_common::cli_contract::exit;

use crate::git;
use crate::git::DefaultGitBackend;
use crate::lock_view::LockDetails;
use crate::messages;
use crate::prompt;
use crate::store::LockStore;

pub fn run(args: &[String]) -> Result<i32> {
    let options = match UnlockOptions::parse(args) {
        Ok(options) => options,
        Err(message) => {
            println!("{message}");
            println!("{}", messages::UNLOCK_USAGE);
            return Ok(exit::USAGE);
        }
    };
    let label_arg = options.label.as_deref();

    let store = LockStore::open()?;
    store.ensure_dir()?;

    let label = match store.resolve_label(label_arg)? {
        Some(label) => label,
        None => {
            println!("❌ No recent git-lock found for {}", store.repo_id());
            println!("Hint: run `git-lock list` to see available locks.");
            return Ok(1);
        }
    };

    let lock_file = store.lock_path(&label);
    if !lock_file.exists() {
        println!(
            "❌ No git-lock named '{label}' found for {}",
            store.repo_id()
        );
        println!("Hint: run `git-lock list` to see available locks.");
        return Ok(1);
    }

    let git_backend = DefaultGitBackend;
    let details = LockDetails::load_from_path(&store, &label, &lock_file, &git_backend)?;

    println!(
        "🔐 Found [{}:{label}] → {}",
        store.repo_id(),
        details.lock.hash
    );
    if !details.lock.note.is_empty() {
        println!("    # {}", details.lock.note);
    }
    if let Some(subject) = details.subject.as_deref() {
        println!("    commit message: {subject}");
    }
    println!();

    let preview = ResetPreview::load(&details.lock.hash, options.verbose)?;

    if options.dry_run {
        println!(
            "Dry run: git-lock unlock [{label}] would reset HEAD to {}.",
            details.lock.hash
        );
        preview.print(options.verbose);
        return Ok(0);
    }

    preview.print(options.verbose);
    println!();

    let prompt = format!(
        "⚠️  Hard reset to [{label}] ({}) and change {} files? [y/N] ",
        details.lock.hash, preview.changed_files
    );
    if !prompt::confirm(&prompt)? {
        return Ok(1);
    }

    let status = git::run_status_inherit(&["reset", "--hard", &details.lock.hash])?;
    if status != 0 {
        return Ok(status);
    }

    println!(
        "⏪ [{}:{label}] Reset to: {}",
        store.repo_id(),
        details.lock.hash
    );

    Ok(0)
}

#[derive(Debug, Default)]
struct UnlockOptions {
    dry_run: bool,
    verbose: bool,
    label: Option<String>,
}

impl UnlockOptions {
    fn parse(args: &[String]) -> std::result::Result<Self, String> {
        let mut options = Self::default();

        for arg in args.iter().map(String::as_str) {
            match arg {
                "--dry-run" => options.dry_run = true,
                "--verbose" | "--show-diff" => options.verbose = true,
                "--help" | "-h" => return Err(String::new()),
                _ if arg.starts_with('-') => {
                    return Err(format!("❗ Unknown unlock option: {arg}"));
                }
                _ => {
                    if options.label.is_some() {
                        return Err("❗ Too many labels provided (expected at most 1)".to_string());
                    }
                    options.label = Some(arg.to_string());
                }
            }
        }

        Ok(options)
    }
}

#[derive(Debug, Clone)]
struct ResetPreview {
    stat: String,
    full_diff: Option<String>,
    changed_files: usize,
}

impl ResetPreview {
    fn load(hash: &str, include_full_diff: bool) -> Result<Self> {
        let stat_output = git::run_output(&["diff", "--stat", "HEAD", hash])?;
        if !stat_output.status.success() {
            anyhow::bail!(
                "git diff --stat HEAD {hash} failed: {}",
                String::from_utf8_lossy(&stat_output.stderr).trim()
            );
        }

        let stat = String::from_utf8_lossy(&stat_output.stdout).to_string();
        let changed_files = count_changed_files(&stat);

        let full_diff = if include_full_diff {
            let diff_output = git::run_output(&["diff", "HEAD", hash])?;
            if !diff_output.status.success() {
                anyhow::bail!(
                    "git diff HEAD {hash} failed: {}",
                    String::from_utf8_lossy(&diff_output.stderr).trim()
                );
            }
            Some(String::from_utf8_lossy(&diff_output.stdout).to_string())
        } else {
            None
        };

        Ok(Self {
            stat,
            full_diff,
            changed_files,
        })
    }

    fn print(&self, verbose: bool) {
        println!("Reset preview:");
        println!("   Changed files: {}", self.changed_files);
        if self.stat.trim().is_empty() {
            println!("   (no file changes)");
        } else {
            for line in self.stat.lines() {
                println!("   {line}");
            }
        }

        if verbose {
            println!();
            println!("Full diff:");
            match self.full_diff.as_deref().map(str::trim_end) {
                Some(diff) if !diff.is_empty() => println!("{diff}"),
                _ => println!("(no diff)"),
            }
        }
    }
}

fn count_changed_files(stat: &str) -> usize {
    let non_empty: Vec<&str> = stat
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    if non_empty.is_empty() {
        return 0;
    }

    let last = non_empty.last().copied().unwrap_or_default();
    if last.contains("file changed") || last.contains("files changed") {
        non_empty.len().saturating_sub(1)
    } else {
        non_empty.len()
    }
}

#[cfg(test)]
mod tests {
    use super::{UnlockOptions, count_changed_files};

    #[test]
    fn parses_flags_and_label() {
        let args = vec![
            "--dry-run".to_string(),
            "--verbose".to_string(),
            "base".to_string(),
        ];
        let parsed = UnlockOptions::parse(&args).expect("parse");
        assert!(parsed.dry_run);
        assert!(parsed.verbose);
        assert_eq!(parsed.label.as_deref(), Some("base"));
    }

    #[test]
    fn rejects_unknown_flags() {
        let args = vec!["--wat".to_string()];
        let err = UnlockOptions::parse(&args).expect_err("reject");
        assert!(err.contains("Unknown unlock option"));
    }

    #[test]
    fn counts_git_stat_file_lines() {
        let stat =
            " a.txt | 2 +-\n b.txt | 1 +\n 2 files changed, 2 insertions(+), 1 deletion(-)\n";
        assert_eq!(count_changed_files(stat), 2);
    }
}
