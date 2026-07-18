//! Task 3.2 — docs-home resolution: `--docs-home` flag, install-symlink
//! derivation, the `AGENT_DOCS_HOME` fallback, and a clear error when none
//! resolve.

use std::fs;
use std::path::Path;

use nils_test_support::cmd;
use tempfile::TempDir;

use super::common::run_cli;

fn canonical(path: &Path) -> String {
    fs::canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .display()
        .to_string()
}

fn list_json_docs_home(args: &[&str], options: &cmd::CmdOptions) -> (i32, Option<String>, String) {
    let mut full = args.to_vec();
    full.extend_from_slice(&["list", "--format", "json"]);
    let out = run_cli(&full, options);
    let docs_home = if out.code == 0 {
        serde_json::from_str::<serde_json::Value>(&out.stdout)
            .ok()
            .and_then(|json| json["docs_home"].as_str().map(ToString::to_string))
    } else {
        None
    };
    (out.code, docs_home, out.stderr)
}

#[test]
fn flag_wins_over_env() {
    let kit = TempDir::new().unwrap();
    let other = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let options = cmd::CmdOptions::default()
        .with_cwd(project.path())
        .with_env("AGENT_DOCS_HOME", other.path().to_str().unwrap());
    let (code, docs_home, stderr) = list_json_docs_home(
        &[
            "--docs-home",
            kit.path().to_str().unwrap(),
            "--project-path",
            project.path().to_str().unwrap(),
        ],
        &options,
    );
    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(
        canonical(Path::new(&docs_home.unwrap())),
        canonical(kit.path())
    );
}

#[cfg(unix)]
#[test]
fn derived_from_install_symlink() {
    let home = TempDir::new().unwrap();
    let kit = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    fs::write(kit.path().join("AGENT_HOME.md"), "# home policy\n").unwrap();
    fs::create_dir_all(home.path().join(".claude")).unwrap();
    std::os::unix::fs::symlink(
        kit.path().join("AGENT_HOME.md"),
        home.path().join(".claude/CLAUDE.md"),
    )
    .unwrap();

    let options = cmd::CmdOptions::default()
        .with_cwd(project.path())
        .with_env("HOME", home.path().to_str().unwrap())
        .with_env_remove("AGENT_DOCS_HOME");
    let (code, docs_home, stderr) = list_json_docs_home(
        &["--project-path", project.path().to_str().unwrap()],
        &options,
    );
    assert_eq!(code, 0, "symlink derivation failed: {stderr}");
    assert_eq!(
        canonical(Path::new(&docs_home.unwrap())),
        canonical(kit.path()),
        "docs-home should derive from the install symlink"
    );
}

#[test]
fn env_fallback_when_no_symlink() {
    let home = TempDir::new().unwrap(); // empty: no install symlink
    let kit = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let options = cmd::CmdOptions::default()
        .with_cwd(project.path())
        .with_env("HOME", home.path().to_str().unwrap())
        .with_env("AGENT_DOCS_HOME", kit.path().to_str().unwrap());
    let (code, docs_home, stderr) = list_json_docs_home(
        &["--project-path", project.path().to_str().unwrap()],
        &options,
    );
    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(
        canonical(Path::new(&docs_home.unwrap())),
        canonical(kit.path())
    );
}

#[test]
fn clear_error_when_unresolvable() {
    let home = TempDir::new().unwrap(); // no symlink
    let project = TempDir::new().unwrap();
    let options = cmd::CmdOptions::default()
        .with_cwd(project.path())
        .with_env("HOME", home.path().to_str().unwrap())
        .with_env_remove("AGENT_DOCS_HOME");
    let out = run_cli(
        &["--project-path", project.path().to_str().unwrap(), "list"],
        &options,
    );
    assert_ne!(out.code, 0, "should fail when docs-home is unresolvable");
    assert!(
        out.stderr.contains("cannot locate docs-home") && out.stderr.contains("--docs-home"),
        "error should be explicit: {}",
        out.stderr
    );
}

#[test]
fn missing_docs_home_is_a_runtime_failure_in_text_mode() {
    let temp = TempDir::new().unwrap();
    let project = temp.path().join("project");
    let missing = temp.path().join("missing-docs-home");
    fs::create_dir(&project).unwrap();

    let out = run_cli(
        &[
            "--docs-home",
            missing.to_str().unwrap(),
            "--project-path",
            project.to_str().unwrap(),
            "preflight",
            "--intent",
            "project-dev",
            "--strict",
        ],
        &cmd::CmdOptions::default().with_cwd(&project),
    );

    assert_eq!(out.code, 4, "stdout={} stderr={}", out.stdout, out.stderr);
    assert!(
        out.stdout.is_empty(),
        "stdout must be empty: {}",
        out.stdout
    );
    assert!(out.stderr.contains("docs-home"), "stderr={}", out.stderr);
}

#[test]
fn missing_or_non_directory_docs_home_is_a_command_stable_json_failure() {
    let temp = TempDir::new().unwrap();
    let project = temp.path().join("project");
    let missing = temp.path().join("missing-docs-home");
    let file = temp.path().join("docs-home-file");
    fs::create_dir(&project).unwrap();
    fs::write(&file, "not a directory\n").unwrap();

    for docs_home in [&missing, &file] {
        let out = run_cli(
            &[
                "--docs-home",
                docs_home.to_str().unwrap(),
                "--project-path",
                project.to_str().unwrap(),
                "preflight",
                "--intent",
                "project-dev",
                "--strict",
                "--format",
                "json",
            ],
            &cmd::CmdOptions::default().with_cwd(&project),
        );

        assert_eq!(out.code, 4, "stdout={} stderr={}", out.stdout, out.stderr);
        assert!(
            out.stderr.is_empty(),
            "stderr must be empty: {}",
            out.stderr
        );
        let json: serde_json::Value = serde_json::from_str(&out.stdout).unwrap();
        assert_eq!(json["schema_version"], "cli.agent-docs.preflight.v2");
        assert_eq!(json["ok"], false);
        assert_eq!(json["error"]["code"], "root-resolution-failed");
    }
}

#[test]
fn existing_catalog_free_docs_home_remains_valid() {
    let temp = TempDir::new().unwrap();
    let docs_home = temp.path().join("docs-home");
    let project = temp.path().join("project");
    fs::create_dir(&docs_home).unwrap();
    fs::create_dir(&project).unwrap();

    let out = run_cli(
        &[
            "--docs-home",
            docs_home.to_str().unwrap(),
            "--project-path",
            project.to_str().unwrap(),
            "preflight",
            "--intent",
            "project-dev",
            "--strict",
            "--format",
            "json",
        ],
        &cmd::CmdOptions::default().with_cwd(&project),
    );

    assert_eq!(out.code, 0, "stdout={} stderr={}", out.stdout, out.stderr);
    assert_eq!(out.json()["schema_version"], "agent-docs.preflight.v2");
    assert_eq!(out.json()["documents"], serde_json::json!([]));
}
