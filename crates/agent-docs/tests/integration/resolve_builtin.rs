use crate::common;
use std::fs;

use agent_docs::config::CONFIG_FILE_NAME;
use agent_docs::model::{Context, DocumentSource, DocumentStatus, OutputFormat, ResolveFormat};
use agent_docs::{output, resolver};
use serde::Deserialize;

#[derive(Debug, Clone, Copy)]
struct ExpectedDocument {
    scope: &'static str,
    file_name: &'static str,
    source: &'static str,
    status: &'static str,
}

#[derive(Debug, Deserialize)]
struct ResolveReportJson {
    context: String,
    strict: bool,
    documents: Vec<ResolvedDocumentJson>,
    summary: ResolveSummaryJson,
}

#[derive(Debug, Deserialize)]
struct ResolvedDocumentJson {
    context: String,
    scope: String,
    path: String,
    required: bool,
    status: String,
    source: String,
    why: String,
}

#[derive(Debug, Deserialize)]
struct ResolveSummaryJson {
    required_total: usize,
    present_required: usize,
    missing_required: usize,
}

#[test]
fn resolve_builtin_all_contexts_text_output_is_covered() {
    let workspace = common::FixtureWorkspace::from_fixtures();
    let roots = workspace.roots();

    for context in all_contexts() {
        let report = resolver::resolve_builtin(context, &roots, false);
        let rendered = output::render_resolve(ResolveFormat::Text, &report).expect("render text");
        let required_lines = common::required_lines(&rendered);
        let expected = expected_documents(context);

        assert!(rendered.contains(&format!("CONTEXT: {context}")));
        assert!(
            rendered.contains("strict=false"),
            "text output should include strict marker"
        );
        assert_eq!(
            required_lines.len(),
            expected.len(),
            "unexpected required line count for context {context}\n{rendered}"
        );

        for (line, expectation) in required_lines.iter().zip(expected.iter()) {
            assert!(
                line.contains(&format!("{context} {}", expectation.scope)),
                "line should include context/scope: {line}"
            );
            assert!(
                line.contains(expectation.file_name),
                "line should include file name {}: {line}",
                expectation.file_name
            );
            assert!(
                line.contains(&format!("source={}", expectation.source)),
                "line should include source {}: {line}",
                expectation.source
            );
            assert!(
                line.contains(&format!("status={}", expectation.status)),
                "line should include status {}: {line}",
                expectation.status
            );
        }
    }
}

#[test]
fn resolve_builtin_all_contexts_json_output_is_covered() {
    let workspace = common::FixtureWorkspace::from_fixtures();
    let roots = workspace.roots();

    for context in all_contexts() {
        let report = resolver::resolve_builtin(context, &roots, false);
        let rendered = output::render_resolve(ResolveFormat::Json, &report).expect("render json");
        let decoded: ResolveReportJson = serde_json::from_str(&rendered).expect("parse json");
        let expected = expected_documents(context);

        assert_eq!(decoded.context, context.as_str());
        assert!(!decoded.strict);
        assert_eq!(decoded.documents.len(), expected.len());
        assert_eq!(decoded.summary.required_total, expected.len());
        assert_eq!(decoded.summary.present_required, expected.len());
        assert_eq!(decoded.summary.missing_required, 0);

        for (doc, expectation) in decoded.documents.iter().zip(expected.iter()) {
            assert_eq!(doc.context, context.as_str());
            assert_eq!(doc.scope, expectation.scope);
            assert!(doc.path.ends_with(expectation.file_name));
            assert!(doc.required);
            assert_eq!(doc.status, expectation.status);
            assert_eq!(doc.source, expectation.source);
            assert!(!doc.why.is_empty());
        }
    }
}

#[test]
fn resolve_builtin_startup_text_output_is_stable_ordered_and_deduped() {
    let workspace = common::FixtureWorkspace::from_fixtures();
    let roots = workspace.roots();

    let first = output::render_resolve(
        ResolveFormat::Text,
        &resolver::resolve_builtin(Context::Startup, &roots, false),
    )
    .expect("first render");
    let second = output::render_resolve(
        ResolveFormat::Text,
        &resolver::resolve_builtin(Context::Startup, &roots, false),
    )
    .expect("second render");
    let lines = common::required_lines(&first);

    assert_eq!(first, second, "startup rendering should be deterministic");
    assert_eq!(lines.len(), 2, "startup should include exactly two docs");
    assert!(
        lines[0].contains("startup home"),
        "startup home doc should appear first"
    );
    assert!(
        lines[1].contains("startup project"),
        "startup project doc should appear second"
    );
    for line in lines {
        assert!(
            line.contains("AGENTS.override.md"),
            "when override exists, startup should not emit AGENTS.md fallback: {line}"
        );
        assert!(line.contains("source=builtin"));
    }
}

#[test]
fn resolve_builtin_strict_and_non_strict_have_different_exit_codes_for_missing_required_docs() {
    let workspace = common::FixtureWorkspace::from_fixtures();
    common::remove_file_if_exists(&workspace.docs_home.join("DEVELOPMENT.md"));

    let non_strict =
        common::run_resolve_exit_code(&workspace, Context::SkillDev, OutputFormat::Text, false);
    let strict =
        common::run_resolve_exit_code(&workspace, Context::SkillDev, OutputFormat::Text, true);

    assert_eq!(non_strict, 0, "non-strict should report but not fail");
    assert_eq!(
        strict, 1,
        "strict should fail when required docs are missing"
    );
}

#[test]
fn resolve_builtin_all_contexts_checklist_output_is_covered() {
    let workspace = common::FixtureWorkspace::from_fixtures();
    let roots = workspace.roots();

    for context in all_contexts() {
        let report = resolver::resolve_builtin(context, &roots, false);
        let rendered =
            output::render_resolve(ResolveFormat::Checklist, &report).expect("render checklist");
        let parsed = common::parse_checklist(&rendered);
        let expected = expected_documents(context);

        assert_eq!(parsed.begin.context, context.as_str());
        assert_eq!(parsed.begin.mode, "non-strict");

        assert_eq!(
            parsed.docs.len(),
            expected.len(),
            "unexpected checklist required line count for context {context}\n{rendered}"
        );
        for (doc, expectation) in parsed.docs.iter().zip(expected.iter()) {
            assert_eq!(
                doc.file_name, expectation.file_name,
                "doc should keep expected file name for context {context}: {doc:?}"
            );
            assert_eq!(
                doc.status, expectation.status,
                "doc should keep expected status for context {context}: {doc:?}"
            );
            assert!(
                doc.path.ends_with(expectation.file_name),
                "doc path should match file name {}: {}",
                expectation.file_name,
                doc.path
            );
        }

        assert_eq!(parsed.end.required, report.summary.required_total);
        assert_eq!(parsed.end.present, report.summary.present_required);
        assert_eq!(parsed.end.missing, report.summary.missing_required);
        assert_eq!(parsed.end.mode, "non-strict");
        assert_eq!(parsed.end.context, context.as_str());
    }
}

#[test]
fn resolve_builtin_startup_checklist_output_is_stable_ordered_and_deduped() {
    let workspace = common::FixtureWorkspace::from_fixtures();
    let roots = workspace.roots();

    let first = output::render_resolve(
        ResolveFormat::Checklist,
        &resolver::resolve_builtin(Context::Startup, &roots, false),
    )
    .expect("first checklist render");
    let second = output::render_resolve(
        ResolveFormat::Checklist,
        &resolver::resolve_builtin(Context::Startup, &roots, false),
    )
    .expect("second checklist render");

    assert_eq!(first, second, "checklist rendering should be deterministic");
    let parsed = common::parse_checklist(&first);
    let home_root = workspace.docs_home.display().to_string();
    let project_root = workspace.project_path.display().to_string();
    assert_eq!(parsed.begin.context, Context::Startup.as_str());
    assert_eq!(parsed.begin.mode, "non-strict");
    assert_eq!(parsed.docs.len(), 2);
    assert_eq!(parsed.docs[0].file_name, "AGENTS.override.md");
    assert_eq!(parsed.docs[0].status, "present");
    assert!(
        parsed.docs[0].path.starts_with(&home_root),
        "startup home override should appear first"
    );
    assert_eq!(parsed.docs[1].file_name, "AGENTS.override.md");
    assert_eq!(parsed.docs[1].status, "present");
    assert!(
        parsed.docs[1].path.starts_with(&project_root),
        "startup project override should appear second"
    );
    assert_eq!(parsed.end.required, 2);
    assert_eq!(parsed.end.present, 2);
    assert_eq!(parsed.end.missing, 0);
    assert_eq!(parsed.end.mode, "non-strict");
    assert_eq!(parsed.end.context, Context::Startup.as_str());
}

#[test]
fn resolve_builtin_startup_prefers_agents_override_and_falls_back_per_scope() {
    let workspace = common::FixtureWorkspace::from_fixtures();
    common::remove_file_if_exists(&workspace.project_path.join("AGENTS.override.md"));
    let roots = workspace.roots();

    let report = resolver::resolve_builtin(Context::Startup, &roots, false);
    assert_eq!(report.documents.len(), 2);

    let home_doc = &report.documents[0];
    assert_eq!(home_doc.scope.as_str(), "home");
    assert!(home_doc.path.ends_with("AGENTS.override.md"));
    assert_eq!(home_doc.source, DocumentSource::Builtin);
    assert_eq!(home_doc.status, DocumentStatus::Present);

    let project_doc = &report.documents[1];
    assert_eq!(project_doc.scope.as_str(), "project");
    assert!(project_doc.path.ends_with("AGENTS.md"));
    assert_eq!(project_doc.source, DocumentSource::BuiltinFallback);
    assert_eq!(project_doc.status, DocumentStatus::Present);
}

#[test]
fn resolve_builtin_project_dev_merges_extensions_with_stable_precedence_and_order() {
    let workspace = common::FixtureWorkspace::from_fixtures();
    fs::create_dir_all(workspace.project_path.join("docs")).expect("create docs directory");
    fs::write(
        workspace.project_path.join("docs/EXTRA_POLICY.md"),
        "# extra policy\n",
    )
    .expect("write extra policy");

    fs::write(
        workspace.docs_home.join(CONFIG_FILE_NAME),
        r#"
[[document]]
context = "project-dev"
scope = "project"
path = "DEVELOPMENT.md"
required = false
notes = "home-duplicate-builtin"

[[document]]
context = "project-dev"
scope = "project"
path = "./BINARY_DEPENDENCIES.md"
required = false
notes = "home-binary-first"

[[document]]
context = "project-dev"
scope = "project"
path = "BINARY_DEPENDENCIES.md"
required = true
notes = "home-binary-second"

[[document]]
context = "project-dev"
scope = "project"
path = "docs/EXTRA_POLICY.md"
required = false
notes = "home-extra"

[[document]]
context = "task-tools"
scope = "home"
path = "CLI_TOOLS.md"
required = true
notes = "other-context-ignored"
"#,
    )
    .expect("write home config");

    fs::write(
        workspace.project_path.join(CONFIG_FILE_NAME),
        r#"
[[document]]
context = "project-dev"
scope = "project"
path = "docs/EXTRA_POLICY.md"
required = true
notes = "project-extra-overrides-home"

[[document]]
context = "project-dev"
scope = "home"
path = "CLI_TOOLS.md"
required = false
notes = "project-home-scope-entry"
"#,
    )
    .expect("write project config");

    let roots = workspace.roots();
    let first = resolver::resolve_builtin(Context::ProjectDev, &roots, false);
    let second = resolver::resolve_builtin(Context::ProjectDev, &roots, false);

    let first_text = output::render_resolve(ResolveFormat::Text, &first).expect("render first");
    let second_text = output::render_resolve(ResolveFormat::Text, &second).expect("render second");
    assert_eq!(
        first_text, second_text,
        "resolve output should be deterministic"
    );

    let documents = &first.documents;
    assert_eq!(documents.len(), 4);
    assert_eq!(
        documents
            .iter()
            .filter(|doc| doc.path.ends_with("DEVELOPMENT.md"))
            .count(),
        1,
        "built-in docs must remain immutable and de-duplicated"
    );

    let builtin = &documents[0];
    assert!(builtin.path.ends_with("DEVELOPMENT.md"));
    assert_eq!(builtin.source, DocumentSource::Builtin);
    assert!(builtin.required);

    let binary = &documents[1];
    assert!(binary.path.ends_with("BINARY_DEPENDENCIES.md"));
    assert_eq!(binary.source, DocumentSource::ExtensionHome);
    assert!(binary.required);
    assert_eq!(binary.status, DocumentStatus::Present);
    assert!(
        binary.why.contains("home-binary-second"),
        "later entries in one config should win"
    );

    let extra = &documents[2];
    assert!(extra.path.ends_with("docs/EXTRA_POLICY.md"));
    assert_eq!(extra.source, DocumentSource::ExtensionProject);
    assert!(extra.required);
    assert_eq!(extra.status, DocumentStatus::Present);
    assert!(
        extra.why.contains("project-extra-overrides-home"),
        "project config should override home config duplicates"
    );

    let home_scoped = &documents[3];
    assert!(home_scoped.path.ends_with("CLI_TOOLS.md"));
    assert_eq!(home_scoped.source, DocumentSource::ExtensionProject);
    assert!(!home_scoped.required);
    assert_eq!(home_scoped.status, DocumentStatus::Present);

    assert_eq!(first.summary.required_total, 3);
    assert_eq!(first.summary.present_required, 3);
    assert_eq!(first.summary.missing_required, 0);
}

fn all_contexts() -> [Context; 4] {
    [
        Context::Startup,
        Context::SkillDev,
        Context::TaskTools,
        Context::ProjectDev,
    ]
}

fn expected_documents(context: Context) -> &'static [ExpectedDocument] {
    match context {
        Context::Startup => &[
            ExpectedDocument {
                scope: "home",
                file_name: "AGENTS.override.md",
                source: "builtin",
                status: "present",
            },
            ExpectedDocument {
                scope: "project",
                file_name: "AGENTS.override.md",
                source: "builtin",
                status: "present",
            },
        ],
        Context::SkillDev => &[ExpectedDocument {
            scope: "home",
            file_name: "DEVELOPMENT.md",
            source: "builtin",
            status: "present",
        }],
        Context::TaskTools => &[ExpectedDocument {
            scope: "home",
            file_name: "CLI_TOOLS.md",
            source: "builtin",
            status: "present",
        }],
        Context::ProjectDev => &[ExpectedDocument {
            scope: "project",
            file_name: "DEVELOPMENT.md",
            source: "builtin",
            status: "present",
        }],
    }
}
