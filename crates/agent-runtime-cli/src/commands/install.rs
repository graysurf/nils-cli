use crate::install::{self, AppliedChange, Mode};
use crate::render::manifest::SourceRoot;
use clap::Args;
use std::path::PathBuf;
use std::time::SystemTime;

#[derive(Args, Debug)]
pub struct InstallArgs {
    /// Source root containing `manifests/`, `core/`, `targets/`, `build/`.
    /// Defaults to the current working directory.
    #[arg(long)]
    pub source_root: Option<PathBuf>,
    /// Product to install (`codex` or `claude`).
    #[arg(long)]
    pub product: String,
    /// Absolute path of the runtime home (e.g. `~/.codex` / `~/.claude`).
    /// Plan 04 Sprint 1 Task 1.3 will add `--live-home` as the polished
    /// alias plus env-var expansion of the runtime-roots template.
    #[arg(long)]
    pub home: PathBuf,
    /// Absolute path of the state home (where backups land).
    #[arg(long)]
    pub state_home: PathBuf,
    /// Print the resolved plan; do not mutate the filesystem.
    #[arg(long, conflicts_with = "apply")]
    pub dry_run: bool,
    /// Reconcile the runtime home to the plan. Idempotent on the
    /// second run.
    #[arg(long, conflicts_with = "dry_run")]
    pub apply: bool,
}

pub fn run(args: InstallArgs) -> anyhow::Result<u8> {
    if !args.home.is_absolute() {
        anyhow::bail!(
            "agent-runtime install: --home must be absolute (got: {})",
            args.home.display()
        );
    }
    if !args.state_home.is_absolute() {
        anyhow::bail!(
            "agent-runtime install: --state-home must be absolute (got: {})",
            args.state_home.display()
        );
    }
    if !args.dry_run && !args.apply {
        anyhow::bail!("agent-runtime install: pass --dry-run or --apply");
    }
    let mode = if args.apply {
        Mode::Apply
    } else {
        Mode::DryRun
    };

    let root = SourceRoot::from_arg_or_cwd(args.source_root.as_deref())?;
    #[allow(clippy::disallowed_methods)]
    let now = SystemTime::now();

    let (plan, changes) = install::run(
        &args.product,
        root.path(),
        &args.home,
        &args.state_home,
        mode,
        now,
    )?;

    eprintln!(
        "agent-runtime install: product={} mode={} actions={} changes={}",
        plan.product,
        if matches!(mode, Mode::Apply) {
            "apply"
        } else {
            "dry-run"
        },
        plan.actions.len(),
        changes
            .iter()
            .filter(|c| !matches!(c, AppliedChange::NoOp { .. }))
            .count(),
    );

    for change in &changes {
        print_change(change);
    }

    Ok(0)
}

fn print_change(c: &AppliedChange) {
    match c {
        AppliedChange::SymlinkCreated {
            entry_id,
            dest,
            source,
        } => eprintln!(
            "  + symlink {} -> {} ({})",
            dest.display(),
            source.display(),
            entry_id
        ),
        AppliedChange::SymlinkReplaced {
            entry_id,
            dest,
            source,
        } => eprintln!(
            "  ~ symlink {} -> {} (replaced; {})",
            dest.display(),
            source.display(),
            entry_id
        ),
        AppliedChange::FileBackedUpThenSymlinked {
            entry_id,
            dest,
            source,
            backup,
        } => eprintln!(
            "  ! backup {} -> {}, then symlink to {} ({})",
            dest.display(),
            backup.display(),
            source.display(),
            entry_id
        ),
        AppliedChange::ManagedBlockApplied {
            entry_id,
            config_file,
        } => eprintln!(
            "  ~ managed-block applied to {} ({})",
            config_file.display(),
            entry_id
        ),
        AppliedChange::NoOp { entry_id, dest } => {
            eprintln!("  = no-op {} ({})", dest.display(), entry_id)
        }
    }
}
