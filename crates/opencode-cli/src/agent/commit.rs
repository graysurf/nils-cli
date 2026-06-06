use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use nils_common::{git as common_git, process};

use crate::prompts;

use super::exec;

#[derive(Clone, Debug, Default)]
pub struct CommitOptions {
    pub push: bool,
    pub auto_stage: bool,
    pub extra: Vec<String>,
}

pub fn run(options: &CommitOptions) -> i32 {
    if !command_exists("git") {
        eprintln!("opencode-commit-with-scope: missing binary: git");
        return 1;
    }

    let git_root = match git_root() {
        Some(value) => value,
        None => {
            eprintln!("opencode-commit-with-scope: not a git repository");
            return 1;
        }
    };

    if options.auto_stage {
        let status = Command::new("git")
            .arg("-C")
            .arg(&git_root)
            .arg("add")
            .arg("-A")
            .status();
        if !status.map(|value| value.success()).unwrap_or(false) {
            return 1;
        }
    } else {
        let staged = staged_files(&git_root);
        if staged.trim().is_empty() {
            eprintln!("opencode-commit-with-scope: no staged changes (stage files then retry)");
            return 1;
        }
    }

    let extra_prompt = options.extra.join(" ");

    if !command_exists("semantic-commit") {
        return run_fallback(&git_root, options.push, &extra_prompt);
    }

    let mode = if options.auto_stage {
        "autostage"
    } else {
        "staged"
    };
    let mut prompt = match semantic_commit_prompt(mode) {
        Some(value) => value,
        None => return 1,
    };

    if options.push {
        prompt.push_str(
            "\n\nFurthermore, please push the committed changes to the remote repository.",
        );
    }

    if !extra_prompt.trim().is_empty() {
        prompt.push_str("\n\nAdditional instructions from user:\n");
        prompt.push_str(extra_prompt.trim());
    }

    let stderr = io::stderr();
    let mut stderr = stderr.lock();
    exec::exec_prompt(&prompt, "opencode-tools:commit-with-scope", &mut stderr)
}

fn run_fallback(git_root: &Path, push_flag: bool, extra_prompt: &str) -> i32 {
    let staged = staged_files(git_root);
    if staged.trim().is_empty() {
        eprintln!("opencode-commit-with-scope: no staged changes (stage files then retry)");
        return 1;
    }

    eprintln!("opencode-commit-with-scope: semantic-commit not found on PATH (fallback mode)");
    if !extra_prompt.trim().is_empty() {
        eprintln!("opencode-commit-with-scope: note: extra prompt is ignored in fallback mode");
    }

    if command_exists("git-scope") {
        let _ = Command::new("git-scope")
            .current_dir(git_root)
            .arg("staged")
            .status();
    } else {
        println!("Staged files:");
        print!("{staged}");
    }

    let suggested_scope = suggested_scope_from_staged(&staged);

    let mut commit_type = match read_prompt("Type [chore]: ") {
        Ok(value) => value,
        Err(_) => return 1,
    };
    commit_type = commit_type.to_ascii_lowercase();
    commit_type.retain(|ch| !ch.is_whitespace());
    if commit_type.is_empty() {
        commit_type = "chore".to_string();
    }

    let scope_prompt = if suggested_scope.is_empty() {
        "Scope (optional): ".to_string()
    } else {
        format!("Scope (optional) [{suggested_scope}]: ")
    };
    let mut scope = match read_prompt(&scope_prompt) {
        Ok(value) => value,
        Err(_) => return 1,
    };
    scope.retain(|ch| !ch.is_whitespace());
    if scope.is_empty() {
        scope = suggested_scope;
    }

    let subject = loop {
        let raw = match read_prompt("Subject: ") {
            Ok(value) => value,
            Err(_) => return 1,
        };
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            break trimmed.to_string();
        }
    };

    let header = if scope.is_empty() {
        format!("{commit_type}: {subject}")
    } else {
        format!("{commit_type}({scope}): {subject}")
    };

    println!();
    println!("Commit message:");
    println!("  {header}");

    let confirm = match read_prompt("Proceed? [y/N] ") {
        Ok(value) => value,
        Err(_) => return 1,
    };
    if !matches!(confirm.trim().chars().next(), Some('y' | 'Y')) {
        eprintln!("Aborted.");
        return 1;
    }

    let status = Command::new("git")
        .arg("-C")
        .arg(git_root)
        .arg("commit")
        .arg("-m")
        .arg(&header)
        .status();
    if !status.map(|value| value.success()).unwrap_or(false) {
        return 1;
    }

    if push_flag {
        let status = Command::new("git")
            .arg("-C")
            .arg(git_root)
            .arg("push")
            .status();
        if !status.map(|value| value.success()).unwrap_or(false) {
            return 1;
        }
    }

    if command_exists("git-scope") {
        let _ = Command::new("git-scope")
            .current_dir(git_root)
            .arg("commit")
            .arg("HEAD")
            .status();
    } else {
        let _ = Command::new("git")
            .arg("-C")
            .arg(git_root)
            .arg("show")
            .arg("-1")
            .arg("--name-status")
            .arg("--oneline")
            .status();
    }

    0
}

fn suggested_scope_from_staged(staged: &str) -> String {
    common_git::suggested_scope_from_staged_paths(staged)
}

fn read_prompt(prompt: &str) -> io::Result<String> {
    print!("{prompt}");
    let _ = io::stdout().flush();

    let mut line = String::new();
    let bytes = io::stdin().read_line(&mut line)?;
    if bytes == 0 {
        return Ok(String::new());
    }
    Ok(line.trim_end_matches(&['\r', '\n'][..]).to_string())
}

fn staged_files(git_root: &Path) -> String {
    common_git::staged_name_only_in(git_root).unwrap_or_default()
}

fn git_root() -> Option<PathBuf> {
    common_git::repo_root().ok().flatten()
}

fn semantic_commit_prompt(mode: &str) -> Option<String> {
    let template_name = match mode {
        "staged" => "semantic-commit-staged",
        "autostage" => "semantic-commit-autostage",
        other => {
            eprintln!("_opencode_tools_semantic_commit_prompt: invalid mode: {other}");
            return None;
        }
    };

    let prompts_dir = match prompts::resolve_prompts_dir() {
        Some(value) => value,
        None => {
            eprintln!(
                "_opencode_tools_semantic_commit_prompt: prompts dir not found (expected: $ZDOTDIR/prompts)"
            );
            return None;
        }
    };

    let prompt_file = prompts_dir.join(format!("{template_name}.md"));
    if !prompt_file.is_file() {
        eprintln!(
            "_opencode_tools_semantic_commit_prompt: prompt template not found: {}",
            prompt_file.to_string_lossy()
        );
        return None;
    }

    match std::fs::read_to_string(&prompt_file) {
        Ok(content) => Some(content),
        Err(_) => {
            eprintln!(
                "_opencode_tools_semantic_commit_prompt: failed to read prompt template: {}",
                prompt_file.to_string_lossy()
            );
            None
        }
    }
}

fn command_exists(name: &str) -> bool {
    process::cmd_exists(name)
}

#[cfg(test)]
mod tests {
    use super::{command_exists, semantic_commit_prompt, suggested_scope_from_staged};
    use nils_test_support::{GlobalStateLock, fs as test_fs, prepend_path};
    use pretty_assertions::assert_eq;
    use std::fs;

    #[test]
    fn suggested_scope_prefers_single_top_level_directory() {
        let staged = "src/main.rs\nsrc/lib.rs\n";
        assert_eq!(suggested_scope_from_staged(staged), "src");
    }

    #[test]
    fn suggested_scope_ignores_root_file_when_single_directory_exists() {
        let staged = "README.md\nsrc/main.rs\n";
        assert_eq!(suggested_scope_from_staged(staged), "src");
    }

    #[test]
    fn suggested_scope_returns_empty_for_multiple_directories() {
        let staged = "src/main.rs\ncrates/a.rs\n";
        assert_eq!(suggested_scope_from_staged(staged), "");
    }

    #[test]
    fn semantic_commit_prompt_rejects_invalid_mode() {
        assert!(semantic_commit_prompt("unknown").is_none());
    }

    #[cfg(unix)]
    #[test]
    fn command_exists_checks_executable_bit() {
        use std::os::unix::fs::PermissionsExt;

        let lock = GlobalStateLock::new();
        let dir = tempfile::TempDir::new().expect("tempdir");
        let executable = dir.path().join("tool-ok");
        let non_executable = dir.path().join("tool-no");
        test_fs::write_executable(&executable, "#!/bin/sh\necho ok\n");
        fs::write(&non_executable, "plain text").expect("write non executable");
        let mut perms = fs::metadata(&non_executable)
            .expect("metadata")
            .permissions();
        perms.set_mode(0o644);
        fs::set_permissions(&non_executable, perms).expect("chmod non executable");

        let _path_guard = prepend_path(&lock, dir.path());
        assert!(command_exists("tool-ok"));
        assert!(!command_exists("tool-no"));
        assert!(!command_exists("tool-missing"));
    }
}
