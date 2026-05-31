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
                Envelope::failure(schema_version_for("agent-docs", "preflight", 1), error);
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
