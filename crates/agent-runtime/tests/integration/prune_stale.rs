//! `agent-runtime prune-stale` integration coverage.
//!
//! The command removes only stale live surfaces that are provably owned by
//! the active agent-runtime-kit source root. It must leave ambiguous user
//! runtime-home content untouched.

use nils_test_support::bin;
use nils_test_support::cmd::{self, CmdOutput};
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

fn agent_runtime_bin() -> PathBuf {
    bin::resolve("agent-runtime")
}

fn run_cli(args: &[&str]) -> CmdOutput {
    cmd::run(&agent_runtime_bin(), args, &[], None)
}

fn run_cli_with_env(args: &[&str], envs: &[(&str, &str)]) -> CmdOutput {
    cmd::run(&agent_runtime_bin(), args, envs, None)
}

fn write(path: &Path, body: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, body).unwrap();
}

fn symlink(source: &Path, dest: &Path) {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    std::os::unix::fs::symlink(source, dest).unwrap();
}

fn build_codex_source_root(tmp: &Path) -> PathBuf {
    let root = tmp.join("src");
    write(
        &root.join("build/codex/plugins/reporting/skills/current/SKILL.md"),
        "# current\n",
    );
    write(
        &root.join("targets/codex/link-map.yaml"),
        r#"schema_version: 1
entries:
  - id: reporting.current
    kind: symlinked-file
    source: build/codex/plugins/reporting/skills/current
    destination: skills/reporting/current
"#,
    );
    fs::canonicalize(&root).unwrap()
}

fn build_claude_source_root(tmp: &Path) -> PathBuf {
    let root = tmp.join("src");
    write(
        &root.join("build/claude/plugins/reporting/skills/current/SKILL.md"),
        "# current\n",
    );
    write(
        &root.join("targets/claude/link-map.yaml"),
        r#"schema_version: 1
entries:
  - id: reporting.skills-tree
    kind: symlinked-file
    source: build/claude/plugins/reporting/skills
    destination: plugins/reporting/skills
    recursive: true
"#,
    );
    fs::canonicalize(&root).unwrap()
}

#[test]
fn help_lists_owned_source_root_as_a_repeatable_structured_option() {
    let output = run_cli(&["prune-stale", "--help"]);

    assert_eq!(output.code, 0);
    let stdout = output.stdout_text();
    assert!(stdout.contains("--owned-source-root <ABSOLUTE_PATH>"));
    assert!(stdout.contains("repeatable"));
}

#[test]
fn dry_run_reports_owned_stale_codex_skill_without_mutating_home() {
    let tmp = TempDir::new().unwrap();
    let source_root = build_codex_source_root(tmp.path());
    let live_home = tmp.path().join("codex-home");
    let stale_dest = live_home.join("skills/reporting/old");
    symlink(
        &source_root.join("build/codex/plugins/reporting/skills/old"),
        &stale_dest,
    );

    let source_arg = source_root.to_string_lossy().into_owned();
    let live_arg = live_home.to_string_lossy().into_owned();
    let output = run_cli(&[
        "prune-stale",
        "--source-root",
        &source_arg,
        "--product",
        "codex",
        "--live-home",
        &live_arg,
        "--dry-run",
    ]);

    assert_eq!(
        output.code,
        0,
        "dry-run should succeed; stderr=\n{}",
        output.stderr_text()
    );
    let stderr = output.stderr_text();
    assert!(
        stderr.contains("would remove symlink"),
        "dry-run should report stale symlink removal; stderr=\n{stderr}"
    );
    assert!(
        stderr.contains("skills/reporting/old"),
        "dry-run should name the stale skill; stderr=\n{stderr}"
    );
    assert!(
        fs::symlink_metadata(&stale_dest)
            .unwrap()
            .file_type()
            .is_symlink(),
        "dry-run must not remove stale symlink"
    );
}

#[test]
fn apply_removes_owned_stale_codex_skill_and_second_apply_is_clean_noop() {
    let tmp = TempDir::new().unwrap();
    let source_root = build_codex_source_root(tmp.path());
    let live_home = tmp.path().join("codex-home");
    let stale_dest = live_home.join("skills/reporting/old");
    symlink(
        &source_root.join("build/codex/plugins/reporting/skills/old"),
        &stale_dest,
    );

    let source_arg = source_root.to_string_lossy().into_owned();
    let live_arg = live_home.to_string_lossy().into_owned();
    let output = run_cli(&[
        "prune-stale",
        "--source-root",
        &source_arg,
        "--product",
        "codex",
        "--live-home",
        &live_arg,
        "--apply",
    ]);

    assert_eq!(
        output.code,
        0,
        "apply should succeed; stderr=\n{}",
        output.stderr_text()
    );
    assert!(
        fs::symlink_metadata(&stale_dest).is_err(),
        "apply should remove stale symlink"
    );

    let second = run_cli(&[
        "prune-stale",
        "--source-root",
        &source_arg,
        "--product",
        "codex",
        "--live-home",
        &live_arg,
        "--apply",
    ]);
    assert_eq!(
        second.code,
        0,
        "second apply should succeed; stderr=\n{}",
        second.stderr_text()
    );
    assert!(
        second.stderr_text().contains("changes=0"),
        "second apply should report no changes; stderr=\n{}",
        second.stderr_text()
    );
}

#[test]
fn apply_prunes_recursive_claude_stale_skill_files_and_empty_directories() {
    let tmp = TempDir::new().unwrap();
    let source_root = build_claude_source_root(tmp.path());
    let live_home = tmp.path().join("claude-home");
    let stale_skill = live_home.join("plugins/reporting/skills/old");
    symlink(
        &source_root.join("build/claude/plugins/reporting/skills/old/SKILL.md"),
        &stale_skill.join("SKILL.md"),
    );
    symlink(
        &source_root.join("build/claude/plugins/reporting/skills/old/scripts/tool.sh"),
        &stale_skill.join("scripts/tool.sh"),
    );

    let source_arg = source_root.to_string_lossy().into_owned();
    let live_arg = live_home.to_string_lossy().into_owned();
    let output = run_cli(&[
        "prune-stale",
        "--source-root",
        &source_arg,
        "--product",
        "claude",
        "--live-home",
        &live_arg,
        "--apply",
    ]);

    assert_eq!(
        output.code,
        0,
        "apply should succeed; stderr=\n{}",
        output.stderr_text()
    );
    assert!(
        !stale_skill.exists(),
        "empty stale skill directory should be removed after stale symlinks"
    );
}

#[test]
fn apply_removes_stale_symlinks_from_explicit_prior_source_roots() {
    let tmp = TempDir::new().unwrap();
    let source_root = build_claude_source_root(tmp.path());
    let prior_source_root = tmp.path().join("prior-src");
    fs::create_dir_all(prior_source_root.join("targets/claude")).unwrap();
    fs::copy(
        source_root.join("targets/claude/link-map.yaml"),
        prior_source_root.join("targets/claude/link-map.yaml"),
    )
    .unwrap();
    let prior_source_root = fs::canonicalize(prior_source_root).unwrap();
    let second_prior_source_root = tmp.path().join("second-prior-src");
    fs::create_dir_all(second_prior_source_root.join("targets/claude")).unwrap();
    fs::copy(
        source_root.join("targets/claude/link-map.yaml"),
        second_prior_source_root.join("targets/claude/link-map.yaml"),
    )
    .unwrap();
    let second_prior_source_root = fs::canonicalize(second_prior_source_root).unwrap();
    let live_home = tmp.path().join("claude-home");
    let scan_root = live_home.join("plugins/reporting/skills");
    let stale = scan_root.join("old/scripts/tool.sh");
    let second_stale = scan_root.join("older/scripts/tool.sh");
    let foreign = scan_root.join("foreign/SKILL.md");

    symlink(
        &prior_source_root.join("build/claude/plugins/reporting/skills/old/scripts/tool.sh"),
        &stale,
    );
    symlink(
        &second_prior_source_root
            .join("build/claude/plugins/reporting/skills/older/scripts/tool.sh"),
        &second_stale,
    );
    symlink(Path::new("/var/empty/foreign-skill"), &foreign);

    let source_arg = source_root.to_string_lossy().into_owned();
    let prior_source_arg = prior_source_root.to_string_lossy().into_owned();
    let prior_source_equals_arg = format!("--owned-source-root={prior_source_arg}");
    let second_prior_source_arg = second_prior_source_root.to_string_lossy().into_owned();
    let live_arg = live_home.to_string_lossy().into_owned();
    let output = run_cli(&[
        "prune-stale",
        "--source-root",
        &source_arg,
        &prior_source_equals_arg,
        "--owned-source-root",
        &second_prior_source_arg,
        "--product",
        "claude",
        "--live-home",
        &live_arg,
        "--apply",
        "--format",
        "json",
    ]);

    assert_eq!(
        output.code,
        0,
        "apply should accept the explicit prior source root; stderr=\n{}",
        output.stderr_text()
    );
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let roots = json["data"]["owned_source_roots"].as_array().unwrap();
    assert_eq!(roots.len(), 3);
    assert!(roots.iter().any(|root| root == &source_arg));
    assert!(roots.iter().any(|root| root == &prior_source_arg));
    assert!(roots.iter().any(|root| root == &second_prior_source_arg));
    assert!(
        fs::symlink_metadata(&stale).is_err(),
        "stale symlink into the explicit prior source root should be removed"
    );
    assert!(
        fs::symlink_metadata(&second_stale).is_err(),
        "stale symlink into the second explicit prior source root should be removed"
    );
    assert!(
        fs::symlink_metadata(&foreign)
            .unwrap()
            .file_type()
            .is_symlink(),
        "unrelated foreign symlink must remain untouched"
    );
}

#[test]
fn rejects_relative_owned_source_root_flag() {
    let tmp = TempDir::new().unwrap();
    let source_root = build_codex_source_root(tmp.path());
    let live_home = tmp.path().join("codex-home");
    let source_arg = source_root.to_string_lossy().into_owned();
    let live_arg = live_home.to_string_lossy().into_owned();
    let output = run_cli(&[
        "prune-stale",
        "--source-root",
        &source_arg,
        "--owned-source-root",
        "prior-source",
        "--product",
        "codex",
        "--live-home",
        &live_arg,
        "--dry-run",
    ]);

    assert_eq!(output.code, 2);
    assert!(output.stderr_text().contains("must be absolute"));
}

#[test]
fn ambient_owned_source_root_variables_do_not_grant_authority() {
    let tmp = TempDir::new().unwrap();
    let source_root = build_codex_source_root(tmp.path());
    let live_home = tmp.path().join("codex-home");
    let source_arg = source_root.to_string_lossy().into_owned();
    let live_arg = live_home.to_string_lossy().into_owned();
    let foreign = live_home.join("skills/reporting/foreign");
    symlink(Path::new("/var/empty/foreign-skill"), &foreign);
    let output = run_cli_with_env(
        &[
            "prune-stale",
            "--source-root",
            &source_arg,
            "--product",
            "codex",
            "--live-home",
            &live_arg,
            "--apply",
        ],
        &[
            ("NILS_AGENT_RUNTIME_PRUNE_OWNED_SOURCE_ROOTS", "/"),
            ("NILS_AGENT_RUNTIME_PRUNE_CONFIRM_OWNED_SOURCE_ROOTS", "1"),
        ],
    );

    assert_eq!(output.code, 0);
    assert!(
        fs::symlink_metadata(&foreign)
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[test]
fn rejects_filesystem_root_without_removing_foreign_symlink() {
    let tmp = TempDir::new().unwrap();
    let source_root = build_codex_source_root(tmp.path());
    let live_home = tmp.path().join("codex-home");
    let foreign = live_home.join("skills/reporting/foreign");
    symlink(Path::new("/var/empty/foreign-skill"), &foreign);
    let source_arg = source_root.to_string_lossy().into_owned();
    let live_arg = live_home.to_string_lossy().into_owned();
    let output = run_cli(&[
        "prune-stale",
        "--source-root",
        &source_arg,
        "--owned-source-root",
        "/",
        "--product",
        "codex",
        "--live-home",
        &live_arg,
        "--apply",
    ]);

    assert_eq!(output.code, 2);
    assert!(output.stderr_text().contains("cannot be a filesystem root"));
    assert!(
        fs::symlink_metadata(&foreign)
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[test]
fn rejects_non_runtime_kit_root_without_removing_its_symlink() {
    let tmp = TempDir::new().unwrap();
    let source_root = build_codex_source_root(tmp.path());
    let unrelated_root = tmp.path().join("unrelated");
    write(
        &unrelated_root.join("targets/codex/link-map.yaml"),
        "schema_version: 1\nentries: []\n",
    );
    let unrelated_root = fs::canonicalize(unrelated_root).unwrap();
    let live_home = tmp.path().join("codex-home");
    let foreign = live_home.join("skills/reporting/foreign");
    symlink(&unrelated_root.join("foreign-skill"), &foreign);
    let source_arg = source_root.to_string_lossy().into_owned();
    let unrelated_arg = unrelated_root.to_string_lossy().into_owned();
    let live_arg = live_home.to_string_lossy().into_owned();
    let output = run_cli(&[
        "prune-stale",
        "--source-root",
        &source_arg,
        "--owned-source-root",
        &unrelated_arg,
        "--product",
        "codex",
        "--live-home",
        &live_arg,
        "--apply",
    ]);

    assert_eq!(output.code, 2);
    assert!(
        output
            .stderr_text()
            .contains("link-map contains no managed entries")
    );
    assert!(
        fs::symlink_metadata(&foreign)
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[test]
fn apply_skips_foreign_symlink_regular_file_and_non_empty_directory() {
    let tmp = TempDir::new().unwrap();
    let source_root = build_claude_source_root(tmp.path());
    let live_home = tmp.path().join("claude-home");
    let scan_root = live_home.join("plugins/reporting/skills");
    let foreign = scan_root.join("foreign/SKILL.md");
    let regular = scan_root.join("regular/SKILL.md");
    let non_empty = scan_root.join("non-empty");
    let empty_user_dir = scan_root.join("empty-user");

    symlink(Path::new("/var/empty/foreign-skill"), &foreign);
    write(&regular, "# user-owned regular file\n");
    write(&non_empty.join("note.txt"), "user-owned note\n");
    fs::create_dir_all(&empty_user_dir).unwrap();

    let source_arg = source_root.to_string_lossy().into_owned();
    let live_arg = live_home.to_string_lossy().into_owned();
    let output = run_cli(&[
        "prune-stale",
        "--source-root",
        &source_arg,
        "--product",
        "claude",
        "--live-home",
        &live_arg,
        "--apply",
    ]);

    assert_eq!(
        output.code,
        0,
        "apply should succeed with skipped candidates; stderr=\n{}",
        output.stderr_text()
    );
    let stderr = output.stderr_text();
    assert!(
        stderr.contains("foreign symlink"),
        "foreign symlink should be reported as skipped; stderr=\n{stderr}"
    );
    assert!(
        stderr.contains("regular file"),
        "regular file should be reported as skipped; stderr=\n{stderr}"
    );
    assert!(
        stderr.contains("non-empty directory"),
        "non-empty directory should be reported as skipped; stderr=\n{stderr}"
    );
    assert!(
        fs::symlink_metadata(&foreign)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        fs::read_to_string(&regular).unwrap(),
        "# user-owned regular file\n"
    );
    assert_eq!(
        fs::read_to_string(non_empty.join("note.txt")).unwrap(),
        "user-owned note\n"
    );
    assert!(
        empty_user_dir.is_dir(),
        "pre-existing empty user directory should not be removed"
    );
}
