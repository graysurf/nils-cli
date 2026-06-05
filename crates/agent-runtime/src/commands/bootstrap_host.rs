use crate::doctor::{self, DoctorClass, DoctorOptions, ResolvedRuntimeRoots};
use crate::install::{self, AppliedChange, InstallOptions};
use crate::prune_stale::{self, PruneOptions};
use crate::render::manifest::{self, ManifestSet, SourceRoot};
use crate::render::writer;
use clap::{Args, ValueEnum};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

const SCHEMA_VERSION: &str = "cli.agent-runtime.bootstrap-host.v1";
const CHECKPOINT_FILE: &str = "checkpoint.json";

#[derive(Args, Debug)]
pub struct BootstrapHostArgs {
    /// Source root containing `manifests/`, `core/`, `targets/`, `build/`.
    /// Defaults to the current working directory.
    #[arg(long)]
    pub source_root: Option<PathBuf>,
    /// Directory for the bootstrap checkpoint and install backup state.
    #[arg(long)]
    pub backup_root: Option<PathBuf>,
    /// Host tool profile to preview or apply.
    #[arg(long, value_enum, default_value_t = BootstrapProfile::Core)]
    pub profile: BootstrapProfile,
    /// Product surface set to bootstrap.
    #[arg(long, value_enum, default_value_t = ProductSelection::Both)]
    pub product: ProductSelection,
    /// Print the phase plan; do not mutate the filesystem.
    #[arg(long, conflicts_with = "apply")]
    pub dry_run: bool,
    /// Render, install, prune, verify, and write a checkpoint.
    #[arg(long, conflicts_with = "dry_run")]
    pub apply: bool,
    /// Leave Homebrew installation to the caller / existing setup wrapper.
    #[arg(long, default_value_t = false)]
    pub skip_homebrew_install: bool,
    /// Leave CLI tool installation to the caller / existing setup wrapper.
    #[arg(long, default_value_t = false)]
    pub skip_cli_tools: bool,
    /// Output format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub format: OutputFormat,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum, Serialize)]
#[clap(rename_all = "lower")]
#[serde(rename_all = "lowercase")]
pub enum BootstrapProfile {
    Core,
    Recommended,
    Full,
}

impl BootstrapProfile {
    fn label(self) -> &'static str {
        match self {
            Self::Core => "core",
            Self::Recommended => "recommended",
            Self::Full => "full",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum, Serialize)]
#[clap(rename_all = "lower")]
#[serde(rename_all = "lowercase")]
pub enum ProductSelection {
    Codex,
    Claude,
    Both,
}

impl ProductSelection {
    fn products(self) -> Vec<&'static str> {
        match self {
            Self::Codex => vec!["codex"],
            Self::Claude => vec!["claude"],
            Self::Both => vec!["codex", "claude"],
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
            Self::Both => "both",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
#[clap(rename_all = "lower")]
pub enum OutputFormat {
    Text,
    Json,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum BootstrapMode {
    DryRun,
    Apply,
}

impl BootstrapMode {
    fn label(self) -> &'static str {
        match self {
            Self::DryRun => "dry-run",
            Self::Apply => "apply",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum PhaseStatus {
    Pending,
    Completed,
    Failed,
    Skipped,
}

impl PhaseStatus {
    fn label(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
        }
    }
}

#[derive(Debug, Serialize)]
struct BootstrapHostReport {
    schema_version: &'static str,
    ok: bool,
    mode: BootstrapMode,
    profile: BootstrapProfile,
    product: ProductSelection,
    source_root: PathBuf,
    checkpoint_root: PathBuf,
    checkpoint_file: PathBuf,
    products: Vec<String>,
    phases: Vec<PhaseReport>,
}

#[derive(Debug, Deserialize, Serialize)]
struct PhaseReport {
    id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    product: Option<String>,
    status: PhaseStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    command: Option<String>,
    message: String,
}

#[derive(Deserialize)]
struct PreviousCheckpoint {
    phases: Vec<PreviousPhase>,
}

#[derive(Deserialize)]
struct PreviousPhase {
    id: String,
    status: PhaseStatus,
}

pub fn run(args: BootstrapHostArgs) -> anyhow::Result<u8> {
    if !args.dry_run && !args.apply {
        anyhow::bail!("pass --dry-run or --apply");
    }
    if let Some(path) = args.backup_root.as_deref()
        && !path.is_absolute()
    {
        anyhow::bail!("--backup-root must be absolute (got: {})", path.display());
    }
    let mode = if args.apply {
        BootstrapMode::Apply
    } else {
        BootstrapMode::DryRun
    };
    let source_root = SourceRoot::from_arg_or_cwd(args.source_root.as_deref())?;
    let checkpoint_root = args
        .backup_root
        .clone()
        .unwrap_or_else(default_checkpoint_root);
    let checkpoint_file = checkpoint_root.join(CHECKPOINT_FILE);
    let previous = previous_phase_statuses(&checkpoint_file)?;

    let mut runner = BootstrapRunner {
        args,
        mode,
        source_root,
        checkpoint_root,
        checkpoint_file,
        previous,
        manifests: None,
        phases: Vec::new(),
        halted: false,
    };
    let report = runner.run()?;
    let exit_code = if report.ok { 0 } else { 2 };

    match runner.args.format {
        OutputFormat::Text => print_text(&report),
        OutputFormat::Json => print_json(&report)?,
    }
    Ok(exit_code)
}

struct BootstrapRunner {
    args: BootstrapHostArgs,
    mode: BootstrapMode,
    source_root: SourceRoot,
    checkpoint_root: PathBuf,
    checkpoint_file: PathBuf,
    previous: BTreeMap<String, PhaseStatus>,
    manifests: Option<Arc<ManifestSet>>,
    phases: Vec<PhaseReport>,
    halted: bool,
}

impl BootstrapRunner {
    fn run(&mut self) -> anyhow::Result<BootstrapHostReport> {
        self.preflight();
        self.prepare_checkpoint_root();
        self.homebrew_phase();
        self.cli_tools_phase();
        for product in self.args.product.products() {
            self.product_phases(product);
        }
        self.delegated_phase(
            "home-prompt-docs",
            "home prompt/docs wiring remains delegated to the runtime-kit setup integration until the bootstrap wrapper is switched over",
        );
        self.delegated_phase(
            "zsh-kit-shell-setup",
            "zsh-kit shell setup remains delegated to the zsh-kit installer; authentication stays out of scope",
        );
        self.delegated_phase(
            "setup-wrapper-delegation",
            "setup scripts can call this command for render/install/prune/doctor phases while retaining their existing host-tool and shell setup gates",
        );

        let mut report = self.report();
        if self.mode == BootstrapMode::Apply {
            std::fs::create_dir_all(&self.checkpoint_root)?;
            write_checkpoint(&self.checkpoint_file, &report)?;
            report.checkpoint_file = self.checkpoint_file.clone();
        }
        Ok(report)
    }

    fn preflight(&mut self) {
        if self.mode == BootstrapMode::DryRun {
            self.apply_phase("preflight-source-root", None, None, |this| {
                let manifests = manifest::load_all(&this.source_root)?;
                this.manifests = Some(Arc::new(manifests));
                Ok("source root and manifests are readable".to_string())
            });
            return;
        }
        self.apply_phase("preflight-source-root", None, None, |this| {
            let manifests = manifest::load_all(&this.source_root)?;
            this.manifests = Some(Arc::new(manifests));
            Ok("source root and manifests are readable".to_string())
        });
    }

    fn prepare_checkpoint_root(&mut self) {
        if self.mode == BootstrapMode::DryRun {
            self.preview_phase(
                "prepare-checkpoint-root",
                None,
                None,
                "would prepare checkpoint root",
            );
            return;
        }
        self.apply_phase("prepare-checkpoint-root", None, None, |this| {
            std::fs::create_dir_all(&this.checkpoint_root)?;
            Ok(format!(
                "checkpoint root ready at {}",
                this.checkpoint_root.display()
            ))
        });
    }

    fn homebrew_phase(&mut self) {
        let message = if self.args.skip_homebrew_install {
            "Homebrew installation skipped by flag"
        } else {
            "Homebrew installation remains delegated to existing setup until wrapper integration"
        };
        self.skip_phase("homebrew", None, message);
    }

    fn cli_tools_phase(&mut self) {
        let message = if self.args.skip_cli_tools {
            "CLI tool installation skipped by flag"
        } else {
            "CLI tool installation remains delegated to existing setup until wrapper integration"
        };
        self.skip_phase("cli-tools", None, message);
    }

    fn product_phases(&mut self, product: &str) {
        self.render_phase(product);
        self.install_phase(product);
        self.prune_phase(product);
        self.doctor_skill_surface_phase(product);
    }

    fn render_phase(&mut self, product: &str) {
        let command = format!(
            "agent-runtime render --source-root {} --product {product}",
            self.source_root.path().display()
        );
        if self.mode == BootstrapMode::DryRun {
            self.preview_phase(
                &format!("render:{product}"),
                Some(product),
                Some(command),
                "would render product surfaces",
            );
            return;
        }
        self.apply_phase(
            &format!("render:{product}"),
            Some(product),
            Some(command),
            |this| {
                let manifests = this.manifests()?;
                let report = writer::write_product(&this.source_root, manifests, product)?;
                Ok(format!(
                    "rendered={} cached={} skipped={} output={}",
                    report.rendered.len(),
                    report.cached.len(),
                    report.skipped.len(),
                    report.output_root.display()
                ))
            },
        );
    }

    fn install_phase(&mut self, product: &str) {
        let roots = self.runtime_roots(product);
        let command = roots
            .as_ref()
            .map(|roots| {
                self.install_command(product, roots, &self.install_state_home(product, roots))
            })
            .unwrap_or_else(|_| {
                format!(
                    "agent-runtime install --source-root {} --product {product} --apply",
                    self.source_root.path().display()
                )
            });
        if self.mode == BootstrapMode::DryRun {
            match roots {
                Ok(_) => self.preview_phase(
                    &format!("install:{product}"),
                    Some(product),
                    Some(command),
                    "would install rendered product surfaces",
                ),
                Err(err) => {
                    self.failed_preview_phase(&format!("install:{product}"), product, command, err)
                }
            }
            return;
        }
        self.apply_phase(
            &format!("install:{product}"),
            Some(product),
            Some(command),
            |this| {
                let roots = this.runtime_roots(product)?;
                let state_home = this.install_state_home(product, &roots);
                let outcome = install::run(
                    product,
                    this.source_root.path(),
                    &roots.live_home,
                    &state_home,
                    install::Mode::Apply,
                    install_now(),
                    &InstallOptions {
                        tag: Some("bootstrap-host".to_string()),
                        ..InstallOptions::default()
                    },
                )?;
                let changes = outcome
                    .changes
                    .iter()
                    .filter(|change| !matches!(change, AppliedChange::NoOp { .. }))
                    .count();
                Ok(format!(
                    "actions={} changes={} live_home={} state_home={}",
                    outcome.plan.actions.len(),
                    changes,
                    roots.live_home.display(),
                    state_home.display()
                ))
            },
        );
    }

    fn prune_phase(&mut self, product: &str) {
        let roots = self.runtime_roots(product);
        let command = roots
            .as_ref()
            .map(|roots| self.prune_command(product, roots))
            .unwrap_or_else(|_| {
                format!(
                    "agent-runtime prune-stale --source-root {} --product {product} --apply",
                    self.source_root.path().display()
                )
            });
        if self.mode == BootstrapMode::DryRun {
            match roots {
                Ok(_) => self.preview_phase(
                    &format!("prune-stale:{product}"),
                    Some(product),
                    Some(command),
                    "would prune stale managed product surfaces",
                ),
                Err(err) => self.failed_preview_phase(
                    &format!("prune-stale:{product}"),
                    product,
                    command,
                    err,
                ),
            }
            return;
        }
        self.apply_phase(
            &format!("prune-stale:{product}"),
            Some(product),
            Some(command),
            |this| {
                let roots = this.runtime_roots(product)?;
                let outcome = prune_stale::run(
                    product,
                    this.source_root.path(),
                    &roots.live_home,
                    prune_stale::Mode::Apply,
                    &PruneOptions::default(),
                )?;
                let changes = outcome
                    .changes
                    .iter()
                    .filter(|change| change.is_change())
                    .count();
                let skipped = outcome
                    .changes
                    .iter()
                    .filter(|change| change.is_skip())
                    .count();
                Ok(format!(
                    "candidates={} changes={} skipped={}",
                    outcome.changes.len(),
                    changes,
                    skipped
                ))
            },
        );
    }

    fn doctor_skill_surface_phase(&mut self, product: &str) {
        let roots = self.runtime_roots(product);
        let command = roots
            .as_ref()
            .map(|roots| self.doctor_command(product, roots, &self.install_state_home(product, roots)))
            .unwrap_or_else(|_| {
                format!(
                    "agent-runtime doctor --source-root {} --product {product} --class skill-surface",
                    self.source_root.path().display()
                )
            });
        if self.mode == BootstrapMode::DryRun {
            match roots {
                Ok(_) => self.preview_phase(
                    &format!("doctor-skill-surface:{product}"),
                    Some(product),
                    Some(command),
                    "would verify rendered skill-surface shape",
                ),
                Err(err) => self.failed_preview_phase(
                    &format!("doctor-skill-surface:{product}"),
                    product,
                    command,
                    err,
                ),
            }
            return;
        }
        self.apply_phase(
            &format!("doctor-skill-surface:{product}"),
            Some(product),
            Some(command),
            |this| {
                let roots = this.runtime_roots(product)?;
                let state_home = this.install_state_home(product, &roots);
                let options = DoctorOptions {
                    cli_tools_profile: this.args.profile.label().to_string(),
                    class_filter: Some(DoctorClass::SkillSurface),
                    ..DoctorOptions::default()
                };
                let outcome = doctor::run(
                    product,
                    this.source_root.path(),
                    Some(&roots.live_home),
                    Some(&state_home),
                    &options,
                )?;
                if outcome.block > 0 {
                    anyhow::bail!(
                        "skill-surface doctor reported {} blocking finding(s)",
                        outcome.block
                    );
                }
                Ok(format!(
                    "checks={} ok={} warn={} block={}",
                    outcome.total_checks(),
                    outcome.ok,
                    outcome.warn,
                    outcome.block
                ))
            },
        );
    }

    fn delegated_phase(&mut self, id: &str, message: &str) {
        self.skip_phase(id, None, message);
    }

    fn preview_phase(
        &mut self,
        id: &str,
        product: Option<&str>,
        command: Option<String>,
        message: &str,
    ) {
        let status = self
            .previous
            .get(id)
            .copied()
            .filter(|status| matches!(status, PhaseStatus::Completed | PhaseStatus::Failed))
            .unwrap_or(PhaseStatus::Pending);
        self.push_phase(id, product, status, command, message.to_string());
    }

    fn failed_preview_phase(
        &mut self,
        id: &str,
        product: &str,
        command: String,
        err: anyhow::Error,
    ) {
        self.halted = true;
        self.push_phase(
            id,
            Some(product),
            PhaseStatus::Failed,
            Some(command),
            format!("{err:#}"),
        );
    }

    fn skip_phase(&mut self, id: &str, product: Option<&str>, message: &str) {
        self.push_phase(id, product, PhaseStatus::Skipped, None, message.to_string());
    }

    fn apply_phase<F>(
        &mut self,
        id: &str,
        product: Option<&str>,
        command: Option<String>,
        action: F,
    ) where
        F: FnOnce(&mut Self) -> anyhow::Result<String>,
    {
        if self.halted {
            self.push_phase(
                id,
                product,
                PhaseStatus::Pending,
                command,
                "pending because an earlier phase failed".to_string(),
            );
            return;
        }
        match action(self) {
            Ok(message) => self.push_phase(id, product, PhaseStatus::Completed, command, message),
            Err(err) => {
                self.halted = true;
                self.push_phase(
                    id,
                    product,
                    PhaseStatus::Failed,
                    command,
                    format!("{err:#}"),
                );
            }
        }
    }

    fn push_phase(
        &mut self,
        id: &str,
        product: Option<&str>,
        status: PhaseStatus,
        command: Option<String>,
        message: String,
    ) {
        self.phases.push(PhaseReport {
            id: id.to_string(),
            product: product.map(str::to_string),
            status,
            command,
            message,
        });
    }

    fn manifests(&self) -> anyhow::Result<Arc<ManifestSet>> {
        self.manifests
            .as_ref()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("manifest preflight did not complete"))
    }

    fn runtime_roots(&self, product: &str) -> anyhow::Result<ResolvedRuntimeRoots> {
        Ok(doctor::resolve_runtime_roots_for_product(
            product,
            self.source_root.path(),
            None,
            None,
        )?)
    }

    fn install_state_home(&self, product: &str, roots: &ResolvedRuntimeRoots) -> PathBuf {
        if self.args.backup_root.is_some() {
            self.checkpoint_root.join("state").join(product)
        } else {
            roots.state_home.clone()
        }
    }

    fn install_command(
        &self,
        product: &str,
        roots: &ResolvedRuntimeRoots,
        state_home: &Path,
    ) -> String {
        format!(
            "agent-runtime install --source-root {} --product {product} --live-home {} --state-home {} --apply",
            self.source_root.path().display(),
            roots.live_home.display(),
            state_home.display()
        )
    }

    fn prune_command(&self, product: &str, roots: &ResolvedRuntimeRoots) -> String {
        format!(
            "agent-runtime prune-stale --source-root {} --product {product} --live-home {} --apply",
            self.source_root.path().display(),
            roots.live_home.display()
        )
    }

    fn doctor_command(
        &self,
        product: &str,
        roots: &ResolvedRuntimeRoots,
        state_home: &Path,
    ) -> String {
        format!(
            "agent-runtime doctor --source-root {} --product {product} --live-home {} --state-home {} --profile {} --class skill-surface",
            self.source_root.path().display(),
            roots.live_home.display(),
            state_home.display(),
            self.args.profile.label()
        )
    }

    fn report(&self) -> BootstrapHostReport {
        let ok = !self
            .phases
            .iter()
            .any(|phase| phase.status == PhaseStatus::Failed);
        BootstrapHostReport {
            schema_version: SCHEMA_VERSION,
            ok,
            mode: self.mode,
            profile: self.args.profile,
            product: self.args.product,
            source_root: self.source_root.path().to_path_buf(),
            checkpoint_root: self.checkpoint_root.clone(),
            checkpoint_file: self.checkpoint_file.clone(),
            products: self
                .args
                .product
                .products()
                .into_iter()
                .map(str::to_string)
                .collect(),
            phases: self
                .phases
                .iter()
                .map(|phase| PhaseReport {
                    id: phase.id.clone(),
                    product: phase.product.clone(),
                    status: phase.status,
                    command: phase.command.clone(),
                    message: phase.message.clone(),
                })
                .collect(),
        }
    }
}

fn previous_phase_statuses(path: &Path) -> anyhow::Result<BTreeMap<String, PhaseStatus>> {
    if !path.exists() {
        return Ok(BTreeMap::new());
    }
    let raw = std::fs::read_to_string(path)?;
    let checkpoint: PreviousCheckpoint = serde_json::from_str(&raw)?;
    Ok(checkpoint
        .phases
        .into_iter()
        .map(|phase| (phase.id, phase.status))
        .collect())
}

fn write_checkpoint(path: &Path, report: &BootstrapHostReport) -> anyhow::Result<()> {
    let body = serde_json::to_string_pretty(report)?;
    std::fs::write(path, format!("{body}\n"))?;
    Ok(())
}

fn print_json(report: &BootstrapHostReport) -> anyhow::Result<()> {
    println!("{}", serde_json::to_string_pretty(report)?);
    Ok(())
}

fn print_text(report: &BootstrapHostReport) {
    eprintln!(
        "agent-runtime bootstrap-host: mode={} profile={} product={} ok={} checkpoint={}",
        report.mode.label(),
        report.profile.label(),
        report.product.label(),
        report.ok,
        report.checkpoint_file.display()
    );
    for phase in &report.phases {
        let product = phase
            .product
            .as_deref()
            .map(|value| format!(" product={value}"))
            .unwrap_or_default();
        eprintln!(
            "  {} {}{} - {}",
            phase.status.label(),
            phase.id,
            product,
            phase.message
        );
    }
}

fn default_checkpoint_root() -> PathBuf {
    if let Some(path) = std::env::var_os("AGENT_HOME") {
        return PathBuf::from(path).join("bootstrap-host");
    }
    if let Some(path) = std::env::var_os("XDG_STATE_HOME") {
        return PathBuf::from(path).join("agent-runtime-kit/bootstrap-host");
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".local/state/agent-runtime-kit/bootstrap-host")
}

#[allow(clippy::disallowed_methods)]
fn install_now() -> SystemTime {
    SystemTime::now()
}
