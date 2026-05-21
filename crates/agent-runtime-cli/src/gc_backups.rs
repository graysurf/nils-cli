//! `agent-runtime gc-backups` body. Plan 04 Sprint 2 Task 2.4.
//!
//! Prune aged backups under `<state_home>/backups/<product>/`. Retention
//! defaults to 5 install runs; `--retention <N>` overrides. Runs whose
//! root contains a `tag-<name>` marker file (written by
//! `install --tag <name>`) are preserved regardless of where they fall in
//! the timestamp ordering. `--surface <name>` restricts the sweep to runs
//! that touched a specific link-map entry. `gc-backups` is the only
//! sanctioned retention enforcer — `install` never prunes silently.
//!
//! ## Backup tree layout
//!
//! ```text
//! <state_home>/backups/<product>/<unix_seconds>/
//!     ├── <entry_id>/<filename>     (directory subtrees — one per entry)
//!     └── tag-<name>                (regular file — the run-level marker)
//! ```
//!
//! ## Canonical tag-marker vs entry_id disambiguation
//!
//! Per Plan 04 Sprint 1 Task 1.3 F-3 advisory, `gc-backups` owns the
//! canonical [`is_tag_marker`] helper: a path is a tag marker iff its
//! basename starts with `tag-` AND it is a regular file (per
//! `symlink_metadata`). An entry-id subdir whose name happens to start
//! with `tag-` is a directory, not a marker, and is not treated as a
//! retention pin. This keeps the two namespaces collision-safe even when
//! a future link-map entry chooses an unfortunate id.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Default install-runs retention when `--retention` is not passed.
pub const DEFAULT_RETENTION: usize = 5;

/// Products that gc-backups can scan when `--product` is not specified.
/// Kept in deterministic order so test golden output stays stable.
pub const ALL_PRODUCTS: &[&str] = &["claude", "codex"];

/// Per-run dry-run / apply selector. Mirrors `install::Mode` /
/// `uninstall::Mode` / `restore_backups::Mode` so the four lifecycle
/// commands share an identical surface. Sprint 4 will fold these into
/// one shared `Mode` (see Plan 04 Task 2.2 advisory R-10).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    DryRun,
    Apply,
}

/// Product scope. `All` walks both `claude` and `codex` subtrees;
/// `One` restricts to one product.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ProductFilter {
    #[default]
    All,
    One(String),
}

/// Per-invocation knobs. Defaults match the plan's stated defaults:
/// every product, no surface filter, retention 5.
#[derive(Debug, Clone)]
pub struct GcOptions {
    pub product: ProductFilter,
    /// Filter to a single link-map entry id. When set, only runs whose
    /// root contains a `<surface>/` subdirectory are considered for
    /// retention — other runs are entirely skipped, never deleted.
    pub surface: Option<String>,
    pub retention: usize,
}

impl Default for GcOptions {
    fn default() -> Self {
        Self {
            product: ProductFilter::All,
            surface: None,
            retention: DEFAULT_RETENTION,
        }
    }
}

/// One decision in the gc sweep. Records the (product, ts) pair plus the
/// absolute path so the CLI can echo each disposition deterministically.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GcChange {
    /// Run was within the retention window (kept; no mutation).
    Retained {
        product: String,
        ts: u64,
        path: PathBuf,
    },
    /// Run carried at least one `tag-*` marker file (kept regardless of
    /// retention; no mutation).
    PreservedByTag {
        product: String,
        ts: u64,
        path: PathBuf,
        marker: PathBuf,
    },
    /// Run was beyond retention and deleted (`Mode::Apply` only).
    Deleted {
        product: String,
        ts: u64,
        path: PathBuf,
    },
    /// Run was beyond retention; `Mode::DryRun` would delete it.
    WouldDelete {
        product: String,
        ts: u64,
        path: PathBuf,
    },
}

impl GcChange {
    pub fn product(&self) -> &str {
        match self {
            GcChange::Retained { product, .. }
            | GcChange::PreservedByTag { product, .. }
            | GcChange::Deleted { product, .. }
            | GcChange::WouldDelete { product, .. } => product,
        }
    }

    pub fn ts(&self) -> u64 {
        match self {
            GcChange::Retained { ts, .. }
            | GcChange::PreservedByTag { ts, .. }
            | GcChange::Deleted { ts, .. }
            | GcChange::WouldDelete { ts, .. } => *ts,
        }
    }

    pub fn path(&self) -> &Path {
        match self {
            GcChange::Retained { path, .. }
            | GcChange::PreservedByTag { path, .. }
            | GcChange::Deleted { path, .. }
            | GcChange::WouldDelete { path, .. } => path,
        }
    }
}

#[derive(Debug, Error)]
pub enum GcError {
    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(
        "--surface `{value}` must be a single path component (no `/`, `\\`, `..`, or leading `.`)"
    )]
    InvalidSurface { value: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GcOutcome {
    pub mode: Mode,
    pub retention: usize,
    pub changes: Vec<GcChange>,
}

impl GcOutcome {
    pub fn deleted(&self) -> usize {
        self.changes
            .iter()
            .filter(|c| matches!(c, GcChange::Deleted { .. }))
            .count()
    }
    pub fn would_delete(&self) -> usize {
        self.changes
            .iter()
            .filter(|c| matches!(c, GcChange::WouldDelete { .. }))
            .count()
    }
    pub fn retained(&self) -> usize {
        self.changes
            .iter()
            .filter(|c| matches!(c, GcChange::Retained { .. }))
            .count()
    }
    pub fn preserved_by_tag(&self) -> usize {
        self.changes
            .iter()
            .filter(|c| matches!(c, GcChange::PreservedByTag { .. }))
            .count()
    }
}

/// Canonical disambiguator between an `install --tag <name>` marker file
/// and an entry-id subdirectory that happens to start with `tag-`.
/// Returns `true` only when `path` exists as a regular file (the install
/// executor always writes the marker as a zero-byte regular file).
///
/// Documented as part of Task 2.4 because gc-backups is the consumer that
/// must protect tagged runs from retention sweeps — restore-backups uses
/// the same shape but only as a "skip top-level files" filter where the
/// outcome is the same regardless of marker / non-marker classification.
pub fn is_tag_marker(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    if !name.starts_with("tag-") {
        return false;
    }
    fs::symlink_metadata(path)
        .map(|m| m.file_type().is_file())
        .unwrap_or(false)
}

/// Execute one gc-backups sweep. Returns a deterministic, ordered set of
/// per-run decisions:
///
/// - For each product (alphabetical when [`ProductFilter::All`]),
/// - Walk `<state_home>/backups/<product>/<ts>/` directories,
/// - Optionally retain only runs that contain a `<surface>/` subdir,
/// - Tagged runs land as [`GcChange::PreservedByTag`] (deterministic),
/// - Untagged runs sort by `ts` descending; the first `retention`
///   become [`GcChange::Retained`], the rest become
///   [`GcChange::Deleted`] / [`GcChange::WouldDelete`] depending on
///   `mode`.
///
/// Returns `Ok` with an empty `changes` vec when `<state_home>/backups/`
/// (or the product subtree) does not exist — gc-backups is idempotent
/// across "never installed" / "already pruned" host states.
pub fn run(state_home: &Path, mode: Mode, options: &GcOptions) -> Result<GcOutcome, GcError> {
    if let Some(s) = options.surface.as_deref() {
        validate_surface(s)?;
    }

    let products: Vec<&str> = match &options.product {
        ProductFilter::All => ALL_PRODUCTS.to_vec(),
        ProductFilter::One(p) => vec![p.as_str()],
    };

    let mut changes = Vec::new();
    for product in products {
        let product_root = state_home.join("backups").join(product);
        if !product_root.is_dir() {
            continue;
        }
        let runs = enumerate_runs(&product_root)?;
        let runs = filter_by_surface(runs, options.surface.as_deref());

        // Partition into (tagged, untagged) preserving deterministic order.
        let mut tagged: Vec<RunRow> = Vec::new();
        let mut untagged: Vec<RunRow> = Vec::new();
        for row in runs {
            match find_tag_marker(&row.path) {
                Some(marker) => tagged.push(RunRow {
                    marker: Some(marker),
                    ..row
                }),
                None => untagged.push(row),
            }
        }

        // Tagged: stable by ts ascending (matches enumerate order).
        for row in tagged {
            changes.push(GcChange::PreservedByTag {
                product: product.to_string(),
                ts: row.ts,
                path: row.path,
                marker: row.marker.expect("tagged row carries its marker"),
            });
        }

        // Untagged: newest first.
        untagged.sort_by_key(|row| std::cmp::Reverse(row.ts));
        let keep_count = options.retention.min(untagged.len());
        let to_delete: Vec<RunRow> = untagged.split_off(keep_count);

        for row in untagged {
            changes.push(GcChange::Retained {
                product: product.to_string(),
                ts: row.ts,
                path: row.path,
            });
        }
        for row in to_delete {
            match mode {
                Mode::DryRun => {
                    changes.push(GcChange::WouldDelete {
                        product: product.to_string(),
                        ts: row.ts,
                        path: row.path,
                    });
                }
                Mode::Apply => {
                    fs::remove_dir_all(&row.path).map_err(|source| GcError::Io {
                        path: row.path.clone(),
                        source,
                    })?;
                    changes.push(GcChange::Deleted {
                        product: product.to_string(),
                        ts: row.ts,
                        path: row.path,
                    });
                }
            }
        }
    }

    Ok(GcOutcome {
        mode,
        retention: options.retention,
        changes,
    })
}

#[derive(Debug, Clone)]
struct RunRow {
    ts: u64,
    path: PathBuf,
    marker: Option<PathBuf>,
}

fn enumerate_runs(product_root: &Path) -> Result<Vec<RunRow>, GcError> {
    let read = fs::read_dir(product_root).map_err(|source| GcError::Io {
        path: product_root.to_path_buf(),
        source,
    })?;
    let mut rows = Vec::new();
    for entry in read {
        let entry = entry.map_err(|source| GcError::Io {
            path: product_root.to_path_buf(),
            source,
        })?;
        let file_type = entry.file_type().map_err(|source| GcError::Io {
            path: entry.path(),
            source,
        })?;
        if !file_type.is_dir() {
            // Stray non-dir at the product root: not a run. Skip silently.
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let Ok(ts) = name.parse::<u64>() else {
            // Non-numeric subdir at the product root: not a run.
            continue;
        };
        rows.push(RunRow {
            ts,
            path: entry.path(),
            marker: None,
        });
    }
    rows.sort_by_key(|row| row.ts);
    Ok(rows)
}

fn filter_by_surface(rows: Vec<RunRow>, surface: Option<&str>) -> Vec<RunRow> {
    let Some(name) = surface else {
        return rows;
    };
    rows.into_iter()
        .filter(|row| has_surface_subdir(&row.path, name))
        .collect()
}

fn has_surface_subdir(run_dir: &Path, surface: &str) -> bool {
    let target = run_dir.join(surface);
    fs::symlink_metadata(&target)
        .map(|m| m.file_type().is_dir())
        .unwrap_or(false)
}

fn find_tag_marker(run_dir: &Path) -> Option<PathBuf> {
    let read = fs::read_dir(run_dir).ok()?;
    let mut markers: Vec<PathBuf> = read
        .flatten()
        .map(|e| e.path())
        .filter(|p| is_tag_marker(p))
        .collect();
    markers.sort();
    markers.into_iter().next()
}

fn validate_surface(s: &str) -> Result<(), GcError> {
    if s.is_empty()
        || s.contains('/')
        || s.contains('\\')
        || s == ".."
        || s == "."
        || s.starts_with('.')
    {
        return Err(GcError::InvalidSurface {
            value: s.to_string(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn seed_run(state: &Path, product: &str, ts: u64, entry_id: &str) -> PathBuf {
        let run = state
            .join("backups")
            .join(product)
            .join(ts.to_string())
            .join(entry_id);
        fs::create_dir_all(&run).unwrap();
        fs::write(run.join("plugin.json"), format!("BACKUP-{product}-{ts}")).unwrap();
        state.join("backups").join(product).join(ts.to_string())
    }

    fn seed_tag(state: &Path, product: &str, ts: u64, name: &str) {
        let path = state
            .join("backups")
            .join(product)
            .join(ts.to_string())
            .join(format!("tag-{name}"));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"").unwrap();
    }

    #[test]
    fn is_tag_marker_file_yes_dir_no() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("tag-pre-bump");
        fs::write(&file, b"").unwrap();
        assert!(is_tag_marker(&file), "regular file with tag- prefix");

        let dir = tmp.path().join("tag-as-dir");
        fs::create_dir_all(&dir).unwrap();
        assert!(
            !is_tag_marker(&dir),
            "directory with tag- prefix is NOT a marker"
        );

        let other = tmp.path().join("not-a-tag");
        fs::write(&other, b"").unwrap();
        assert!(!is_tag_marker(&other), "non-tag- prefix");

        let missing = tmp.path().join("tag-missing");
        assert!(!is_tag_marker(&missing), "non-existent path");
    }

    #[test]
    fn run_retains_five_of_seven_in_apply_mode() {
        let tmp = TempDir::new().unwrap();
        let state = tmp.path();
        for ts in [100u64, 200, 300, 400, 500, 600, 700] {
            seed_run(state, "claude", ts, "entry");
        }
        let outcome = run(
            state,
            Mode::Apply,
            &GcOptions {
                product: ProductFilter::One("claude".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(outcome.retained(), 5);
        assert_eq!(outcome.deleted(), 2);
        assert_eq!(outcome.would_delete(), 0);

        // Newest 5 survive on disk; oldest 2 deleted.
        for keep in [300u64, 400, 500, 600, 700] {
            assert!(
                state.join("backups/claude").join(keep.to_string()).is_dir(),
                "retained ts={keep}"
            );
        }
        for gone in [100u64, 200] {
            assert!(
                !state.join("backups/claude").join(gone.to_string()).exists(),
                "deleted ts={gone}"
            );
        }
    }

    #[test]
    fn dry_run_classifies_without_mutating() {
        let tmp = TempDir::new().unwrap();
        let state = tmp.path();
        for ts in [100u64, 200, 300, 400, 500, 600, 700] {
            seed_run(state, "claude", ts, "entry");
        }
        let outcome = run(
            state,
            Mode::DryRun,
            &GcOptions {
                product: ProductFilter::One("claude".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(outcome.would_delete(), 2);
        assert_eq!(outcome.deleted(), 0);
        // Every dir still on disk.
        for ts in [100u64, 200, 300, 400, 500, 600, 700] {
            assert!(state.join("backups/claude").join(ts.to_string()).is_dir());
        }
    }

    #[test]
    fn tag_marker_pins_run_even_outside_retention_window() {
        let tmp = TempDir::new().unwrap();
        let state = tmp.path();
        // Oldest carries a tag — it MUST survive a default retention=5
        // sweep against 7 runs.
        for ts in [100u64, 200, 300, 400, 500, 600, 700] {
            seed_run(state, "claude", ts, "entry");
        }
        seed_tag(state, "claude", 100, "pre-bump");

        let outcome = run(
            state,
            Mode::Apply,
            &GcOptions {
                product: ProductFilter::One("claude".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(outcome.preserved_by_tag(), 1);
        assert!(state.join("backups/claude/100").is_dir(), "tagged survives");
        // With 6 untagged runs and retention=5, exactly 1 gets deleted (ts=200).
        assert_eq!(outcome.deleted(), 1);
        assert!(!state.join("backups/claude/200").exists());
    }

    #[test]
    fn retention_three_keeps_three() {
        let tmp = TempDir::new().unwrap();
        let state = tmp.path();
        for ts in [100u64, 200, 300, 400, 500] {
            seed_run(state, "claude", ts, "entry");
        }
        let outcome = run(
            state,
            Mode::Apply,
            &GcOptions {
                product: ProductFilter::One("claude".into()),
                retention: 3,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(outcome.retained(), 3);
        assert_eq!(outcome.deleted(), 2);
    }

    #[test]
    fn surface_filter_skips_runs_without_the_named_subdir() {
        let tmp = TempDir::new().unwrap();
        let state = tmp.path();
        seed_run(state, "claude", 100, "alpha");
        seed_run(state, "claude", 200, "beta");
        seed_run(state, "claude", 300, "alpha");
        // retention=1 with --surface alpha:
        // candidate runs = {100,300}; newest kept = 300; 100 deleted.
        // Run 200 has no `alpha/` subdir — must be untouched.
        let outcome = run(
            state,
            Mode::Apply,
            &GcOptions {
                product: ProductFilter::One("claude".into()),
                surface: Some("alpha".into()),
                retention: 1,
            },
        )
        .unwrap();
        assert_eq!(outcome.deleted(), 1);
        assert!(state.join("backups/claude/200").is_dir(), "beta survives");
        assert!(state.join("backups/claude/300").is_dir(), "newest alpha");
        assert!(!state.join("backups/claude/100").exists());
    }

    #[test]
    fn product_all_walks_both_subtrees() {
        let tmp = TempDir::new().unwrap();
        let state = tmp.path();
        for ts in [100u64, 200, 300, 400, 500, 600] {
            seed_run(state, "claude", ts, "entry");
        }
        for ts in [10u64, 20, 30] {
            seed_run(state, "codex", ts, "entry");
        }
        let outcome = run(state, Mode::Apply, &GcOptions::default()).unwrap();
        // claude: 6 runs, retain 5, delete 1.
        // codex: 3 runs, retain 3, delete 0.
        assert_eq!(outcome.retained(), 8);
        assert_eq!(outcome.deleted(), 1);
    }

    #[test]
    fn missing_state_or_product_root_is_clean_noop() {
        let tmp = TempDir::new().unwrap();
        let outcome = run(tmp.path(), Mode::Apply, &GcOptions::default()).unwrap();
        assert!(outcome.changes.is_empty());
    }

    #[test]
    fn entry_id_subdir_starting_with_tag_dash_is_not_treated_as_marker() {
        // Plan 04 Task 1.3 F-3 namespace-overlap regression-pin.
        // A link-map entry named `tag-pre-bump` would create
        // `<ts>/tag-pre-bump/` (directory) — `is_tag_marker` must NOT
        // classify it as a tag marker, so retention sweeps the run.
        let tmp = TempDir::new().unwrap();
        let state = tmp.path();
        for ts in [100u64, 200] {
            seed_run(state, "claude", ts, "tag-pre-bump");
        }
        let outcome = run(
            state,
            Mode::Apply,
            &GcOptions {
                product: ProductFilter::One("claude".into()),
                retention: 1,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(outcome.preserved_by_tag(), 0, "subdir is not a marker");
        assert_eq!(outcome.deleted(), 1);
        assert!(!state.join("backups/claude/100").exists());
    }

    #[test]
    fn invalid_surface_with_path_separators_is_rejected() {
        let tmp = TempDir::new().unwrap();
        for bad in ["", "..", ".", ".hidden", "a/b", "a\\b"] {
            let err = run(
                tmp.path(),
                Mode::DryRun,
                &GcOptions {
                    surface: Some(bad.into()),
                    ..Default::default()
                },
            )
            .unwrap_err();
            assert!(
                matches!(err, GcError::InvalidSurface { .. }),
                "expected InvalidSurface for {bad:?}, got {err:?}"
            );
        }
    }
}
