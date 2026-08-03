use anyhow::{Context, Result};
use nils_common::cli_contract::{Envelope, EnvelopeError, schema_version_for};
use serde::Serialize;
use serde_json::json;

use crate::model::{
    AuditReport, InitMode, InitReport, ListReport, OutputFormat, PreflightReport, RemoveReport,
    ResolvedDocument, ValidationContract,
};

pub fn render_audit(format: OutputFormat, report: &AuditReport) -> Result<String> {
    match format {
        OutputFormat::Json => {
            serde_json::to_string_pretty(report).context("failed to serialize audit output")
        }
        OutputFormat::Text => Ok(render_audit_text(report)),
    }
}

pub fn render_preflight(format: OutputFormat, report: &PreflightReport) -> Result<String> {
    match format {
        OutputFormat::Json => {
            serde_json::to_string_pretty(report).context("failed to serialize preflight output")
        }
        OutputFormat::Text => Ok(render_preflight_text(report)),
    }
}

pub fn render_undeclared_intent_error(
    format: OutputFormat,
    intent: &str,
    available_intents: &[String],
) -> Result<String> {
    match format {
        OutputFormat::Json => {
            let error = EnvelopeError::new(
                "undeclared-intent",
                format!("intent `{intent}` is not declared for this project"),
            )
            .with_details(json!({
                "intent": intent,
                "available_intents": available_intents,
            }));
            let envelope: Envelope<()> =
                Envelope::failure(schema_version_for("agent-docs", "preflight", 2), error);
            serde_json::to_string_pretty(&envelope)
                .context("failed to serialize undeclared-intent error")
        }
        OutputFormat::Text => {
            let available = if available_intents.is_empty() {
                "none".to_string()
            } else {
                available_intents.join(", ")
            };
            Ok(format!(
                "error: undeclared intent `{intent}`; available intents: {available}"
            ))
        }
    }
}

pub fn render_list(format: OutputFormat, report: &ListReport) -> Result<String> {
    match format {
        OutputFormat::Json => {
            serde_json::to_string_pretty(report).context("failed to serialize list output")
        }
        OutputFormat::Text => Ok(render_list_text(report)),
    }
}

pub fn render_remove(format: OutputFormat, report: &RemoveReport) -> Result<String> {
    match format {
        OutputFormat::Json => {
            serde_json::to_string_pretty(report).context("failed to serialize remove output")
        }
        OutputFormat::Text => Ok(format!(
            "remove: outcome={} context={} scope={} path={} config={} remaining_documents={}",
            report.outcome,
            report.context,
            report.scope,
            report.path.display(),
            report.config_path.display(),
            report.remaining_documents
        )),
    }
}

pub fn render_init(format: OutputFormat, report: &InitReport) -> Result<String> {
    if report.mode == InitMode::Print {
        // Print mode emits the stub verbatim regardless of --format so it can be
        // redirected straight into AGENT_DOCS.toml.
        return Ok(report.stub.clone());
    }
    match format {
        OutputFormat::Json => {
            serde_json::to_string_pretty(report).context("failed to serialize init output")
        }
        OutputFormat::Text => Ok(format!(
            "init: mode={} target={} wrote={}",
            report.mode,
            report.target_path.display(),
            report.wrote
        )),
    }
}

#[derive(Debug, Serialize)]
pub struct ExplainIntent<'a> {
    pub intent: &'a str,
    pub documents: &'a [ResolvedDocument],
    pub validation: &'a ValidationContract,
}

#[derive(Debug, Serialize)]
pub struct ExplainIntents<'a> {
    pub intents: &'a [String],
}

pub fn render_explain_intent(format: OutputFormat, payload: &ExplainIntent<'_>) -> Result<String> {
    match format {
        OutputFormat::Json => {
            serde_json::to_string_pretty(payload).context("failed to serialize explain output")
        }
        OutputFormat::Text => {
            let mut lines = vec![format!("INTENT: {}", payload.intent)];
            if payload.documents.is_empty() {
                lines.push("  (no documents declared for this intent)".to_string());
            }
            for doc in payload.documents {
                lines.push(format!(
                    "  - {} (scope={}, {}) status={} valid={} why=\"{}\"",
                    doc.path.display(),
                    doc.scope,
                    required_label(doc),
                    doc.status,
                    doc.validation.valid,
                    doc.why
                ));
            }
            lines.push(render_contract_line(payload.validation));
            Ok(lines.join("\n"))
        }
    }
}

pub fn render_explain_intents(
    format: OutputFormat,
    payload: &ExplainIntents<'_>,
) -> Result<String> {
    match format {
        OutputFormat::Json => serde_json::to_string_pretty(payload)
            .context("failed to serialize explain intents output"),
        OutputFormat::Text => {
            if payload.intents.is_empty() {
                return Ok("no intents declared in the catalog".to_string());
            }
            let mut lines = vec!["INTENTS:".to_string()];
            lines.extend(payload.intents.iter().map(|intent| format!("  - {intent}")));
            lines.push("run `agent-docs explain --intent <intent>` for details".to_string());
            Ok(lines.join("\n"))
        }
    }
}

fn render_audit_text(report: &AuditReport) -> String {
    let mut lines = Vec::new();
    lines.push(format!("AUDIT: {}", report.target));
    if let Some(product) = report.product {
        lines.push(format!("product: {product}"));
    }
    lines.push(format!("docs_home: {}", report.docs_home.display()));
    lines.push(format!("project_path: {}", report.project_path.display()));
    lines.push(String::new());

    lines.push("wiring:".to_string());
    if report.wiring.is_empty() {
        lines.push("  - (none for this target)".to_string());
    }
    for check in &report.wiring {
        lines.push(format!(
            "  [{}] {}: {}",
            if check.ok { "ok" } else { "FAIL" },
            check.name,
            check.detail
        ));
    }

    // The skills section is omitted entirely when the project did not opt in,
    // so audits in non-participating repos keep their previous output.
    if !report.skills.is_empty() {
        lines.push(String::new());
        lines.push("skills:".to_string());
        for check in &report.skills {
            lines.push(format!(
                "  [{}] {}: {}",
                if check.ok { "ok" } else { "FAIL" },
                check.name,
                check.detail
            ));
        }
    }

    lines.push(String::new());
    lines.push("documents:".to_string());
    if report.documents.is_empty() {
        lines.push("  - (no documents declared)".to_string());
    }
    for doc in &report.documents {
        lines.push(format!(
            "  [{}] {} (context={}, scope={}) status={} valid={} when=\"{}\"",
            required_label(doc),
            doc.path.display(),
            doc.context,
            doc.scope,
            doc.status,
            doc.validation.valid,
            doc.when
        ));
    }

    lines.push(String::new());
    lines.push(format!("problems: {}", report.problems));
    lines.push("suggested_actions:".to_string());
    if report.suggested_actions.is_empty() {
        lines.push("  - (none)".to_string());
    } else {
        lines.extend(
            report
                .suggested_actions
                .iter()
                .map(|action| format!("  - {action}")),
        );
    }

    lines.join("\n")
}

fn render_preflight_text(report: &PreflightReport) -> String {
    let mut lines = Vec::new();
    lines.push(format!("PREFLIGHT: intent={}", report.intent));
    lines.push(format!("docs_home: {}", report.docs_home.display()));
    lines.push(format!("project_path: {}", report.project_path.display()));
    lines.push(String::new());

    lines.push("documents:".to_string());
    if report.documents.is_empty() {
        lines.push("  - (no documents resolved for this intent)".to_string());
    }
    for doc in &report.documents {
        lines.push(format!(
            "  [{}] {} status={} valid={} when=\"{}\"",
            required_label(doc),
            doc.path.display(),
            doc.status,
            doc.validation.valid,
            doc.when
        ));
    }

    lines.push(String::new());
    lines.push(render_contract_line(&report.validation));

    lines.push(String::new());
    lines.push(format!(
        "summary: required_total={} satisfied_required={} missing_required={} invalid_required={} strict={}",
        report.summary.required_total,
        report.summary.satisfied_required,
        report.summary.missing_required,
        report.summary.invalid_required,
        report.strict
    ));

    lines.join("\n")
}

fn render_list_text(report: &ListReport) -> String {
    let mut lines = Vec::new();
    lines.push(format!("docs_home: {}", report.docs_home.display()));
    lines.push(format!("project_path: {}", report.project_path.display()));
    lines.push(String::new());

    lines.push(format!("intents: {}", report.intents.join(", ")));
    lines.push(String::new());

    lines.push("documents:".to_string());
    if report.documents.is_empty() {
        lines.push("  - (none)".to_string());
    }
    for doc in &report.documents {
        lines.push(format!(
            "  [{}] context={} scope={} {} status={} source={}",
            required_label(doc),
            doc.context,
            doc.scope,
            doc.path.display(),
            doc.status,
            doc.source
        ));
    }

    lines.push(String::new());
    lines.push("validation contracts:".to_string());
    if report.validations.is_empty() {
        lines.push("  - (none)".to_string());
    }
    for contract in &report.validations {
        lines.push(format!(
            "  context={} commands={:?}",
            contract.context, contract.commands
        ));
    }

    lines.join("\n")
}

fn render_contract_line(contract: &ValidationContract) -> String {
    if !contract.declared {
        return format!(
            "validation contract: none declared for intent {}",
            contract.context
        );
    }
    let mut line = format!(
        "validation contract ({}): commands={:?}",
        contract.context, contract.commands
    );
    if let Some(marker) = &contract.marker {
        line.push_str(&format!(" marker={marker}"));
    }
    if let Some(description) = &contract.description {
        line.push_str(&format!(" description=\"{description}\""));
    }
    line
}

fn required_label(doc: &ResolvedDocument) -> &'static str {
    if doc.required { "required" } else { "optional" }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::model::{
        Context as IntentContext, DocumentSource, DocumentStatus, DocumentValidation,
        FreshnessCheck, RemoveOutcome, ResolveSummary, Scope,
    };
    use pretty_assertions::assert_eq;
    use std::path::PathBuf;

    fn intent(name: &str) -> IntentContext {
        IntentContext::parse(name).expect("intent")
    }

    fn document(path: &str, required: bool, present: bool) -> ResolvedDocument {
        ResolvedDocument {
            context: intent("project-dev"),
            scope: Scope::Project,
            path: PathBuf::from(path),
            products: Vec::new(),
            phases: Vec::new(),
            declared_required: required,
            required,
            when: "always".to_string(),
            when_satisfied: true,
            status: if present {
                DocumentStatus::Present
            } else {
                DocumentStatus::Missing
            },
            validation: if present {
                DocumentValidation {
                    exists: true,
                    non_empty: true,
                    marker_present: None,
                    freshness: FreshnessCheck::NotDeclared,
                    valid: true,
                }
            } else {
                DocumentValidation::missing()
            },
            source: DocumentSource::Project,
            why: "declared in the project catalog".to_string(),
            content: None,
        }
    }

    fn contract(declared: bool) -> ValidationContract {
        ValidationContract {
            context: intent("project-dev"),
            declared,
            commands: if declared {
                vec!["cargo test".to_string()]
            } else {
                Vec::new()
            },
            marker: declared.then(|| "test-marker".to_string()),
            description: declared.then(|| "run the suite".to_string()),
        }
    }

    fn preflight(documents: Vec<ResolvedDocument>) -> PreflightReport {
        PreflightReport {
            schema_version: PreflightReport::SCHEMA_VERSION,
            intent: intent("project-dev"),
            product: None,
            phase: None,
            strict: false,
            docs_home: PathBuf::from("/home/docs"),
            project_path: PathBuf::from("/repo"),
            is_linked_worktree: false,
            summary: ResolveSummary::from_documents(&documents),
            documents,
            validation: contract(true),
        }
    }

    #[test]
    fn json_rendering_is_pretty_printed_and_parses_back() {
        let report = preflight(vec![document("DEVELOPMENT.md", true, true)]);

        let json = render_preflight(OutputFormat::Json, &report).expect("json");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        assert_eq!(
            parsed["schema_version"].as_str(),
            Some(PreflightReport::SCHEMA_VERSION)
        );
        assert!(json.contains('\n'), "json output must be pretty-printed");
    }

    #[test]
    fn preflight_text_lists_documents_and_a_summary_line() {
        let report = preflight(vec![
            document("DEVELOPMENT.md", true, true),
            document("OPTIONAL.md", false, false),
        ]);

        let text = render_preflight(OutputFormat::Text, &report).expect("text");

        assert!(text.contains("DEVELOPMENT.md"), "{text}");
        assert!(text.contains("OPTIONAL.md"), "{text}");
        assert!(
            text.contains(
                "summary: required_total=1 satisfied_required=1 missing_required=0 invalid_required=0 strict=false"
            ),
            "{text}"
        );
        assert!(text.contains("validation contract (project-dev)"), "{text}");
    }

    #[test]
    fn a_list_report_renders_every_section_even_when_empty() {
        let empty = ListReport {
            docs_home: PathBuf::from("/home/docs"),
            project_path: PathBuf::from("/repo"),
            intents: Vec::new(),
            documents: Vec::new(),
            validations: Vec::new(),
        };

        let text = render_list(OutputFormat::Text, &empty).expect("text");
        assert!(text.contains("docs_home: /home/docs"), "{text}");
        assert!(text.contains("project_path: /repo"), "{text}");
        assert!(text.contains("intents: \n"), "{text}");
        assert!(text.contains("documents:\n  - (none)"), "{text}");
        assert!(text.contains("validation contracts:\n  - (none)"), "{text}");

        let populated = ListReport {
            intents: vec!["project-dev".to_string(), "task-tools".to_string()],
            documents: vec![document("DEVELOPMENT.md", true, true)],
            validations: vec![contract(true)],
            ..empty
        };
        let text = render_list(OutputFormat::Text, &populated).expect("text");
        assert!(text.contains("intents: project-dev, task-tools"), "{text}");
        assert!(
            text.contains(
                "  [required] context=project-dev scope=project DEVELOPMENT.md status=present source=project"
            ),
            "{text}"
        );
        assert!(
            text.contains("  context=project-dev commands=[\"cargo test\"]"),
            "{text}"
        );

        let json = render_list(OutputFormat::Json, &populated).expect("json");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        assert_eq!(parsed["intents"].as_array().map(Vec::len), Some(2));
    }

    #[test]
    fn an_undeclared_intent_names_the_intents_that_do_exist() {
        let available = vec!["project-dev".to_string(), "task-tools".to_string()];

        let text =
            render_undeclared_intent_error(OutputFormat::Text, "nope", &available).expect("text");
        assert_eq!(
            text,
            "error: undeclared intent `nope`; available intents: project-dev, task-tools"
        );

        // With nothing declared the operator still gets an actionable message.
        let text = render_undeclared_intent_error(OutputFormat::Text, "nope", &[]).expect("text");
        assert_eq!(
            text,
            "error: undeclared intent `nope`; available intents: none"
        );

        let json =
            render_undeclared_intent_error(OutputFormat::Json, "nope", &available).expect("json");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        assert_eq!(parsed["ok"].as_bool(), Some(false));
        assert_eq!(parsed["error"]["code"].as_str(), Some("undeclared-intent"));
        assert_eq!(
            parsed["error"]["details"]["available_intents"]
                .as_array()
                .map(Vec::len),
            Some(2)
        );
    }

    #[test]
    fn explain_renders_a_single_intent_and_the_intent_index() {
        let documents = vec![document("DEVELOPMENT.md", true, true)];
        let validation = contract(true);
        let payload = ExplainIntent {
            intent: "project-dev",
            documents: &documents,
            validation: &validation,
        };

        let text = render_explain_intent(OutputFormat::Text, &payload).expect("text");
        assert!(text.starts_with("INTENT: project-dev"), "{text}");
        assert!(text.contains("marker=test-marker"), "{text}");
        assert!(text.contains("description=\"run the suite\""), "{text}");

        let empty_validation = contract(false);
        let empty = ExplainIntent {
            intent: "project-dev",
            documents: &[],
            validation: &empty_validation,
        };
        let text = render_explain_intent(OutputFormat::Text, &empty).expect("text");
        assert!(
            text.contains("(no documents declared for this intent)"),
            "{text}"
        );
        assert!(
            text.contains("validation contract: none declared for intent project-dev"),
            "{text}"
        );

        let json = render_explain_intent(OutputFormat::Json, &payload).expect("json");
        assert!(serde_json::from_str::<serde_json::Value>(&json).is_ok());

        let intents = vec!["project-dev".to_string()];
        let text =
            render_explain_intents(OutputFormat::Text, &ExplainIntents { intents: &intents })
                .expect("text");
        assert!(text.starts_with("INTENTS:\n  - project-dev"), "{text}");
        let text = render_explain_intents(OutputFormat::Text, &ExplainIntents { intents: &[] })
            .expect("text");
        assert_eq!(text, "no intents declared in the catalog");
        let json =
            render_explain_intents(OutputFormat::Json, &ExplainIntents { intents: &intents })
                .expect("json");
        assert!(serde_json::from_str::<serde_json::Value>(&json).is_ok());
    }

    #[test]
    fn remove_reports_its_outcome_on_one_line() {
        let report = RemoveReport {
            config_path: PathBuf::from("/repo/AGENT_DOCS.toml"),
            outcome: RemoveOutcome::Removed,
            context: "project-dev".to_string(),
            scope: Scope::Project,
            path: PathBuf::from("DEVELOPMENT.md"),
            remaining_documents: 2,
        };

        let text = render_remove(OutputFormat::Text, &report).expect("text");
        assert_eq!(
            text,
            "remove: outcome=removed context=project-dev scope=project path=DEVELOPMENT.md config=/repo/AGENT_DOCS.toml remaining_documents=2"
        );

        let json = render_remove(OutputFormat::Json, &report).expect("json");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        assert_eq!(parsed["outcome"].as_str(), Some("removed"));
    }

    #[test]
    fn init_print_mode_emits_the_stub_verbatim_for_every_format() {
        let report = InitReport {
            mode: InitMode::Print,
            target_path: PathBuf::from("/repo/AGENT_DOCS.toml"),
            wrote: false,
            stub: "# stub\n[[document]]\n".to_string(),
        };

        // Print mode is redirected straight into the catalog file, so `--format
        // json` must not wrap it in an envelope.
        for format in [OutputFormat::Text, OutputFormat::Json] {
            assert_eq!(
                render_init(format, &report).expect("init"),
                "# stub\n[[document]]\n"
            );
        }

        let wrote = InitReport {
            mode: InitMode::Write,
            wrote: true,
            ..report
        };
        assert_eq!(
            render_init(OutputFormat::Text, &wrote).expect("init"),
            "init: mode=write target=/repo/AGENT_DOCS.toml wrote=true"
        );
        let json = render_init(OutputFormat::Json, &wrote).expect("json");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        assert_eq!(parsed["wrote"].as_bool(), Some(true));
    }

    #[test]
    fn the_required_label_reflects_the_resolved_requirement() {
        assert_eq!(required_label(&document("A.md", true, true)), "required");
        assert_eq!(required_label(&document("A.md", false, true)), "optional");
    }
}
