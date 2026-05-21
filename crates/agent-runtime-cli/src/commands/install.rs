use crate::install::{self, AppliedChange, InstallOptions, Mode};
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
    /// Relative paths are rejected so install never writes outside an
    /// explicitly-chosen directory.
    #[arg(long)]
    pub live_home: PathBuf,
    /// Absolute path of the state home (where backups land).
    #[arg(long)]
    pub state_home: PathBuf,
    /// Tag the backup-run root with a `tag-<name>` marker file so
    /// `gc-backups` (Plan 04 Sprint 2 Task 2.4) can preserve it across
    /// retention sweeps. Tag must be ASCII alphanumeric / `-` / `_`.
    #[arg(long)]
    pub tag: Option<String>,
    /// Skip the optional `.private/link-map.overrides.yaml` overlay
    /// merge. Default: merge if file exists.
    #[arg(long, default_value_t = false)]
    pub no_overlay: bool,
    /// Override the overlay file location. When set, the conventional
    /// `<source-root>/.private/link-map.overrides.yaml` is ignored.
    #[arg(long, conflicts_with = "no_overlay")]
    pub overlay_path: Option<PathBuf>,
    /// Print the resolved plan; do not mutate the filesystem.
    #[arg(long, conflicts_with = "apply")]
    pub dry_run: bool,
    /// Reconcile the runtime home to the plan. Idempotent on the
    /// second run.
    #[arg(long, conflicts_with = "dry_run")]
    pub apply: bool,
}

pub fn run(args: InstallArgs) -> anyhow::Result<u8> {
    if !args.live_home.is_absolute() {
        // Open question Q1 default (resolved 2026-05-21): `--live-home`
        // must be absolute. Relative paths exit non-zero with a usage
        // error naming the flag so callers can fix their invocation.
        anyhow::bail!(
            "agent-runtime install: --live-home must be absolute (got: {}); pass an absolute path such as /tmp/claude-sandbox or $HOME/.claude",
            args.live_home.display()
        );
    }
    if !args.state_home.is_absolute() {
        anyhow::bail!(
            "agent-runtime install: --state-home must be absolute (got: {})",
            args.state_home.display()
        );
    }
    if let Some(tag) = args.tag.as_deref()
        && !install::is_trusted_tag(tag)
    {
        // The executor re-validates as defense in depth; the CLI catches
        // the same shape early so the error message can name the flag.
        anyhow::bail!(
            "agent-runtime install: --tag `{tag}` is not a trusted tag name (allowed: ASCII alphanumeric / `-` / `_`)"
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

    let options = InstallOptions {
        tag: args.tag.clone(),
        overlay_enabled: !args.no_overlay,
        overlay_path: args.overlay_path.clone(),
    };

    let outcome = install::run(
        &args.product,
        root.path(),
        &args.live_home,
        &args.state_home,
        mode,
        now,
        &options,
    )?;

    if let Some(s) = outcome.overlay.as_ref() {
        // Operator-visible notice that the overlay was consumed; matches
        // the architecture-doc requirement that dry-run print the
        // post-merge effective state, not just the inputs.
        eprintln!(
            "agent-runtime install: overlay merged (dropped={} replaced={} added={})",
            s.dropped, s.replaced, s.added,
        );
    }

    eprintln!(
        "agent-runtime install: product={} mode={} actions={} changes={}",
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
            .filter(|c| !matches!(c, AppliedChange::NoOp { .. }))
            .count(),
    );

    for change in &outcome.changes {
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
