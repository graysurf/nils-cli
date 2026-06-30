use crate::prune_stale::{self, Mode, PruneChange, PruneOptions};
use crate::render::manifest::SourceRoot;
use clap::{Args, ValueEnum};
use serde::Serialize;
use std::path::PathBuf;

const SCHEMA_VERSION: &str = "cli.agent-runtime.prune-stale.v1";

#[derive(Args, Debug)]
pub struct PruneStaleArgs {
    /// Source root containing `manifests/`, `core/`, `targets/`, `build/`.
    /// Defaults to the current working directory.
    #[arg(long)]
    pub source_root: Option<PathBuf>,
    /// Product to prune (`codex`, `claude`, or `hermes`).
    #[arg(long)]
    pub product: String,
    /// Absolute path of the runtime home to scan and optionally repair.
    #[arg(long)]
    pub live_home: PathBuf,
    /// Skip the optional `.private/link-map.overrides.yaml` overlay merge.
    #[arg(long, default_value_t = false)]
    pub no_overlay: bool,
    /// Override the overlay file location. When set, the conventional
    /// `<source-root>/.private/link-map.overrides.yaml` is ignored.
    #[arg(long, conflicts_with = "no_overlay")]
    pub overlay_path: Option<PathBuf>,
    /// Print stale candidates; do not mutate the filesystem.
    #[arg(long, conflicts_with = "apply")]
    pub dry_run: bool,
    /// Remove provably owned stale symlinks and empty directories.
    #[arg(long, conflicts_with = "dry_run")]
    pub apply: bool,
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

#[derive(Serialize)]
struct JsonEnvelope<'a> {
    schema_version: &'static str,
    ok: bool,
    data: JsonData<'a>,
}

#[derive(Serialize)]
struct JsonData<'a> {
    product: &'a str,
    source_root: &'a std::path::Path,
    live_home: &'a std::path::Path,
    mode: Mode,
    candidates: usize,
    changes: usize,
    skipped: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    overlay: Option<crate::install::OverlaySummary>,
    records: &'a [PruneChange],
}

pub fn run(args: PruneStaleArgs) -> anyhow::Result<u8> {
    if args.product != "claude" && args.product != "codex" && args.product != "hermes" {
        anyhow::bail!(
            "agent-runtime prune-stale: unknown --product `{}` (expected one of: claude, codex, hermes)",
            args.product
        );
    }
    if !args.live_home.is_absolute() {
        anyhow::bail!(
            "agent-runtime prune-stale: --live-home must be absolute (got: {})",
            args.live_home.display()
        );
    }
    if !args.dry_run && !args.apply {
        anyhow::bail!("agent-runtime prune-stale: pass --dry-run or --apply");
    }
    let mode = if args.apply {
        Mode::Apply
    } else {
        Mode::DryRun
    };
    let root = SourceRoot::from_arg_or_cwd(args.source_root.as_deref())?;
    let options = PruneOptions {
        overlay_enabled: !args.no_overlay,
        overlay_path: args.overlay_path.clone(),
    };
    let outcome = prune_stale::run(&args.product, root.path(), &args.live_home, mode, &options)?;
    match args.format {
        OutputFormat::Text => print_text(&outcome),
        OutputFormat::Json => print_json(&outcome)?,
    }
    Ok(0)
}

fn print_text(outcome: &prune_stale::PruneOutcome) {
    if let Some(s) = outcome.overlay.as_ref() {
        eprintln!(
            "agent-runtime prune-stale: overlay merged (dropped={} replaced={} added={})",
            s.dropped, s.replaced, s.added,
        );
    }
    let changes = outcome.changes.iter().filter(|c| c.is_change()).count();
    let skipped = outcome.changes.iter().filter(|c| c.is_skip()).count();
    eprintln!(
        "agent-runtime prune-stale: product={} mode={} candidates={} changes={} skipped={}",
        outcome.product,
        outcome.mode.label(),
        outcome.changes.len(),
        changes,
        skipped,
    );
    for change in &outcome.changes {
        eprintln!("{}", format_change(change));
    }
}

fn print_json(outcome: &prune_stale::PruneOutcome) -> anyhow::Result<()> {
    let envelope = JsonEnvelope {
        schema_version: SCHEMA_VERSION,
        ok: true,
        data: JsonData {
            product: &outcome.product,
            source_root: &outcome.source_root,
            live_home: &outcome.live_home,
            mode: outcome.mode,
            candidates: outcome.changes.len(),
            changes: outcome.changes.iter().filter(|c| c.is_change()).count(),
            skipped: outcome.changes.iter().filter(|c| c.is_skip()).count(),
            overlay: outcome.overlay,
            records: &outcome.changes,
        },
    };
    println!("{}", serde_json::to_string_pretty(&envelope)?);
    Ok(())
}

fn format_change(change: &PruneChange) -> String {
    match change {
        PruneChange::WouldRemoveSymlink {
            rel_path, target, ..
        } => format!(
            "  - would remove symlink {} -> {}",
            rel_path.display(),
            target.display()
        ),
        PruneChange::RemovedSymlink {
            rel_path, target, ..
        } => format!(
            "  - removed symlink {} -> {}",
            rel_path.display(),
            target.display()
        ),
        PruneChange::NoOpSymlink {
            rel_path, target, ..
        } => format!(
            "  = no-op symlink {} -> {}",
            rel_path.display(),
            target.display()
        ),
        PruneChange::WouldRemoveEmptyDirectory { rel_path, .. } => {
            format!("  - would remove empty directory {}", rel_path.display())
        }
        PruneChange::RemovedEmptyDirectory { rel_path, .. } => {
            format!("  - removed empty directory {}", rel_path.display())
        }
        PruneChange::NoOpEmptyDirectory { rel_path, .. } => {
            format!("  = no-op empty directory {}", rel_path.display())
        }
        PruneChange::SkippedForeignSymlink {
            rel_path, target, ..
        } => format!(
            "  ? skip foreign symlink {} -> {}",
            rel_path.display(),
            target.display()
        ),
        PruneChange::SkippedRegularFile { rel_path, .. } => {
            format!("  ? skip regular file {}", rel_path.display())
        }
        PruneChange::SkippedNonEmptyDirectory { rel_path, .. } => {
            format!("  ? skip non-empty directory {}", rel_path.display())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;
    use tempfile::TempDir;

    fn write(path: &Path, content: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, content).unwrap();
    }

    fn make_hermes_source_root() -> TempDir {
        // Mirrors the real `targets/hermes/link-map.yaml`: destinations are
        // relative to the `~/.hermes` live-home and start with a bare
        // `skills/` / `plugins/` (no `.hermes/` prefix).
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        write(
            &root.join("manifests/runtime-roots.yaml"),
            "schema_version: 1\nproducts: {}\n",
        );
        write(
            &root.join("targets/hermes/link-map.yaml"),
            r#"schema_version: 1
entries:
  - id: reporting.plugin-manifest
    kind: plugin-manifest-copy
    source: targets/hermes/plugins/reporting/.hermes-plugin/plugin.json
    destination: plugins/reporting/.hermes-plugin/plugin.json
  - id: reporting.skills-tree
    kind: symlinked-file
    source: build/hermes/plugins/reporting/skills
    destination: skills/reporting
    recursive: true
"#,
        );
        write(
            &root.join("targets/hermes/plugins/reporting/.hermes-plugin/plugin.json"),
            "{}\n",
        );
        write(
            &root.join("build/hermes/plugins/reporting/skills/daily-brief/SKILL.md"),
            "# daily-brief\n",
        );
        tmp
    }

    fn hermes_args(source_root: &Path, live_home: &Path, product: &str) -> PruneStaleArgs {
        PruneStaleArgs {
            source_root: Some(source_root.to_path_buf()),
            product: product.to_string(),
            live_home: live_home.to_path_buf(),
            no_overlay: true,
            overlay_path: None,
            dry_run: true,
            apply: false,
            format: OutputFormat::Json,
        }
    }

    #[test]
    fn accepts_hermes_product() {
        // Before hermes was added to the product guard, `run` bailed with
        // "unknown --product `hermes`"; pruning an empty live-home must now
        // succeed (the sync workflow runs prune-stale for hermes).
        let src = make_hermes_source_root();
        let live = TempDir::new().unwrap();
        let args = hermes_args(src.path(), live.path(), "hermes");
        assert_eq!(run(args).unwrap(), 0);
    }

    #[test]
    fn rejects_unknown_product() {
        let src = make_hermes_source_root();
        let live = TempDir::new().unwrap();
        let args = hermes_args(src.path(), live.path(), "vscode");
        let err = run(args).unwrap_err();
        assert!(err.to_string().contains("unknown --product"));
    }
}
