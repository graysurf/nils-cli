use crate::common;
use agent_docs::model::Context;

#[test]
fn resolve_checklist_startup_is_machine_parseable_and_summary_consistent() {
    let workspace = common::FixtureWorkspace::from_fixtures();
    let output = common::run_agent_docs_command(
        &workspace,
        &[
            "resolve",
            "--context",
            Context::Startup.as_str(),
            "--format",
            "checklist",
        ],
    );
    assert!(
        output.success(),
        "resolve startup checklist should succeed: code={} stderr=\n{}",
        output.exit_code,
        output.stderr
    );

    let parsed = common::parse_checklist(&output.stdout);
    assert_eq!(parsed.begin.context, Context::Startup.as_str());
    assert_eq!(parsed.begin.mode, "non-strict");
    assert_eq!(parsed.end.context, Context::Startup.as_str());
    assert_eq!(parsed.end.mode, "non-strict");
    assert_eq!(parsed.docs.len(), parsed.end.required);
    assert_eq!(
        parsed
            .docs
            .iter()
            .filter(|doc| doc.status == "present")
            .count(),
        parsed.end.present
    );
    assert_eq!(
        parsed
            .docs
            .iter()
            .filter(|doc| doc.status == "missing")
            .count(),
        parsed.end.missing
    );
    assert_eq!(
        parsed.end.required,
        parsed.end.present + parsed.end.missing,
        "required count should equal present + missing"
    );
    assert!(
        parsed
            .docs
            .iter()
            .all(|doc| !doc.file_name.is_empty() && !doc.path.is_empty()),
        "all checklist rows should include filename and path"
    );
}

#[test]
fn resolve_checklist_project_dev_is_machine_parseable_and_summary_consistent() {
    let workspace = common::FixtureWorkspace::from_fixtures();
    let output = common::run_agent_docs_command(
        &workspace,
        &[
            "resolve",
            "--context",
            Context::ProjectDev.as_str(),
            "--format",
            "checklist",
        ],
    );
    assert!(
        output.success(),
        "resolve project-dev checklist should succeed: code={} stderr=\n{}",
        output.exit_code,
        output.stderr
    );

    let parsed = common::parse_checklist(&output.stdout);
    assert_eq!(parsed.begin.context, Context::ProjectDev.as_str());
    assert_eq!(parsed.begin.mode, "non-strict");
    assert_eq!(parsed.end.context, Context::ProjectDev.as_str());
    assert_eq!(parsed.end.mode, "non-strict");
    assert_eq!(parsed.docs.len(), parsed.end.required);
    assert_eq!(
        parsed
            .docs
            .iter()
            .filter(|doc| doc.status == "present")
            .count(),
        parsed.end.present
    );
    assert_eq!(
        parsed
            .docs
            .iter()
            .filter(|doc| doc.status == "missing")
            .count(),
        parsed.end.missing
    );
    assert!(
        parsed.docs.iter().all(|doc| doc.path.starts_with('/')),
        "checklist paths should be absolute in fixture runs"
    );
}

#[test]
fn resolve_checklist_output_is_deterministic_across_repeated_runs() {
    let workspace = common::FixtureWorkspace::from_fixtures();
    let first = common::run_agent_docs_command(
        &workspace,
        &[
            "resolve",
            "--context",
            Context::Startup.as_str(),
            "--format",
            "checklist",
        ],
    );
    let second = common::run_agent_docs_command(
        &workspace,
        &[
            "resolve",
            "--context",
            Context::Startup.as_str(),
            "--format",
            "checklist",
        ],
    );

    assert!(first.success(), "first run failed: {}", first.stderr);
    assert!(second.success(), "second run failed: {}", second.stderr);
    assert_eq!(
        first.stdout, second.stdout,
        "checklist output should be deterministic for stable fixture inputs"
    );
}
