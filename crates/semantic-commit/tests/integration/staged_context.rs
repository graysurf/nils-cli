use crate::common;
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use pretty_assertions::assert_eq;

fn as_str(output: &[u8]) -> String {
    String::from_utf8_lossy(output).to_string()
}

fn env_refs<'a>(envs: &'a [(&'static str, String)]) -> Vec<(&'static str, &'a str)> {
    envs.iter()
        .map(|(key, value)| (*key, value.as_str()))
        .collect()
}

fn stage_file(repo: &Path, name: &str, contents: &str) {
    common::write_file(repo, name, contents);
    common::git(repo, &["add", name]);
}

fn extract_context_json(stdout: &str) -> Value {
    let (_, rest) = stdout
        .split_once("===== commit-context.json =====")
        .expect("missing commit-context header");
    let (json_part, _) = rest
        .split_once("===== staged.patch =====")
        .expect("missing staged.patch header");
    serde_json::from_str(json_part.trim()).expect("parse commit-context.json")
}

fn parse_json(stdout: &str) -> Value {
    serde_json::from_str(stdout.trim()).expect("parse json output")
}

fn find_file<'a>(files: &'a [Value], path: &str) -> &'a Value {
    files
        .iter()
        .find(|value| value.get("path").and_then(|p| p.as_str()) == Some(path))
        .expect("missing file entry")
}

#[test]
fn staged_context_outside_git_repo_errors() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let output = common::run_semantic_commit_output(dir.path(), &["staged-context"], &[], None);

    assert_eq!(output.status.code(), Some(1));
    assert!(as_str(&output.stderr).contains("error: must run inside a git work tree"));
}

#[test]
fn staged_context_missing_git_dependency_exits_5() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let tool_dir = tempfile::TempDir::new().expect("tempdir");
    let path_env = tool_dir.path().to_str().expect("tool dir path");
    let envs_owned = vec![("PATH", path_env.to_string())];
    let envs = env_refs(&envs_owned);

    let output =
        common::run_semantic_commit_output(dir.path(), &["staged-context", "--json"], &envs, None);

    assert_eq!(output.status.code(), Some(5));
    assert!(as_str(&output.stderr).contains("error: git is required"));
}

#[test]
fn staged_context_help_flag_prints_usage() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let output =
        common::run_semantic_commit_output(dir.path(), &["staged-context", "--help"], &[], None);

    assert_eq!(output.status.code(), Some(0));
    assert!(
        as_str(&output.stdout)
            .contains("semantic-commit staged-context [--format <bundle|json|patch>]")
    );
}

#[test]
fn staged_context_unknown_argument_errors_before_git_checks() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let output =
        common::run_semantic_commit_output(dir.path(), &["staged-context", "--bogus"], &[], None);

    assert_eq!(output.status.code(), Some(1));
    assert!(as_str(&output.stderr).contains("error: unknown argument: --bogus"));
}

#[test]
fn staged_context_invalid_format_value_errors() {
    let repo = common::init_repo();
    stage_file(repo.path(), "a.txt", "hello\n");

    let output = common::run_semantic_commit_output(
        repo.path(),
        &["staged-context", "--format", "yaml"],
        &[],
        None,
    );

    assert_eq!(output.status.code(), Some(1));
    assert!(as_str(&output.stderr).contains("error: invalid --format value: yaml"));
}

#[test]
fn staged_context_no_staged_changes_exits_2() {
    let repo = common::init_repo();
    let output = common::run_semantic_commit_output(repo.path(), &["staged-context"], &[], None);

    assert_eq!(output.status.code(), Some(2));
    assert!(
        as_str(&output.stderr)
            .contains("error: no staged changes (stage files with git add first)")
    );
}

#[test]
fn staged_context_bundle_format_prints_json_and_patch() {
    let repo = common::init_repo();
    stage_file(repo.path(), "a.txt", "hello\n");

    let output = common::run_semantic_commit_output(repo.path(), &["staged-context"], &[], None);

    assert_eq!(output.status.code(), Some(0));
    assert!(as_str(&output.stderr).is_empty());
    let stdout = as_str(&output.stdout);
    assert!(stdout.contains("===== commit-context.json ====="));
    assert!(stdout.contains("\"schemaVersion\":1"));
    assert!(stdout.contains("===== staged.patch ====="));
    assert!(stdout.contains("diff --git a/a.txt b/a.txt"));
}

#[test]
fn staged_context_json_format_outputs_single_json_document() {
    let repo = common::init_repo();
    stage_file(repo.path(), "a.txt", "hello\n");

    let output = common::run_semantic_commit_output(
        repo.path(),
        &["staged-context", "--format", "json"],
        &[],
        None,
    );

    assert_eq!(output.status.code(), Some(0));
    let stdout = as_str(&output.stdout);
    assert!(!stdout.contains("===== commit-context.json ====="));
    assert!(!stdout.contains("===== staged.patch ====="));

    let context = parse_json(&stdout);
    assert_eq!(context["schemaVersion"].as_i64(), Some(1));
    assert_eq!(
        context["staged"]["patch"]["path"].as_str(),
        Some("staged.patch")
    );
}

#[test]
fn staged_context_json_flag_alias_outputs_json() {
    let repo = common::init_repo();
    stage_file(repo.path(), "a.txt", "hello\n");

    let output =
        common::run_semantic_commit_output(repo.path(), &["staged-context", "--json"], &[], None);

    assert_eq!(output.status.code(), Some(0));
    let context = parse_json(&as_str(&output.stdout));
    assert_eq!(context["schemaVersion"].as_i64(), Some(1));
}

#[test]
fn staged_context_patch_format_outputs_patch_only() {
    let repo = common::init_repo();
    stage_file(repo.path(), "a.txt", "hello\n");

    let output = common::run_semantic_commit_output(
        repo.path(),
        &["staged-context", "--format", "patch"],
        &[],
        None,
    );

    assert_eq!(output.status.code(), Some(0));
    let stdout = as_str(&output.stdout);
    assert!(!stdout.contains("===== commit-context.json ====="));
    assert!(!stdout.contains("===== staged.patch ====="));
    assert!(stdout.contains("diff --git a/a.txt b/a.txt"));
}

#[test]
fn staged_context_summary_counts_and_flags() {
    let repo = common::init_repo();
    common::write_file(repo.path(), "README.md", "hello\n");
    common::write_file(repo.path(), "package-lock.json", "{}\n");
    fs::create_dir_all(repo.path().join("src")).expect("mkdir src");
    common::write_file(repo.path(), "src/lib.rs", "fn main() {}\n");
    fs::create_dir_all(repo.path().join("assets")).expect("mkdir assets");
    fs::write(repo.path().join("assets/logo.bin"), [0, 159, 146, 150]).expect("write binary");
    common::git(repo.path(), &["add", "."]);

    let output = common::run_semantic_commit_output(repo.path(), &["staged-context"], &[], None);

    assert_eq!(output.status.code(), Some(0));
    let stdout = as_str(&output.stdout);
    let context = extract_context_json(&stdout);

    let repo_name = repo
        .path()
        .file_name()
        .and_then(|name| name.to_str())
        .expect("repo name");
    assert_eq!(context["schemaVersion"].as_i64(), Some(1));
    assert_eq!(context["repo"]["name"].as_str(), Some(repo_name));
    assert_eq!(context["git"]["branch"].as_str(), Some("main"));

    let summary = &context["staged"]["summary"];
    assert_eq!(summary["fileCount"].as_i64(), Some(4));
    assert_eq!(summary["rootFileCount"].as_i64(), Some(2));
    assert_eq!(summary["binaryFileCount"].as_i64(), Some(1));
    assert_eq!(summary["lockfileCount"].as_i64(), Some(1));
    assert_eq!(summary["topLevelDirCount"].as_i64(), Some(2));
    assert_eq!(summary["insertions"].as_i64(), Some(3));
    assert_eq!(summary["deletions"].as_i64(), Some(0));

    let mut status_map = BTreeMap::new();
    for entry in context["staged"]["statusCounts"]
        .as_array()
        .expect("statusCounts array")
    {
        let status = entry["status"].as_str().expect("status str").to_string();
        let count = entry["count"].as_i64().expect("count int");
        status_map.insert(status, count);
    }
    assert_eq!(status_map.get("A"), Some(&4));

    let mut dir_map = BTreeMap::new();
    for entry in context["staged"]["structure"]["topLevelDirs"]
        .as_array()
        .expect("topLevelDirs array")
    {
        let name = entry["name"].as_str().expect("dir name").to_string();
        let count = entry["count"].as_i64().expect("dir count");
        dir_map.insert(name, count);
    }
    assert_eq!(dir_map.get("assets"), Some(&1));
    assert_eq!(dir_map.get("src"), Some(&1));

    let files = context["staged"]["files"].as_array().expect("files array");
    let lockfile = find_file(files, "package-lock.json");
    assert_eq!(lockfile["lockfile"].as_bool(), Some(true));
    assert_eq!(lockfile["binary"].as_bool(), Some(false));

    let binary = find_file(files, "assets/logo.bin");
    assert_eq!(binary["binary"].as_bool(), Some(true));
    assert!(binary["insertions"].is_null());
    assert!(binary["deletions"].is_null());
}

#[test]
fn staged_context_records_renames() {
    let repo = common::init_repo();
    common::git(repo.path(), &["config", "diff.renames", "true"]);
    common::write_file(repo.path(), "old.txt", "hello\n");
    common::git(repo.path(), &["add", "old.txt"]);
    common::git(repo.path(), &["commit", "-m", "chore: init"]);

    common::git(repo.path(), &["mv", "old.txt", "new.txt"]);
    common::git(repo.path(), &["add", "-A"]);

    let output = common::run_semantic_commit_output(repo.path(), &["staged-context"], &[], None);

    assert_eq!(output.status.code(), Some(0));
    let stdout = as_str(&output.stdout);
    let context = extract_context_json(&stdout);
    let files = context["staged"]["files"].as_array().expect("files array");

    let entry = find_file(files, "new.txt");
    assert_eq!(entry["status"].as_str(), Some("R"));
    assert_eq!(entry["oldPath"].as_str(), Some("old.txt"));
    assert_eq!(context["staged"]["summary"]["fileCount"].as_i64(), Some(1));
    let status_counts = context["staged"]["statusCounts"]
        .as_array()
        .expect("statusCounts array");
    assert!(
        status_counts
            .iter()
            .any(|item| item["status"].as_str() == Some("R"))
    );
}

#[test]
fn staged_context_repo_flag_runs_from_external_cwd() {
    let outer = tempfile::TempDir::new().expect("tempdir");
    let repo = common::init_repo();
    stage_file(repo.path(), "a.txt", "hello\n");

    let repo_path = repo.path().to_str().expect("repo path");
    let output = common::run_semantic_commit_output(
        outer.path(),
        &["staged-context", "--repo", repo_path, "--json"],
        &[],
        None,
    );

    assert_eq!(output.status.code(), Some(0));
    let context = parse_json(&as_str(&output.stdout));
    assert_eq!(context["schemaVersion"].as_i64(), Some(1));
}
