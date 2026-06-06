use crate::{confirm, fzf, git_commit_select, open, open_changed_files, util};
use nils_common::git as common_git;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub fn run(args: &[String]) -> i32 {
    if !common_git::is_inside_work_tree().unwrap_or(false) {
        eprintln!("❌ Not inside a Git repository. Aborting.");
        return 1;
    }

    let (snapshot, query_parts) = match parse_snapshot_flag(args) {
        Ok(v) => v,
        Err(code) => return code,
    };

    let open_with = if util::env_or_default("FZF_FILE_OPEN_WITH", "vi").trim() == "vscode" {
        open::OpenWith::Vscode
    } else {
        open::OpenWith::Vi
    };

    let repo_root = match util::run_capture("git", &["rev-parse", "--show-toplevel"]) {
        Ok(v) => v.trim().to_string(),
        Err(err) => {
            eprintln!("{err:#}");
            return 1;
        }
    };
    let repo_root_path = PathBuf::from(repo_root);

    let input = util::join_args(&query_parts);
    let mut commit_query = resolve_to_short_hash_or_query(&input);
    let mut selected_commit = String::new();

    loop {
        let selected_ref = if selected_commit.is_empty() {
            None
        } else {
            Some(selected_commit.as_str())
        };
        let pick = match git_commit_select::pick_commit(&commit_query, selected_ref) {
            Ok(Some(p)) => p,
            Ok(None) => return 1,
            Err(err) => {
                eprintln!("{err:#}");
                return 1;
            }
        };
        let commit_query_restore = pick.query;
        let commit = pick.hash;
        selected_commit = commit.clone();

        let (file_list, file_paths, diff_parent) = match build_commit_file_list(&commit) {
            Ok(v) => v,
            Err(err) => {
                eprintln!("{err:#}");
                return 1;
            }
        };

        let input = format!("{}\n", file_list.join("\n"));
        let header = if snapshot {
            "enter: open snapshot | ctrl-o: open selected (worktree)"
        } else {
            "enter: open all (worktree) | ctrl-o: open selected"
        };

        let diff_cmd = if diff_parent.is_some() {
            r#"git diff --color=always "$FZF_DIFF_PARENT" "$FZF_COMMIT" -- "$filepath""#
        } else {
            r#"git diff --color=always "$FZF_COMMIT"^! -- "$filepath""#
        };
        let preview = format!(
            r#"bash -c 'line="$1"; filepath=$(printf "%s\n" "$line" | sed -E "s/^\[[^]]+\] //; s/ *\[\+.*\]$//"); if command -v delta >/dev/null 2>&1; then {diff_cmd} | delta --width=100 --line-numbers | awk "NR==1 && NF==0 {{next}} {{print}}"; else {diff_cmd} | cat; fi' -- {{}}"#,
            diff_cmd = diff_cmd
        );

        let fzf_args: Vec<String> = vec![
            "--ansi".to_string(),
            "--expect".to_string(),
            "ctrl-o".to_string(),
            "--header".to_string(),
            header.to_string(),
            "--prompt".to_string(),
            format!("📄 Files in {commit} > "),
            "--preview-window=right:50%:wrap".to_string(),
            "--preview".to_string(),
            preview,
        ];
        let fzf_ref: Vec<&str> = fzf_args.iter().map(|s| s.as_str()).collect();

        let mut fzf_envs: Vec<(&str, &str)> = vec![("FZF_COMMIT", &commit)];
        if let Some(parent) = diff_parent.as_deref() {
            fzf_envs.push(("FZF_DIFF_PARENT", parent));
        }
        let (code, key, rest) = match fzf::run_expect(&input, &fzf_ref, &fzf_envs) {
            Ok(v) => v,
            Err(err) => {
                eprintln!("{err:#}");
                return 1;
            }
        };
        if code != 0 {
            commit_query = commit_query_restore;
            continue;
        }

        let mode_key = key.unwrap_or_default();
        let selected_line = rest.first().cloned().unwrap_or_default();
        let selected_file = parse_file_path_from_line(&selected_line);
        if selected_file.is_empty() {
            commit_query = commit_query_restore;
            continue;
        }

        let mut open_snapshot = snapshot;
        if mode_key == "ctrl-o" {
            open_snapshot = false;
        }

        if mode_key != "ctrl-o" && !open_snapshot {
            let max_files = util::env_or_default("OPEN_CHANGED_FILES_MAX_FILES", "5")
                .parse::<usize>()
                .unwrap_or(5);
            if max_files == 0 {
                commit_query = commit_query_restore;
                continue;
            }

            let mut worktree_files: Vec<PathBuf> = Vec::new();
            for rel in &file_paths {
                let abs = repo_root_path.join(rel);
                if abs.is_file() {
                    worktree_files.push(abs);
                }
            }
            if worktree_files.is_empty() {
                eprintln!("❌ No files exist in working tree for commit: {commit}");
                commit_query = commit_query_restore;
                continue;
            }
            worktree_files.truncate(max_files);

            return open_many(open_with, &repo_root_path, &worktree_files);
        }

        let worktree_file = repo_root_path.join(&selected_file);
        if !open_snapshot {
            if worktree_file.exists() {
                let _ =
                    open::open_file_in_workspace(open_with, &repo_root_path, &worktree_file, false);
                return 0;
            }

            eprintln!("❌ File no longer exists in working tree: {selected_file}");
            match confirm::confirm(&format!("🧾 Open snapshot from {commit} instead? [y/N] ")) {
                Ok(true) => open_snapshot = true,
                Ok(false) => return 1,
                Err(err) => {
                    eprintln!("{err:#}");
                    return 1;
                }
            }
        }

        if open_snapshot {
            return open_snapshot_file(open_with, &repo_root_path, &commit, &selected_file);
        }
    }
}

fn parse_snapshot_flag(args: &[String]) -> Result<(bool, Vec<String>), i32> {
    let mut snapshot = false;
    let mut rest: Vec<String> = Vec::new();
    for a in args {
        if a == "--snapshot" {
            snapshot = true;
            continue;
        }
        if a.starts_with("--") {
            eprintln!("❌ Unknown flag: {a}");
            return Err(2);
        }
        rest.push(a.clone());
    }
    Ok((snapshot, rest))
}

fn resolve_to_short_hash_or_query(input: &str) -> String {
    if input.trim().is_empty() {
        return String::new();
    }
    if let Ok(Some(full)) =
        common_git::rev_parse(&["--verify", "--quiet", &format!("{input}^{{commit}}")])
        && full.len() >= 7
    {
        return full[..7].to_string();
    }
    input.to_string()
}

fn commit_parents(commit: &str) -> Vec<String> {
    let out =
        util::run_capture("git", &["rev-list", "--parents", "-n", "1", commit]).unwrap_or_default();
    let mut parts = out.split_whitespace();
    let _ = parts.next();
    parts.map(|s| s.to_string()).collect()
}

fn build_commit_file_list(
    commit: &str,
) -> anyhow::Result<(Vec<String>, Vec<String>, Option<String>)> {
    let parents = commit_parents(commit);
    let (diff_out, numstat_out, diff_parent) = if parents.len() > 1 {
        let parent = parents.first().cloned().unwrap_or_default();
        (
            util::run_capture("git", &["diff", "--name-status", &parent, commit])?,
            util::run_capture("git", &["diff", "--numstat", &parent, commit])?,
            Some(parent),
        )
    } else {
        (
            util::run_capture(
                "git",
                &["diff-tree", "--no-commit-id", "--name-status", "-r", commit],
            )?,
            util::run_capture("git", &["show", "--numstat", "--format=", commit])?,
            None,
        )
    };
    let numstat = parse_numstat(&numstat_out);

    let mut file_list: Vec<String> = Vec::new();
    let mut file_paths: Vec<String> = Vec::new();

    for line in diff_out.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() < 2 {
            continue;
        }
        let kind = parts[0].to_string();
        let path1 = parts.get(1).copied().unwrap_or("").to_string();
        let path2 = parts.get(2).copied().unwrap_or("").to_string();

        let filepath = if (kind.starts_with('R') || kind.starts_with('C')) && !path2.is_empty() {
            path2
        } else {
            path1
        };
        if filepath.is_empty() {
            continue;
        }

        let (a, d) = numstat.get(&filepath).copied().unwrap_or((0, 0));
        let stat = format!("  [+{a} / -{d}]");
        file_list.push(format!("[{kind}] {filepath}{stat}"));
        file_paths.push(filepath);
    }

    Ok((file_list, file_paths, diff_parent))
}

fn parse_numstat(input: &str) -> HashMap<String, (i64, i64)> {
    let mut map = HashMap::new();
    for line in input.lines() {
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() != 3 {
            continue;
        }
        let added = parts[0].parse::<i64>().unwrap_or(0);
        let deleted = parts[1].parse::<i64>().unwrap_or(0);
        let file = parts[2].to_string();
        map.insert(file, (added, deleted));
    }
    map
}

fn parse_file_path_from_line(line: &str) -> String {
    let stripped = line.trim();
    if stripped.is_empty() {
        return String::new();
    }
    let without_prefix = stripped
        .strip_prefix('[')
        .and_then(|s| s.split_once("] "))
        .map(|(_, rest)| rest)
        .unwrap_or(stripped);
    without_prefix
        .split("  [+")
        .next()
        .unwrap_or(without_prefix)
        .trim()
        .to_string()
}

fn open_many(open_with: open::OpenWith, _repo_root: &Path, files: &[PathBuf]) -> i32 {
    match open_with {
        open::OpenWith::Vscode => {
            let max_files = files.len().max(1);
            open_changed_files::open_vscode_files(
                files.to_vec(),
                open_changed_files::WorkspaceMode::Git,
                max_files,
                false,
                false,
            )
        }
        open::OpenWith::Vi => {
            let mut cmd = Command::new("vi");
            cmd.arg("--");
            for f in files {
                cmd.arg(f);
            }
            let status = cmd
                .stdin(Stdio::inherit())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .status();
            status.ok().and_then(|s| s.code()).unwrap_or(1)
        }
    }
}

fn open_snapshot_file(
    open_with: open::OpenWith,
    repo_root: &Path,
    commit: &str,
    file: &str,
) -> i32 {
    let tmp = match tempfile::NamedTempFile::new() {
        Ok(v) => v,
        Err(err) => {
            eprintln!("{err:#}");
            return 1;
        }
    };

    let ok = extract_snapshot(commit, file, tmp.path());
    if !ok {
        eprintln!("❌ Failed to extract snapshot: {commit}:{file} (or {commit}^:{file})");
        return 1;
    }

    let _ = open::open_file_in_workspace(open_with, repo_root, tmp.path(), true);
    0
}

fn extract_snapshot(commit: &str, file: &str, out_path: &Path) -> bool {
    if extract_snapshot_single(commit, file, out_path) {
        return true;
    }
    extract_snapshot_single(&format!("{commit}^"), file, out_path)
}

fn extract_snapshot_single(commitish: &str, file: &str, out_path: &Path) -> bool {
    let spec = format!("{commitish}:{file}");
    let Ok(output) = common_git::run_output(&["show", &spec]) else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    std::fs::write(out_path, output.stdout).is_ok()
}

#[cfg(test)]
mod tests {
    use super::{
        parse_file_path_from_line, parse_numstat, parse_snapshot_flag,
        resolve_to_short_hash_or_query,
    };
    use pretty_assertions::assert_eq;

    #[test]
    fn parse_snapshot_flag_accepts_snapshot_and_positional_query() {
        let args = vec![
            "--snapshot".to_string(),
            "feat".to_string(),
            "search".to_string(),
        ];
        let (snapshot, rest) = parse_snapshot_flag(&args).expect("parse");
        assert!(snapshot);
        assert_eq!(rest, vec!["feat".to_string(), "search".to_string()]);
    }

    #[test]
    fn parse_snapshot_flag_rejects_unknown_flags() {
        let args = vec!["--bad".to_string()];
        let err = parse_snapshot_flag(&args).expect_err("expected usage error");
        assert_eq!(err, 2);
    }

    #[test]
    fn parse_snapshot_flag_keeps_plain_args_when_no_flags() {
        let args = vec!["abc".to_string(), "def".to_string()];
        let (snapshot, rest) = parse_snapshot_flag(&args).expect("parse");
        assert!(!snapshot);
        assert_eq!(rest, args);
    }

    #[test]
    fn parse_numstat_ignores_malformed_rows() {
        let map = parse_numstat("10\t2\tsrc/a.rs\nbad\n-\t-\tbinary.bin\n");
        assert_eq!(map.get("src/a.rs"), Some(&(10, 2)));
        assert_eq!(map.get("binary.bin"), Some(&(0, 0)));
        assert_eq!(map.len(), 2);
    }

    #[test]
    fn parse_file_path_from_line_extracts_path_and_stat_suffix() {
        assert_eq!(
            parse_file_path_from_line("[M] src/main.rs  [+10 / -2]"),
            "src/main.rs"
        );
        assert_eq!(
            parse_file_path_from_line("plain/path.rs  [+1 / -0]"),
            "plain/path.rs"
        );
        assert_eq!(parse_file_path_from_line(""), "");
    }

    #[test]
    fn resolve_to_short_hash_or_query_returns_empty_for_blank() {
        assert_eq!(resolve_to_short_hash_or_query("   "), "");
    }

    #[test]
    fn resolve_to_short_hash_or_query_keeps_unknown_ref() {
        let query = "not-a-real-ref-123456789";
        assert_eq!(resolve_to_short_hash_or_query(query), query);
    }
}
