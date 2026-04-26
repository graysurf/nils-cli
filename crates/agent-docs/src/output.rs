use anyhow::{Context, Result};
use serde::Serialize;

use crate::commands::scaffold_baseline::ScaffoldBaselineReport;
use crate::model::{
    BaselineCheckReport, Context as DocContext, OutputFormat, ResolveFormat, ResolveReport,
    StubReport,
};

#[derive(Debug, Serialize)]
struct ContextsOutput<'a> {
    contexts: &'a [DocContext],
}

pub fn render_contexts(format: ResolveFormat, contexts: &[DocContext]) -> Result<String> {
    match format {
        ResolveFormat::Text => Ok(contexts
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")),
        ResolveFormat::Json => serde_json::to_string_pretty(&ContextsOutput { contexts })
            .context("failed to serialize contexts output"),
        ResolveFormat::Checklist => Ok(render_contexts_checklist(contexts)),
    }
}

fn render_contexts_checklist(contexts: &[DocContext]) -> String {
    let total = contexts.len();
    let mut lines = Vec::with_capacity(total + 2);
    lines.push(format!("CONTEXTS_BEGIN total={total}"));
    for ctx in contexts {
        lines.push(ctx.to_string());
    }
    lines.push(format!("CONTEXTS_END total={total}"));
    lines.join("\n")
}

pub fn render_resolve(format: ResolveFormat, report: &ResolveReport) -> Result<String> {
    match format {
        ResolveFormat::Text => Ok(render_resolve_text(report)),
        ResolveFormat::Json => {
            serde_json::to_string_pretty(report).context("failed to serialize resolve output")
        }
        ResolveFormat::Checklist => Ok(render_resolve_checklist(report)),
    }
}

pub fn render_stub(
    format: OutputFormat,
    command: &str,
    message: impl Into<String>,
) -> Result<String> {
    let report = StubReport {
        command: command.to_string(),
        implemented: false,
        message: message.into(),
    };

    match format {
        OutputFormat::Text => Ok(format!("{}: {}", report.command, report.message)),
        OutputFormat::Json => {
            serde_json::to_string_pretty(&report).context("failed to serialize stub output")
        }
    }
}

pub fn render_baseline(format: OutputFormat, report: &BaselineCheckReport) -> Result<String> {
    match format {
        OutputFormat::Text => Ok(render_baseline_text(report)),
        OutputFormat::Json => {
            serde_json::to_string_pretty(report).context("failed to serialize baseline output")
        }
    }
}

pub fn render_scaffold_baseline(
    format: OutputFormat,
    report: &ScaffoldBaselineReport,
) -> Result<String> {
    match format {
        OutputFormat::Text => Ok(render_scaffold_baseline_text(report)),
        OutputFormat::Json => serde_json::to_string_pretty(report)
            .context("failed to serialize scaffold-baseline output"),
    }
}

fn render_resolve_text(report: &ResolveReport) -> String {
    let mut lines = Vec::new();
    lines.push(format!("CONTEXT: {}", report.context));
    lines.push(format!("AGENT_DOCS_HOME: {}", report.docs_home.display()));
    lines.push(format!("PROJECT_PATH: {}", report.project_path.display()));
    lines.push(String::new());

    for doc in &report.documents {
        let required_label = if doc.required { "required" } else { "optional" };
        lines.push(format!(
            "[{}] {} {} {} source={} status={} why=\"{}\"",
            required_label,
            doc.context,
            doc.scope,
            doc.path.display(),
            doc.source,
            doc.status,
            doc.why
        ));
    }

    lines.push(String::new());
    lines.push(format!(
        "summary: required_total={} present_required={} missing_required={} strict={}",
        report.summary.required_total,
        report.summary.present_required,
        report.summary.missing_required,
        report.strict
    ));

    lines.join("\n")
}

fn render_resolve_checklist(report: &ResolveReport) -> String {
    let mode = if report.strict {
        "strict"
    } else {
        "non-strict"
    };
    let mut lines = Vec::new();
    lines.push(format!(
        "REQUIRED_DOCS_BEGIN context={} mode={mode}",
        report.context
    ));

    for doc in report.documents.iter().filter(|doc| doc.required) {
        let file_name = doc
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| doc.path.display().to_string());
        lines.push(format!(
            "{} status={} path={}",
            file_name,
            doc.status,
            doc.path.display()
        ));
    }

    lines.push(format!(
        "REQUIRED_DOCS_END required={} present={} missing={} mode={mode} context={}",
        report.summary.required_total,
        report.summary.present_required,
        report.summary.missing_required,
        report.context
    ));

    lines.join("\n")
}

fn render_baseline_text(report: &BaselineCheckReport) -> String {
    let mut lines = Vec::new();
    lines.push(format!("BASELINE CHECK: {}", report.target));
    lines.push(format!("AGENT_DOCS_HOME: {}", report.docs_home.display()));
    lines.push(format!("PROJECT_PATH: {}", report.project_path.display()));
    lines.push(String::new());

    for item in &report.items {
        let required_label = if item.required {
            "required"
        } else {
            "optional"
        };
        lines.push(format!(
            "[{}] {:<15} {} {} {} source={} why=\"{}\"",
            item.scope,
            item.label,
            item.path.display(),
            required_label,
            item.status,
            item.source,
            item.why
        ));
    }

    lines.push(String::new());
    lines.push(format!("missing_required: {}", report.missing_required));
    lines.push(format!("missing_optional: {}", report.missing_optional));
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

fn render_scaffold_baseline_text(report: &ScaffoldBaselineReport) -> String {
    let mut lines = Vec::new();
    lines.push(format!("SCAFFOLD BASELINE: {}", report.target));
    lines.push(format!("AGENT_DOCS_HOME: {}", report.docs_home.display()));
    lines.push(format!("PROJECT_PATH: {}", report.project_path.display()));
    lines.push(String::new());

    for item in &report.items {
        lines.push(format!(
            "[{}] {:<15} {} action={} reason=\"{}\"",
            item.scope,
            item.label,
            item.path.display(),
            item.action,
            item.reason
        ));
    }

    lines.push(String::new());
    lines.push(format!(
        "summary: created={} overwritten={} skipped={} planned_create={} planned_overwrite={} planned_skip={}",
        report.created,
        report.overwritten,
        report.skipped,
        report.planned_create,
        report.planned_overwrite,
        report.planned_skip
    ));

    lines.join("\n")
}
