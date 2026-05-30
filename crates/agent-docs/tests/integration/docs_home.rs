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
