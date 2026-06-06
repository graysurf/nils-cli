use std::fs;
use std::path::Path;

use nils_test_support::cmd::{CmdOptions, CmdOutput, run_resolved};
use nils_test_support::fs::write_text_in_dir;
use nils_test_support::git::{InitRepoOptions, git, init_repo_at_with};
use pretty_assertions::assert_eq;
use serde_json::Value;

fn run_zsh_kit(args: &[&str], home: &Path) -> CmdOutput {
    run_zsh_kit_with(args, home, &[])
}

fn run_zsh_kit_with(args: &[&str], home: &Path, envs: &[(&str, &str)]) -> CmdOutput {
    let mut options = CmdOptions::new().with_env("HOME", &home.to_string_lossy());
    for (key, value) in envs {
        options = options.with_env(key, value);
    }
    run_resolved("zsh-kit", args, &options)
}

fn fixture_plugin_repo(root: &Path) {
    fs::create_dir_all(root).expect("repo dir");
    init_repo_at_with(
        root,
        InitRepoOptions::new()
            .with_branch("main")
            .with_initial_commit(),
    );
    write_text_in_dir(root, "plugin.plugin.zsh", "echo plugin\n");
    git(root, &["add", "plugin.plugin.zsh"]);
    git(root, &["commit", "-m", "add plugin file"]);
}

#[test]
fn plugin_fetch_refuses_invalid_entry_without_touching_plugin_dirs() {
    let temp = tempfile::tempdir().expect("tempdir");
    let plugins_dir = temp.path().join("plugins");
    fs::create_dir_all(&plugins_dir).expect("plugins dir");
    fs::write(plugins_dir.join("keep.txt"), "sentinel\n").expect("sentinel");

    let traversal_dir = temp.path().join("evil");
    fs::create_dir_all(&traversal_dir).expect("traversal dir");
    fs::write(traversal_dir.join("keep.txt"), "evil\n").expect("traversal sentinel");

    let output = run_zsh_kit(
        &[
            "plugin",
            "fetch",
            "--entry",
            "../evil",
            "--plugins-dir",
            &plugins_dir.to_string_lossy(),
            "--force",
            "--format",
            "json",
        ],
        temp.path(),
    );

    assert_eq!(output.code, 65);
    assert!(plugins_dir.join("keep.txt").is_file());
    assert!(traversal_dir.join("keep.txt").is_file());
    let json: Value = serde_json::from_str(&output.stdout_text()).expect("json");
    assert_eq!(json["ok"], false);
    assert_eq!(json["error"]["code"], "invalid-plugin-entry");
}

#[test]
fn plugin_fetch_dry_run_reports_clone_without_creating_plugin_dir() {
    let temp = tempfile::tempdir().expect("tempdir");
    let plugins_dir = temp.path().join("plugins");

    let output = run_zsh_kit(
        &[
            "plugin",
            "fetch",
            "--entry",
            "demo::demo.plugin.zsh::git=https://example.com/demo.git",
            "--plugins-dir",
            &plugins_dir.to_string_lossy(),
            "--dry-run",
        ],
        temp.path(),
    );

    assert_eq!(output.code, 0, "stderr: {}", output.stderr_text());
    assert!(output.stdout_text().contains("Cloning demo from"));
    assert!(!plugins_dir.join("demo").exists());
}

#[test]
fn plugin_fetch_clones_local_git_repo() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = temp.path().join("source");
    let plugins_dir = temp.path().join("plugins");
    fixture_plugin_repo(&repo);

    let entry = format!("demo::plugin.plugin.zsh::git={}", repo.to_string_lossy());
    let output = run_zsh_kit(
        &[
            "plugin",
            "fetch",
            "--entry",
            &entry,
            "--plugins-dir",
            &plugins_dir.to_string_lossy(),
            "--format",
            "json",
        ],
        temp.path(),
    );

    assert_eq!(output.code, 0, "stderr: {}", output.stderr_text());
    assert!(plugins_dir.join("demo/.git").is_dir());
    assert!(plugins_dir.join("demo/plugin.plugin.zsh").is_file());
    let json: Value = serde_json::from_str(&output.stdout_text()).expect("json");
    assert_eq!(json["ok"], true);
    assert_eq!(json["data"]["command"], "fetch");
    assert_eq!(json["data"]["mutation_status"], "applied");
    assert_eq!(json["data"]["updated"], 1);
}

#[test]
fn plugin_update_dry_run_lists_git_plugin_repositories() {
    let temp = tempfile::tempdir().expect("tempdir");
    let plugins_dir = temp.path().join("plugins");
    let plugin = plugins_dir.join("demo");
    fixture_plugin_repo(&plugin);
    fs::create_dir_all(plugins_dir.join("not-git")).expect("not git dir");

    let output = run_zsh_kit(
        &[
            "plugin",
            "update",
            "--plugins-dir",
            &plugins_dir.to_string_lossy(),
            "--dry-run",
        ],
        temp.path(),
    );

    assert_eq!(output.code, 0, "stderr: {}", output.stderr_text());
    let stdout = output.stdout_text();
    assert!(stdout.contains("Updating plugins in:"));
    assert!(stdout.contains("Updating demo"));
    assert!(stdout.contains("git -C"));
    assert!(!stdout.contains("not-git"));
}

#[test]
fn plugin_status_reports_interval_from_timestamp_file() {
    let temp = tempfile::tempdir().expect("tempdir");
    let timestamp = temp.path().join("cache/plugin.timestamp");
    let now = 1_700_000_000_i64;
    fs::create_dir_all(timestamp.parent().unwrap()).expect("cache dir");
    fs::write(&timestamp, format!("{}\n", now - (2 * 86_400 + 43_200))).expect("timestamp");

    let output = run_zsh_kit_with(
        &[
            "plugin",
            "status",
            "--timestamp-file",
            &timestamp.to_string_lossy(),
            "--interval-days",
            "7",
            "--format",
            "json",
        ],
        temp.path(),
        &[("ZSH_KIT_PLUGIN_NOW_EPOCH", &now.to_string())],
    );

    assert_eq!(output.code, 0, "stderr: {}", output.stderr_text());
    let json: Value = serde_json::from_str(&output.stdout_text()).expect("json");
    assert_eq!(json["ok"], true);
    assert_eq!(json["data"]["days_ago"], 2);
    assert_eq!(json["data"]["days_left"], 5);
    assert_eq!(json["data"]["update_due"], false);
}

#[test]
fn plugin_maybe_update_writes_timestamp_when_due() {
    let temp = tempfile::tempdir().expect("tempdir");
    let plugins_dir = temp.path().join("plugins");
    fs::create_dir_all(&plugins_dir).expect("plugins dir");
    let timestamp = temp.path().join("cache/plugin.timestamp");
    let now = 1_700_000_000_i64;
    fs::create_dir_all(timestamp.parent().unwrap()).expect("cache dir");
    fs::write(&timestamp, format!("{}\n", now - 8 * 86_400)).expect("timestamp");

    let output = run_zsh_kit_with(
        &[
            "plugin",
            "maybe-update",
            "--plugins-dir",
            &plugins_dir.to_string_lossy(),
            "--timestamp-file",
            &timestamp.to_string_lossy(),
            "--interval-days",
            "7",
            "--format",
            "json",
        ],
        temp.path(),
        &[("ZSH_KIT_PLUGIN_NOW_EPOCH", &now.to_string())],
    );

    assert_eq!(output.code, 0, "stderr: {}", output.stderr_text());
    assert_eq!(fs::read_to_string(&timestamp).unwrap(), format!("{now}\n"));
    let json: Value = serde_json::from_str(&output.stdout_text()).expect("json");
    assert_eq!(json["data"]["command"], "maybe-update");
    assert_eq!(json["data"]["update_due"], true);
    assert_eq!(json["data"]["mutation_status"], "unchanged");
}
