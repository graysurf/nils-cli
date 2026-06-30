//! End-to-end coverage for `agent-runtime bootstrap-host`.

use nils_test_support::bin;
use nils_test_support::cmd::{self, CmdOutput};
use pretty_assertions::assert_eq;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

fn agent_runtime_bin() -> PathBuf {
    bin::resolve("agent-runtime")
}

fn run(args: &[&str]) -> CmdOutput {
    cmd::run(&agent_runtime_bin(), args, &[], None)
}

fn write(path: &Path, body: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, body).unwrap();
}

fn fixture(tmp: &TempDir) -> PathBuf {
    let root = tmp.path().join("src");
    let live = tmp.path().join("live");
    let state = tmp.path().join("state");
    let codex_live = live.join("codex");
    let claude_live = live.join("claude");
    let hermes_live = live.join("hermes");

    write(
        &root.join("manifests/skills.yaml"),
        r#"
schema_version: 1
skills:
  - id: sample.tool
    domain: sample
    source: core/skills/sample/tool
    products:
      codex:
        name: /sample-tool
        render_to: skills/sample/tool/SKILL.md
      claude:
        name: sample-tool
        render_to: plugins/sample/skills/tool/SKILL.md
    required_clis: {}
"#,
    );
    write(
        &root.join("manifests/plugins.yaml"),
        r#"
schema_version: 1
plugins: []
"#,
    );
    write(
        &root.join("manifests/product-capabilities.yaml"),
        r#"
schema_version: 1
products:
  codex:
    nested_skill_support: true
    plugin_manifest:
      path_pattern: "ignored"
      loaded_at_runtime: false
      schema_ref: "ignored"
    hooks_model:
      config_surface: "ignored"
      payload_shape: "ignored"
      supports_inline_python: false
    config_activation:
      - "$CODEX_HOME/AGENTS.md"
    runtime_state:
      state_home_env: "STATE"
      default_path: "/tmp/state"
  claude:
    nested_skill_support: true
    plugin_manifest:
      path_pattern: "ignored"
      loaded_at_runtime: true
      schema_ref: "ignored"
    hooks_model:
      config_surface: "ignored"
      payload_shape: "ignored"
      supports_inline_python: true
    config_activation:
      - "$HOME/.claude/settings.json"
    runtime_state:
      state_home_env: "STATE"
      default_path: "/tmp/state"
  hermes:
    nested_skill_support: true
    plugin_manifest:
      path_pattern: "ignored"
      loaded_at_runtime: false
      schema_ref: "ignored"
    hooks_model:
      config_surface: "n/a"
      payload_shape: "n/a"
      supports_inline_python: false
    config_activation:
      - "$HOME/.hermes/SOUL.md"
    runtime_state:
      state_home_env: "HERMES_HOME"
      default_path: "/tmp/state"
"#,
    );
    write(
        &root.join("manifests/runtime-roots.yaml"),
        &format!(
            r#"
schema_version: 1
products:
  codex:
    live_home: "{}"
    docs_home: "{}"
    state_home: "{}"
    plugin_root: "{}/plugins"
    hook_config_strategy: managed-block
    min_version: "0.0.0"
    recommended_version: "0.0.0"
    min_version_effective_from: "2026-01-01"
    version_probe: "codex --version"
  claude:
    live_home: "{}"
    docs_home: "{}"
    state_home: "{}"
    plugin_root: "{}/plugins"
    hook_config_strategy: settings-json
    min_version: "0.0.0"
    recommended_version: "0.0.0"
    min_version_effective_from: "2026-01-01"
    version_probe: "claude --version"
  hermes:
    live_home: "{}"
    docs_home: "{}"
    state_home: "{}"
    min_version: "0.0.0"
    recommended_version: "0.0.0"
    min_version_effective_from: "2026-01-01"
    version_probe: "hermes --version"
"#,
            codex_live.display(),
            codex_live.display(),
            state.join("codex").display(),
            codex_live.display(),
            claude_live.display(),
            claude_live.display(),
            state.join("claude").display(),
            claude_live.display(),
            hermes_live.display(),
            hermes_live.display(),
            state.join("hermes").display(),
        ),
    );
    write(
        &root.join("manifests/cli-tools.yaml"),
        r#"
schema_version: 1
profiles:
  core: [ripgrep]
  recommended: [ripgrep]
  full: [ripgrep]
formulas:
  ripgrep:
    brew: ripgrep
    command: rg
    linux_only_alternative: null
    categories: [search]
"#,
    );
    write(
        &root.join("core/skills/sample/tool/SKILL.md.tera"),
        "# {{ skill_ref(id=\"sample.tool\") }}\n",
    );
    write(
        &root.join("targets/codex/link-map.yaml"),
        r#"
schema_version: 1
entries:
  - id: sample.codex-skill
    kind: symlinked-file
    source: build/codex/skills/sample/tool
    destination: skills/sample/tool
    recursive: false
"#,
    );
    write(
        &root.join("targets/claude/link-map.yaml"),
        r#"
schema_version: 1
entries:
  - id: sample.claude-skill
    kind: symlinked-file
    source: build/claude/plugins/sample/skills/tool
    destination: plugins/sample/skills/tool
    recursive: false
"#,
    );

    fs::canonicalize(root).unwrap()
}

fn phase<'a>(json: &'a Value, id: &str) -> &'a Value {
    json["phases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|phase| phase["id"] == id)
        .unwrap_or_else(|| panic!("missing phase {id}: {json:#}"))
}

fn verification<'a>(json: &'a Value, id: &str) -> &'a Value {
    json["verification"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == id)
        .unwrap_or_else(|| panic!("missing verification {id}: {json:#}"))
}

#[test]
fn dry_run_json_lists_pending_phases_without_mutation() {
    let tmp = TempDir::new().unwrap();
    let root = fixture(&tmp);
    let backup_root = tmp.path().join("checkpoint");
    let out = run(&[
        "bootstrap-host",
        "--source-root",
        root.to_str().unwrap(),
        "--backup-root",
        backup_root.to_str().unwrap(),
        "--profile",
        "core",
        "--product",
        "both",
        "--dry-run",
        "--format",
        "json",
        "--skip-homebrew-install",
        "--skip-cli-tools",
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let json: Value = serde_json::from_str(&out.stdout_text()).unwrap();

    assert_eq!(json["schema_version"], "agent-runtime.bootstrap-report.v1");
    assert_eq!(json["mode"], "dry-run");
    assert_eq!(json["exit_code"], 0);
    assert!(json["started_at_unix_ms"].is_number(), "{json:#}");
    assert!(json["completed_at_unix_ms"].is_number(), "{json:#}");
    assert!(json["duration_ms"].is_number(), "{json:#}");
    assert_eq!(json["summary"]["pending"], 9);
    assert_eq!(json["summary"]["skipped"], 5);
    assert_eq!(json["summary"]["failed"], 0);
    assert_eq!(phase(&json, "render:codex")["status"], "pending");
    assert!(phase(&json, "render:codex")["started_at_unix_ms"].is_number());
    assert!(phase(&json, "render:codex")["completed_at_unix_ms"].is_number());
    assert_eq!(phase(&json, "render:codex")["exit_code"], Value::Null);
    assert_eq!(phase(&json, "install:claude")["status"], "pending");
    let install_command = phase(&json, "install:claude")["command"].as_str().unwrap();
    assert!(install_command.contains("--live-home"), "{install_command}");
    assert!(
        install_command.contains("--state-home"),
        "{install_command}"
    );
    assert!(
        install_command.contains(&backup_root.join("state/claude").display().to_string()),
        "{install_command}"
    );
    assert!(
        !backup_root.join("checkpoint.json").exists(),
        "dry-run should not write a checkpoint"
    );
    assert!(
        !root.join("build/codex").exists(),
        "dry-run should not render product output"
    );
    assert_eq!(
        verification(&json, "installed-versions")["status"],
        "skipped"
    );
    assert_eq!(verification(&json, "docs-audit")["status"], "skipped");
    assert_eq!(verification(&json, "zsh-kit-smoke")["status"], "skipped");
    assert_eq!(verification(&json, "codex-doctor")["status"], "pending");
    assert_eq!(verification(&json, "claude-doctor")["status"], "pending");
    assert_eq!(
        verification(&json, "codex-prompt-input")["status"],
        "skipped"
    );
}

#[test]
fn apply_json_writes_checkpoint_and_installs_sandbox() {
    let tmp = TempDir::new().unwrap();
    let root = fixture(&tmp);
    let backup_root = tmp.path().join("checkpoint");
    let out = run(&[
        "bootstrap-host",
        "--source-root",
        root.to_str().unwrap(),
        "--backup-root",
        backup_root.to_str().unwrap(),
        "--profile",
        "core",
        "--product",
        "both",
        "--apply",
        "--format",
        "json",
        "--skip-homebrew-install",
        "--skip-cli-tools",
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let json: Value = serde_json::from_str(&out.stdout_text()).unwrap();

    assert_eq!(json["mode"], "apply");
    assert_eq!(json["exit_code"], 0);
    assert_eq!(phase(&json, "render:codex")["status"], "completed");
    assert_eq!(phase(&json, "render:codex")["exit_code"], 0);
    assert!(
        phase(&json, "render:codex")["evidence"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item.as_str().unwrap().contains("output=")),
        "{json:#}"
    );
    assert_eq!(phase(&json, "install:claude")["status"], "completed");
    assert_eq!(
        phase(&json, "doctor-skill-surface:codex")["status"],
        "completed"
    );
    assert!(
        backup_root.join("checkpoint.json").exists(),
        "apply should write a checkpoint"
    );
    assert!(
        root.join("build/codex/skills/sample/tool/SKILL.md")
            .exists(),
        "codex render output missing"
    );
    assert!(
        root.join("build/claude/plugins/sample/skills/tool/SKILL.md")
            .exists(),
        "claude render output missing"
    );
    assert!(
        tmp.path()
            .join("live/codex/skills/sample/tool")
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink(),
        "codex skill should be installed as a symlink"
    );
    assert!(
        tmp.path()
            .join("live/claude/plugins/sample/skills/tool")
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink(),
        "claude skill should be installed as a symlink"
    );

    let rerun = run(&[
        "bootstrap-host",
        "--source-root",
        root.to_str().unwrap(),
        "--backup-root",
        backup_root.to_str().unwrap(),
        "--profile",
        "core",
        "--product",
        "both",
        "--dry-run",
        "--format",
        "json",
        "--skip-homebrew-install",
        "--skip-cli-tools",
    ]);
    assert_eq!(rerun.code, 0, "stderr: {}", rerun.stderr_text());
    let rerun_json: Value = serde_json::from_str(&rerun.stdout_text()).unwrap();
    assert_eq!(phase(&rerun_json, "render:codex")["status"], "completed");
    assert_eq!(phase(&rerun_json, "install:claude")["status"], "completed");
}

#[test]
fn apply_failure_writes_report_and_checkpoint_with_pending_remainder() {
    let tmp = TempDir::new().unwrap();
    let root = fixture(&tmp);
    let backup_root = tmp.path().join("checkpoint");
    fs::remove_file(root.join("targets/codex/link-map.yaml")).unwrap();

    let out = run(&[
        "bootstrap-host",
        "--source-root",
        root.to_str().unwrap(),
        "--backup-root",
        backup_root.to_str().unwrap(),
        "--profile",
        "core",
        "--product",
        "codex",
        "--apply",
        "--format",
        "json",
        "--skip-homebrew-install",
        "--skip-cli-tools",
    ]);
    assert_eq!(out.code, 2, "stderr: {}", out.stderr_text());
    let json: Value = serde_json::from_str(&out.stdout_text()).unwrap();

    assert_eq!(json["schema_version"], "agent-runtime.bootstrap-report.v1");
    assert_eq!(json["ok"], false);
    assert_eq!(json["exit_code"], 2);
    assert_eq!(json["summary"]["completed"], 3);
    assert_eq!(json["summary"]["failed"], 1);
    assert_eq!(json["summary"]["pending"], 2);
    assert_eq!(phase(&json, "render:codex")["status"], "completed");
    assert_eq!(phase(&json, "install:codex")["status"], "failed");
    assert_eq!(phase(&json, "install:codex")["exit_code"], 2);
    assert_eq!(phase(&json, "prune-stale:codex")["status"], "pending");
    assert!(
        phase(&json, "install:codex")["message"]
            .as_str()
            .unwrap()
            .contains("link-map.yaml"),
        "{json:#}"
    );

    let checkpoint: Value =
        serde_json::from_str(&fs::read_to_string(backup_root.join("checkpoint.json")).unwrap())
            .unwrap();
    assert_eq!(checkpoint["exit_code"], 2);
    assert_eq!(phase(&checkpoint, "install:codex")["status"], "failed");
    assert_eq!(phase(&checkpoint, "prune-stale:codex")["status"], "pending");
}
