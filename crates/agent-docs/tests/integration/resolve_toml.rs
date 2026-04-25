use crate::common;
use agent_docs::config::CONFIG_FILE_NAME;

#[test]
fn resolve_toml_malformed_config_returns_non_zero_with_actionable_stderr() {
    let workspace = common::FixtureWorkspace::from_fixtures();
    common::write_text(
        &workspace.project_path.join(CONFIG_FILE_NAME),
        r#"
[[document
context = "project-dev"
scope = "project"
path = "BINARY_DEPENDENCIES.md"
required = true
when = "always"
"#,
    );

    let output = common::run_agent_docs_command(
        &workspace,
        &[
            "resolve",
            "--context",
            "project-dev",
            "--format",
            "checklist",
        ],
    );

    assert_eq!(
        output.exit_code, 3,
        "malformed TOML should fail with config exit code, stderr:\n{}",
        output.stderr
    );
    assert!(
        output.stdout.is_empty(),
        "resolve should not emit stdout on parse failure, got:\n{}",
        output.stdout
    );
    assert!(
        output.stderr.contains("error:"),
        "stderr should include error prefix, got:\n{}",
        output.stderr
    );
    assert!(
        output.stderr.contains(
            &workspace
                .project_path
                .join(CONFIG_FILE_NAME)
                .display()
                .to_string()
        ),
        "stderr should include config path, got:\n{}",
        output.stderr
    );
    assert!(
        output.stderr.contains("[parse]"),
        "stderr should include parse classification, got:\n{}",
        output.stderr
    );
    assert!(
        output.stderr.contains("invalid TOML in AGENT_DOCS.toml"),
        "stderr should include actionable parse message, got:\n{}",
        output.stderr
    );
}

#[test]
fn resolve_toml_checklist_includes_required_extension_docs_with_status() {
    let workspace = common::FixtureWorkspace::from_fixtures();
    common::write_text(
        &workspace.project_path.join(CONFIG_FILE_NAME),
        r#"
[[document]]
context = "project-dev"
scope = "project"
path = "BINARY_DEPENDENCIES.md"
required = true
when = "always"
notes = "required extension present"

[[document]]
context = "project-dev"
scope = "project"
path = "docs/MISSING_POLICY.md"
required = true
when = "always"
notes = "required extension missing"
"#,
    );

    let output = common::run_agent_docs_command(
        &workspace,
        &[
            "resolve",
            "--context",
            "project-dev",
            "--format",
            "checklist",
        ],
    );
    assert!(
        output.success(),
        "resolve(project-dev checklist) should succeed in non-strict mode, got code={} stderr={}",
        output.exit_code,
        output.stderr
    );

    let lines: Vec<&str> = output.stdout.lines().collect();
    assert!(
        lines
            .first()
            .is_some_and(|line| *line == "REQUIRED_DOCS_BEGIN context=project-dev mode=non-strict"),
        "checklist output should include begin marker:\n{}",
        output.stdout
    );

    let binary_line = lines
        .iter()
        .find(|line| line.starts_with("BINARY_DEPENDENCIES.md status=present path="))
        .expect("checklist output should include required extension doc with present status");
    assert!(
        binary_line.contains("BINARY_DEPENDENCIES.md"),
        "binary extension line should include expected path: {binary_line}"
    );

    let missing_line = lines
        .iter()
        .find(|line| line.starts_with("MISSING_POLICY.md status=missing path="))
        .expect("checklist output should include required missing extension doc");
    assert!(
        missing_line.contains("docs/MISSING_POLICY.md"),
        "missing extension line should include expected path: {missing_line}"
    );

    let end_line = lines
        .last()
        .expect("checklist output should include end marker");
    assert!(
        end_line.contains("required=3"),
        "end marker should report required docs count: {end_line}"
    );
    assert!(
        end_line.contains("present=2"),
        "end marker should report present docs count: {end_line}"
    );
    assert!(
        end_line.contains("missing=1"),
        "end marker should report missing docs count: {end_line}"
    );
    assert!(
        end_line.contains("mode=non-strict"),
        "end marker should report non-strict mode: {end_line}"
    );
}

#[test]
fn resolve_toml_checklist_strict_mode_returns_non_zero_for_missing_required_extension_doc() {
    let workspace = common::FixtureWorkspace::from_fixtures();
    common::write_text(
        &workspace.project_path.join(CONFIG_FILE_NAME),
        r#"
[[document]]
context = "project-dev"
scope = "project"
path = "docs/MISSING_POLICY.md"
required = true
when = "always"
notes = "required extension missing in strict mode"
"#,
    );

    let output = common::run_agent_docs_command(
        &workspace,
        &[
            "resolve",
            "--context",
            "project-dev",
            "--format",
            "checklist",
            "--strict",
        ],
    );
    assert_ne!(
        output.exit_code, 0,
        "strict resolve should fail for missing required docs, stdout:\n{}\nstderr:\n{}",
        output.stdout, output.stderr
    );
    assert_eq!(
        output.exit_code, 1,
        "strict resolve should use missing-required exit code, stderr:\n{}",
        output.stderr
    );
    assert!(
        output
            .stdout
            .contains("MISSING_POLICY.md status=missing path="),
        "strict checklist output should include missing extension status:\n{}",
        output.stdout
    );
    assert!(
        output.stdout.contains("mode=strict"),
        "strict checklist output should include strict mode markers:\n{}",
        output.stdout
    );
}

#[test]
fn resolve_toml_project_dev_includes_binary_dependencies_from_extension() {
    let workspace = common::FixtureWorkspace::from_fixtures();
    common::write_text(
        &workspace.project_path.join(CONFIG_FILE_NAME),
        r#"
[[document]]
context = "project-dev"
scope = "project"
path = "BINARY_DEPENDENCIES.md"
required = true
when = "always"
notes = "project binary dependencies from toml"
"#,
    );

    let output = common::run_agent_docs_command(
        &workspace,
        &["resolve", "--context", "project-dev", "--format", "text"],
    );
    assert!(
        output.success(),
        "resolve(project-dev) should succeed, got code={} stderr={}",
        output.exit_code,
        output.stderr
    );

    let required_lines = common::required_lines(&output.stdout);
    assert_eq!(
        required_lines.len(),
        2,
        "project-dev should include builtin + extension required docs:\n{}",
        output.stdout
    );
    assert!(
        required_lines
            .iter()
            .any(|line| line.contains("DEVELOPMENT.md") && line.contains("source=builtin")),
        "project-dev output should include builtin DEVELOPMENT.md:\n{}",
        output.stdout
    );

    let binary_line = required_lines
        .iter()
        .find(|line| line.contains("BINARY_DEPENDENCIES.md"))
        .expect("project-dev output should include BINARY_DEPENDENCIES.md extension");
    assert!(
        binary_line.contains("source=extension-project"),
        "extension line should be sourced from project TOML: {binary_line}"
    );
    assert!(
        binary_line.contains("status=present"),
        "extension line should report present status: {binary_line}"
    );
    assert!(
        binary_line.contains("project binary dependencies from toml"),
        "extension line should include notes-driven why message: {binary_line}"
    );
}
