use anyhow::Result;
use std::path::PathBuf;

use crate::git::DefaultGitBackend;
use crate::lock_view::LockDetails;
use crate::store::LockStore;
use nils_term::progress::{Progress, ProgressFinish, ProgressOptions};

pub fn run(_args: &[String]) -> Result<i32> {
    let store = LockStore::open()?;
    let lock_dir = store.lock_dir().to_path_buf();

    if !lock_dir.is_dir() {
        println!("📬 No git-locks found for [{}]", store.repo_id());
        return Ok(0);
    }

    let latest = store.read_latest_label()?.unwrap_or_default();

    let git_backend = DefaultGitBackend;
    let mut entries = collect_entries(&store, &lock_dir, &git_backend)?;
    if entries.is_empty() {
        println!("📬 No git-locks found for [{}]", store.repo_id());
        return Ok(0);
    }

    entries.sort_by_key(|entry| std::cmp::Reverse(entry.epoch));

    println!("🔐 git-lock list for [{}]:", store.repo_id());

    for entry in entries {
        println!();
        let label = entry.label.as_str();
        print!(" - 🏷️  tag:     {label}");
        if label == latest {
            print!("  ⭐ (latest)");
        }
        println!();
        println!("   🧬 commit:  {}", entry.lock.hash);

        if let Some(subject) = entry.subject.as_deref() {
            println!("   📄 message: {subject}");
        }

        if !entry.lock.note.is_empty() {
            println!("   📝 note:    {}", entry.lock.note);
        }

        if let Some(timestamp) = entry.lock.timestamp
            && !timestamp.is_empty()
        {
            println!("   📅 time:    {timestamp}");
        }
    }

    Ok(0)
}

fn collect_entries(
    store: &LockStore,
    lock_dir: &PathBuf,
    git_backend: &DefaultGitBackend,
) -> Result<Vec<LockDetails>> {
    let prefix = format!("{}-", store.repo_id());

    let dir_entries = match std::fs::read_dir(lock_dir) {
        Ok(value) => value,
        Err(_) => return Ok(Vec::new()),
    };

    let mut candidates: Vec<(PathBuf, String)> = Vec::new();
    for entry in dir_entries {
        let entry = match entry {
            Ok(value) => value,
            Err(_) => continue,
        };
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let file_name = match path.file_name().and_then(|v| v.to_str()) {
            Some(value) => value.to_string(),
            None => continue,
        };
        if !file_name.starts_with(&prefix) || !file_name.ends_with(".lock") {
            continue;
        }

        let label = file_name
            .trim_end_matches(".lock")
            .trim_start_matches(&prefix)
            .to_string();

        candidates.push((path, label));
    }

    if candidates.is_empty() {
        return Ok(Vec::new());
    }

    let progress = Progress::new(
        candidates.len() as u64,
        ProgressOptions::default().with_finish(ProgressFinish::Clear),
    );
    progress.set_message("scanning");

    let mut entries = Vec::new();
    for (path, label) in candidates {
        let details = match LockDetails::load_from_path(store, &label, &path, git_backend) {
            Ok(v) => v,
            Err(err) => {
                progress.finish_and_clear();
                return Err(err);
            }
        };

        entries.push(details);
        progress.inc(1);
    }

    progress.finish_and_clear();
    Ok(entries)
}
