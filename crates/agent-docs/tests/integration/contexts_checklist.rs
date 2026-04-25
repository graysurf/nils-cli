use crate::common;
use agent_docs::model::SUPPORTED_CONTEXTS;

#[test]
fn contexts_checklist_emits_begin_end_envelope_with_total() {
    let workspace = common::FixtureWorkspace::from_fixtures();
    let output = common::run_agent_docs_command(&workspace, &["contexts", "--format", "checklist"]);

    assert!(
        output.success(),
        "contexts --format checklist should succeed: code={} stderr=\n{}",
        output.exit_code,
        output.stderr
    );

    let total = SUPPORTED_CONTEXTS.len();
    let lines: Vec<&str> = output.stdout.trim_end().lines().collect();
    assert_eq!(
        lines.first().copied(),
        Some(format!("CONTEXTS_BEGIN total={total}").as_str()),
        "expected leading envelope line; got stdout:\n{}",
        output.stdout
    );
    assert_eq!(
        lines.last().copied(),
        Some(format!("CONTEXTS_END total={total}").as_str()),
        "expected trailing envelope line; got stdout:\n{}",
        output.stdout
    );

    let body: Vec<String> = lines[1..lines.len() - 1]
        .iter()
        .map(|line| line.to_string())
        .collect();
    assert_eq!(body.len(), total, "envelope body should hold every context");

    for (idx, ctx) in SUPPORTED_CONTEXTS.iter().enumerate() {
        assert_eq!(
            body[idx],
            ctx.as_str(),
            "context order should match SUPPORTED_CONTEXTS"
        );
    }
}

#[test]
fn contexts_checklist_text_format_keeps_existing_one_per_line_shape() {
    let workspace = common::FixtureWorkspace::from_fixtures();
    let output = common::run_agent_docs_command(&workspace, &["contexts", "--format", "text"]);
    assert!(output.success(), "contexts --format text should succeed");

    let lines: Vec<&str> = output.stdout.trim_end().lines().collect();
    assert_eq!(lines.len(), SUPPORTED_CONTEXTS.len());
    for (idx, ctx) in SUPPORTED_CONTEXTS.iter().enumerate() {
        assert_eq!(lines[idx], ctx.as_str());
    }
}
