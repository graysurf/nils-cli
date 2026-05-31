use crate::gc_backups::{
    self, DEFAULT_RETENTION, GcChange, GcError, GcOptions, Mode, ProductFilter,
};
use clap::Args;
use std::path::PathBuf;

#[derive(Args, Debug)]
pub struct GcBackupsArgs {
    /// Absolute path of the state home whose `backups/` subtree is
    /// subject to retention. Relative paths are rejected so gc-backups
    /// never deletes outside an explicitly-chosen directory.
    #[arg(long)]
    pub state_home: PathBuf,
    /// Product to prune (`codex` or `claude`). Default: both products.
    #[arg(long)]
    pub product: Option<String>,
    /// Filter to a single link-map entry id. Runs whose root has no
    /// `<surface>/` subdir are entirely skipped (never deleted).
    #[arg(long)]
    pub surface: Option<String>,
    /// Number of newest install runs to retain per (product, surface)
    /// bucket. Default: 5.
    #[arg(long)]
    pub retention: Option<usize>,
    /// Print the planned deletions; do not mutate the filesystem.
    #[arg(long, conflicts_with = "apply")]
    pub dry_run: bool,
    /// Delete the planned set under `<state_home>/backups/<product>/`.
    #[arg(long, conflicts_with = "dry_run")]
    pub apply: bool,
}

pub fn run(args: GcBackupsArgs) -> anyhow::Result<u8> {
    if !args.state_home.is_absolute() {
        anyhow::bail!(
            "agent-runtime gc-backups: --state-home must be absolute (got: {})",
            args.state_home.display()
        );
    }
    if !args.dry_run && !args.apply {
        anyhow::bail!("agent-runtime gc-backups: pass --dry-run or --apply");
    }

    let product = match args.product.as_deref() {
        None => ProductFilter::All,
        Some("claude") | Some("codex") => ProductFilter::One(args.product.unwrap()),
        Some(other) => anyhow::bail!(
            "agent-runtime gc-backups: --product must be `claude` or `codex` (got `{other}`)"
        ),
    };

    let mode = if args.apply {
        Mode::Apply
    } else {
        Mode::DryRun
    };
    let retention = args.retention.unwrap_or(DEFAULT_RETENTION);
    let options = GcOptions {
        product,
        surface: args.surface.clone(),
        retention,
    };

    let outcome = match gc_backups::run(&args.state_home, mode, &options) {
        Ok(o) => o,
        Err(GcError::InvalidSurface { value }) => {
            anyhow::bail!(
                "agent-runtime gc-backups: --surface `{value}` must be a single path component"
            );
        }
        Err(err) => return Err(err.into()),
    };

    eprintln!(
        "agent-runtime gc-backups: mode={} retention={} retained={} preserved-by-tag={} {}={}",
        if matches!(mode, Mode::Apply) {
            "apply"
        } else {
            "dry-run"
        },
        outcome.retention,
        outcome.retained(),
        outcome.preserved_by_tag(),
        if matches!(mode, Mode::Apply) {
            "deleted"
        } else {
            "would-delete"
        },
        if matches!(mode, Mode::Apply) {
            outcome.deleted()
        } else {
            outcome.would_delete()
        },
    );

    for change in &outcome.changes {
        print_change(change);
    }

    Ok(0)
}

fn print_change(change: &GcChange) {
    match change {
        GcChange::Retained { product, ts, path } => {
            eprintln!("  . retain {product} ts={ts} {}", path.display());
        }
        GcChange::PreservedByTag {
            product,
            ts,
            path,
            marker,
        } => {
            eprintln!(
                "  # tagged {product} ts={ts} {} (marker: {})",
                path.display(),
                marker.display()
            );
        }
        GcChange::WouldDelete { product, ts, path } => {
            eprintln!("  - would-delete {product} ts={ts} {}", path.display());
        }
        GcChange::Deleted { product, ts, path } => {
            eprintln!("  - deleted {product} ts={ts} {}", path.display());
        }
    }
}
