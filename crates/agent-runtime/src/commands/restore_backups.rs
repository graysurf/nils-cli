use crate::render::manifest::SourceRoot;
use crate::restore_backups::{
    self, BackupRunSelector, Mode, RestoreError, RestoreOptions, RestoredChange,
};
use clap::Args;
use std::path::PathBuf;

#[derive(Args, Debug)]
pub struct RestoreBackupsArgs {
    /// Source root containing `manifests/`, `core/`, `targets/`, `build/`.
    /// Defaults to the current working directory.
    #[arg(long)]
    pub source_root: Option<PathBuf>,
    /// Product whose backups should be restored (`codex` or `claude`).
    #[arg(long)]
    pub product: String,
    /// Absolute path of the runtime home (e.g. `~/.codex` / `~/.claude`).
    /// Relative paths are rejected so restore never writes outside an
    /// explicitly-chosen directory.
    #[arg(long)]
    pub live_home: PathBuf,
    /// Absolute path of the state home (where backups live).
    #[arg(long)]
    pub state_home: PathBuf,
    /// Backup run to restore from: `latest` or a unix-seconds timestamp
    /// matching a directory under `<state_home>/backups/<product>/`.
    /// Required — running without it exits non-zero with the available
    /// timestamp list.
    #[arg(long)]
    pub from: Option<String>,
    /// Filter to a single link-map entry id. Default: restore every
    /// backup file in the run.
    #[arg(long)]
    pub surface: Option<String>,
    /// Skip the optional `.private/link-map.overrides.yaml` overlay
    /// merge. Default: merge if file exists, so overlay-discovered
    /// entries are restored alongside the canonical link map.
    #[arg(long, default_value_t = false)]
    pub no_overlay: bool,
    /// Override the overlay file location. When set, the conventional
    /// `<source-root>/.private/link-map.overrides.yaml` is ignored.
    #[arg(long, conflicts_with = "no_overlay")]
    pub overlay_path: Option<PathBuf>,
    /// Print the resolved plan; do not mutate the filesystem.
    #[arg(long, conflicts_with = "apply")]
    pub dry_run: bool,
    /// Restore backup files into their original install destinations.
    #[arg(long, conflicts_with = "dry_run")]
    pub apply: bool,
}

pub fn run(args: RestoreBackupsArgs) -> anyhow::Result<u8> {
    if !args.live_home.is_absolute() {
        anyhow::bail!(
            "agent-runtime restore-backups: --live-home must be absolute (got: {})",
            args.live_home.display()
        );
    }
    if !args.state_home.is_absolute() {
        anyhow::bail!(
            "agent-runtime restore-backups: --state-home must be absolute (got: {})",
            args.state_home.display()
        );
    }
    if !args.dry_run && !args.apply {
        anyhow::bail!("agent-runtime restore-backups: pass --dry-run or --apply");
    }
    let from = match args.from.as_deref() {
        Some(s) => s,
        None => {
            print_available_timestamps(&args.state_home, &args.product);
            anyhow::bail!(
                "agent-runtime restore-backups: --from is required (use `latest` or one of the timestamps printed above)"
            );
        }
    };
    let selector: BackupRunSelector = from.parse().map_err(|err: String| anyhow::anyhow!(err))?;

    let mode = if args.apply {
        Mode::Apply
    } else {
        Mode::DryRun
    };

    let root = SourceRoot::from_arg_or_cwd(args.source_root.as_deref())?;
    let options = RestoreOptions {
        selector: selector.clone(),
        surface: args.surface.clone(),
        overlay_enabled: !args.no_overlay,
        overlay_path: args.overlay_path.clone(),
    };

    let outcome = match restore_backups::run(
        &args.product,
        root.path(),
        &args.live_home,
        &args.state_home,
        mode,
        &options,
    ) {
        Ok(o) => o,
        Err(RestoreError::NoBackupRun {
            root,
            selector: _,
            available,
        }) => {
            print_timestamps_list(&args.product, &available);
            anyhow::bail!(
                "agent-runtime restore-backups: no backup run matches --from {from} under {}",
                root.display()
            );
        }
        Err(err) => return Err(err.into()),
    };

    if let Some(s) = outcome.overlay.as_ref() {
        eprintln!(
            "agent-runtime restore-backups: overlay merged (dropped={} replaced={} added={})",
            s.dropped, s.replaced, s.added,
        );
    }

    eprintln!(
        "agent-runtime restore-backups: product={} mode={} from={} actions={} restored={}",
        outcome.plan.product,
        if matches!(mode, Mode::Apply) {
            "apply"
        } else {
            "dry-run"
        },
        outcome.backup_run.display(),
        outcome.plan.actions.len(),
        outcome
            .changes
            .iter()
            .filter(|c| matches!(c, RestoredChange::FileRestored { .. }))
            .count(),
    );

    for change in &outcome.changes {
        print_change(change);
    }

    Ok(0)
}

fn print_available_timestamps(state_home: &std::path::Path, product: &str) {
    let timestamps = restore_backups::list_available_timestamps(state_home, product);
    print_timestamps_list(product, &timestamps);
}

fn print_timestamps_list(product: &str, timestamps: &[u64]) {
    if timestamps.is_empty() {
        eprintln!(
            "agent-runtime restore-backups: no backup runs found under <state_home>/backups/{product}/"
        );
    } else {
        eprintln!("agent-runtime restore-backups: available --from values for product={product}:");
        for ts in timestamps {
            eprintln!("  - {ts}");
        }
        eprintln!("  - latest (resolves to {})", timestamps.last().unwrap());
    }
}

fn print_change(c: &RestoredChange) {
    match c {
        RestoredChange::FileRestored {
            entry_id,
            dest,
            from_backup,
        } => eprintln!(
            "  + restore {} <- {} ({})",
            dest.display(),
            from_backup.display(),
            entry_id
        ),
        RestoredChange::SkippedDestRegularFile {
            entry_id,
            dest,
            from_backup: _,
        } => eprintln!(
            "  ? skip {} (regular file at dest; not overwriting; {})",
            dest.display(),
            entry_id
        ),
        RestoredChange::SkippedDestDirectory {
            entry_id,
            dest,
            from_backup: _,
        } => eprintln!(
            "  ? skip {} (directory at dest; refuse to destroy; {})",
            dest.display(),
            entry_id
        ),
        RestoredChange::SkippedNoMatch {
            entry_id,
            from_backup,
        } => eprintln!(
            "  ? skip {} (no link-map match for entry; {})",
            from_backup.display(),
            entry_id
        ),
        RestoredChange::SkippedAmbiguous {
            entry_id,
            from_backup,
            candidates,
        } => {
            eprintln!(
                "  ? skip {} (ambiguous: {} candidates; {})",
                from_backup.display(),
                candidates.len(),
                entry_id
            );
            for cand in candidates {
                eprintln!("      candidate: {}", cand.display());
            }
        }
        RestoredChange::SkippedSymlinkForeign {
            entry_id,
            dest,
            actual_target,
            expected_install_source,
            from_backup: _,
        } => eprintln!(
            "  ? skip {} (foreign target: {}; expected: {}; {})",
            dest.display(),
            actual_target.display(),
            expected_install_source.display(),
            entry_id
        ),
    }
}
