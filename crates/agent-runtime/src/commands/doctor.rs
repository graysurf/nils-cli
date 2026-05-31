use crate::doctor::{self, DoctorClass, DoctorFinding, DoctorOptions, DoctorSeverity, upgrade};
use crate::render::manifest::SourceRoot;
use clap::{Args, ValueEnum};
use serde::Serialize;
use std::path::PathBuf;

#[derive(Args, Debug)]
pub struct DoctorArgs {
    /// Source root containing `manifests/`, `core/`, `targets/`, `build/`.
    /// Defaults to the current working directory.
    #[arg(long)]
    pub source_root: Option<PathBuf>,
    /// Product to diagnose (`codex` or `claude`). Required for every class
    /// except `version-alignment`, which is product-agnostic.
    #[arg(long)]
    pub product: Option<String>,
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
    /// Active cli-tools profile to check.
    #[arg(long, default_value = "recommended")]
    pub profile: String,
    /// Print copy-pasteable Homebrew upgrade commands for non-ok probes.
    #[arg(long, default_value_t = false)]
    pub suggest_upgrade: bool,
    /// Inspect a consuming repo's `.agents/scripts/` project-local overlays.
    #[arg(long)]
    pub check_project: Option<PathBuf>,
    /// Run a single doctor class instead of the default host/runtime probes.
    #[arg(long = "class", value_enum)]
    pub class: Option<DoctorClassArg>,
    /// Pin manifest (`<pin-spec>`, YAML or JSON) for `--class version-alignment`.
    #[arg(long)]
    pub pin: Option<PathBuf>,
    /// Output format.
    #[arg(long, value_enum, default_value = "text")]
    pub format: OutputFormat,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
#[clap(rename_all = "kebab-case")]
pub enum DoctorClassArg {
    SkillSurface,
    VersionAlignment,
}

impl From<DoctorClassArg> for DoctorClass {
    fn from(value: DoctorClassArg) -> Self {
        match value {
            DoctorClassArg::SkillSurface => DoctorClass::SkillSurface,
            DoctorClassArg::VersionAlignment => DoctorClass::VersionAlignment,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
#[clap(rename_all = "lower")]
pub enum OutputFormat {
    Text,
    Json,
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

    let class: Option<DoctorClass> = args.class.map(Into::into);

    // `version-alignment` is product-agnostic; every other class requires a
    // product. Resolve a placeholder for the former so the shared entrypoint
    // signature is unchanged.
    let product = match class {
        Some(DoctorClass::VersionAlignment) => {
            args.product.clone().unwrap_or_else(|| "host".to_string())
        }
        _ => match args.product.clone() {
            Some(product) => product,
            None => anyhow::bail!(
                "agent-runtime doctor: --product <codex|claude> is required unless --class version-alignment"
            ),
        },
    };

    let root = SourceRoot::from_arg_or_cwd(args.source_root.as_deref())?;
    let options = DoctorOptions {
        overlay_enabled: !args.no_overlay,
        overlay_path: args.overlay_path.clone(),
        cli_tools_profile: args.profile.clone(),
        check_project: args.check_project.clone(),
        class_filter: class,
        pin_path: args.pin.clone(),
    };
    let outcome = doctor::run(
        &product,
        root.path(),
        args.live_home.as_deref(),
        args.state_home.as_deref(),
        &options,
    )?;

    if args.format == OutputFormat::Json {
        print_json(&outcome, args.suggest_upgrade)?;
        return Ok(outcome.exit_code());
    }

    print_text(&outcome, args.suggest_upgrade);
    Ok(outcome.exit_code())
}

fn print_text(outcome: &doctor::DoctorOutcome, suggest_upgrade: bool) {
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
    for probe in &outcome.coverage_probes {
        let parsed = probe.parsed_version.as_deref().unwrap_or("unknown");
        let required = probe.required_version.as_deref().unwrap_or("n/a");
        eprintln!(
            "  {} {} status={} name={} command=`{}` required={} parsed={}",
            match probe.severity {
                DoctorSeverity::Ok => "ok",
                DoctorSeverity::Warn => "warn",
                DoctorSeverity::Block => "block",
            },
            probe.kind.check(),
            probe.status.as_str(),
            probe.name,
            probe.command,
            required,
            parsed,
        );
    }
    for probe in &outcome.project_probes {
        eprintln!(
            "  {} project-overlay status={} script={} path={}",
            match probe.severity {
                DoctorSeverity::Ok => "ok",
                DoctorSeverity::Warn => "warn",
                DoctorSeverity::Block => "block",
            },
            probe.status.as_str(),
            probe.script,
            probe.path.display(),
        );
    }
    if let Some(report) = outcome.version_alignment.as_ref() {
        for item in &report.items {
            eprintln!(
                "  {} {} target={} expected={} observed={}",
                match item.severity {
                    DoctorSeverity::Ok => "ok",
                    DoctorSeverity::Warn => "warn",
                    DoctorSeverity::Block => "block",
                },
                item.check,
                item.target,
                item.expected,
                item.observed.as_deref().unwrap_or("none"),
            );
        }
    }
    for finding in &outcome.findings {
        print_finding(finding);
    }
    if suggest_upgrade {
        for suggestion in upgrade::suggestions(outcome) {
            println!("{}", suggestion.command);
        }
    }
    if let Some(boundary) = outcome.acceptance_boundary.as_deref() {
        eprintln!("agent-runtime doctor: acceptance-boundary: {boundary}");
    }
}

#[derive(Serialize)]
struct DoctorJson<'a> {
    schema_version: &'static str,
    product: &'a str,
    checks: usize,
    ok: usize,
    warn: usize,
    block: usize,
    exit_code: u8,
    findings: &'a [DoctorFinding],
    #[serde(skip_serializing_if = "Option::is_none")]
    skill_surface: Option<&'a doctor::skill_surface::SkillSurfaceReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    version_alignment: Option<&'a doctor::version_alignment::VersionAlignmentReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    acceptance_boundary: Option<&'a str>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    upgrade_suggestions: Vec<String>,
}

fn print_json(outcome: &doctor::DoctorOutcome, suggest_upgrade: bool) -> anyhow::Result<()> {
    let upgrade_suggestions = if suggest_upgrade {
        upgrade::suggestions(outcome)
            .into_iter()
            .map(|suggestion| suggestion.command)
            .collect()
    } else {
        Vec::new()
    };
    let envelope = DoctorJson {
        schema_version: "agent-runtime-cli.doctor.v1",
        product: &outcome.product,
        checks: outcome.total_checks(),
        ok: outcome.ok,
        warn: outcome.warn,
        block: outcome.block,
        exit_code: outcome.exit_code(),
        findings: &outcome.findings,
        skill_surface: outcome.skill_surface.as_ref(),
        version_alignment: outcome.version_alignment.as_ref(),
        acceptance_boundary: outcome.acceptance_boundary.as_deref(),
        upgrade_suggestions,
    };
    println!("{}", serde_json::to_string_pretty(&envelope)?);
    Ok(())
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
