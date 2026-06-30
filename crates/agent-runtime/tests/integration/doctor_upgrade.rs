//! Integration coverage for Plan 04 Sprint 3 Task 3.3 doctor upgrade,
//! project-overlay, and CLI coverage probes.

use agent_runtime::doctor::project::{self, ProjectOverlayStatus};
use nils_test_support::cmd::{self, CmdOutput};
use nils_test_support::{StubBinDir, bin};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

fn agent_runtime_bin() -> PathBuf {
    bin::resolve("agent-runtime")
}

fn run(args: &[&str], path: &str) -> CmdOutput {
    cmd::run(&agent_runtime_bin(), args, &[("PATH", path)], None)
}

fn write_exe(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
    let mut perms = fs::metadata(path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).unwrap();
}

fn write_source_root(
    tmp: &TempDir,
    home: &Path,
    state_home: &Path,
    required_clis: &str,
    cli_tools: &str,
) -> PathBuf {
    let root = tmp.path().join("src");
    fs::create_dir_all(home.join("plugins")).unwrap();
    fs::create_dir_all(root.join("manifests")).unwrap();
    fs::create_dir_all(root.join("targets").join("codex")).unwrap();
    fs::write(
        root.join("targets").join("codex").join("link-map.yaml"),
        "schema_version: 1\nentries: []\n",
    )
    .unwrap();
    fs::write(
        root.join("manifests").join("runtime-roots.yaml"),
        format!(
            "\
schema_version: 1
products:
  codex:
    live_home: \"{home}\"
    docs_home: \"{home}\"
    state_home: \"{state_home}\"
    plugin_root: \"{home}/plugins\"
    hook_config_strategy: managed-block
    min_version: \"0.1.0\"
    recommended_version: \"0.2.0\"
    min_version_effective_from: \"2099-01-01\"
    version_probe: \"codex --version\"
  claude:
    live_home: \"{home}\"
    docs_home: \"{home}\"
    state_home: \"{state_home}\"
    hook_config_strategy: settings-json
    min_version: \"0.1.0\"
    recommended_version: \"0.2.0\"
    min_version_effective_from: \"2099-01-01\"
    version_probe: \"claude --version\"
  hermes:
    live_home: \"{home}\"
    docs_home: \"{home}\"
    state_home: \"{state_home}\"
    min_version: \"0.1.0\"
    recommended_version: \"0.2.0\"
    min_version_effective_from: \"2099-01-01\"
    version_probe: \"hermes --version\"
",
            home = home.display(),
            state_home = state_home.display(),
        ),
    )
    .unwrap();
    fs::write(
        root.join("manifests").join("skills.yaml"),
        format!(
            "\
schema_version: 1
skills:
  - id: reporting.daily-brief
    domain: reporting
    source: core/skills/reporting/daily-brief
    products:
      codex:
        name: daily-brief
        render_to: plugins/reporting/skills/daily-brief/SKILL.md
    required_clis:
{required_clis}
"
        ),
    )
    .unwrap();
    fs::write(root.join("manifests").join("cli-tools.yaml"), cli_tools).unwrap();
    fs::canonicalize(&root).unwrap()
}

fn empty_cli_tools() -> &'static str {
    "\
schema_version: 1
profiles:
  core: []
  recommended: []
  full: []
formulas: {}
"
}

#[test]
fn missing_required_cli_binary_blocks() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let state_home = tmp.path().join("state");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&state_home).unwrap();
    let source_root = write_source_root(
        &tmp,
        &home,
        &state_home,
        "      agent-out: \">=0.13.0\"\n",
        empty_cli_tools(),
    );
    let bin_dir = StubBinDir::new();
    bin_dir.write_exe("codex", "#!/bin/sh\necho 'codex 0.2.0'\n");

    let output = run(
        &[
            "doctor",
            "--product",
            "codex",
            "--source-root",
            source_root.to_str().unwrap(),
            "--profile",
            "core",
        ],
        &bin_dir.path_str(),
    );

    assert_eq!(output.code, 2);
    let stderr = output.stderr_text();
    assert!(stderr.contains("block required-cli"), "{stderr}");
    assert!(stderr.contains("status=missing"), "{stderr}");
    assert!(stderr.contains("agent-out"), "{stderr}");
}

#[test]
fn outdated_required_cli_binary_warns() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let state_home = tmp.path().join("state");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&state_home).unwrap();
    let source_root = write_source_root(
        &tmp,
        &home,
        &state_home,
        "      agent-out: \">=0.13.0\"\n",
        empty_cli_tools(),
    );
    let bin_dir = StubBinDir::new();
    bin_dir.write_exe("codex", "#!/bin/sh\necho 'codex 0.2.0'\n");
    bin_dir.write_exe("agent-out", "#!/bin/sh\necho 'agent-out 0.12.0'\n");

    let output = run(
        &[
            "doctor",
            "--product",
            "codex",
            "--source-root",
            source_root.to_str().unwrap(),
            "--profile",
            "core",
        ],
        &bin_dir.path_str(),
    );

    assert_eq!(output.code, 1);
    let stderr = output.stderr_text();
    assert!(stderr.contains("warn required-cli"), "{stderr}");
    assert!(stderr.contains("status=outdated"), "{stderr}");
    assert!(stderr.contains("agent-out"), "{stderr}");
}

#[test]
fn suggest_upgrade_prints_one_brew_upgrade_line_per_formula() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let state_home = tmp.path().join("state");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&state_home).unwrap();
    let source_root = write_source_root(
        &tmp,
        &home,
        &state_home,
        "      agent-out: \">=0.13.0\"\n      git-scope: \">=0.13.0\"\n",
        "\
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
",
    );
    let bin_dir = StubBinDir::new();
    bin_dir.write_exe("codex", "#!/bin/sh\necho 'codex 0.1.0'\n");
    bin_dir.write_exe("agent-out", "#!/bin/sh\necho 'agent-out 0.12.0'\n");
    bin_dir.write_exe("git-scope", "#!/bin/sh\necho 'git-scope 0.12.0'\n");
    bin_dir.write_exe("rg", "#!/bin/sh\necho 'ripgrep 14.1.0'\n");
    bin_dir.write_exe(
        "brew",
        "#!/bin/sh\nif [ \"$1\" = \"outdated\" ]; then echo 'ripgrep'; fi\n",
    );

    let output = run(
        &[
            "doctor",
            "--product",
            "codex",
            "--source-root",
            source_root.to_str().unwrap(),
            "--profile",
            "core",
            "--suggest-upgrade",
        ],
        &bin_dir.path_str(),
    );

    assert_eq!(output.code, 1);
    let stdout = output.stdout_text();
    let mut lines = stdout.lines().collect::<Vec<_>>();
    lines.sort_unstable();
    assert_eq!(
        lines,
        vec![
            "brew upgrade codex",
            "brew upgrade nils-cli",
            "brew upgrade ripgrep",
        ]
    );
}

#[test]
fn check_project_reports_missing_overlay_scripts() {
    let tmp = TempDir::new().unwrap();
    let scripts = tmp.path().join(".agents").join("scripts");
    fs::create_dir_all(&scripts).unwrap();
    write_exe(&scripts.join("bench.sh"), "#!/bin/sh\nexit 0\n");

    let findings = project::probe_project(tmp.path());

    assert!(findings.iter().any(|finding| {
        finding.script == "bench" && finding.status == ProjectOverlayStatus::Wired
    }));
    assert!(findings.iter().any(|finding| {
        finding.script == "deploy" && finding.status == ProjectOverlayStatus::Missing
    }));
}
