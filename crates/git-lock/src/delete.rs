use anyhow::Result;
use nils_common::cli_contract::exit;
use std::fs;
use std::io::{self, IsTerminal};

use crate::git::DefaultGitBackend;
use crate::lock_view::LockDetails;
use crate::messages;
use crate::prompt;
use crate::store::LockStore;

pub fn run(args: &[String]) -> Result<i32> {
    let options = match DeleteOptions::parse(args) {
        Ok(options) => options,
        Err(message) => {
            println!("{message}");
            println!("{}", messages::DELETE_USAGE);
            return Ok(exit::USAGE);
        }
    };

    let store = LockStore::open()?;
    let lock_dir = store.lock_dir().to_path_buf();

    if !lock_dir.is_dir() {
        println!("{}", messages::NO_GIT_LOCKS_FOUND);
        return Ok(1);
    }

    let label_arg = options.label.as_deref();
    let label = match store.resolve_label(label_arg)? {
        Some(label) => label,
        None => {
            println!("❌ No label provided and no latest git-lock exists");
            println!("Hint: run `git-lock list` to see available locks.");
            return Ok(1);
        }
    };

    let lock_file = store.lock_path(&label);
    if !lock_file.exists() {
        println!("❌ git-lock [{label}] not found");
        println!("Hint: run `git-lock list` to see available locks.");
        return Ok(1);
    }

    let git_backend = DefaultGitBackend;
    let details = LockDetails::load_from_path(&store, &label, &lock_file, &git_backend)?;

    println!("🗑️  Candidate for deletion:");
    println!("   🏷️  tag:     {label}");
    println!("   🧬 commit:  {}", details.lock.hash);
    if let Some(subject) = details.subject.as_deref() {
        println!("   📄 message: {subject}");
    }
    if !details.lock.note.is_empty() {
        println!("   📝 note:    {}", details.lock.note);
    }
    if let Some(timestamp) = details.lock.timestamp.as_deref()
        && !timestamp.is_empty()
    {
        println!("   📅 time:    {timestamp}");
    }
    println!();

    if !options.force {
        if !io::stdin().is_terminal() {
            eprintln!("error: git-lock delete requires --force when stdin is not a TTY");
            return Ok(exit::USAGE);
        }

        println!("{}", delete_warning());
        let prompt = "⚠️  Delete this git-lock? [y/N] ";
        if !prompt::confirm(prompt)? {
            return Ok(1);
        }
    }

    fs::remove_file(&lock_file)?;
    println!("🗑️  Deleted git-lock [{}:{label}]", store.repo_id());

    if store.remove_latest_if_matches(&label)? {
        println!("🧼 Removed latest marker (was [{label}])");
    }

    Ok(0)
}

fn delete_warning() -> &'static str {
    "WARNING: this permanently deletes the lock record from disk"
}

#[derive(Debug, Default)]
struct DeleteOptions {
    force: bool,
    label: Option<String>,
}

impl DeleteOptions {
    fn parse(args: &[String]) -> std::result::Result<Self, String> {
        let mut options = Self::default();

        for arg in args.iter().map(String::as_str) {
            match arg {
                "--force" | "-f" => options.force = true,
                "--help" | "-h" => return Err(String::new()),
                _ if arg.starts_with('-') => {
                    return Err(format!("❗ Unknown delete option: {arg}"));
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

#[cfg(test)]
mod tests {
    use super::{DeleteOptions, delete_warning};

    #[test]
    fn parses_force_and_label() {
        let args = vec!["--force".to_string(), "wip".to_string()];
        let parsed = DeleteOptions::parse(&args).expect("parse");
        assert!(parsed.force);
        assert_eq!(parsed.label.as_deref(), Some("wip"));
    }

    #[test]
    fn rejects_unknown_flags() {
        let args = vec!["--wat".to_string()];
        let err = DeleteOptions::parse(&args).expect_err("reject");
        assert!(err.contains("Unknown delete option"));
    }

    #[test]
    fn warning_mentions_permanent_disk_delete() {
        assert!(delete_warning().contains("permanently deletes the lock record from disk"));
    }
}
