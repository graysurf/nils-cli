use anyhow::Result;
use nils_common::{git as common_git, process};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::prompts;
use crate::runtime::{
    AgentRuntimeMode, AgentRuntimeOptions, GeneratedCommitMessage, generate_commit_message,
};

use super::exec;

pub struct CommitOptions {
    pub runtime: Option<AgentRuntimeMode>,
    pub push: bool,
    pub auto_stage: bool,
    pub ephemeral: bool,
    pub extra: Vec<String>,
}

pub fn run(options: &CommitOptions) -> Result<i32> {
    let runtime = match (AgentRuntimeOptions {
        runtime: options.runtime,
        ephemeral: options.ephemeral,
    })
    .resolve()
    {
        Ok(runtime) => runtime,
        Err(message) => {
            eprintln!("codex-cli agent: {message}");
            return Ok(64);
        }
    };
    match runtime {
        AgentRuntimeMode::Isolated => run_isolated(options),
        AgentRuntimeMode::Inherited => run_inherited(options),
    }
}

fn run_inherited(options: &CommitOptions) -> Result<i32> {
    if !command_exists("git") {
        eprintln!("codex-commit-with-scope: missing binary: git");
        return Ok(1);
    }

    let git_root = match git_root() {
        Some(value) => value,
        None => {
            eprintln!("codex-commit-with-scope: not a git repository");
            return Ok(1);
        }
    };

    if options.auto_stage {
        let status = Command::new("git")
            .arg("-C")
            .arg(&git_root)
            .arg("add")
            .arg("-A")
            .status()?;
        if !status.success() {
            return Ok(1);
        }
    } else {
        let staged = staged_files(&git_root);
        if staged.trim().is_empty() {
            eprintln!("codex-commit-with-scope: no staged changes (stage files then retry)");
            return Ok(1);
        }
    }

    let extra_prompt = options.extra.join(" ");

    if !command_exists("semantic-commit") {
        return run_fallback(&git_root, options.push, &extra_prompt);
    }

    {
        let stderr = io::stderr();
        let mut stderr = stderr.lock();
        if !exec::require_allow_dangerous(Some("codex-commit-with-scope"), &mut stderr) {
            return Ok(1);
        }
    }

    let mode = if options.auto_stage {
        "autostage"
    } else {
        "staged"
    };
    let mut prompt = match semantic_commit_prompt(mode) {
        Some(value) => value,
        None => return Ok(1),
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
    Ok(exec::exec_dangerous_with_options(
        &prompt,
        "codex-commit-with-scope",
        &mut stderr,
        exec::ExecOptions {
            ephemeral: options.ephemeral,
        },
    ))
}

fn run_isolated(options: &CommitOptions) -> Result<i32> {
    if !command_exists("git") {
        eprintln!("codex-cli agent commit: missing dependency: git");
        return Ok(1);
    }
    if !command_exists("semantic-commit") {
        eprintln!("codex-cli agent commit: missing dependency: semantic-commit");
        return Ok(1);
    }
    let git_root = match git_root().and_then(|path| std::fs::canonicalize(path).ok()) {
        Some(value) => value,
        None => {
            eprintln!("codex-cli agent commit: not a git repository");
            return Ok(1);
        }
    };

    if options.auto_stage {
        let status = Command::new("git")
            .args(["-C"])
            .arg(&git_root)
            .args(["add", "-A"])
            .status()?;
        if !status.success() {
            return Ok(1);
        }
    }
    if staged_files(&git_root).trim().is_empty() {
        eprintln!("codex-cli agent commit: no staged changes (stage files then retry)");
        return Ok(1);
    }

    let old_head = match git_stdout(&git_root, &["rev-parse", "--verify", "HEAD^{commit}"]) {
        Ok(value) => value,
        Err(message) => {
            eprintln!("codex-cli agent commit: {message}");
            return Ok(1);
        }
    };
    let staged_tree = match git_stdout(&git_root, &["write-tree"]) {
        Ok(value) => value,
        Err(message) => {
            eprintln!("codex-cli agent commit: {message}");
            return Ok(1);
        }
    };
    let context = Command::new("semantic-commit")
        .args(["staged-context", "--format", "bundle", "--repo"])
        .arg(&git_root)
        .output()?;
    if !context.status.success() {
        eprintln!("codex-cli agent commit: staged-context failed; index remains staged");
        return Ok(1);
    }
    if context.stdout.len() > 2 * 1024 * 1024 {
        eprintln!("codex-cli agent commit: staged-context exceeds 2 MiB; index remains staged");
        return Ok(1);
    }
    let context = match String::from_utf8(context.stdout) {
        Ok(value) => value,
        Err(_) => {
            eprintln!("codex-cli agent commit: staged-context was not UTF-8; index remains staged");
            return Ok(1);
        }
    };
    let extra = options.extra.join(" ");
    let prompt = format!(
        "Generate only the semantic commit message fields required by the response schema. \
Use only the staged-context bundle below. Do not request or describe commands. \
User wording guidance: {}\n\n{}",
        if extra.trim().is_empty() {
            "(none)"
        } else {
            extra.trim()
        },
        context
    );
    let message = {
        let stderr = io::stderr();
        let mut stderr = stderr.lock();
        match generate_commit_message(&prompt, &mut stderr) {
            Ok(message) => message,
            Err(message) => {
                eprintln!("{message}; index remains staged");
                return Ok(1);
            }
        }
    };

    let current_head = git_stdout(&git_root, &["rev-parse", "--verify", "HEAD^{commit}"]);
    let current_tree = git_stdout(&git_root, &["write-tree"]);
    if current_head.as_deref() != Ok(old_head.as_str())
        || current_tree.as_deref() != Ok(staged_tree.as_str())
    {
        eprintln!(
            "codex-cli agent commit: repository changed during message generation; no commit created"
        );
        return Ok(1);
    }

    let status = run_semantic_commit(&git_root, &old_head, &message)?;
    if !status.success() {
        eprintln!("codex-cli agent commit: semantic-commit failed; index remains staged");
        return Ok(status.code().unwrap_or(1));
    }
    let new_head = git_stdout(&git_root, &["rev-parse", "--verify", "HEAD^{commit}"])
        .unwrap_or_else(|_| "unknown".to_string());
    eprintln!("codex-cli agent commit: committed {new_head}");

    if options.push {
        let status = Command::new("git")
            .args(["-C"])
            .arg(&git_root)
            .arg("push")
            .status()?;
        if !status.success() {
            eprintln!(
                "codex-cli agent commit: commit {new_head} succeeded, but push failed; local commit was preserved"
            );
            return Ok(status.code().unwrap_or(1));
        }
    }
    Ok(0)
}

fn run_semantic_commit(
    git_root: &Path,
    old_head: &str,
    message: &GeneratedCommitMessage,
) -> Result<std::process::ExitStatus> {
    let mut command = Command::new("semantic-commit");
    command
        .arg("commit")
        .args(["--type", message.commit_type.as_str()]);
    if let Some(scope) = &message.scope {
        command.args(["--scope", scope]);
    }
    command.args(["--subject", message.subject.as_str()]);
    for bullet in &message.body_bullets {
        command.args(["--body-bullet", bullet]);
    }
    command
        .args(["--expect-head", old_head, "--repo"])
        .arg(git_root)
        .args(["--summary", "git-show", "--automation"])
        .status()
        .map_err(Into::into)
}

fn git_stdout(git_root: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .args(["-C"])
        .arg(git_root)
        .args(args)
        .output()
        .map_err(|error| format!("failed to run git: {error}"))?;
    if !output.status.success() {
        return Err(format!("git {} failed", args.join(" ")));
    }
    String::from_utf8(output.stdout)
        .map(|value| value.trim().to_string())
        .map_err(|_| format!("git {} returned non-UTF-8 output", args.join(" ")))
}

fn run_fallback(git_root: &Path, push_flag: bool, extra_prompt: &str) -> Result<i32> {
    let staged = staged_files(git_root);
    if staged.trim().is_empty() {
        eprintln!("codex-commit-with-scope: no staged changes (stage files then retry)");
        return Ok(1);
    }

    eprintln!("codex-commit-with-scope: semantic-commit not found on PATH (fallback mode)");
    if !extra_prompt.trim().is_empty() {
        eprintln!("codex-commit-with-scope: note: extra prompt is ignored in fallback mode");
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

    let mut commit_type = read_prompt("Type [chore]: ")?;
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
    let mut scope = read_prompt(&scope_prompt)?;
    scope.retain(|ch| !ch.is_whitespace());
    if scope.is_empty() {
        scope = suggested_scope;
    }

    let subject = loop {
        let raw = read_prompt("Subject: ")?;
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

    let confirm = read_prompt("Proceed? [y/N] ")?;
    if !matches!(confirm.trim().chars().next(), Some('y' | 'Y')) {
        eprintln!("Aborted.");
        return Ok(1);
    }

    let status = Command::new("git")
        .arg("-C")
        .arg(git_root)
        .arg("commit")
        .arg("-m")
        .arg(&header)
        .status()?;
    if !status.success() {
        return Ok(1);
    }

    if push_flag {
        let status = Command::new("git")
            .arg("-C")
            .arg(git_root)
            .arg("push")
            .status()?;
        if !status.success() {
            return Ok(1);
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

    Ok(0)
}

fn suggested_scope_from_staged(staged: &str) -> String {
    common_git::suggested_scope_from_staged_paths(staged)
}

fn read_prompt(prompt: &str) -> Result<String> {
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
            eprintln!("_codex_tools_semantic_commit_prompt: invalid mode: {other}");
            return None;
        }
    };

    let prompts_dir = match prompts::resolve_prompts_dir() {
        Some(value) => value,
        None => {
            eprintln!(
                "_codex_tools_semantic_commit_prompt: prompts dir not found (expected: $ZDOTDIR/prompts)"
            );
            return None;
        }
    };

    let prompt_file = prompts_dir.join(format!("{template_name}.md"));
    if !prompt_file.is_file() {
        eprintln!(
            "_codex_tools_semantic_commit_prompt: prompt template not found: {}",
            prompt_file.to_string_lossy()
        );
        return None;
    }

    match std::fs::read_to_string(&prompt_file) {
        Ok(content) => Some(content),
        Err(_) => {
            eprintln!(
                "_codex_tools_semantic_commit_prompt: failed to read prompt template: {}",
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
    use nils_test_support::{GlobalStateLock, prepend_path};
    use pretty_assertions::assert_eq;

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
        std::fs::write(&executable, "#!/bin/sh\necho ok\n").expect("write executable");
        std::fs::write(&non_executable, "plain text").expect("write non executable");

        let mut perms = std::fs::metadata(&executable)
            .expect("metadata")
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&executable, perms).expect("chmod executable");

        let mut perms = std::fs::metadata(&non_executable)
            .expect("metadata")
            .permissions();
        perms.set_mode(0o644);
        std::fs::set_permissions(&non_executable, perms).expect("chmod non executable");

        let _path_guard = prepend_path(&lock, dir.path());
        assert!(command_exists("tool-ok"));
        assert!(!command_exists("tool-no"));
        assert!(!command_exists("tool-missing"));
    }
}
