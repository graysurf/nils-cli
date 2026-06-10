use std::fs;
use std::path::Path;

use nils_test_support::cmd::{CmdOptions, CmdOutput, run_resolved};
use pretty_assertions::assert_eq;

fn run(root: &Path, args: &[&str]) -> CmdOutput {
    let options = CmdOptions::new()
        .with_env("AGENT_MEMORY_HOME", &root.to_string_lossy())
        .with_env("HOME", &root.join("home").to_string_lossy())
        .with_env_remove("XDG_CONFIG_HOME");
    run_resolved("agent-memory", args, &options)
}

#[test]
fn no_args_and_help_print_usage() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    seed_layout(tmp.path());

    let no_args = run(tmp.path(), &[]);
    assert_eq!(no_args.code, 0, "stderr={}", no_args.stderr_text());
    assert!(
        no_args
            .stdout_text()
            .contains("Usage: agent-memory <COMMAND>")
    );

    let help = run(tmp.path(), &["help"]);
    assert_eq!(help.code, 0, "stderr={}", help.stderr_text());
    assert!(help.stdout_text().contains("Usage: agent-memory <COMMAND>"));
}

fn seed_layout(root: &Path) {
    fs::create_dir_all(root.join("global")).expect("global dir");
    fs::create_dir_all(root.join("agents")).expect("agents dir");
    fs::create_dir_all(root.join("personas")).expect("personas dir");
    fs::write(root.join("global").join("MEMORY.md"), "# Global\n").expect("global memory");
    fs::write(root.join("global").join("user.md"), "# User\n").expect("global note");
}

#[test]
fn resolves_root_and_global_scopes() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    seed_layout(tmp.path());

    let root_output = run(tmp.path(), &["path"]);
    assert_eq!(root_output.code, 0, "stderr={}", root_output.stderr_text());
    assert_eq!(
        root_output.stdout_text(),
        format!("{}\n", tmp.path().display())
    );

    let global_output = run(tmp.path(), &["path", "global"]);
    assert_eq!(global_output.code, 0);
    assert_eq!(
        global_output.stdout_text(),
        format!("{}/global\n", tmp.path().display())
    );
}

#[test]
fn lists_and_prints_memory_index() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    seed_layout(tmp.path());

    let list = run(tmp.path(), &["list", "global"]);
    assert_eq!(list.code, 0, "stderr={}", list.stderr_text());
    assert_eq!(list.stdout_text(), "MEMORY.md\nuser.md\n");

    let index = run(tmp.path(), &["index", "global"]);
    assert_eq!(index.code, 0, "stderr={}", index.stderr_text());
    assert_eq!(index.stdout_text(), "# Global\n");
}

#[test]
fn initializes_agent_scope() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    seed_layout(tmp.path());

    let output = run(tmp.path(), &["init-agent", "codex"]);
    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    assert!(output.stdout_text().contains("created:"));
    assert_eq!(
        fs::read_to_string(tmp.path().join("agents/codex/MEMORY.md")).expect("memory"),
        "# Memory index (codex)\n\n"
    );

    let agents = run(tmp.path(), &["agents"]);
    assert_eq!(agents.stdout_text(), "codex\n");
}

#[test]
fn initializes_persona_scope() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    seed_layout(tmp.path());

    let output = run(tmp.path(), &["init-persona", "work"]);
    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    assert!(tmp.path().join("personas/work/CLAUDE.md").is_file());
    assert!(tmp.path().join("personas/work/memory/MEMORY.md").is_file());
    assert!(
        tmp.path()
            .join("personas/work/.claude/settings.local.json")
            .is_file()
    );
}

#[test]
fn resolve_prints_global_and_agent_paths() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    seed_layout(tmp.path());

    let output = run(tmp.path(), &["resolve", "codex"]);
    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    assert_eq!(
        output.stdout_text(),
        format!(
            "global\t{}/global\nagent\t{}/agents/codex\n",
            tmp.path().display(),
            tmp.path().display()
        )
    );
}

#[test]
fn env_prints_exports() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    seed_layout(tmp.path());

    let output = run(tmp.path(), &["env"]);
    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let stdout = output.stdout_text();
    assert!(stdout.contains("export AGENT_MEMORY_HOME="));
    assert!(stdout.contains("export AGENT_MEMORY_GLOBAL="));
    assert!(stdout.contains("export AGENT_MEMORY_AGENTS="));
    assert!(stdout.contains("export AGENT_MEMORY_PERSONAS="));
}

#[test]
fn doctor_reports_layout() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    seed_layout(tmp.path());

    let output = run(tmp.path(), &["doctor"]);
    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let stdout = output.stdout_text();
    assert!(stdout.contains("[ok]      root present"));
    assert!(stdout.contains("[ok]      global (real dir)"));
    assert!(stdout.contains("[ok]      agents/"));
    assert!(stdout.contains("[ok]      personas/"));
}

#[test]
fn missing_scope_returns_runtime_error() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    seed_layout(tmp.path());

    let output = run(tmp.path(), &["list", "missing"]);
    assert_eq!(output.code, 1);
    assert!(output.stderr_text().contains("agent-memory: not found:"));
}

#[test]
fn invalid_id_returns_usage_error() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    seed_layout(tmp.path());

    let output = run(tmp.path(), &["resolve", "bad/id"]);
    assert_eq!(output.code, 64);
    assert!(output.stderr_text().contains("invalid id"));
}
