//! Behavioral coverage for zsh-kit plugin branches the existing `plugin` suite
//! does not exercise: force/exists/no-url fetch outcomes, real fast-forward and
//! failure update paths, status/maybe-update timing branches, interval and
//! now-epoch validation, env-driven directory resolution, and text-format
//! error rendering.

use std::fs;
use std::path::Path;

use nils_test_support::cmd::{CmdOptions, CmdOutput, run_resolved};
use nils_test_support::git::{InitRepoOptions, commit_file, git, init_repo_at_with};
use pretty_assertions::assert_eq;
use serde_json::Value;

/// Run zsh-kit with a clean plugin environment so the developer's own
/// ZDOTDIR / cache settings never leak into resolution tests. `HOME` points at
/// `home`; callers add only the env they intend to test.
fn run_plugin(args: &[&str], home: &Path, envs: &[(&str, &str)]) -> CmdOutput {
    let mut options = CmdOptions::new()
        .with_env("HOME", &home.to_string_lossy())
        .with_env_remove_many(&[
            "ZSH_PLUGINS_DIR",
            "ZDOTDIR",
            "ZSH_CACHE_DIR",
            "PLUGIN_UPDATE_FILE",
            "PLUGIN_UPDATE_INTERVAL_DAYS",
            "ZSH_KIT_PLUGIN_NOW_EPOCH",
        ]);
    for (key, value) in envs {
        options = options.with_env(key, value);
    }
    run_resolved("zsh-kit", args, &options)
}

fn json(output: &CmdOutput) -> Value {
    serde_json::from_str(&output.stdout_text()).expect("stdout should be json")
}

fn fixture_plugin_repo(root: &Path) {
    fs::create_dir_all(root).expect("repo dir");
    init_repo_at_with(
        root,
        InitRepoOptions::new()
            .with_branch("main")
            .with_initial_commit(),
    );
}

#[test]
fn fetch_force_dry_run_plans_reclone_then_reports_existing_dir() {
    let temp = tempfile::tempdir().expect("tempdir");
    let plugins_dir = temp.path().join("plugins");
    fs::create_dir_all(plugins_dir.join("demo")).expect("existing plugin dir");

    let output = run_plugin(
        &[
            "plugin",
            "fetch",
            "--entry",
            "demo::git=https://example.test/demo.git",
            "--plugins-dir",
            &plugins_dir.to_string_lossy(),
            "--force",
            "--dry-run",
            "--format",
            "json",
        ],
        temp.path(),
        &[],
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let value = json(&output);
    assert_eq!(value["data"]["force"], true);
    let kinds: Vec<&str> = value["data"]["actions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|action| action["kind"].as_str().unwrap())
        .collect();
    assert!(kinds.contains(&"remove"), "actions={kinds:?}");
    // Dry-run leaves the directory in place, so it is still reported as existing.
    assert!(plugins_dir.join("demo").is_dir());
}

#[test]
fn fetch_without_git_url_is_skipped() {
    let temp = tempfile::tempdir().expect("tempdir");
    let plugins_dir = temp.path().join("plugins");

    let output = run_plugin(
        &[
            "plugin",
            "fetch",
            "--entry",
            "demo",
            "--plugins-dir",
            &plugins_dir.to_string_lossy(),
            "--format",
            "json",
        ],
        temp.path(),
        &[],
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let value = json(&output);
    assert_eq!(value["data"]["mutation_status"], "unchanged");
    assert_eq!(value["data"]["skipped"], 1);
    assert!(!plugins_dir.join("demo").exists());
}

#[test]
fn update_missing_directory_is_unchanged() {
    let temp = tempfile::tempdir().expect("tempdir");
    let plugins_dir = temp.path().join("absent");

    let output = run_plugin(
        &[
            "plugin",
            "update",
            "--plugins-dir",
            &plugins_dir.to_string_lossy(),
            "--format",
            "json",
        ],
        temp.path(),
        &[],
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    assert_eq!(json(&output)["data"]["mutation_status"], "unchanged");
}

#[test]
fn update_without_remote_records_failure() {
    let temp = tempfile::tempdir().expect("tempdir");
    let plugins_dir = temp.path().join("plugins");
    fixture_plugin_repo(&plugins_dir.join("demo"));

    let output = run_plugin(
        &[
            "plugin",
            "update",
            "--plugins-dir",
            &plugins_dir.to_string_lossy(),
            "--format",
            "json",
        ],
        temp.path(),
        &[],
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let value = json(&output);
    assert_eq!(value["data"]["failed"], 1);
    assert_eq!(
        value["data"]["failures"][0]["plugin_id"].as_str().unwrap(),
        "demo"
    );
}

#[test]
fn update_reports_up_to_date_when_current() {
    let temp = tempfile::tempdir().expect("tempdir");
    let upstream = temp.path().join("upstream");
    fixture_plugin_repo(&upstream);
    let plugins_dir = temp.path().join("plugins");
    fs::create_dir_all(&plugins_dir).expect("plugins dir");
    git(
        temp.path(),
        &[
            "clone",
            &upstream.to_string_lossy(),
            &plugins_dir.join("demo").to_string_lossy(),
        ],
    );

    let output = run_plugin(
        &[
            "plugin",
            "update",
            "--plugins-dir",
            &plugins_dir.to_string_lossy(),
            "--format",
            "json",
        ],
        temp.path(),
        &[],
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let value = json(&output);
    assert_eq!(value["data"]["failed"], 0);
    assert_eq!(value["data"]["unchanged"], 1);
    assert_eq!(value["data"]["mutation_status"], "unchanged");
}

#[test]
fn update_fast_forwards_new_upstream_commits() {
    let temp = tempfile::tempdir().expect("tempdir");
    let upstream = temp.path().join("upstream");
    fixture_plugin_repo(&upstream);
    let plugins_dir = temp.path().join("plugins");
    fs::create_dir_all(&plugins_dir).expect("plugins dir");
    git(
        temp.path(),
        &[
            "clone",
            &upstream.to_string_lossy(),
            &plugins_dir.join("demo").to_string_lossy(),
        ],
    );
    // Advance the upstream so the plugin clone is one commit behind.
    commit_file(&upstream, "added.txt", "new\n", "add file");

    let output = run_plugin(
        &[
            "plugin",
            "update",
            "--plugins-dir",
            &plugins_dir.to_string_lossy(),
            "--format",
            "json",
        ],
        temp.path(),
        &[],
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let value = json(&output);
    assert_eq!(value["data"]["updated"], 1);
    assert_eq!(value["data"]["mutation_status"], "applied");
    assert!(plugins_dir.join("demo/added.txt").is_file());
}

#[test]
fn status_reports_never_updated_without_timestamp() {
    let temp = tempfile::tempdir().expect("tempdir");
    let timestamp = temp.path().join("missing.timestamp");

    let output = run_plugin(
        &[
            "plugin",
            "status",
            "--timestamp-file",
            &timestamp.to_string_lossy(),
            "--format",
            "json",
        ],
        temp.path(),
        &[],
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let value = json(&output);
    assert_eq!(value["data"]["update_due"], true);
    assert!(
        output.stdout_text().contains("never updated")
            || value["data"]["mutation_status"] == "reported"
    );
}

#[test]
fn status_reports_due_when_interval_elapsed() {
    let temp = tempfile::tempdir().expect("tempdir");
    let timestamp = temp.path().join("plugin.timestamp");
    let now = 1_700_000_000_i64;
    fs::write(&timestamp, format!("{}\n", now - 30 * 86_400)).expect("timestamp");

    let output = run_plugin(
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

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let value = json(&output);
    assert_eq!(value["data"]["update_due"], true);
    assert_eq!(value["data"]["days_ago"], 30);
}

#[test]
fn maybe_update_skips_when_not_due() {
    let temp = tempfile::tempdir().expect("tempdir");
    let plugins_dir = temp.path().join("plugins");
    fs::create_dir_all(&plugins_dir).expect("plugins dir");
    let timestamp = temp.path().join("plugin.timestamp");
    let now = 1_700_000_000_i64;
    fs::write(&timestamp, format!("{}\n", now - 86_400)).expect("timestamp");

    let output = run_plugin(
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

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let value = json(&output);
    assert_eq!(value["data"]["update_due"], false);
    assert_eq!(value["data"]["mutation_status"], "unchanged");
}

#[test]
fn maybe_update_dry_run_when_due_does_not_write_timestamp() {
    let temp = tempfile::tempdir().expect("tempdir");
    let plugins_dir = temp.path().join("plugins");
    fs::create_dir_all(&plugins_dir).expect("plugins dir");
    let timestamp = temp.path().join("plugin.timestamp");
    let now = 1_700_000_000_i64;
    let original = format!("{}\n", now - 30 * 86_400);
    fs::write(&timestamp, &original).expect("timestamp");

    let output = run_plugin(
        &[
            "plugin",
            "maybe-update",
            "--plugins-dir",
            &plugins_dir.to_string_lossy(),
            "--timestamp-file",
            &timestamp.to_string_lossy(),
            "--interval-days",
            "7",
            "--dry-run",
            "--format",
            "json",
        ],
        temp.path(),
        &[("ZSH_KIT_PLUGIN_NOW_EPOCH", &now.to_string())],
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let value = json(&output);
    assert_eq!(value["data"]["update_due"], true);
    assert_eq!(value["data"]["dry_run"], true);
    assert_eq!(
        fs::read_to_string(&timestamp).expect("timestamp"),
        original,
        "dry-run must not advance the timestamp"
    );
}

#[test]
fn interval_days_zero_is_rejected() {
    let temp = tempfile::tempdir().expect("tempdir");
    let timestamp = temp.path().join("plugin.timestamp");

    let output = run_plugin(
        &[
            "plugin",
            "status",
            "--timestamp-file",
            &timestamp.to_string_lossy(),
            "--interval-days",
            "0",
            "--format",
            "json",
        ],
        temp.path(),
        &[],
    );

    assert_eq!(output.code, 65, "stderr={}", output.stderr_text());
    assert_eq!(json(&output)["error"]["code"], "invalid-interval-days");
}

#[test]
fn interval_days_resolved_from_environment() {
    let temp = tempfile::tempdir().expect("tempdir");
    let timestamp = temp.path().join("missing.timestamp");

    let output = run_plugin(
        &[
            "plugin",
            "status",
            "--timestamp-file",
            &timestamp.to_string_lossy(),
            "--format",
            "json",
        ],
        temp.path(),
        &[("PLUGIN_UPDATE_INTERVAL_DAYS", "3")],
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    assert_eq!(json(&output)["data"]["interval_days"], 3);
}

#[test]
fn invalid_now_epoch_is_rejected() {
    let temp = tempfile::tempdir().expect("tempdir");
    let timestamp = temp.path().join("plugin.timestamp");
    fs::write(&timestamp, "1700000000\n").expect("timestamp");

    let output = run_plugin(
        &[
            "plugin",
            "status",
            "--timestamp-file",
            &timestamp.to_string_lossy(),
            "--format",
            "json",
        ],
        temp.path(),
        &[("ZSH_KIT_PLUGIN_NOW_EPOCH", "not-a-number")],
    );

    assert_eq!(output.code, 65, "stderr={}", output.stderr_text());
    assert_eq!(json(&output)["error"]["code"], "invalid-now-epoch");
}

#[test]
fn plugins_dir_resolves_from_zsh_plugins_dir_env() {
    let temp = tempfile::tempdir().expect("tempdir");
    let plugins_dir = temp.path().join("env-plugins");

    let output = run_plugin(
        &["plugin", "update", "--format", "json"],
        temp.path(),
        &[("ZSH_PLUGINS_DIR", &plugins_dir.to_string_lossy())],
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let value = json(&output);
    assert_eq!(value["data"]["mutation_status"], "unchanged");
    assert_eq!(
        value["data"]["plugins_dir"].as_str().unwrap(),
        plugins_dir.to_string_lossy()
    );
}

#[test]
fn plugins_dir_resolves_from_zdotdir_env() {
    let temp = tempfile::tempdir().expect("tempdir");
    let zdotdir = temp.path().join("zdot");

    let output = run_plugin(
        &["plugin", "update", "--format", "json"],
        temp.path(),
        &[("ZDOTDIR", &zdotdir.to_string_lossy())],
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    assert_eq!(
        json(&output)["data"]["plugins_dir"].as_str().unwrap(),
        zdotdir.join("plugins").to_string_lossy()
    );
}

#[test]
fn plugins_dir_is_required_without_env_or_home() {
    // No temp dir needed: resolution errors out before touching the filesystem.
    let options = CmdOptions::new().with_env_remove_many(&[
        "HOME",
        "ZSH_PLUGINS_DIR",
        "ZDOTDIR",
        "ZSH_CACHE_DIR",
        "PLUGIN_UPDATE_FILE",
    ]);
    let output = run_resolved(
        "zsh-kit",
        &["plugin", "update", "--format", "json"],
        &options,
    );

    assert_eq!(output.code, 65, "stderr={}", output.stderr_text());
    assert_eq!(json(&output)["error"]["code"], "plugin-dir-not-set");
}

#[test]
fn timestamp_file_resolves_from_cache_dir_env() {
    let temp = tempfile::tempdir().expect("tempdir");
    let cache = temp.path().join("cache");

    let output = run_plugin(
        &["plugin", "status", "--format", "json"],
        temp.path(),
        &[("ZSH_CACHE_DIR", &cache.to_string_lossy())],
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    assert_eq!(
        json(&output)["data"]["timestamp_file"].as_str().unwrap(),
        cache.join("plugin.timestamp").to_string_lossy()
    );
}

#[test]
fn fetch_invalid_entry_text_format_writes_error_to_stderr() {
    let temp = tempfile::tempdir().expect("tempdir");
    let plugins_dir = temp.path().join("plugins");

    let output = run_plugin(
        &[
            "plugin",
            "fetch",
            "--entry",
            "bad/id",
            "--plugins-dir",
            &plugins_dir.to_string_lossy(),
        ],
        temp.path(),
        &[],
    );

    assert_eq!(output.code, 65, "stdout={}", output.stdout_text());
    assert!(
        output.stderr_text().contains("error:"),
        "stderr={}",
        output.stderr_text()
    );
}

// ---- timestamp / interval resolution ----

#[test]
fn timestamp_file_resolves_from_plugin_update_file_env() {
    let temp = tempfile::tempdir().expect("tempdir");
    let ts = temp.path().join("explicit.timestamp");

    let output = run_plugin(
        &["plugin", "status", "--format", "json"],
        temp.path(),
        &[("PLUGIN_UPDATE_FILE", &ts.to_string_lossy())],
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    assert_eq!(
        json(&output)["data"]["timestamp_file"].as_str().unwrap(),
        ts.to_string_lossy()
    );
}

#[test]
fn timestamp_file_falls_back_to_home_cache_dir() {
    let temp = tempfile::tempdir().expect("tempdir");
    // No timestamp env at all -> HOME/.cache/zsh/plugin.timestamp.
    let output = run_plugin(&["plugin", "status", "--format", "json"], temp.path(), &[]);

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    assert_eq!(
        json(&output)["data"]["timestamp_file"].as_str().unwrap(),
        temp.path()
            .join(".cache/zsh/plugin.timestamp")
            .to_string_lossy()
    );
}

#[test]
fn timestamp_file_is_required_without_env_or_home() {
    let options = CmdOptions::new().with_env_remove_many(&[
        "HOME",
        "PLUGIN_UPDATE_FILE",
        "ZSH_CACHE_DIR",
        "ZDOTDIR",
    ]);
    let output = run_resolved(
        "zsh-kit",
        &["plugin", "status", "--format", "json"],
        &options,
    );

    assert_eq!(output.code, 65, "stderr={}", output.stderr_text());
    assert_eq!(json(&output)["error"]["code"], "timestamp-file-not-set");
}

#[test]
fn invalid_interval_env_is_rejected() {
    let temp = tempfile::tempdir().expect("tempdir");
    let ts = temp.path().join("plugin.timestamp");

    let output = run_plugin(
        &[
            "plugin",
            "status",
            "--timestamp-file",
            &ts.to_string_lossy(),
            "--format",
            "json",
        ],
        temp.path(),
        &[("PLUGIN_UPDATE_INTERVAL_DAYS", "not-a-number")],
    );

    assert_eq!(output.code, 65, "stderr={}", output.stderr_text());
    assert_eq!(json(&output)["error"]["code"], "invalid-interval-days");
}

#[test]
fn plugins_dir_tilde_is_expanded_against_home() {
    let temp = tempfile::tempdir().expect("tempdir");

    let output = run_plugin(
        &[
            "plugin",
            "update",
            "--plugins-dir",
            "~/plugins",
            "--format",
            "json",
        ],
        temp.path(),
        &[],
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    assert_eq!(
        json(&output)["data"]["plugins_dir"].as_str().unwrap(),
        temp.path().join("plugins").to_string_lossy()
    );
}

#[test]
fn status_uses_real_clock_when_no_epoch_override() {
    let temp = tempfile::tempdir().expect("tempdir");
    let ts = temp.path().join("plugin.timestamp");
    // A far-past timestamp; with the real clock this is well past any interval.
    fs::write(&ts, "1700000000\n").expect("timestamp");

    // No ZSH_KIT_PLUGIN_NOW_EPOCH -> exercises the SystemTime::now path.
    let output = run_plugin(
        &[
            "plugin",
            "status",
            "--timestamp-file",
            &ts.to_string_lossy(),
            "--interval-days",
            "7",
            "--format",
            "json",
        ],
        temp.path(),
        &[],
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    assert_eq!(json(&output)["data"]["update_due"], true);
}

// ---- fetch failure / force re-clone ----

#[test]
fn fetch_reports_clone_failure_for_unreachable_source() {
    let temp = tempfile::tempdir().expect("tempdir");
    let plugins_dir = temp.path().join("plugins");
    let missing = temp.path().join("no-such-repo.git");

    let output = run_plugin(
        &[
            "plugin",
            "fetch",
            "--entry",
            &format!("demo::git={}", missing.to_string_lossy()),
            "--plugins-dir",
            &plugins_dir.to_string_lossy(),
            "--format",
            "json",
        ],
        temp.path(),
        &[],
    );

    assert_eq!(output.code, 1, "stdout={}", output.stdout_text());
    assert_eq!(json(&output)["ok"], false);
    assert_eq!(json(&output)["error"]["code"], "git-command-failed");
    assert!(!plugins_dir.join("demo").exists());
}

#[test]
fn fetch_force_reclones_over_existing_directory() {
    let temp = tempfile::tempdir().expect("tempdir");
    let source = temp.path().join("source");
    fixture_plugin_repo(&source);
    let plugins_dir = temp.path().join("plugins");
    // Pre-existing stale (non-git) directory that --force must remove.
    fs::create_dir_all(plugins_dir.join("demo")).expect("stale dir");
    fs::write(plugins_dir.join("demo/stale.txt"), "stale\n").expect("stale file");

    let output = run_plugin(
        &[
            "plugin",
            "fetch",
            "--entry",
            &format!("demo::git={}", source.to_string_lossy()),
            "--plugins-dir",
            &plugins_dir.to_string_lossy(),
            "--force",
            "--format",
            "json",
        ],
        temp.path(),
        &[],
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    assert!(
        plugins_dir.join("demo/.git").is_dir(),
        "should re-clone a real repo"
    );
    assert!(
        !plugins_dir.join("demo/stale.txt").exists(),
        "force must remove the stale directory before cloning"
    );
}
