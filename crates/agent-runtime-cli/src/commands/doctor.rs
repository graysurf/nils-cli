use crate::doctor::{self, DoctorFinding, DoctorOptions, DoctorSeverity};
use crate::render::manifest::SourceRoot;
use clap::Args;
use std::path::PathBuf;

#[derive(Args, Debug)]
pub struct DoctorArgs {
    /// Source root containing `manifests/`, `core/`, `targets/`, `build/`.
    /// Defaults to the current working directory.
    #[arg(long)]
    pub source_root: Option<PathBuf>,
    /// Product to diagnose (`codex` or `claude`).
    #[arg(long)]
    pub product: String,
    /// Absolute path of the runtime home to inspect. Defaults to the
    /// product's `live_home` from `manifests/runtime-roots.yaml`.
    #[arg(long)]
    pub live_home: Option<PathBuf>,
    /// Absolute path of the state home to inspect. Defaults to the
    /// product's `state_home` from `manifests/runtime-roots.yaml`.
    #[arg(long)]
    pub state_home: Option<PathBuf>,
    /// Skip the optional `.private/link-map.overrides.yaml` overlay
    /// merge. Default: merge if file exists, matching install/uninstall.
    #[arg(long, default_value_t = false)]
    pub no_overlay: bool,
    /// Override the overlay file location. When set, the conventional
    /// `<source-root>/.private/link-map.overrides.yaml` is ignored.
    #[arg(long, conflicts_with = "no_overlay")]
    pub overlay_path: Option<PathBuf>,
}

pub fn run(args: DoctorArgs) -> anyhow::Result<u8> {
    if let Some(path) = args.live_home.as_deref()
        && !path.is_absolute()
    {
        anyhow::bail!(
            "agent-runtime doctor: --live-home must be absolute (got: {}); pass an absolute path such as /tmp/claude-sandbox or $HOME/.claude",
            path.display()
        );
    }
    if let Some(path) = args.state_home.as_deref()
        && !path.is_absolute()
    {
        anyhow::bail!(
            "agent-runtime doctor: --state-home must be absolute (got: {})",
            path.display()
        );
    }

    let root = SourceRoot::from_arg_or_cwd(args.source_root.as_deref())?;
    let options = DoctorOptions {
        overlay_enabled: !args.no_overlay,
        overlay_path: args.overlay_path.clone(),
    };
    let outcome = doctor::run(
        &args.product,
        root.path(),
        args.live_home.as_deref(),
        args.state_home.as_deref(),
        &options,
    )?;

    if let Some(summary) = outcome.overlay.as_ref() {
        eprintln!(
            "agent-runtime doctor: overlay merged (dropped={} replaced={} added={})",
            summary.dropped, summary.replaced, summary.added,
        );
    }

    eprintln!(
        "agent-runtime doctor: product={} checks={} ok={} warn={} block={}",
        outcome.product,
        outcome.total_checks(),
        outcome.ok,
        outcome.warn,
        outcome.block,
    );
    for probe in &outcome.version_probes {
        let parsed = probe.parsed_version.as_deref().unwrap_or("unparseable");
        eprintln!(
            "  {} version-probe status={} parsed={} command=`{}`",
            match probe.severity {
                DoctorSeverity::Ok => "ok",
                DoctorSeverity::Warn => "warn",
                DoctorSeverity::Block => "block",
            },
            probe.status.as_str(),
            parsed,
            probe.command,
        );
    }
    for finding in &outcome.findings {
        print_finding(finding);
    }

    Ok(outcome.exit_code())
}

fn print_finding(finding: &DoctorFinding) {
    let severity = match finding.severity {
        DoctorSeverity::Ok => "ok",
        DoctorSeverity::Warn => "warn",
        DoctorSeverity::Block => "block",
    };
    let path = finding
        .path
        .as_ref()
        .map(|p| format!(" {}", p.display()))
        .unwrap_or_default();
    let entry = finding
        .entry_id
        .as_ref()
        .map(|id| format!(" ({id})"))
        .unwrap_or_default();
    eprintln!(
        "  {severity} {}{}{}: {}",
        finding.check, entry, path, finding.message,
    );
}
