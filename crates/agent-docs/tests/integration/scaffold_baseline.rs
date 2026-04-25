use crate::common;
use std::fs;
use std::path::Path;

fn seed_cargo_workspace(root: &Path) {
    common::write_text(&root.join("Cargo.toml"), "[workspace]\nmembers = []\n");
}

fn read_file(path: &Path) -> String {
    fs::read_to_string(path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()))
}

fn read_fixture(relative: &str) -> String {
    read_file(&common::fixture_path(relative))
}

#[test]
fn scaffold_baseline_missing_only_creates_only_missing_files() {
    let workspace = common::FixtureWorkspace::from_fixtures();
    seed_cargo_workspace(&workspace.agent_home);
    seed_cargo_workspace(&workspace.project_path);

    let home_agents = workspace.agent_home.join("AGENTS.md");
    let home_development = workspace.agent_home.join("DEVELOPMENT.md");
    let home_cli_tools = workspace.agent_home.join("CLI_TOOLS.md");
    let project_agents = workspace.project_path.join("AGENTS.md");
    let project_development = workspace.project_path.join("DEVELOPMENT.md");

    let home_agents_before = read_file(&home_agents);
    let home_development_before = read_file(&home_development);
    let project_agents_before = read_file(&project_agents);

    common::remove_file_if_exists(&home_cli_tools);
    common::remove_file_if_exists(&project_development);
    assert!(
        !home_cli_tools.exists(),
        "precondition: home CLI_TOOLS.md missing"
    );
    assert!(
        !project_development.exists(),
        "precondition: project DEVELOPMENT.md missing"
    );

    let output = common::run_agent_docs_command(
        &workspace,
        &["scaffold-baseline", "--target", "all", "--missing-only"],
    );
    assert!(
        output.success(),
        "scaffold-baseline --missing-only should succeed, code={} stderr={}",
        output.exit_code,
        output.stderr
    );
    assert!(
        output
            .stdout
            .contains("summary: created=2 overwritten=0 skipped=3"),
        "expected created/skipped summary, got:\n{}",
        output.stdout
    );

    assert_eq!(read_file(&home_agents), home_agents_before);
    assert_eq!(read_file(&home_development), home_development_before);
    assert_eq!(read_file(&project_agents), project_agents_before);

    let expected_home_cli_tools = read_fixture("home/CLI_TOOLS.template.expected.md");
    let expected_project_development = read_fixture("project/DEVELOPMENT.template.expected.md");
    assert_eq!(read_file(&home_cli_tools), expected_home_cli_tools);
    assert_eq!(
        read_file(&project_development),
        expected_project_development
    );
}

#[test]
fn scaffold_baseline_force_overwrite_updates_existing_baseline_files() {
    let workspace = common::FixtureWorkspace::from_fixtures();
    seed_cargo_workspace(&workspace.agent_home);
    seed_cargo_workspace(&workspace.project_path);

    let home_cli_tools = workspace.agent_home.join("CLI_TOOLS.md");
    let project_development = workspace.project_path.join("DEVELOPMENT.md");
    common::write_text(&home_cli_tools, "# stale home cli tools\n");
    common::write_text(&project_development, "# stale project development\n");

    let output = common::run_agent_docs_command(
        &workspace,
        &["scaffold-baseline", "--target", "all", "--force"],
    );
    assert!(
        output.success(),
        "scaffold-baseline --force should succeed, code={} stderr={}",
        output.exit_code,
        output.stderr
    );
    assert!(
        output
            .stdout
            .contains("summary: created=0 overwritten=5 skipped=0"),
        "expected overwrite summary, got:\n{}",
        output.stdout
    );

    let expected_home_cli_tools = read_fixture("home/CLI_TOOLS.template.expected.md");
    let expected_project_development = read_fixture("project/DEVELOPMENT.template.expected.md");
    let actual_home_cli_tools = read_file(&home_cli_tools);
    let actual_project_development = read_file(&project_development);
    assert_eq!(actual_home_cli_tools, expected_home_cli_tools);
    assert_eq!(actual_project_development, expected_project_development);
    assert!(
        !actual_home_cli_tools.contains("stale"),
        "force mode should overwrite stale CLI_TOOLS.md"
    );
    assert!(
        !actual_project_development.contains("stale"),
        "force mode should overwrite stale DEVELOPMENT.md"
    );
}

#[test]
fn scaffold_baseline_generated_templates_include_required_sections_and_actionable_commands() {
    let workspace = common::FixtureWorkspace::from_fixtures();
    seed_cargo_workspace(&workspace.agent_home);
    seed_cargo_workspace(&workspace.project_path);

    let output = common::run_agent_docs_command(
        &workspace,
        &["scaffold-baseline", "--target", "all", "--force"],
    );
    assert!(
        output.success(),
        "scaffold-baseline should succeed, code={} stderr={}",
        output.exit_code,
        output.stderr
    );

    let development = read_file(&workspace.project_path.join("DEVELOPMENT.md"));
    for required in [
        "# DEVELOPMENT.md",
        "## Setup",
        "## Build",
        "## Test",
        "## Notes",
        "cargo fetch",
        "cargo build --workspace",
        "cargo fmt --all -- --check",
        "cargo clippy --all-targets --all-features -- -D warnings",
        "cargo test --workspace",
    ] {
        assert!(
            development.contains(required),
            "generated DEVELOPMENT.md missing `{required}`:\n{development}"
        );
    }
    assert!(
        !development.contains("{{SETUP_COMMANDS}}")
            && !development.contains("{{BUILD_COMMANDS}}")
            && !development.contains("{{TEST_COMMANDS}}"),
        "generated DEVELOPMENT.md should resolve template placeholders:\n{}",
        development
    );

    let cli_tools = read_file(&workspace.agent_home.join("CLI_TOOLS.md"));
    for required in [
        "# CLI_TOOLS.md",
        "## Tool Selection",
        "## Setup Command",
        "## Build Command",
        "## Test Command",
        "## Maintenance",
        "cargo fetch",
        "cargo build --workspace",
        "cargo fmt --all -- --check",
        "cargo clippy --all-targets --all-features -- -D warnings",
        "cargo test --workspace",
    ] {
        assert!(
            cli_tools.contains(required),
            "generated CLI_TOOLS.md missing `{required}`:\n{cli_tools}"
        );
    }
    assert!(
        !cli_tools.contains("{{SETUP_COMMANDS}}")
            && !cli_tools.contains("{{BUILD_COMMANDS}}")
            && !cli_tools.contains("{{TEST_COMMANDS}}"),
        "generated CLI_TOOLS.md should resolve template placeholders:\n{}",
        cli_tools
    );
}
