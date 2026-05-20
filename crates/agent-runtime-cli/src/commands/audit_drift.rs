use crate::audit_drift;
use crate::render::manifest::SourceRoot;
use clap::Args;
use std::path::PathBuf;

#[derive(Args, Debug)]
pub struct AuditDriftArgs {
    /// Source root containing `manifests/`, `core/`, `targets/`, `build/`.
    /// Defaults to the current working directory.
    #[arg(long)]
    pub source_root: Option<PathBuf>,
}

pub fn run(args: AuditDriftArgs) -> anyhow::Result<u8> {
    let root = SourceRoot::from_arg_or_cwd(args.source_root.as_deref())?;
    let report = audit_drift::run(&root)?;
    for f in &report.findings {
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
    let exit_code = report.exit_code();
    if exit_code == 0 {
        eprintln!("audit-drift: clean ({} findings)", report.findings.len());
    } else {
        eprintln!(
            "audit-drift: {n} finding(s); highest-severity exit={exit}",
            n = report.findings.len(),
            exit = exit_code,
        );
    }
    Ok(exit_code)
}
