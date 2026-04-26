use crate::common;
use std::ffi::OsString;
use std::fs;

use agent_docs::run_with_args;
use tempfile::TempDir;

fn run_scaffold_agents_project(home: &TempDir, project: &TempDir, force: bool) -> i32 {
    let mut args = vec![
        OsString::from("agent-docs"),
        OsString::from("--docs-home"),
        home.path().as_os_str().to_owned(),
        OsString::from("--project-path"),
        project.path().as_os_str().to_owned(),
        OsString::from("scaffold-agents"),
        OsString::from("--target"),
        OsString::from("project"),
    ];
    if force {
        args.push(OsString::from("--force"));
    }
    run_with_args(args)
}

#[test]
fn scaffold_agents_first_time_scaffold_success_creates_agents_md() {
    let home = TempDir::new().expect("create home tempdir");
    let project = TempDir::new().expect("create project tempdir");
    let output = project.path().join("AGENTS.md");
    assert!(!output.exists(), "precondition: AGENTS.md should not exist");

    let exit_code = run_scaffold_agents_project(&home, &project, false);

    assert_eq!(exit_code, 0);
    assert!(output.exists(), "AGENTS.md should be scaffolded");
    let rendered = fs::read_to_string(output).expect("read scaffolded AGENTS.md");
    assert!(
        rendered.starts_with("# AGENTS.md"),
        "scaffolded AGENTS.md should have template header"
    );
}

#[test]
fn scaffold_agents_non_force_does_not_overwrite_existing_agents_md() {
    let home = TempDir::new().expect("create home tempdir");
    let project = TempDir::new().expect("create project tempdir");
    let output = project.path().join("AGENTS.md");
    let original = "# keep-me\n";
    fs::write(&output, original).expect("seed AGENTS.md");

    let exit_code = run_scaffold_agents_project(&home, &project, false);

    assert_eq!(exit_code, 1, "existing file should fail without --force");
    let persisted = fs::read_to_string(output).expect("read persisted AGENTS.md");
    assert_eq!(persisted, original);
}

#[test]
fn scaffold_agents_force_overwrites_and_matches_expected_fixture_template() {
    let home = TempDir::new().expect("create home tempdir");
    let project = TempDir::new().expect("create project tempdir");
    let output = project.path().join("AGENTS.md");
    fs::write(&output, "# stale template\n").expect("seed stale AGENTS.md");

    let exit_code = run_scaffold_agents_project(&home, &project, true);

    assert_eq!(exit_code, 0);
    let expected = fs::read_to_string(common::fixture_path("project/AGENTS.template.expected.md"))
        .expect("read expected AGENTS template fixture");
    let actual = fs::read_to_string(output).expect("read overwritten AGENTS.md");
    assert_eq!(actual, expected);
}

#[test]
fn scaffold_agents_template_includes_required_guidance_strings() {
    let home = TempDir::new().expect("create home tempdir");
    let project = TempDir::new().expect("create project tempdir");
    let output = project.path().join("AGENTS.md");

    let exit_code = run_scaffold_agents_project(&home, &project, false);
    assert_eq!(exit_code, 0);

    let rendered = fs::read_to_string(output).expect("read scaffolded AGENTS.md");
    assert!(rendered.contains("agent-docs resolve --context startup"));
    assert!(rendered.contains("agent-docs resolve --context project-dev"));
    assert!(rendered.contains("AGENT_DOCS.toml"));
}
