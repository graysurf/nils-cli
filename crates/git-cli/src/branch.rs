use crate::commit_shared::{git_output, git_status_success, git_stdout_trimmed};
use crate::prompt;
use crate::worktree;
use std::collections::HashSet;
use std::process::Output;

pub fn dispatch(cmd: &str, args: &[String]) -> Option<i32> {
    match cmd {
        "cleanup" | "delete-merged" => Some(run_cleanup(args)),
        _ => None,
    }
}

struct CleanupArgs {
    base_ref: String,
    squash_mode: bool,
    remove_worktrees: bool,
    help: bool,
}

fn run_cleanup(args: &[String]) -> i32 {
    let parsed = match parse_args(args) {
        Ok(value) => value,
        Err(code) => return code,
    };

    if parsed.help {
        print_help();
        return 0;
    }

    if !git_status_success(&["rev-parse", "--is-inside-work-tree"]) {
        eprintln!("❌ Not in a git repository");
        return 1;
    }

    let base_ref = parsed.base_ref;
    let squash_mode = parsed.squash_mode;
    let remove_worktrees = parsed.remove_worktrees;

    if !git_status_success(&["rev-parse", "--verify", "--quiet", &base_ref]) {
        eprintln!("❌ Invalid base ref: {base_ref}");
        return 1;
    }

    let base_commit = match git_stdout_trimmed(&["rev-parse", &format!("{base_ref}^{{commit}}")]) {
        Ok(value) => value,
        Err(_) => {
            eprintln!("❌ Unable to resolve base commit: {base_ref}");
            return 1;
        }
    };

    let head_commit = match git_stdout_trimmed(&["rev-parse", "HEAD"]) {
        Ok(value) => value,
        Err(_) => {
            eprintln!("❌ Unable to resolve HEAD commit");
            return 1;
        }
    };

    let delete_flag = if base_commit != head_commit {
        "-D"
    } else {
        "-d"
    };

    let current_branch = match git_stdout_trimmed(&["rev-parse", "--abbrev-ref", "HEAD"]) {
        Ok(value) => value,
        Err(_) => {
            eprintln!("❌ Unable to resolve current branch");
            return 1;
        }
    };

    let mut protected: HashSet<String> = ["main", "master", "develop", "trunk"]
        .iter()
        .map(|name| (*name).to_string())
        .collect();

    if current_branch != "HEAD" {
        protected.insert(current_branch.clone());
    }
    protected.insert(base_ref.clone());

    if let Some(base_local) = resolve_base_local(&base_ref) {
        protected.insert(base_local);
    }

    let merged_branches = match git_output(&[
        "for-each-ref",
        "--merged",
        &base_ref,
        "--format=%(refname:short)",
        "refs/heads",
    ]) {
        Ok(output) => parse_lines(&output),
        Err(err) => {
            eprintln!("{err:#}");
            return 1;
        }
    };

    let mut merged_set: HashSet<String> = HashSet::new();
    for branch in &merged_branches {
        merged_set.insert(branch.clone());
    }

    let linked_worktrees = match worktree::linked_worktrees_by_branch() {
        Ok(value) => value,
        Err(err) => {
            eprintln!("{err:#}");
            return 1;
        }
    };

    if !squash_mode && merged_branches.is_empty() {
        println!("✅ No merged local branches found.");
        return 0;
    }

    let mut candidates: Vec<String> = Vec::new();

    if squash_mode {
        let local_branches =
            match git_output(&["for-each-ref", "--format=%(refname:short)", "refs/heads"]) {
                Ok(output) => parse_lines(&output),
                Err(err) => {
                    eprintln!("{err:#}");
                    return 1;
                }
            };

        if local_branches.is_empty() {
            println!("✅ No local branches found.");
            return 0;
        }

        for branch in local_branches {
            if protected.contains(&branch) {
                continue;
            }

            if merged_set.contains(&branch) {
                candidates.push(branch);
                continue;
            }

            // Providers squash a branch's commits into a single commit on base,
            // so the branch's per-commit patch ids never match a plain
            // `git cherry base branch`. Collapse the branch's whole diff into one
            // synthetic commit on top of the merge-base, then ask whether base
            // already contains a patch-equivalent commit.
            let merge_base = match git_stdout_trimmed(&["merge-base", &base_ref, &branch]) {
                Ok(value) => value,
                // No merge-base means unrelated history (e.g. an orphan fixture
                // branch). It cannot be a squash-merge of base, so skip it
                // instead of aborting the whole sweep.
                Err(_) => continue,
            };

            let branch_tree =
                match git_stdout_trimmed(&["rev-parse", &format!("{branch}^{{tree}}")]) {
                    Ok(value) => value,
                    Err(_) => {
                        eprintln!("❌ Failed to resolve tree for {branch}");
                        return 1;
                    }
                };

            let squashed_commit = match git_stdout_trimmed(&[
                "commit-tree",
                &branch_tree,
                "-p",
                &merge_base,
                "-m",
                "squash-detect",
            ]) {
                Ok(value) => value,
                Err(_) => {
                    eprintln!("❌ Failed to synthesize squash commit for {branch}");
                    return 1;
                }
            };

            let cherry_output = match git_output(&["cherry", &base_ref, &squashed_commit]) {
                Ok(output) => output,
                Err(_) => {
                    eprintln!("❌ Failed to compare {branch} against {base_ref}");
                    return 1;
                }
            };

            let cherry_text = String::from_utf8_lossy(&cherry_output.stdout);
            let has_plus = cherry_text.lines().any(|line| line.starts_with('+'));
            if has_plus {
                continue;
            }

            candidates.push(branch);
        }
    } else {
        for branch in merged_branches {
            if protected.contains(&branch) {
                continue;
            }
            candidates.push(branch);
        }
    }

    if candidates.is_empty() {
        if squash_mode {
            println!("✅ No deletable branches found.");
        } else {
            println!("✅ No deletable merged branches.");
        }
        return 0;
    }

    if squash_mode {
        println!("🧹 Branches to delete (base: {base_ref}, mode: squash):");
    } else {
        println!("🧹 Merged branches to delete (base: {base_ref}):");
    }
    for branch in &candidates {
        println!("  - {branch}");
    }

    if remove_worktrees {
        let removable_worktrees: Vec<_> = candidates
            .iter()
            .filter_map(|branch| {
                linked_worktrees
                    .get(branch)
                    .map(|worktree_path| (branch, worktree_path))
            })
            .collect();

        if !removable_worktrees.is_empty() {
            println!("⚠️  Linked worktrees to remove (--remove-worktrees):");
            for (branch, worktree_path) in removable_worktrees {
                println!("  - {branch}: {worktree_path}");
            }
        }
    }

    if prompt::confirm_or_abort("❓ Proceed with deleting these branches? [y/N] ").is_err() {
        return 1;
    }

    let mut deleted_count = 0usize;
    let mut removed_worktrees_count = 0usize;
    let mut failed_deletions: Vec<(String, String)> = Vec::new();

    for branch in &candidates {
        let mut branch_delete_flag = delete_flag;
        if delete_flag == "-d" && squash_mode && !merged_set.contains(branch) {
            branch_delete_flag = "-D";
        }

        if remove_worktrees && let Some(worktree_path) = linked_worktrees.get(branch) {
            match git_output(&["worktree", "remove", "--force", worktree_path]) {
                Ok(_) => {
                    removed_worktrees_count += 1;
                }
                Err(err) => {
                    failed_deletions.push((
                        branch.clone(),
                        format!(
                            "failed to remove linked worktree {worktree_path}: {}",
                            summarize_git_error(&err.to_string())
                        ),
                    ));
                    continue;
                }
            }
        }

        match git_output(&["branch", branch_delete_flag, "--", branch]) {
            Ok(_) => {
                deleted_count += 1;
            }
            Err(err) => {
                failed_deletions.push((branch.clone(), summarize_git_error(&err.to_string())));
            }
        }
    }

    if removed_worktrees_count > 0 {
        println!("✅ Removed {removed_worktrees_count} linked worktree(s).");
    }

    if !failed_deletions.is_empty() {
        if deleted_count > 0 {
            println!("✅ Deleted {deleted_count} branch(es).");
        }

        eprintln!(
            "⚠️  Failed to delete {} branch(es):",
            failed_deletions.len()
        );
        for (branch, reason) in &failed_deletions {
            eprintln!("  - {branch}: {reason}");
        }
        return 1;
    }

    println!("✅ Deleted {deleted_count} branch(es).");
    0
}

fn parse_args(args: &[String]) -> Result<CleanupArgs, i32> {
    let mut base_ref = "HEAD".to_string();
    let mut squash_mode = false;
    let mut remove_worktrees = false;
    let mut help = false;

    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "-h" | "--help" => {
                help = true;
            }
            "-s" | "--squash" => {
                squash_mode = true;
            }
            "-w" | "--remove-worktrees" => {
                remove_worktrees = true;
            }
            "-b" | "--base" => {
                let Some(value) = args.get(i + 1) else {
                    return Err(2);
                };
                base_ref = value.to_string();
                i += 1;
            }
            _ => {}
        }
        i += 1;
    }

    Ok(CleanupArgs {
        base_ref,
        squash_mode,
        remove_worktrees,
        help,
    })
}

fn print_help() {
    println!(
        "Usage: git-delete-merged-branches [-b|--base <ref>] [-s|--squash] [-w|--remove-worktrees]"
    );
    println!("  -b, --base <ref>  Base ref used to determine merged branches (default: HEAD)");
    println!("  -s, --squash      Include branches whose squashed diff already landed on base");
    println!("  -w, --remove-worktrees  Force-remove linked worktrees for candidate branches");
}

fn parse_lines(output: &Output) -> Vec<String> {
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.to_string())
        .collect()
}

fn summarize_git_error(message: &str) -> String {
    let trimmed = message.trim();
    let summary = trimmed
        .rsplit_once(" failed: ")
        .map(|(_, suffix)| suffix.trim())
        .unwrap_or(trimmed);
    summary.replace('\n', " ")
}

fn resolve_base_local(base_ref: &str) -> Option<String> {
    let remote_ref = format!("refs/remotes/{base_ref}");
    if git_status_success(&["show-ref", "--verify", "--quiet", &remote_ref]) {
        return Some(
            base_ref
                .split_once('/')
                .map(|(_, tail)| tail.to_string())
                .unwrap_or_else(|| base_ref.to_string()),
        );
    }

    let local_ref = format!("refs/heads/{base_ref}");
    if git_status_success(&["show-ref", "--verify", "--quiet", &local_ref]) {
        return Some(base_ref.to_string());
    }

    None
}

#[cfg(test)]
mod tests {
    use super::{dispatch, parse_args, parse_lines, resolve_base_local, summarize_git_error};
    use crate::worktree;
    use nils_test_support::{EnvGuard, GlobalStateLock, StubBinDir};
    use pretty_assertions::assert_eq;
    use std::process::Command;

    #[test]
    fn dispatch_unknown_returns_none() {
        assert_eq!(dispatch("unknown", &[]), None);
    }

    #[test]
    fn cleanup_help_exits_success_without_git_runtime() {
        let args = vec!["--help".to_string()];
        assert_eq!(dispatch("cleanup", &args), Some(0));
        assert_eq!(dispatch("delete-merged", &args), Some(0));
    }

    #[test]
    fn parse_args_supports_base_and_squash_flags() {
        let args = vec![
            "--base".to_string(),
            "origin/main".to_string(),
            "--squash".to_string(),
            "--remove-worktrees".to_string(),
            "--unknown".to_string(),
        ];
        let parsed = parse_args(&args).expect("parsed");
        assert_eq!(parsed.base_ref, "origin/main");
        assert!(parsed.squash_mode);
        assert!(parsed.remove_worktrees);
        assert!(!parsed.help);
    }

    #[test]
    fn parse_args_requires_value_for_base_flag() {
        let args = vec!["--base".to_string()];
        let err_code = match parse_args(&args) {
            Ok(_) => panic!("expected usage error"),
            Err(code) => code,
        };
        assert_eq!(err_code, 2);
    }

    #[test]
    fn parse_lines_skips_blank_entries() {
        let output = Command::new("/bin/sh")
            .arg("-c")
            .arg("printf 'main\\n\\nfeature/a\\n'")
            .output()
            .expect("output");
        let lines = parse_lines(&output);
        assert_eq!(lines, vec!["main".to_string(), "feature/a".to_string()]);
    }

    #[test]
    fn summarize_git_error_strips_prefix_and_normalizes_lines() {
        let message =
            "git [\"branch\", \"-d\"] failed: error: cannot delete branch\nhint: checked out";
        let summary = summarize_git_error(message);
        assert_eq!(summary, "error: cannot delete branch hint: checked out");
    }

    #[test]
    fn linked_worktrees_by_branch_parses_porcelain_output() {
        let lock = GlobalStateLock::new();
        let stubs = StubBinDir::new();
        stubs.write_exe(
            "git",
            r#"#!/bin/bash
set -euo pipefail
if [[ "${1:-}" == "worktree" && "${2:-}" == "list" && "${3:-}" == "--porcelain" ]]; then
  printf 'worktree /repo\n'
  printf 'HEAD 1111111111111111111111111111111111111111\n'
  printf 'branch refs/heads/main\n'
  printf '\n'
  printf 'worktree /repo/wt/topic\n'
  printf 'HEAD 2222222222222222222222222222222222222222\n'
  printf 'branch refs/heads/feature/topic\n'
  exit 0
fi
exit 1
"#,
        );

        let _guard = EnvGuard::set(&lock, "PATH", &stubs.path_str());
        let mapping = worktree::linked_worktrees_by_branch().expect("parse linked worktrees");
        assert_eq!(
            mapping.get("feature/topic"),
            Some(&"/repo/wt/topic".to_string())
        );
        assert_eq!(mapping.get("main"), Some(&"/repo".to_string()));
    }

    #[test]
    fn resolve_base_local_prefers_remote_then_local_then_none() {
        let lock = GlobalStateLock::new();

        let remote_stubs = StubBinDir::new();
        remote_stubs.write_exe(
            "git",
            r#"#!/bin/bash
set -euo pipefail
if [[ "${1:-}" == "show-ref" && "${2:-}" == "--verify" && "${3:-}" == "--quiet" ]]; then
  if [[ "${4:-}" == "refs/remotes/origin/main" ]]; then
    exit 0
  fi
  exit 1
fi
exit 1
"#,
        );
        let remote_guard = EnvGuard::set(&lock, "PATH", &remote_stubs.path_str());
        assert_eq!(resolve_base_local("origin/main"), Some("main".to_string()));
        drop(remote_guard);

        let local_stubs = StubBinDir::new();
        local_stubs.write_exe(
            "git",
            r#"#!/bin/bash
set -euo pipefail
if [[ "${1:-}" == "show-ref" && "${2:-}" == "--verify" && "${3:-}" == "--quiet" ]]; then
  if [[ "${4:-}" == "refs/heads/main" ]]; then
    exit 0
  fi
  exit 1
fi
exit 1
"#,
        );
        let local_guard = EnvGuard::set(&lock, "PATH", &local_stubs.path_str());
        assert_eq!(resolve_base_local("main"), Some("main".to_string()));
        drop(local_guard);

        let none_stubs = StubBinDir::new();
        none_stubs.write_exe(
            "git",
            r#"#!/bin/bash
set -euo pipefail
exit 1
"#,
        );
        let _none_guard = EnvGuard::set(&lock, "PATH", &none_stubs.path_str());
        assert_eq!(resolve_base_local("feature/topic"), None);
    }
}
