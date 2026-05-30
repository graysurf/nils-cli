use std::fs;
use std::path::Path;

use agent_docs::commands::baseline::check_builtin_baseline;
use agent_docs::config::CONFIG_FILE_NAME;
use agent_docs::env::ResolvedRoots;
use agent_docs::model::{BaselineTarget, DocumentSource, DocumentStatus, OutputFormat, Scope};
use agent_docs::output::render_baseline;
use agent_docs::run_with_args;
use serde_json::Value;
use tempfile::TempDir;

fn write_text(path: &Path, body: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("failed to create parent directory");
    }
    fs::write(path, body).expect("failed to write fixture file");
}

fn write_markdown(path: &Path) {
    write_text(path, "# fixture\n");
}

fn roots(home: &TempDir, project: &TempDir) -> ResolvedRoots {
    ResolvedRoots {
        docs_home: home.path().to_path_buf(),
        project_path: project.path().to_path_buf(),
        is_linked_worktree: false,
        git_common_dir: None,
        primary_worktree_path: None,
    }
}

#[test]
fn baseline_check_target_home_reports_three_home_items() {
    let home = TempDir::new().expect("failed to create home tempdir");
    let project = TempDir::new().expect("failed to create project tempdir");
    write_markdown(&home.path().join("AGENTS.md"));

    let report = check_builtin_baseline(BaselineTarget::Home, &roots(&home, &project), false)
        .expect("baseline check should succeed");
    assert_eq!(report.items.len(), 3);
    assert!(report.items.iter().all(|item| item.scope == Scope::Home));
}

#[test]
fn baseline_check_target_home_includes_required_global_extension_docs() {
    let home = TempDir::new().expect("failed to create home tempdir");
    let project = TempDir::new().expect("failed to create project tempdir");
    write_markdown(&home.path().join("AGENTS.md"));
    write_markdown(&home.path().join("GLOBAL_POLICY.md"));
    write_text(
        &home.path().join(CONFIG_FILE_NAME),
        r#"
[[document]]
context = "project-dev"
scope = "global"
path = "GLOBAL_POLICY.md"
required = true
when = "always"
notes = "cross-repo global project-dev policy"
"#,
    );

    let report = check_builtin_baseline(BaselineTarget::Home, &roots(&home, &project), false)
        .expect("baseline check should succeed");
    let global_item = report
        .items
        .iter()
        .find(|item| item.scope == Scope::Global && item.path.ends_with("GLOBAL_POLICY.md"))
        .expect("baseline --target home should include global extension docs");

    assert_eq!(global_item.source, DocumentSource::ExtensionHome);
    assert_eq!(global_item.status, DocumentStatus::Present);
    assert_eq!(global_item.path, home.path().join("GLOBAL_POLICY.md"));
}

#[test]
fn baseline_check_target_project_reports_present_and_missing_in_text_output() {
    let home = TempDir::new().expect("failed to create home tempdir");
    let project = TempDir::new().expect("failed to create project tempdir");

    write_markdown(&project.path().join("AGENTS.md"));

    let report = check_builtin_baseline(BaselineTarget::Project, &roots(&home, &project), false)
        .expect("baseline check should succeed");
    assert_eq!(report.items.len(), 2);
    assert_eq!(report.missing_required, 1);
    assert_eq!(report.missing_optional, 0);
    assert_eq!(
        report.suggested_actions,
        vec!["agent-docs scaffold-baseline --missing-only --target project".to_string()]
    );
    assert_eq!(report.items[0].status, DocumentStatus::Present);
    assert_eq!(report.items[1].status, DocumentStatus::Missing);

    let text =
        render_baseline(OutputFormat::Text, &report).expect("failed to render baseline text");
    assert!(text.contains("[project] startup policy"));
    assert!(text.contains("[project] project-dev"));
    assert!(text.contains(" present "));
    assert!(text.contains(" missing "));
}

#[test]
fn baseline_check_target_all_json_contains_required_fields_and_actions() {
    let home = TempDir::new().expect("failed to create home tempdir");
    let project = TempDir::new().expect("failed to create project tempdir");

    write_markdown(&home.path().join("AGENTS.override.md"));
    write_markdown(&home.path().join("core/policies/cli-tools.md"));
    write_markdown(&project.path().join("AGENTS.md"));

    let report = check_builtin_baseline(BaselineTarget::All, &roots(&home, &project), false)
        .expect("baseline check should succeed");
    assert_eq!(report.items.len(), 5);
    assert_eq!(report.missing_required, 2);
    assert_eq!(report.missing_optional, 0);
    assert_eq!(
        report.suggested_actions,
        vec![
            "agent-docs scaffold-baseline --missing-only --target home".to_string(),
            "agent-docs scaffold-baseline --missing-only --target project".to_string(),
        ]
    );
    assert_eq!(report.items[0].path, home.path().join("AGENTS.override.md"));

    let json =
        render_baseline(OutputFormat::Json, &report).expect("failed to render baseline json");
    let value: Value = serde_json::from_str(&json).expect("failed to parse baseline json");
    assert_eq!(value["target"], "all");
    assert_eq!(value["missing_required"], 2);
    assert_eq!(value["missing_optional"], 0);
    assert_eq!(
        value["suggested_actions"],
        serde_json::json!([
            "agent-docs scaffold-baseline --missing-only --target home",
            "agent-docs scaffold-baseline --missing-only --target project"
        ])
    );
}

#[test]
fn baseline_check_strict_mode_returns_non_zero_when_required_docs_missing() {
    let home = TempDir::new().expect("failed to create home tempdir");
    let project = TempDir::new().expect("failed to create project tempdir");
    write_markdown(&project.path().join("AGENTS.md"));

    let exit_missing = run_with_args([
        "agent-docs",
        "--docs-home",
        home.path().to_str().expect("home path should be utf-8"),
        "--project-path",
        project
            .path()
            .to_str()
            .expect("project path should be utf-8"),
        "baseline",
        "--check",
        "--target",
        "project",
        "--strict",
    ]);
    assert_eq!(exit_missing, 1);

    write_markdown(&project.path().join("DEVELOPMENT.md"));
    let exit_complete = run_with_args([
        "agent-docs",
        "--docs-home",
        home.path().to_str().expect("home path should be utf-8"),
        "--project-path",
        project
            .path()
            .to_str()
            .expect("project path should be utf-8"),
        "baseline",
        "--check",
        "--target",
        "project",
        "--strict",
    ]);
    assert_eq!(exit_complete, 0);
}

#[test]
fn baseline_check_includes_required_extension_docs_as_missing_or_present() {
    let home = TempDir::new().expect("failed to create home tempdir");
    let project = TempDir::new().expect("failed to create project tempdir");
    write_markdown(&project.path().join("AGENTS.md"));
    write_markdown(&project.path().join("DEVELOPMENT.md"));
    write_text(
        &project.path().join(CONFIG_FILE_NAME),
        r#"
[[document]]
context = "project-dev"
scope = "project"
path = "BINARY_DEPENDENCIES.md"
required = true
when = "always"

[[document]]
context = "project-dev"
scope = "project"
path = "OPTIONAL_GUIDE.md"
required = false
when = "always"
"#,
    );

    let missing_report =
        check_builtin_baseline(BaselineTarget::Project, &roots(&home, &project), false)
            .expect("baseline check should succeed");
    assert_eq!(missing_report.items.len(), 3);
    assert_eq!(missing_report.missing_required, 1);
    let extension_missing = missing_report
        .items
        .iter()
        .find(|item| {
            item.path == project.path().join("BINARY_DEPENDENCIES.md")
                && item.source == DocumentSource::ExtensionProject
        })
        .expect("required extension item should be included");
    assert_eq!(extension_missing.status, DocumentStatus::Missing);

    write_markdown(&project.path().join("BINARY_DEPENDENCIES.md"));
    let present_report =
        check_builtin_baseline(BaselineTarget::Project, &roots(&home, &project), false)
            .expect("baseline check should succeed");
    assert_eq!(present_report.items.len(), 3);
    assert_eq!(present_report.missing_required, 0);
    let extension_present = present_report
        .items
        .iter()
        .find(|item| {
            item.path == project.path().join("BINARY_DEPENDENCIES.md")
                && item.source == DocumentSource::ExtensionProject
        })
        .expect("required extension item should be included");
    assert_eq!(extension_present.status, DocumentStatus::Present);
}

#[test]
fn baseline_check_strict_mode_fails_on_missing_required_extension_doc() {
    let home = TempDir::new().expect("failed to create home tempdir");
    let project = TempDir::new().expect("failed to create project tempdir");
    write_markdown(&project.path().join("AGENTS.md"));
    write_markdown(&project.path().join("DEVELOPMENT.md"));
    write_text(
        &project.path().join(CONFIG_FILE_NAME),
        r#"
[[document]]
context = "project-dev"
scope = "project"
path = "BINARY_DEPENDENCIES.md"
required = true
when = "always"
"#,
    );

    let exit_missing_extension = run_with_args([
        "agent-docs",
        "--docs-home",
        home.path().to_str().expect("home path should be utf-8"),
        "--project-path",
        project
            .path()
            .to_str()
            .expect("project path should be utf-8"),
        "baseline",
        "--check",
        "--target",
        "project",
        "--strict",
    ]);
    assert_eq!(exit_missing_extension, 1);

    write_markdown(&project.path().join("BINARY_DEPENDENCIES.md"));
    let exit_complete = run_with_args([
        "agent-docs",
        "--docs-home",
        home.path().to_str().expect("home path should be utf-8"),
        "--project-path",
        project
            .path()
            .to_str()
            .expect("project path should be utf-8"),
        "baseline",
        "--check",
        "--target",
        "project",
        "--strict",
    ]);
    assert_eq!(exit_complete, 0);
}

#[test]
fn baseline_check_returns_config_error_on_malformed_toml() {
    let home = TempDir::new().expect("failed to create home tempdir");
    let project = TempDir::new().expect("failed to create project tempdir");
    write_markdown(&project.path().join("AGENTS.md"));
    write_markdown(&project.path().join("DEVELOPMENT.md"));
    write_text(
        &project.path().join(CONFIG_FILE_NAME),
        r#"
[[document
context = "project-dev"
scope = "project"
path = "BINARY_DEPENDENCIES.md"
required = true
when = "always"
"#,
    );

    let exit_code = run_with_args([
        "agent-docs",
        "--docs-home",
        home.path().to_str().expect("home path should be utf-8"),
        "--project-path",
        project
            .path()
            .to_str()
            .expect("project path should be utf-8"),
        "baseline",
        "--check",
        "--target",
        "project",
    ]);

    assert_eq!(exit_code, 3);
}

#[test]
fn baseline_check_skips_unrelated_home_project_entries_before_project_extensions() {
    let home = TempDir::new().expect("failed to create home tempdir");
    let project = TempDir::new().expect("failed to create project tempdir");
    write_markdown(&project.path().join("AGENTS.md"));
    write_markdown(&project.path().join("DEVELOPMENT.md"));
    write_markdown(&project.path().join("EXTRA_ALPHA.md"));
    write_markdown(&project.path().join("EXTRA_BETA.md"));

    write_text(
        &home.path().join(CONFIG_FILE_NAME),
        r#"
[[document]]
context = "project-dev"
scope = "project"
path = "EXTRA_ALPHA.md"
required = true
when = "always"
notes = "home-alpha"

[[document]]
context = "project-dev"
scope = "project"
path = "EXTRA_BETA.md"
required = true
when = "always"
notes = "home-beta"
"#,
    );
    write_text(
        &project.path().join(CONFIG_FILE_NAME),
        r#"
[[document]]
context = "project-dev"
scope = "project"
path = "EXTRA_ALPHA.md"
required = true
when = "always"
notes = "project-alpha"
"#,
    );

    let report = check_builtin_baseline(BaselineTarget::Project, &roots(&home, &project), false)
        .expect("baseline check should succeed");
    let extension_items: Vec<_> = report
        .items
        .iter()
        .filter(|item| {
            matches!(
                item.source,
                DocumentSource::ExtensionHome | DocumentSource::ExtensionProject
            )
        })
        .collect();

    assert_eq!(extension_items.len(), 1);
    assert_eq!(
        extension_items[0].path,
        project.path().join("EXTRA_ALPHA.md"),
        "project config should supply the project-scope extension for unrelated repos"
    );
    assert_eq!(extension_items[0].source, DocumentSource::ExtensionProject);
    assert!(extension_items[0].why.contains("project-alpha"));
}

#[test]
fn baseline_project_config_opt_out_downgrades_builtin_development_doc() {
    let home = TempDir::new().expect("failed to create home tempdir");
    let project = TempDir::new().expect("failed to create project tempdir");
    write_markdown(&project.path().join("AGENTS.md"));
    // No DEVELOPMENT.md on disk: the project opts out of the builtin requirement.
    write_text(
        &project.path().join(CONFIG_FILE_NAME),
        r#"
[[document]]
context = "project-dev"
scope = "project"
path = "DEVELOPMENT.md"
required = false
notes = "content/archive repo: no DEVELOPMENT.md needed"
"#,
    );

    let report = check_builtin_baseline(BaselineTarget::Project, &roots(&home, &project), false)
        .expect("baseline check should succeed");

    assert_eq!(report.items.len(), 2);
    let project_dev = report
        .items
        .iter()
        .find(|item| item.path.ends_with("DEVELOPMENT.md"))
        .expect("project-dev item should be present");
    assert_eq!(project_dev.source, DocumentSource::BuiltinOptOut);
    assert!(
        !project_dev.required,
        "opted-out builtin must become optional"
    );
    assert_eq!(project_dev.status, DocumentStatus::Missing);
    assert!(
        project_dev.why.contains("opted out"),
        "opt-out why should stay auditable: {}",
        project_dev.why
    );

    assert_eq!(report.missing_required, 0);
    assert!(
        report.suggested_actions.is_empty(),
        "opted-out builtin must not suggest scaffolding: {:?}",
        report.suggested_actions
    );
}

#[test]
fn baseline_strict_passes_when_project_opts_out_of_development_doc() {
    let home = TempDir::new().expect("failed to create home tempdir");
    let project = TempDir::new().expect("failed to create project tempdir");
    write_markdown(&project.path().join("AGENTS.md"));
    write_text(
        &project.path().join(CONFIG_FILE_NAME),
        r#"
[[document]]
context = "project-dev"
scope = "project"
path = "DEVELOPMENT.md"
required = false
"#,
    );

    let exit = run_with_args([
        "agent-docs",
        "--docs-home",
        home.path().to_str().expect("home path should be utf-8"),
        "--project-path",
        project
            .path()
            .to_str()
            .expect("project path should be utf-8"),
        "baseline",
        "--check",
        "--target",
        "project",
        "--strict",
    ]);
    assert_eq!(exit, 0, "strict baseline should pass after opt-out");
}
