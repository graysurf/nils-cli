use crate::render::manifest::SourceRoot;
use crate::uninstall::{self, Mode, UninstallOptions, UninstalledChange};
use clap::Args;
use std::path::PathBuf;

#[derive(Args, Debug)]
pub struct UninstallArgs {
    /// Source root containing `manifests/`, `core/`, `targets/`, `build/`.
    /// Defaults to the current working directory.
    #[arg(long)]
    pub source_root: Option<PathBuf>,
    /// Product to uninstall (`codex` or `claude`).
    #[arg(long)]
    pub product: String,
    /// Absolute path of the runtime home (e.g. `~/.codex` / `~/.claude`).
    /// Relative paths are rejected so uninstall never reads outside an
    /// explicitly-chosen directory.
    #[arg(long)]
    pub live_home: PathBuf,
    /// Skip the optional `.private/link-map.overrides.yaml` overlay
    /// merge. Default: merge if file exists, so overlay-discovered
    /// entries are reversed alongside the canonical link map.
    #[arg(long, default_value_t = false)]
    pub no_overlay: bool,
    /// Override the overlay file location. When set, the conventional
    /// `<source-root>/.private/link-map.overrides.yaml` is ignored.
    #[arg(long, conflicts_with = "no_overlay")]
    pub overlay_path: Option<PathBuf>,
    /// Print the resolved plan; do not mutate the filesystem.
    #[arg(long, conflicts_with = "apply")]
    pub dry_run: bool,
    /// Reconcile the runtime home by removing every link-map-owned
    /// symlink and managed-block. Idempotent on the second run.
    #[arg(long, conflicts_with = "dry_run")]
    pub apply: bool,
}

pub fn run(args: UninstallArgs) -> anyhow::Result<u8> {
    if !args.live_home.is_absolute() {
        anyhow::bail!(
            "agent-runtime uninstall: --live-home must be absolute (got: {}); pass an absolute path such as /tmp/claude-sandbox or $HOME/.claude",
            args.live_home.display()
        );
    }
    if !args.dry_run && !args.apply {
        anyhow::bail!("agent-runtime uninstall: pass --dry-run or --apply");
    }
    let mode = if args.apply {
        Mode::Apply
    } else {
        Mode::DryRun
    };

    let root = SourceRoot::from_arg_or_cwd(args.source_root.as_deref())?;
    let options = UninstallOptions {
        overlay_enabled: !args.no_overlay,
        overlay_path: args.overlay_path.clone(),
    };

    let outcome = uninstall::run(&args.product, root.path(), &args.live_home, mode, &options)?;

    if let Some(s) = outcome.overlay.as_ref() {
        eprintln!(
            "agent-runtime uninstall: overlay merged (dropped={} replaced={} added={})",
            s.dropped, s.replaced, s.added,
        );
    }

    eprintln!(
        "agent-runtime uninstall: product={} mode={} actions={} changes={}",
        outcome.plan.product,
        if matches!(mode, Mode::Apply) {
            "apply"
        } else {
            "dry-run"
        },
        outcome.plan.actions.len(),
        outcome
            .changes
            .iter()
            .filter(|c| !matches!(c, UninstalledChange::NoOp { .. }))
            .count(),
    );

    for change in &outcome.changes {
        print_change(change);
    }

    Ok(0)
}

fn print_change(c: &UninstalledChange) {
    match c {
        UninstalledChange::SymlinkRemoved { entry_id, dest } => {
            eprintln!("  - symlink {} ({})", dest.display(), entry_id)
        }
        UninstalledChange::ManagedBlockRemoved {
            entry_id,
            config_file,
        } => eprintln!(
            "  - managed-block removed from {} ({})",
            config_file.display(),
            entry_id
        ),
        UninstalledChange::SymlinkSkippedForeign {
            entry_id,
            dest,
            actual_target,
            expected_source,
        } => eprintln!(
            "  ? skip {} (foreign target: {}; expected: {}; {})",
            dest.display(),
            actual_target.display(),
            expected_source.display(),
            entry_id
        ),
        UninstalledChange::SymlinkSkippedRegularFile { entry_id, dest } => {
            eprintln!(
                "  ? skip {} (regular file; restore-backups owns; {})",
                dest.display(),
                entry_id
            )
        }
        UninstalledChange::NoOp { entry_id, dest } => {
            eprintln!("  = no-op {} ({})", dest.display(), entry_id)
        }
    }
}
