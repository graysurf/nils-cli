//! Behavioral coverage for agent-memory layout resolution, scope listing,
//! init guards, doctor diagnostics, and the env/completion contracts that the
//! happy-path `cli` suite does not exercise.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use pretty_assertions::assert_eq;

struct Out {
    code: i32,
    stdout: String,
    stderr: String,
}

fn agent_memory_bin() -> PathBuf {
    for key in ["CARGO_BIN_EXE_agent-memory", "CARGO_BIN_EXE_agent_memory"] {
        if let Ok(path) = std::env::var(key) {
            return path.into();
        }
    }
    let exe = std::env::current_exe().expect("current exe");
    let target_dir = exe
        .parent()
        .and_then(|path| path.parent())
        .expect("target dir");
    target_dir.join(format!("agent-memory{}", std::env::consts::EXE_SUFFIX))
}

/// Run the binary with a fully controlled environment. `AGENT_MEMORY_HOME`,
/// `XDG_CONFIG_HOME`, and `HOME` are stripped first so the child can never
/// inherit the developer's real configuration; only the provided pairs are set.
fn run_env(args: &[&str], envs: &[(&str, &str)]) -> Out {
    let mut cmd = Command::new(agent_memory_bin());
    cmd.args(args);
    for key in ["AGENT_MEMORY_HOME", "XDG_CONFIG_HOME", "HOME"] {
        cmd.env_remove(key);
    }
    for (key, value) in envs {
        cmd.env(key, value);
    }
    let output = cmd.output().expect("agent-memory command");
    Out {
        code: output.status.code().unwrap_or(1),
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
    }
}

fn run_home(root: &Path, args: &[&str]) -> Out {
    let amh = root.to_string_lossy().into_owned();
    let home = root.join("home").to_string_lossy().into_owned();
    run_env(args, &[("AGENT_MEMORY_HOME", &amh), ("HOME", &home)])
}

fn seed_layout(root: &Path) {
    fs::create_dir_all(root.join("global")).expect("global dir");
    fs::create_dir_all(root.join("agents")).expect("agents dir");
    fs::create_dir_all(root.join("personas")).expect("personas dir");
    fs::write(root.join("global").join("MEMORY.md"), "# Global\n").expect("global memory");
}

#[test]
fn from_env_prefers_xdg_config_home_when_amh_absent() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let xdg = tmp.path().join("xdg");
    let out = run_env(&["path"], &[("XDG_CONFIG_HOME", &xdg.to_string_lossy())]);
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    assert_eq!(out.stdout, format!("{}/agent-memory\n", xdg.display()));
}

#[test]
fn from_env_falls_back_to_home_config_dir() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let home = tmp.path().join("home");
    let out = run_env(&["path"], &[("HOME", &home.to_string_lossy())]);
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    assert_eq!(
        out.stdout,
        format!("{}/.config/agent-memory\n", home.display())
    );
}

#[test]
fn from_env_errors_when_home_and_amh_absent() {
    let out = run_env(&["path"], &[]);
    assert_eq!(out.code, 1);
    assert!(
        out.stderr.contains("HOME is not set"),
        "stderr={}",
        out.stderr
    );
}

#[test]
fn resolve_scope_honors_agents_and_personas_prefixes() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    seed_layout(tmp.path());

    let agents = run_home(tmp.path(), &["path", "agents/codex"]);
    assert_eq!(agents.code, 0, "stderr={}", agents.stderr);
    assert_eq!(
        agents.stdout,
        format!("{}/agents/codex\n", tmp.path().display())
    );

    let personas = run_home(tmp.path(), &["path", "personas/work"]);
    assert_eq!(
        personas.stdout,
        format!("{}/personas/work\n", tmp.path().display())
    );

    // A bare value with no known prefix resolves under agents/.
    let bare = run_home(tmp.path(), &["path", "codex"]);
    assert_eq!(
        bare.stdout,
        format!("{}/agents/codex\n", tmp.path().display())
    );
}

#[test]
fn index_errors_when_scope_has_no_memory_index() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    fs::create_dir_all(tmp.path().join("global")).expect("global dir");

    let out = run_home(tmp.path(), &["index", "global"]);
    assert_eq!(out.code, 1);
    assert!(out.stderr.contains("no MEMORY.md"), "stderr={}", out.stderr);
}

#[test]
fn listing_named_dirs_is_empty_ok_when_directory_absent() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    // No agents/ directory at all.
    let out = run_home(tmp.path(), &["agents"]);
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    assert_eq!(out.stdout, "");
}

#[test]
fn personas_lists_existing_persona_directories() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    fs::create_dir_all(tmp.path().join("personas").join("work")).expect("persona dir");
    fs::create_dir_all(tmp.path().join("personas").join("home")).expect("persona dir");

    let out = run_home(tmp.path(), &["personas"]);
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    assert_eq!(out.stdout, "home\nwork\n");
}

#[test]
fn init_agent_refuses_to_overwrite_existing_scope() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    seed_layout(tmp.path());

    let first = run_home(tmp.path(), &["init-agent", "codex"]);
    assert_eq!(first.code, 0, "stderr={}", first.stderr);

    let second = run_home(tmp.path(), &["init-agent", "codex"]);
    assert_eq!(second.code, 1);
    assert!(
        second.stderr.contains("already exists"),
        "stderr={}",
        second.stderr
    );
}

#[test]
fn init_persona_refuses_to_overwrite_existing_scope() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    seed_layout(tmp.path());

    let first = run_home(tmp.path(), &["init-persona", "work"]);
    assert_eq!(first.code, 0, "stderr={}", first.stderr);

    let second = run_home(tmp.path(), &["init-persona", "work"]);
    assert_eq!(second.code, 1);
    assert!(
        second.stderr.contains("already exists"),
        "stderr={}",
        second.stderr
    );
}

#[test]
fn init_persona_renders_tilde_settings_when_store_lives_under_home() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let home = tmp.path();
    let amh = home.join(".config").join("agent-memory");
    let amh_str = amh.to_string_lossy().into_owned();
    let home_str = home.to_string_lossy().into_owned();

    let out = run_env(
        &["init-persona", "work"],
        &[("AGENT_MEMORY_HOME", &amh_str), ("HOME", &home_str)],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr);

    let settings = fs::read_to_string(amh.join("personas/work/.claude/settings.local.json"))
        .expect("settings file");
    assert!(
        settings.contains("\"~/.config/agent-memory/personas/work/memory\""),
        "settings should use a HOME-relative path; got: {settings}"
    );
}

#[test]
fn doctor_reports_missing_root_and_global() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let missing_root = tmp.path().join("does-not-exist");
    let amh = missing_root.to_string_lossy().into_owned();
    let home = tmp.path().join("home").to_string_lossy().into_owned();

    let out = run_env(&["doctor"], &[("AGENT_MEMORY_HOME", &amh), ("HOME", &home)]);
    assert_eq!(out.code, 1);
    assert!(
        out.stderr.contains("[missing] root"),
        "stderr={}",
        out.stderr
    );
    assert!(
        out.stderr.contains("[missing] global"),
        "stderr={}",
        out.stderr
    );
}

#[test]
fn doctor_reports_empty_agents_and_personas() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    fs::create_dir_all(tmp.path().join("global")).expect("global dir");

    let out = run_home(tmp.path(), &["doctor"]);
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    assert!(
        out.stdout.contains("[empty]   agents/"),
        "stdout={}",
        out.stdout
    );
    assert!(
        out.stdout.contains("[empty]   personas/"),
        "stdout={}",
        out.stdout
    );
}

#[cfg(unix)]
#[test]
fn doctor_recognizes_global_symlink_target() {
    use std::os::unix::fs::symlink;

    let tmp = tempfile::TempDir::new().expect("tempdir");
    let real_global = tmp.path().join("real-global");
    fs::create_dir_all(&real_global).expect("real global dir");
    symlink(&real_global, tmp.path().join("global")).expect("symlink global");

    let out = run_home(tmp.path(), &["doctor"]);
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    assert!(
        out.stdout.contains("[ok]      global ->"),
        "stdout={}",
        out.stdout
    );
}

#[cfg(unix)]
#[test]
fn doctor_flags_broken_global_symlink() {
    use std::os::unix::fs::symlink;

    let tmp = tempfile::TempDir::new().expect("tempdir");
    symlink(tmp.path().join("missing-target"), tmp.path().join("global")).expect("symlink global");

    let out = run_home(tmp.path(), &["doctor"]);
    assert_eq!(out.code, 1);
    assert!(
        out.stderr.contains("[broken]  global ->"),
        "stderr={}",
        out.stderr
    );
}

#[test]
fn env_quotes_paths_with_shell_unsafe_characters() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let root = tmp.path().join("memory store"); // space forces quoting
    fs::create_dir_all(&root).expect("root dir");
    let amh = root.to_string_lossy().into_owned();
    let home = tmp.path().join("home").to_string_lossy().into_owned();

    let out = run_env(&["env"], &[("AGENT_MEMORY_HOME", &amh), ("HOME", &home)]);
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    assert!(
        out.stdout
            .contains(&format!("export AGENT_MEMORY_HOME='{}'", root.display())),
        "stdout={}",
        out.stdout
    );
}

#[test]
fn invalid_flag_is_a_usage_error() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    seed_layout(tmp.path());

    let out = run_home(tmp.path(), &["--definitely-not-a-flag"]);
    assert_eq!(out.code, 64, "stderr={}", out.stderr);
}

#[test]
fn completion_scripts_render_for_supported_shells() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    seed_layout(tmp.path());

    let bash = run_home(tmp.path(), &["completion", "bash"]);
    assert_eq!(bash.code, 0, "stderr={}", bash.stderr);
    assert!(
        bash.stdout.contains("agent-memory"),
        "bash completion should mention the binary"
    );
    assert!(
        !bash.stdout.contains("__subcmd__"),
        "bash completion placeholder should be normalized away"
    );

    let zsh = run_home(tmp.path(), &["completion", "zsh"]);
    assert_eq!(zsh.code, 0, "stderr={}", zsh.stderr);
    assert!(
        zsh.stdout.contains("#compdef agent-memory"),
        "zsh completion should declare the compdef header"
    );
}
