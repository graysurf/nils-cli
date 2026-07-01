//! Behavioral coverage for agent-memory layout resolution, scope listing,
//! init guards, doctor diagnostics, and the env/completion contracts that the
//! happy-path `cli` suite does not exercise.

use std::fs;
use std::path::Path;

use nils_test_support::cmd::{CmdOptions, run_resolved};
use pretty_assertions::assert_eq;

/// Decoded view over [`nils_test_support::cmd::CmdOutput`] so assertions can
/// compare `stdout`/`stderr` as strings directly.
struct Out {
    code: i32,
    stdout: String,
    stderr: String,
}

/// Run the binary with a fully controlled environment. `AGENT_MEMORY_HOME`,
/// `XDG_CONFIG_HOME`, and `HOME` are stripped first so the child can never
/// inherit the developer's real configuration; only the provided pairs are set.
fn run_env(args: &[&str], envs: &[(&str, &str)]) -> Out {
    let options = CmdOptions::new()
        .with_env_remove_many(&["AGENT_MEMORY_HOME", "XDG_CONFIG_HOME", "HOME"])
        .with_envs(envs);
    let output = run_resolved("agent-memory", args, &options);
    Out {
        code: output.code,
        stdout: output.stdout_text(),
        stderr: output.stderr_text(),
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
fn completion_zsh_emits_dynamic_registration() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    seed_layout(tmp.path());

    let zsh = run_home(tmp.path(), &["completion", "zsh"]);
    assert_eq!(zsh.code, 0, "stderr={}", zsh.stderr);
    // agent-memory is a `completion_engine=dynamic` CLI: the exported zsh script
    // is a clap_complete `CompleteEnv` registration stub, not a static
    // `generate()` script. The dynamic completer calls back into the binary at
    // TAB time to enumerate live memory scopes.
    assert!(
        zsh.stdout.contains("#compdef agent-memory"),
        "dynamic zsh registration keeps the #compdef header"
    );
    assert!(
        zsh.stdout.contains("_clap_dynamic_completer_agent_memory"),
        "dynamic zsh registration defines the CompleteEnv completer function"
    );
    assert!(
        zsh.stdout
            .contains("compdef _clap_dynamic_completer_agent_memory agent-memory"),
        "dynamic zsh registration binds the completer to agent-memory"
    );
    // The static `generate()` surface (`_arguments`) must be gone: candidates are
    // computed at runtime, not baked into the stub.
    assert!(
        !zsh.stdout.contains("_arguments"),
        "dynamic stub must not embed the static `_arguments` surface"
    );
}

#[test]
fn dynamic_completion_enumerates_live_scopes() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    seed_layout(tmp.path());
    fs::create_dir_all(tmp.path().join("agents/alpha")).expect("agent alpha");
    fs::create_dir_all(tmp.path().join("agents/beta")).expect("agent beta");
    fs::create_dir_all(tmp.path().join("personas/reviewer/memory")).expect("persona reviewer");

    // Drive the clap_complete `CompleteEnv` runtime completer directly: with
    // `COMPLETE=zsh` set and the cursor on the `SCOPE` positional, the binary
    // should print the live scope candidates attached via `#[arg(add = ...)]`.
    let amh = tmp.path().to_string_lossy().into_owned();
    let home = tmp.path().join("home").to_string_lossy().into_owned();
    let options = CmdOptions::new()
        .with_env_remove_many(&["XDG_CONFIG_HOME"])
        .with_envs(&[
            ("AGENT_MEMORY_HOME", amh.as_str()),
            ("HOME", home.as_str()),
            ("COMPLETE", "zsh"),
            ("_CLAP_COMPLETE_INDEX", "2"),
            ("_CLAP_IFS", "\n"),
        ]);
    let output = run_resolved(
        "agent-memory",
        &["--", "agent-memory", "path", ""],
        &options,
    );
    let stdout = output.stdout_text();

    for expected in [
        "root",
        "global",
        "agents/alpha",
        "agents/beta",
        "personas/reviewer",
    ] {
        assert!(
            stdout.lines().any(|line| line == expected),
            "scope completion should offer `{expected}`, got:\n{stdout}"
        );
    }
}

#[test]
fn completion_bash_emits_dynamic_registration() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    seed_layout(tmp.path());

    let bash = run_home(tmp.path(), &["completion", "bash"]);
    assert_eq!(bash.code, 0, "stderr={}", bash.stderr);
    assert!(
        bash.stdout.contains("_clap_complete_agent_memory"),
        "dynamic bash registration defines the CompleteEnv completer function"
    );
    assert!(
        bash.stdout
            .contains("-F _clap_complete_agent_memory agent-memory"),
        "dynamic bash registration binds the completer to agent-memory via complete -F"
    );
    assert!(
        !bash.stdout.contains("__subcmd__"),
        "dynamic stub must not carry the static subcommand placeholder"
    );
}
