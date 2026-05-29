use anyhow::{Context, Result, anyhow};
use nils_common::git as common_git;
use std::process::Output;

pub(crate) fn trim_trailing_newlines(input: &str) -> String {
    common_git::trim_trailing_newlines(input)
}

pub(crate) fn git_output(args: &[&str]) -> Result<Output> {
    let output = run_git_output(args).with_context(|| format!("spawn git {:?}", args))?;
    if !output.status.success() {
        return Err(anyhow!(
            "git {:?} failed: {}{}",
            args,
            String::from_utf8_lossy(&output.stderr),
            String::from_utf8_lossy(&output.stdout),
        ));
    }
    Ok(output)
}

pub(crate) fn git_output_optional(args: &[&str]) -> Option<Output> {
    let output = run_git_output(args).ok()?;
    if !output.status.success() {
        return None;
    }
    Some(output)
}

pub(crate) fn git_status_success(args: &[&str]) -> bool {
    common_git::run_status_quiet(args)
        .map(|status| status.success())
        .unwrap_or(false)
}

pub(crate) fn git_status_code(args: &[&str]) -> Option<i32> {
    common_git::run_status_quiet(args)
        .ok()
        .map(|status| status.code().unwrap_or(1))
}

pub(crate) fn git_stdout_trimmed(args: &[&str]) -> Result<String> {
    let output = git_output(args)?;
    Ok(trim_trailing_newlines(&String::from_utf8_lossy(
        &output.stdout,
    )))
}

pub(crate) fn git_stdout_trimmed_optional(args: &[&str]) -> Option<String> {
    let output = git_output_optional(args)?;
    let out = trim_trailing_newlines(&String::from_utf8_lossy(&output.stdout));
    if out.is_empty() { None } else { Some(out) }
}

fn run_git_output(args: &[&str]) -> std::io::Result<Output> {
    common_git::run_output(args)
}

#[derive(Debug, Clone)]
pub(crate) struct NameStatusEntry {
    pub status_raw: String,
    pub path: String,
    pub old_path: Option<String>,
}

pub(crate) fn parse_name_status_z(bytes: &[u8]) -> Result<Vec<NameStatusEntry>> {
    let parsed = common_git::parse_name_status_z(bytes).map_err(|err| anyhow!("{err}"))?;
    Ok(parsed
        .into_iter()
        .map(|entry| NameStatusEntry {
            status_raw: String::from_utf8_lossy(entry.status_raw).to_string(),
            path: String::from_utf8_lossy(entry.path).to_string(),
            old_path: entry
                .old_path
                .map(|old_path| String::from_utf8_lossy(old_path).to_string()),
        })
        .collect())
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct DiffNumstat {
    pub added: Option<i64>,
    pub deleted: Option<i64>,
    pub binary: bool,
}

pub(crate) fn diff_numstat(path: &str) -> Result<DiffNumstat> {
    let output = git_stdout_trimmed(&[
        "-c",
        "core.quotepath=false",
        "diff",
        "--cached",
        "--numstat",
        "--",
        path,
    ])?;

    let line = output.lines().next().unwrap_or("");
    if line.trim().is_empty() {
        return Ok(DiffNumstat {
            added: None,
            deleted: None,
            binary: false,
        });
    }

    let mut parts = line.split('\t');
    let added = parts.next().unwrap_or("");
    let deleted = parts.next().unwrap_or("");

    if added == "-" || deleted == "-" {
        return Ok(DiffNumstat {
            added: None,
            deleted: None,
            binary: true,
        });
    }

    let added_num = added.parse::<i64>().ok();
    let deleted_num = deleted.parse::<i64>().ok();

    Ok(DiffNumstat {
        added: added_num,
        deleted: deleted_num,
        binary: false,
    })
}

pub(crate) fn is_lockfile(path: &str) -> bool {
    common_git::is_lockfile_path(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nils_test_support::git::{InitRepoOptions, commit_file, git, init_repo_with};
    use nils_test_support::{CwdGuard, GlobalStateLock};
    use pretty_assertions::assert_eq;
    use std::fs;

    #[test]
    fn parse_name_status_z_handles_rename_and_copy() {
        let bytes = b"R100\0old.txt\0new.txt\0C90\0src.rs\0dst.rs\0M\0file.txt\0";
        let entries = parse_name_status_z(bytes).expect("parse name-status");

        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].status_raw, "R100");
        assert_eq!(entries[0].path, "new.txt");
        assert_eq!(entries[0].old_path.as_deref(), Some("old.txt"));
        assert_eq!(entries[1].status_raw, "C90");
        assert_eq!(entries[1].path, "dst.rs");
        assert_eq!(entries[1].old_path.as_deref(), Some("src.rs"));
        assert_eq!(entries[2].status_raw, "M");
        assert_eq!(entries[2].path, "file.txt");
        assert_eq!(entries[2].old_path, None);
    }

    #[test]
    fn parse_name_status_z_errors_on_malformed_input() {
        let err = parse_name_status_z(b"R100\0old.txt\0").expect_err("expected parse failure");
        assert!(
            err.to_string().contains("malformed name-status output"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn diff_numstat_reports_counts_for_text_changes() {
        let lock = GlobalStateLock::new();
        let repo = init_repo_with(InitRepoOptions::new().with_initial_commit());
        commit_file(repo.path(), "file.txt", "one\n", "add file");
        fs::write(repo.path().join("file.txt"), "one\ntwo\nthree\n").expect("write file");
        git(repo.path(), &["add", "file.txt"]);

        let _cwd = CwdGuard::set(&lock, repo.path()).expect("cwd");
        let diff = diff_numstat("file.txt").expect("diff numstat");

        assert_eq!(diff.added, Some(2));
        assert_eq!(diff.deleted, Some(0));
        assert!(!diff.binary);
    }

    #[test]
    fn diff_numstat_reports_binary_for_binary_file() {
        let lock = GlobalStateLock::new();
        let repo = init_repo_with(InitRepoOptions::new().with_initial_commit());
        fs::write(repo.path().join("bin.dat"), b"\x00\x01binary\x00").expect("write bin");
        git(repo.path(), &["add", "bin.dat"]);

        let _cwd = CwdGuard::set(&lock, repo.path()).expect("cwd");
        let diff = diff_numstat("bin.dat").expect("diff numstat");

        assert!(diff.binary);
        assert_eq!(diff.added, None);
        assert_eq!(diff.deleted, None);
    }

    #[test]
    fn diff_numstat_reports_none_when_no_changes() {
        let lock = GlobalStateLock::new();
        let repo = init_repo_with(InitRepoOptions::new().with_initial_commit());
        commit_file(repo.path(), "file.txt", "one\n", "add file");

        let _cwd = CwdGuard::set(&lock, repo.path()).expect("cwd");
        let diff = diff_numstat("file.txt").expect("diff numstat");

        assert_eq!(diff.added, None);
        assert_eq!(diff.deleted, None);
        assert!(!diff.binary);
    }

    #[test]
    fn is_lockfile_detects_known_names() {
        for name in [
            "yarn.lock",
            "package-lock.json",
            "pnpm-lock.yaml",
            "bun.lockb",
            "bun.lock",
            "npm-shrinkwrap.json",
            "path/to/yarn.lock",
        ] {
            assert!(is_lockfile(name), "expected {name} to be a lockfile");
        }

        assert!(!is_lockfile("Cargo.lock"));
        assert!(!is_lockfile("README.md"));
    }
}
