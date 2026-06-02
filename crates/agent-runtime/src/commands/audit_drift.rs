use crate::audit_drift;
use crate::render::manifest::SourceRoot;
use clap::{Args, ValueEnum};
use serde::Serialize;
use std::path::PathBuf;

#[derive(Args, Debug)]
pub struct AuditDriftArgs {
    /// Source root containing `manifests/`, `core/`, `targets/`, `build/`.
    /// Defaults to the current working directory.
    #[arg(long)]
    pub source_root: Option<PathBuf>,

    /// Include suppressed drift findings in the report.
    #[arg(long)]
    pub verbose: bool,

    /// Severity that makes the command exit non-zero. `warn` (the default)
    /// fails on any warn- or block-tier finding; `block` fails only on
    /// block-tier findings, reporting warns without failing the gate.
    #[arg(long = "fail-on", value_enum, default_value = "warn")]
    pub fail_on: FailOn,

    /// Output format.
    #[arg(long, value_enum, default_value = "text")]
    pub format: OutputFormat,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
#[clap(rename_all = "lower")]
pub enum OutputFormat {
    Text,
    Json,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
#[clap(rename_all = "lower")]
pub enum FailOn {
    Warn,
    Block,
}

impl FailOn {
    fn label(self) -> &'static str {
        match self {
            FailOn::Warn => "warn",
            FailOn::Block => "block",
        }
    }

    /// Map the report's raw exit code to the effective exit code under this
    /// gating policy. `warn` passes it through; `block` demotes the
    /// warn-tier exit (1) to 0 while leaving block-tier (2) fatal.
    fn gate(self, raw_exit: u8) -> u8 {
        match self {
            FailOn::Warn => raw_exit,
            FailOn::Block if raw_exit >= BLOCK_EXIT_CODE => raw_exit,
            FailOn::Block => 0,
        }
    }
}

const BLOCK_EXIT_CODE: u8 = 2;

#[derive(Serialize)]
struct AuditDriftJson<'a> {
    schema_version: &'static str,
    fail_on: &'static str,
    total: usize,
    block: usize,
    warn: usize,
    info: usize,
    suppressed: usize,
    exit_code: u8,
    findings: &'a [&'a audit_drift::Finding],
}

pub fn run(args: AuditDriftArgs) -> anyhow::Result<u8> {
    let root = SourceRoot::from_arg_or_cwd(args.source_root.as_deref())?;
    let report = audit_drift::run(&root)?;

    let visible_findings: Vec<&audit_drift::Finding> = report
        .findings
        .iter()
        .filter(|f| args.verbose || f.severity != audit_drift::Severity::Suppressed)
        .collect();

    let raw_exit = report.exit_code();
    let exit_code = args.fail_on.gate(raw_exit);

    if args.format == OutputFormat::Json {
        let envelope = AuditDriftJson {
            schema_version: "agent-runtime-cli.audit-drift.v1",
            fail_on: args.fail_on.label(),
            total: visible_findings.len(),
            block: count_severity(&report, audit_drift::Severity::Block),
            warn: count_severity(&report, audit_drift::Severity::Warn),
            info: count_severity(&report, audit_drift::Severity::Info),
            suppressed: count_severity(&report, audit_drift::Severity::Suppressed),
            exit_code,
            findings: &visible_findings,
        };
        println!("{}", serde_json::to_string_pretty(&envelope)?);
        return Ok(exit_code);
    }

    for f in &visible_findings {
        eprintln!(
            "audit-drift [{class}/{severity}{product}] {path}: {msg}",
            class = f.class,
            severity = f.severity.label(),
            product = f
                .product
                .as_deref()
                .map(|p| format!("/{p}"))
                .unwrap_or_default(),
            path = f.path.display(),
            msg = f.message,
        );
    }
    if exit_code == 0 && raw_exit != 0 {
        eprintln!(
            "audit-drift: {n} finding(s); highest raw severity exit={raw}, gated to 0 by --fail-on block",
            n = visible_findings.len(),
            raw = raw_exit,
        );
    } else if exit_code == 0 {
        eprintln!("audit-drift: clean ({} findings)", visible_findings.len());
    } else {
        eprintln!(
            "audit-drift: {n} finding(s); highest-severity exit={exit}",
            n = visible_findings.len(),
            exit = exit_code,
        );
    }
    Ok(exit_code)
}

fn count_severity(report: &audit_drift::DriftReport, severity: audit_drift::Severity) -> usize {
    report
        .findings
        .iter()
        .filter(|f| f.severity == severity)
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fail_on_warn_passes_every_exit_code_through() {
        assert_eq!(FailOn::Warn.gate(0), 0);
        assert_eq!(FailOn::Warn.gate(1), 1);
        assert_eq!(FailOn::Warn.gate(2), 2);
    }

    #[test]
    fn fail_on_block_demotes_warn_but_keeps_block() {
        assert_eq!(FailOn::Block.gate(0), 0);
        assert_eq!(FailOn::Block.gate(1), 0, "warn-tier becomes non-fatal");
        assert_eq!(FailOn::Block.gate(2), 2, "block-tier stays fatal");
    }
}
