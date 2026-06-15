//! `evidence migrate` — batch dry-run and apply.
//!
//! NET-NEW batch semantics (not a direct mirror of plan-archive's
//! single-folder, non-scrubbing, source-deleting migrate):
//!
//! - **Batch**: enumerates *many* `skill-usage.record.json` files under the
//!   agent-out tree, filtered by `--repo/--skill/--since/--until/--promotion-only`.
//! - **Inline scrub**: every text field and child-evidence file is scrubbed
//!   (via the shared `nils-scrub` crate, borrowed from plan-archive *refresh*,
//!   not its migrate) before any byte is written; a `<id>.scrub.log` is emitted
//!   only when something fired.
//! - **Dedup**: a record whose `source_digest` already appears in the archive
//!   `catalog.json` is classified "already archived" and not re-written.
//! - **One batch commit**: the whole run is a single `semantic-commit`, since
//!   `catalog.json` is a single shared artifact regenerated once per run.
//! - **No deletion**: source records are never deleted. Idempotency is the
//!   catalog `source_digest` dedup above — a re-run re-reads the catalog and
//!   skips any digest already archived.

use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command as ProcCommand;

use nils_common::cli_contract::{Envelope, EnvelopeError, OutputFormat, exit, schema_version_for};
use serde::Serialize;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::record::SkillUsageRecord;
use crate::source::{self, SourceError};
use crate::validate::hosts::{HostClass, HostEntry};

pub mod identity;
pub mod path;

pub use identity::{IdentityError, RepoIdentity, derive_repo_identity};
pub use path::{archive_target_path, encode_basic_stamp, rollup_id, skill_slug};

const COMMAND: &str = "migrate";
const BINARY: &str = "evidence";

/// Rollup record schema id written into every `skill-usage.rollup.json`.
pub const ROLLUP_SCHEMA: &str = "skill-usage.rollup.v1";
/// Source `skill-usage.record.json` schema this migrator knows how to roll up.
/// A record carrying any other schema (e.g. a future `skill-usage.record.v2`
/// with changed semantics) is skipped with a warning rather than silently
/// normalized to a `skill-usage.rollup.v1`.
pub const SUPPORTED_RECORD_SCHEMA: &str = "skill-usage.record.v1";
/// Provenance sidecar schema version written into every `metadata.yaml`.
pub const METADATA_VERSION: u32 = 1;
/// Default tool name synthesized for pre-producer records.
const FALLBACK_PRODUCER_TOOL: &str = "skill-usage";

/// Args forwarded from `cli::run`.
pub struct DispatchArgs {
    pub source_out: Option<PathBuf>,
    pub archive: Option<PathBuf>,
    pub hosts: Option<PathBuf>,
    pub repo: Option<String>,
    pub skill: Option<String>,
    pub since: Option<String>,
    pub until: Option<String>,
    pub promotion_only: bool,
    pub apply: bool,
    /// Operator-supplied host override for slug-only records (`--host`). When
    /// present and the agent-out slug resolves to `(org, repo)`, this host is
    /// used directly (after validation against `config/hosts.yaml`), bypassing
    /// the multi-host cwd ambiguity.
    pub host: Option<String>,
    /// Local-checkout roots (from the machine-local config `working_repo_roots`)
    /// used as a last-resort host-resolution hint when a record's recorded `cwd`
    /// no longer exists. Empty disables the rescue.
    pub working_repo_roots: Vec<PathBuf>,
    pub format: OutputFormat,
}

/// Structured repo identity stored in a rollup (queryable columns).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RollupRepo {
    pub host: String,
    pub org: String,
    pub repo: String,
}

/// Outcome block in a rollup. `status` is a free string (never an enum).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RollupOutcome {
    pub status: String,
    pub summary: String,
}

/// Per-record counts.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RollupCounts {
    pub validation: usize,
    pub failures: usize,
}

/// Provenance block carried by every rollup.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RollupProducer {
    pub tool: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nils_cli_version: Option<String>,
}

/// A linked child evidence entry, sorted by (type, path) for determinism.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct LinkedEvidence {
    #[serde(rename = "type")]
    pub evidence_type: String,
    pub path: String,
}

/// Promotion linkage (presence-only; query never traverses the back-ref).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Promotion {
    pub heuristic_inbox_case: String,
}

/// The normalized `skill-usage.rollup.json` payload.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RollupRecord {
    pub schema: String,
    pub id: String,
    pub archived_at: String,
    pub skill: String,
    pub intent: String,
    pub trigger: String,
    pub repo: RollupRepo,
    pub cwd: String,
    pub started_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
    pub outcome: RollupOutcome,
    pub producer: RollupProducer,
    pub counts: RollupCounts,
    pub linked_evidence: Vec<LinkedEvidence>,
    pub source_digest: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub promotion: Option<Promotion>,
}

/// Provenance sidecar written next to the rollup.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct MetadataPayload {
    pub metadata_version: u32,
    pub source: MetadataSource,
    pub captured_classification: ClassificationSnapshot,
    pub archived_at: String,
    pub refs: MetadataRefs,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub promotion: Option<Promotion>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct MetadataSource {
    pub producer: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nils_cli_version: Option<String>,
    pub agent_out_path: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq, Default)]
pub struct MetadataRefs {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_evidence: Option<String>,
}

/// Classification snapshot pulled out of `config/hosts.yaml`.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ClassificationSnapshot {
    pub host: String,
    pub class: HostClass,
    pub employer: Option<String>,
    pub primary_identity: Option<String>,
    pub retention: Option<String>,
}

impl ClassificationSnapshot {
    fn from(host: &str, entry: &HostEntry) -> Self {
        Self {
            host: host.to_string(),
            class: entry.class,
            employer: entry.employer.clone(),
            primary_identity: entry.primary_identity.clone(),
            retention: entry.retention.clone(),
        }
    }
}

/// One prepared record (surviving dedup) staged for write.
#[derive(Debug, Clone, Serialize)]
pub struct PreparedRecord {
    pub rollup: RollupRecord,
    pub metadata: MetadataPayload,
    pub archive_target: ArchiveTarget,
    /// Files that would be written for this record (rollup, metadata, scrub
    /// log if any, child evidence).
    pub files: Vec<String>,
    pub scrub: ScrubSummary,
    /// Source record path (agent-out absolute).
    pub source_path: String,
    /// Warnings about this record (e.g. synthesized producer).
    pub warnings: Vec<String>,
    // --- internals not serialized into the report payload ---
    #[serde(skip)]
    scrub_matches: Vec<nils_scrub::Match>,
    #[serde(skip)]
    child_files: Vec<StagedChild>,
}

#[derive(Debug, Clone)]
struct StagedChild {
    /// Archive-relative path under the record dir.
    rel: PathBuf,
    /// Bytes to write (scrubbed for UTF-8 children; verbatim for binary).
    bytes: Vec<u8>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScrubSummary {
    pub patterns_triggered: Vec<String>,
    pub total_matches: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct ArchiveTarget {
    pub id: String,
    pub absolute_path: String,
    pub relative_path: String,
    /// Whether the target dir already exists (would refuse on `--apply`).
    pub exists: bool,
}

/// A record skipped because its digest is already archived.
#[derive(Debug, Clone, Serialize)]
pub struct AlreadyArchived {
    pub source_path: String,
    pub source_digest: String,
}

/// A record skipped (not fatal) and reported with a reason: an unresolvable
/// repo identity (multi-host config + slug-only dir + ephemeral/removed `cwd`),
/// a resolved host that is absent from `config/hosts.yaml` (not classified),
/// a read/parse failure, a rollup-prep failure, or an unsupported source-record
/// `schema` (e.g. a future `skill-usage.record.v2`, never normalized to v1).
/// The batch continues; the operator sees what was skipped and why, and can
/// re-run with `--host` to vouch for records they recognize, or add the host to
/// `config/hosts.yaml` to classify it.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct BlockedRecord {
    pub record_path: String,
    pub reason: String,
}

/// Result of `migrate` dry-run (also the prelude of an apply).
#[derive(Debug, Clone, Serialize)]
pub struct DryRunReport {
    pub source_out: String,
    pub archive: String,
    pub records: Vec<PreparedRecord>,
    pub already_archived: Vec<AlreadyArchived>,
    /// Records skipped and reported with a reason (unresolvable identity,
    /// read/parse failure, rollup-prep failure, or unsupported schema). Never
    /// fatal — an all-blocked run is a successful no-op.
    pub blocked: Vec<BlockedRecord>,
    pub scanned: usize,
    pub eligible: usize,
    pub skipped: usize,
}

/// Result of a successful `migrate --apply`.
#[derive(Debug, Clone, Serialize)]
pub struct ApplyReport {
    pub archive_commit: String,
    pub archived: usize,
    pub skipped: usize,
    /// Records skipped because their repo identity was unresolvable (same as
    /// the dry-run `blocked` list; only the resolved records are written).
    pub blocked: Vec<BlockedRecord>,
    pub targets: Vec<String>,
    pub rollup_paths: Vec<String>,
    pub scrub_log_paths: Vec<String>,
    pub warnings: Vec<String>,
}

/// Errors produced by the migration pipeline.
#[derive(Debug, thiserror::Error)]
pub enum MigrateError {
    #[error("agent-out projects root not found at `{0}`")]
    SourceOutMissing(PathBuf),
    #[error(
        "archive clone path not found at `{0}` (set `--archive` or seed `archive_clone_path` in the local config)"
    )]
    ArchiveCloneMissing(PathBuf),
    #[error("failed to load archive `config/hosts.yaml`: {0}")]
    HostsLoadFailed(String),
    #[error("failed to parse archive `config/hosts.yaml`: {0}")]
    HostsParseFailed(String),
    #[error("archive target `{0}` already exists; resolve the conflict and re-run")]
    ArchiveTargetExists(String),
    #[error("archive clone has uncommitted changes under `evidence/` or `catalog.json`")]
    ArchiveRepoDirty,
    #[error("failed to format timestamp: {0}")]
    Timestamp(String),
    #[error("io error during migration: {0}")]
    Io(String),
    #[error("subprocess `{0}` failed: {1}")]
    Subprocess(String, String),
}

impl MigrateError {
    pub fn code(&self) -> &'static str {
        match self {
            MigrateError::SourceOutMissing(_) => "migrate-source-out-missing",
            MigrateError::ArchiveCloneMissing(_) => "migrate-archive-clone-missing",
            MigrateError::HostsLoadFailed(_) => "migrate-hosts-load-failed",
            MigrateError::HostsParseFailed(_) => "migrate-hosts-parse-failed",
            MigrateError::ArchiveTargetExists(_) => "migrate-archive-target-exists",
            MigrateError::ArchiveRepoDirty => "migrate-archive-repo-dirty",
            MigrateError::Timestamp(_) => "migrate-timestamp-failed",
            MigrateError::Io(_) => "migrate-io-error",
            MigrateError::Subprocess(_, _) => "migrate-subprocess-failed",
        }
    }
}

impl From<SourceError> for MigrateError {
    fn from(err: SourceError) -> Self {
        match err {
            SourceError::SourceOutMissing(p) => MigrateError::SourceOutMissing(p),
            SourceError::ArchiveCloneMissing(p) => MigrateError::ArchiveCloneMissing(p),
            SourceError::HostsLoadFailed(s) => MigrateError::HostsLoadFailed(s),
            SourceError::HostsParseFailed(s) => MigrateError::HostsParseFailed(s),
            SourceError::Io(s) => MigrateError::Io(s),
        }
    }
}

/// Entry point called from `cli::run`.
pub fn dispatch(args: DispatchArgs) -> i32 {
    let format = args.format;
    match prepare(&args) {
        Ok(report) => {
            if args.apply {
                match apply(&args, report) {
                    Ok(applied) => emit_apply(format, &applied),
                    Err(err) => emit_error(format, err.code(), &err.to_string()),
                }
            } else {
                emit_dry_run(format, &report)
            }
        }
        Err(err) => emit_error(format, err.code(), &err.to_string()),
    }
}

/// Read-only preparation. Enumerates records, applies filters, dedups against
/// the catalog, rolls each surviving record up, scrubs it, and assembles the
/// dry-run report. Performs no writes.
pub fn prepare(args: &DispatchArgs) -> Result<DryRunReport, MigrateError> {
    let source_out = source::resolve_source_out(args.source_out.as_deref())?;
    let archive = source::resolve_archive(args.archive.as_deref())?;
    let hosts_path = source::hosts_path_for(&archive, args.hosts.as_deref());
    let hosts = source::load_hosts(&hosts_path)?;

    let archived_at = now_rfc3339()?;
    let existing_digests = crate::catalog::existing_source_digests(&archive)
        .map_err(|e| MigrateError::Io(e.to_string()))?;

    let record_paths = enumerate_records(&source_out);
    let scanned = record_paths.len();

    let mut prepared = Vec::new();
    let mut already = Vec::new();
    let mut blocked = Vec::new();

    for record_path in record_paths {
        // A per-record read or parse failure (e.g. a truncated or malformed
        // skill-usage.record.json) must not abort the batch: record it as
        // blocked, skip it, and continue.
        let raw = match fs::read(&record_path) {
            Ok(bytes) => bytes,
            Err(e) => {
                blocked.push(BlockedRecord {
                    record_path: record_path.display().to_string(),
                    reason: format!("read failed: {e}"),
                });
                continue;
            }
        };
        // Schema gate FIRST: peek the `schema` discriminator and reject an
        // unsupported source schema before attempting the v1 deserialization,
        // so a future `skill-usage.record.v2` cannot silently corrupt the
        // archive and is reported as unsupported (not as a v1 parse failure).
        match SkillUsageRecord::peek_schema(&raw) {
            Ok(schema) if schema == SUPPORTED_RECORD_SCHEMA => {}
            Ok(schema) => {
                blocked.push(BlockedRecord {
                    record_path: record_path.display().to_string(),
                    reason: format!(
                        "unsupported source schema `{schema}` (expected `{SUPPORTED_RECORD_SCHEMA}`); skipped"
                    ),
                });
                continue;
            }
            Err(e) => {
                blocked.push(BlockedRecord {
                    record_path: record_path.display().to_string(),
                    reason: format!("parse failed: {e}"),
                });
                continue;
            }
        }

        let record = match SkillUsageRecord::from_json_bytes(&raw) {
            Ok(r) => r,
            Err(e) => {
                blocked.push(BlockedRecord {
                    record_path: record_path.display().to_string(),
                    reason: format!("parse failed: {e}"),
                });
                continue;
            }
        };

        let source_digest = format!("sha256:{}", sha256_hex(&raw));

        // Filters.
        if let Some(skill) = &args.skill
            && !record.skill.contains(skill)
        {
            continue;
        }
        let project_dir = project_dir_name(&source_out, &record_path);
        if let Some(repo_filter) = &args.repo
            && !project_matches_repo(&project_dir, repo_filter)
        {
            continue;
        }
        let day = iso_date_part(&record.started_at);
        if let Some(since) = &args.since
            && day.as_str() < since.as_str()
        {
            continue;
        }
        if let Some(until) = &args.until
            && day.as_str() > until.as_str()
        {
            continue;
        }
        let promotion = promotion_from_linked(&record.linked_records);
        if args.promotion_only && promotion.is_none() {
            continue;
        }

        // Dedup against the catalog.
        if existing_digests.contains(&source_digest) {
            already.push(AlreadyArchived {
                source_path: record_path.display().to_string(),
                source_digest,
            });
            continue;
        }

        // Resolve the repo identity FIRST. A per-record identity failure must
        // not abort the whole batch: record it as blocked, skip it (no rollup,
        // no staged files), and continue. The operator can re-run with `--host`
        // to vouch for records they recognize.
        let repo_identity =
            match derive_repo_identity(&project_dir, &hosts, &record.cwd, args.host.as_deref()) {
                Ok(id) => id,
                Err(e) => {
                    // Last-resort rescue for an UNRESOLVABLE identity (the record's
                    // recorded cwd is gone, e.g. a removed agent worktree): if a
                    // configured working_repo_root holds a matching local checkout,
                    // recover the host from its `origin`. Other identity errors
                    // (operator typos in `--host`, etc.) are not rescued.
                    let rescued = matches!(e, IdentityError::Unresolvable(_, _))
                        .then(|| {
                            rescue_identity_via_working_roots(
                                &project_dir,
                                &args.working_repo_roots,
                            )
                        })
                        .flatten();
                    match rescued {
                        Some(id) => id,
                        None => {
                            blocked.push(BlockedRecord {
                                record_path: record_path.display().to_string(),
                                reason: e.to_string(),
                            });
                            continue;
                        }
                    }
                }
            };

        // A resolved host that is ABSENT from `config/hosts.yaml` must NOT be
        // archived: the archive only holds records for hosts the operator has
        // explicitly classified (personal/employer). Block-and-report instead of
        // silently recording it as "unknown personal" — that would leak and
        // mis-classify (e.g. employer evidence stored as personal, escaping the
        // employer `delete-on-termination` retention class). To archive such a
        // record, the operator first opts the host in by adding it to hosts.yaml.
        if !hosts.hosts.contains_key(&repo_identity.host) {
            blocked.push(BlockedRecord {
                record_path: record_path.display().to_string(),
                reason: format!(
                    "host `{}` is not classified in config/hosts.yaml; add it (personal or employer) to archive this record",
                    repo_identity.host
                ),
            });
            continue;
        }

        // A per-record rollup/scrub/staging failure must not abort the batch
        // either: record it as blocked and continue.
        let prepared_record = match build_prepared(
            &archive,
            &source_out,
            &record_path,
            &record,
            &hosts,
            &archived_at,
            source_digest,
            promotion,
            repo_identity,
        ) {
            Ok(p) => p,
            Err(e) => {
                blocked.push(BlockedRecord {
                    record_path: record_path.display().to_string(),
                    reason: format!("rollup preparation failed: {e}"),
                });
                continue;
            }
        };
        prepared.push(prepared_record);
    }

    let eligible = prepared.len();
    let skipped = already.len();
    Ok(DryRunReport {
        source_out: source_out.display().to_string(),
        archive: archive.display().to_string(),
        records: prepared,
        already_archived: already,
        blocked,
        scanned,
        eligible,
        skipped,
    })
}

/// Maximum directory depth searched under each `working_repo_roots` entry for a
/// matching checkout. Covers flat `<root>/<owner>/<repo>` layouts (depth 2) and
/// nested provider groups such as `<root>/acme/platform/backend/svc` (depth 4),
/// while bounding the filesystem walk for this last-resort rescue.
const MAX_RESCUE_WALK_DEPTH: usize = 6;

/// Last-resort identity rescue: when `derive_repo_identity` returns
/// `Unresolvable` (the record's agent-out `<owner__repo>` slug is ambiguous
/// under a multi-host config and its recorded `cwd` no longer exists — a removed
/// agent worktree is the common case), find a matching local checkout under a
/// configured `working_repo_roots` entry and recover the host from its `origin`
/// remote.
///
/// A checkout matches when its `origin` `(org, repo)`, normalized with the SAME
/// rule that produced the agent-out slug (`agent_out::project_slug_from_owner_repo`,
/// which keeps only the last owner segment), equals the record's slug. This lets
/// a nested provider group — full origin org such as `acme/platform/backend`,
/// agent-out slug `backend__svc` — still match, and the FULL origin org/repo is
/// preserved in the recovered identity (the slug's truncated owner is not used).
fn rescue_identity_via_working_roots(
    project_dir_name: &str,
    working_repo_roots: &[PathBuf],
) -> Option<RepoIdentity> {
    // Only `<owner__repo>` slugs are rescuable (the record carries no host).
    identity::split_owner_repo(project_dir_name)?;
    for root in working_repo_roots {
        if let Some(found) = find_checkout_for_slug(root, project_dir_name, 0) {
            return Some(found);
        }
    }
    None
}

/// Walk `dir` (bounded by [`MAX_RESCUE_WALK_DEPTH`]) for a git checkout whose
/// `origin` normalizes to the agent-out slug `target_slug`, returning its full
/// origin identity. A directory holding `.git` is treated as a checkout and not
/// descended into.
fn find_checkout_for_slug(dir: &Path, target_slug: &str, depth: usize) -> Option<RepoIdentity> {
    if depth > MAX_RESCUE_WALK_DEPTH {
        return None;
    }
    if dir.join(".git").exists() {
        let found = identity::identity_from_cwd(&dir.to_string_lossy())?;
        let normalized = nils_common::slug::project_slug_from_owner_repo(&format!(
            "{}/{}",
            found.org, found.repo
        ));
        return (normalized.as_deref() == Some(target_slug)).then_some(found);
    }
    let entries = fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir()
            && let Some(found) = find_checkout_for_slug(&path, target_slug, depth + 1)
        {
            return Some(found);
        }
    }
    None
}

#[allow(clippy::too_many_arguments)]
fn build_prepared(
    archive: &Path,
    source_out: &Path,
    record_path: &Path,
    record: &SkillUsageRecord,
    hosts: &crate::validate::hosts::HostsConfig,
    archived_at: &str,
    source_digest: String,
    promotion: Option<Promotion>,
    repo_identity: RepoIdentity,
) -> Result<PreparedRecord, MigrateError> {
    let mut warnings = Vec::new();

    // The caller (`prepare`) blocks any record whose host is absent from
    // hosts.yaml before reaching here, so an unclassified host is an internal
    // invariant violation rather than an "unknown personal" fallback.
    let host_entry = hosts.hosts.get(&repo_identity.host).ok_or_else(|| {
        MigrateError::Io(format!(
            "internal invariant violated: host `{}` reached build_prepared without a config/hosts.yaml classification",
            repo_identity.host
        ))
    })?;
    let classification = ClassificationSnapshot::from(&repo_identity.host, host_entry);

    // Producer: read or synthesize-with-warning.
    let producer = match &record.producer {
        Some(p) => RollupProducer {
            tool: p.tool.clone(),
            nils_cli_version: Some(p.nils_cli_version.clone()),
        },
        None => {
            warnings.push(
                "record has no `producer` block (pre-v1.4.0); synthesizing tool with absent version"
                    .to_string(),
            );
            RollupProducer {
                tool: FALLBACK_PRODUCER_TOOL.to_string(),
                nils_cli_version: None,
            }
        }
    };

    // Linked evidence: copy each child into a typed subdir, scrubbing its
    // bytes. Sort by (type, path) for determinism.
    let mut linked: Vec<LinkedEvidence> = Vec::new();
    let mut child_files: Vec<StagedChild> = Vec::new();
    let mut all_matches: Vec<nils_scrub::Match> = Vec::new();
    // Track relative child paths already staged for this record so two links
    // that would map to the same `rel` never silently overwrite each other
    // (F5). The first claimant keeps the bare path; later collisions get a
    // stable index suffix before the extension.
    let mut used_rels: BTreeSet<PathBuf> = BTreeSet::new();

    let mut sorted_links = record.linked_records.clone();
    sorted_links.sort_by(|a, b| {
        (a.record_type.as_str(), a.path.as_str()).cmp(&(b.record_type.as_str(), b.path.as_str()))
    });
    for link in &sorted_links {
        let scrubbed_ref = nils_scrub::scrub_text(&link.path);
        all_matches.extend(scrubbed_ref.matches.clone());
        // F1+F3: derive BOTH the type subdir and the child basename from the
        // *scrubbed* reference, sanitized to a single safe in-target segment,
        // so a secret never lands in a committed path and a malicious
        // `record_type` (e.g. `../../etc`) cannot escape the archive target.
        // The `type` is scrubbed (not just sanitized) so a token embedded in it
        // never reaches the committed subdir name or the rollup `type` field.
        let scrubbed_type = scrub_collect(&link.record_type, &mut all_matches);
        let type_slug = sanitize_path_segment(&scrubbed_type, "evidence");
        let child_name = sanitize_path_segment(&child_basename(&scrubbed_ref.redacted), "evidence");
        let rel = unique_rel(&mut used_rels, &type_slug, &child_name);
        let rel_display = rel.to_string_lossy().replace('\\', "/");
        // F1: the staged path must stay a descendant of the archive target.
        // `rel` is built from sanitized single segments, so this is a
        // defense-in-depth assertion, not the primary guard.
        if !rel_is_descendant(&rel) {
            return Err(MigrateError::Io(format!(
                "refusing to stage linked evidence outside the archive target: `{rel_display}`"
            )));
        }
        // F2: try to read the child file ONLY from the record's run-dir
        // subtree; an absolute path, a `..` escape, or a URL records the
        // reference but stages no bytes.
        let child_abs = resolve_child_source(record_path, &link.path);
        if let Some(bytes) = child_abs.and_then(|p| fs::read(&p).ok()) {
            // Binary children must not be lossily mangled: only scrub valid
            // UTF-8, otherwise copy the bytes verbatim and warn.
            match std::str::from_utf8(&bytes) {
                Ok(text) => {
                    let scrubbed = nils_scrub::scrub_text(text);
                    all_matches.extend(scrubbed.matches.clone());
                    child_files.push(StagedChild {
                        rel: rel.clone(),
                        bytes: scrubbed.redacted.into_bytes(),
                    });
                }
                Err(_) => {
                    warnings.push(format!(
                        "linked child `{rel_display}` is not valid UTF-8; copied verbatim without scrubbing"
                    ));
                    child_files.push(StagedChild {
                        rel: rel.clone(),
                        bytes,
                    });
                }
            }
        }
        // F3: the rollup reference points at the exact `rel` that was (or
        // would be) written, so the three values always agree.
        linked.push(LinkedEvidence {
            evidence_type: scrubbed_type,
            path: rel_display,
        });
    }
    linked.sort_by(|a, b| {
        (a.evidence_type.as_str(), a.path.as_str())
            .cmp(&(b.evidence_type.as_str(), b.path.as_str()))
    });

    // Scrub the scalar text fields.
    let scrubbed_intent = scrub_collect(&record.intent, &mut all_matches);
    let scrubbed_trigger = scrub_collect(&record.trigger, &mut all_matches);
    let scrubbed_summary = scrub_collect(&record.outcome.summary, &mut all_matches);
    // Outcome status is a free string; scrub it for consistency so a secret
    // never lands in the rollup or the catalog `outcome_status` column.
    let scrubbed_status = scrub_collect(&record.outcome.status, &mut all_matches);
    let scrubbed_cwd = scrub_collect(&scrub_cwd(&record.cwd), &mut all_matches);
    // Home-relativize an absolute skill path FIRST so neither the rollup
    // `skill` field nor the derived `id`/slug (computed from `scrubbed_skill`
    // below) leaks a machine home (e.g. `/Users/<user>/…` → `users-<user>-…`).
    // Then scrub unconditionally: a bare id (no `/`) can still embed a token,
    // and scrubbing a clean id is a no-op.
    let scrubbed_skill = scrub_collect(&scrub_skill_path(&record.skill), &mut all_matches);
    // Scrub the promotion case link like every other archived text field so a
    // token in a heuristic-inbox URL/path never lands raw in the rollup,
    // metadata sidecar, or catalog.
    let promotion = promotion.map(|p| Promotion {
        heuristic_inbox_case: scrub_collect(&p.heuristic_inbox_case, &mut all_matches),
    });

    let id = rollup_id(&record.started_at, &scrubbed_skill, &source_digest);
    let archive_target_rel = archive_target_path(
        &repo_identity.host,
        &repo_identity.org,
        &repo_identity.repo,
        &id,
    );
    let archive_target_abs = archive.join(&archive_target_rel);

    let rollup = RollupRecord {
        schema: ROLLUP_SCHEMA.to_string(),
        id: id.clone(),
        archived_at: archived_at.to_string(),
        skill: scrubbed_skill,
        intent: scrubbed_intent,
        trigger: scrubbed_trigger,
        repo: RollupRepo {
            host: repo_identity.host.clone(),
            org: repo_identity.org.clone(),
            repo: repo_identity.repo.clone(),
        },
        cwd: scrubbed_cwd,
        started_at: record.started_at.clone(),
        ended_at: record.ended_at.clone(),
        outcome: RollupOutcome {
            status: scrubbed_status,
            summary: scrubbed_summary,
        },
        producer: producer.clone(),
        counts: RollupCounts {
            validation: record.validation.len(),
            failures: record.failures.len(),
        },
        linked_evidence: linked,
        source_digest: source_digest.clone(),
        promotion: promotion.clone(),
    };

    let metadata = MetadataPayload {
        metadata_version: METADATA_VERSION,
        source: MetadataSource {
            producer: producer.tool.clone(),
            nils_cli_version: producer.nils_cli_version.clone(),
            agent_out_path: agent_out_relative(source_out, record_path),
        },
        captured_classification: classification,
        archived_at: archived_at.to_string(),
        refs: MetadataRefs {
            session_evidence: None,
        },
        promotion: promotion.clone(),
    };

    // Build the file list for the report.
    let mut files = vec![
        format!("{}/skill-usage.rollup.json", archive_target_rel.display()),
        format!("{}/metadata.yaml", archive_target_rel.display()),
    ];
    if !all_matches.is_empty() {
        files.push(format!("{}/{}.scrub.log", archive_target_rel.display(), id));
    }
    for child in &child_files {
        files.push(format!(
            "{}/{}",
            archive_target_rel.display(),
            child.rel.display()
        ));
    }
    files.sort();

    let mut patterns: Vec<String> = all_matches.iter().map(|m| m.pattern_id.clone()).collect();
    patterns.sort();
    patterns.dedup();

    Ok(PreparedRecord {
        rollup,
        metadata,
        archive_target: ArchiveTarget {
            id,
            absolute_path: archive_target_abs.display().to_string(),
            relative_path: archive_target_rel.display().to_string(),
            exists: archive_target_abs.exists(),
        },
        files,
        scrub: ScrubSummary {
            patterns_triggered: patterns,
            total_matches: all_matches.len(),
        },
        source_path: record_path.display().to_string(),
        warnings,
        scrub_matches: all_matches,
        child_files,
    })
}

fn scrub_collect(input: &str, sink: &mut Vec<nils_scrub::Match>) -> String {
    let r = nils_scrub::scrub_text(input);
    sink.extend(r.matches.clone());
    r.redacted
}

/// Apply path. Refuses on any pre-existing target or a dirty archive, writes
/// every record, regenerates the catalog, commits the whole batch once, then
/// pushes. Idempotency is the catalog `source_digest` dedup in `prepare`.
pub fn apply(args: &DispatchArgs, report: DryRunReport) -> Result<ApplyReport, MigrateError> {
    let archive = source::resolve_archive(args.archive.as_deref())?;

    if report.records.is_empty() {
        // Nothing to do; surface a clean no-op apply. An all-blocked run lands
        // here too — it is a success that reports the skipped records.
        return Ok(ApplyReport {
            archive_commit: String::new(),
            archived: 0,
            skipped: report.skipped,
            blocked: report.blocked.clone(),
            targets: Vec::new(),
            rollup_paths: Vec::new(),
            scrub_log_paths: Vec::new(),
            warnings: report
                .records
                .iter()
                .flat_map(|r| r.warnings.clone())
                .collect(),
        });
    }

    // Refuse if any target already exists.
    for rec in &report.records {
        if rec.archive_target.exists {
            return Err(MigrateError::ArchiveTargetExists(
                rec.archive_target.relative_path.clone(),
            ));
        }
    }

    // Refuse if the archive is dirty under evidence/ or catalog.json.
    if source::has_dirty_path(&archive, Path::new("evidence"))?
        || source::has_dirty_path(&archive, Path::new("catalog.json"))?
    {
        return Err(MigrateError::ArchiveRepoDirty);
    }

    let mut targets = Vec::new();
    let mut rollup_paths = Vec::new();
    let mut scrub_log_paths = Vec::new();
    let mut stage_paths: BTreeSet<String> = BTreeSet::new();
    let mut warnings = Vec::new();

    for rec in &report.records {
        warnings.extend(rec.warnings.clone());
        let target_abs = PathBuf::from(&rec.archive_target.absolute_path);
        fs::create_dir_all(&target_abs).map_err(|e| MigrateError::Io(e.to_string()))?;

        // rollup.json
        let rollup_json = serde_json::to_string_pretty(&rec.rollup)
            .map_err(|e| MigrateError::Io(format!("rollup serialize: {e}")))?;
        let rollup_abs = target_abs.join("skill-usage.rollup.json");
        fs::write(&rollup_abs, format!("{rollup_json}\n"))
            .map_err(|e| MigrateError::Io(e.to_string()))?;
        rollup_paths.push(format!(
            "{}/skill-usage.rollup.json",
            rec.archive_target.relative_path
        ));

        // metadata.yaml
        let metadata_yaml = serde_yaml_ng::to_string(&rec.metadata)
            .map_err(|e| MigrateError::Io(format!("metadata serialize: {e}")))?;
        fs::write(target_abs.join("metadata.yaml"), metadata_yaml)
            .map_err(|e| MigrateError::Io(e.to_string()))?;

        // scrubbed child evidence
        for child in &rec.child_files {
            // F1 defense-in-depth: the staged `rel` was sanitized to safe
            // single segments at prepare time, but re-assert that the final
            // destination stays under the record's archive target before any
            // byte is written.
            if !rel_is_descendant(&child.rel) {
                return Err(MigrateError::Io(format!(
                    "refusing to write linked evidence outside the archive target: `{}`",
                    child.rel.display()
                )));
            }
            let dest = target_abs.join(&child.rel);
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent).map_err(|e| MigrateError::Io(e.to_string()))?;
            }
            fs::write(&dest, &child.bytes).map_err(|e| MigrateError::Io(e.to_string()))?;
        }

        // scrub.log (only when something fired)
        let scrub_log_abs = target_abs.join(format!("{}.scrub.log", rec.archive_target.id));
        let wrote = nils_scrub::write_log_if_any("evidence", &scrub_log_abs, &rec.scrub_matches)
            .map_err(|e| MigrateError::Io(e.to_string()))?;
        if wrote {
            scrub_log_paths.push(format!(
                "{}/{}.scrub.log",
                rec.archive_target.relative_path, rec.archive_target.id
            ));
        }

        targets.push(rec.archive_target.relative_path.clone());
        stage_paths.insert(rec.archive_target.relative_path.clone());
    }

    // Regenerate the catalog once after the batch.
    crate::catalog::write_catalog(&archive).map_err(|e| MigrateError::Io(e.to_string()))?;
    stage_paths.insert("catalog.json".to_string());

    // Stage + one batch commit.
    let mut stage_args: Vec<String> = vec!["add".to_string(), "--".to_string()];
    stage_args.extend(stage_paths.iter().cloned());
    let stage_ref: Vec<&str> = stage_args.iter().map(String::as_str).collect();
    let stage_out = nils_common::git::run_output_in(&archive, &stage_ref)
        .map_err(|e| MigrateError::Io(e.to_string()))?;
    if !stage_out.status.success() {
        return Err(MigrateError::Subprocess(
            "git add (archive)".to_string(),
            String::from_utf8_lossy(&stage_out.stderr).to_string(),
        ));
    }

    let commit_msg = batch_commit_message(&report.records);
    run_semantic_commit(&archive, &commit_msg)?;
    let archive_commit = head_sha(&archive)?;
    push_archive(&archive)?;

    Ok(ApplyReport {
        archive_commit,
        archived: report.records.len(),
        skipped: report.skipped,
        blocked: report.blocked.clone(),
        targets,
        rollup_paths,
        scrub_log_paths,
        warnings,
    })
}

// === helpers ===

fn now_rfc3339() -> Result<String, MigrateError> {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .map_err(|e| MigrateError::Timestamp(e.to_string()))
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for byte in digest {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

/// Enumerate `*/<ts>-skill-usage/skill-usage.record.json` files under the
/// agent-out projects root. Globs ONLY this filename. Returns a sorted list
/// for determinism.
fn enumerate_records(source_out: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(projects) = fs::read_dir(source_out) else {
        return out;
    };
    for project in projects.flatten() {
        let project_path = project.path();
        if !project_path.is_dir() {
            continue;
        }
        let Ok(runs) = fs::read_dir(&project_path) else {
            continue;
        };
        for run in runs.flatten() {
            let run_path = run.path();
            if !run_path.is_dir() {
                continue;
            }
            let candidate = run_path.join("skill-usage.record.json");
            if candidate.is_file() {
                out.push(candidate);
            }
        }
    }
    out.sort();
    out
}

/// The agent-out project dir name for a record (e.g.
/// `graysurf__agent-runtime-kit`).
fn project_dir_name(source_out: &Path, record_path: &Path) -> String {
    record_path
        .strip_prefix(source_out)
        .ok()
        .and_then(|rel| rel.components().next())
        .map(|c| c.as_os_str().to_string_lossy().to_string())
        .unwrap_or_default()
}

/// Agent-out-relative path of the record (for the metadata sidecar).
fn agent_out_relative(source_out: &Path, record_path: &Path) -> String {
    record_path
        .strip_prefix(source_out)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| record_path.display().to_string())
}

/// `--repo` matches either the full `<owner__repo>` slug or just the repo
/// half.
fn project_matches_repo(project_dir: &str, repo_filter: &str) -> bool {
    if project_dir == repo_filter {
        return true;
    }
    // Match the full `<owner>__<repo>` slug (handled by the `==` above) or the
    // bare repo half — never a substring, so `--repo kit` does not pull in
    // `acme__toolkit`. A non-slug dir matches only on its exact name.
    match project_dir.split_once("__") {
        Some((_, repo)) => repo == repo_filter,
        None => false,
    }
}

fn iso_date_part(iso: &str) -> String {
    iso.split('T').next().unwrap_or(iso).to_string()
}

/// Home-relativize an absolute `skill` path so neither the rollup `skill` field
/// nor the derived rollup `id`/slug leaks a machine home dir (e.g. an absolute
/// `/Users/<user>/…/SKILL.md` would otherwise slug to `users-<user>-…` in the
/// committed id and directory name).
///
/// Unlike [`scrub_cwd`], a `skill` is frequently a bare id (`issue-follow-up`)
/// or a relative render path (`build/codex/…/SKILL.md`) that carries no home
/// prefix; those pass through unchanged. Only an absolute path is normalized:
/// under `$HOME` it becomes `~/…`; otherwise it redacts (an absolute path
/// outside `$HOME` would leak another machine location).
fn scrub_skill_path(skill: &str) -> String {
    if is_absolute_path(skill) {
        scrub_cwd(skill)
    } else {
        skill.to_string()
    }
}

/// True when `s` is an absolute filesystem path (POSIX `/…`, UNC/`\…`, or a
/// Windows drive `C:\…` / `C:/…`). A bare skill id or a relative render path
/// returns false and is left untouched by [`scrub_skill_path`].
fn is_absolute_path(s: &str) -> bool {
    if s.starts_with('/') || s.starts_with('\\') {
        return true;
    }
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic())
        && chars.next() == Some(':')
        && matches!(chars.next(), Some('/') | Some('\\'))
}

/// $HOME-relative `cwd`, or `[REDACTED]` when it cannot be made relative.
///
/// F4: a cwd outside `$HOME` (or with no `$HOME` set) must never commit a raw
/// absolute machine path; it collapses to the redaction token. The scrub pass
/// still runs over the `~`-relative form so secrets embedded in a path under
/// `$HOME` are still caught.
fn scrub_cwd(cwd: &str) -> String {
    if cwd.is_empty() {
        return String::new();
    }
    if let Some(home) = std::env::var_os("HOME") {
        let home = home.to_string_lossy();
        // Trim a trailing separator so `/home/me/` and `/home/me` behave alike.
        let home = home.trim_end_matches(['/', '\\']);
        // Match on path-component boundaries, not raw byte prefix: with
        // HOME=/Users/alice, a sibling like /Users/alice-work/repo is NOT under
        // $HOME and must redact rather than leak its tail as `~-work/repo`.
        if !home.is_empty()
            && let Some(rest) = cwd.strip_prefix(home)
            && (rest.is_empty() || rest.starts_with('/') || rest.starts_with('\\'))
        {
            return format!("~{rest}");
        }
    }
    nils_scrub::REDACTION_TOKEN.to_string()
}

/// Sanitize an arbitrary string into a single safe path segment: keep only
/// `[A-Za-z0-9._-]`, map every other byte (including `/`, `\\`, and the path
/// separators inside a `..`) to `-`, then reject a segment that is empty or
/// resolves to `.`/`..`, falling back to `fallback`. The result can never
/// contain a path separator or a parent-dir traversal, so joining it under a
/// target dir cannot escape that target.
fn sanitize_path_segment(input: &str, fallback: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() || ch == '.' || ch == '_' || ch == '-' {
            out.push(ch);
        } else {
            out.push('-');
        }
    }
    let trimmed = out.trim_matches('-');
    if trimmed.is_empty() || trimmed == "." || trimmed == ".." {
        return fallback.to_string();
    }
    trimmed.to_string()
}

/// Build a `<type_slug>/<child_name>` relative path that is unique within
/// `used` (F5). On a collision, an index suffix is inserted before the final
/// extension (`name.json` -> `name-1.json`).
fn unique_rel(used: &mut BTreeSet<PathBuf>, type_slug: &str, child_name: &str) -> PathBuf {
    let base = PathBuf::from(type_slug).join(child_name);
    if used.insert(base.clone()) {
        return base;
    }
    let (stem, ext) = match child_name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => (stem.to_string(), Some(ext.to_string())),
        _ => (child_name.to_string(), None),
    };
    for n in 1.. {
        let candidate_name = match &ext {
            Some(ext) => format!("{stem}-{n}.{ext}"),
            None => format!("{stem}-{n}"),
        };
        let candidate = PathBuf::from(type_slug).join(candidate_name);
        if used.insert(candidate.clone()) {
            return candidate;
        }
    }
    unreachable!("an unbounded counter always finds a free name")
}

/// A staged child `rel` is a descendant of its record dir iff every component
/// is a plain name (no `..`, no absolute-root prefix). The sanitizer already
/// guarantees this; the check is a cheap, explicit backstop before any write.
fn rel_is_descendant(rel: &Path) -> bool {
    use std::path::Component;
    !rel.as_os_str().is_empty()
        && rel
            .components()
            .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
}

fn promotion_from_linked(links: &[crate::record::LinkedRecord]) -> Option<Promotion> {
    links
        .iter()
        .find(|l| l.record_type == "heuristic-inbox" || l.record_type == "heuristic-inbox-case")
        .map(|l| Promotion {
            heuristic_inbox_case: l.path.clone(),
        })
}

fn child_basename(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    trimmed
        .rsplit(['/', '\\'])
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or("evidence")
        .to_string()
}

/// Resolve a child evidence file referenced by a record, **confined to the
/// record's own run-dir subtree** (F2).
///
/// Only a relative reference that resolves (after `..`/`.` normalization) to a
/// file inside the run dir is honored. An absolute path, a URL, or any
/// reference that escapes the run dir returns `None`: the link is still
/// recorded in the rollup, but no bytes are copied — matching the URL-link
/// behavior — so a malicious `path` can never read an arbitrary machine file
/// into the archive.
fn resolve_child_source(record_path: &Path, child: &str) -> Option<PathBuf> {
    if child.starts_with("http://") || child.starts_with("https://") {
        return None;
    }
    let p = Path::new(child);
    if p.is_absolute() {
        return None;
    }
    let base = record_path.parent()?;
    let joined = base.join(child);
    // Canonicalize the run dir, then resolve the joined path against it and
    // confirm the result stays under the run dir. Prefer a real canonicalize
    // (collapses symlinks + `..`); fall back to a lexical normalization when
    // the target file does not exist.
    let base_canon = base.canonicalize().ok()?;
    match joined.canonicalize() {
        Ok(canon) => (canon.starts_with(&base_canon) && canon.is_file()).then_some(canon),
        Err(_) => {
            let normalized = lexical_normalize(&joined)?;
            (normalized.starts_with(base) && normalized.is_file()).then_some(normalized)
        }
    }
}

/// Lexically normalize a path (resolve `.`/`..` without touching the
/// filesystem). Returns `None` if a `..` would escape above the root of the
/// accumulated path (i.e. the reference tries to climb out of its base).
fn lexical_normalize(path: &Path) -> Option<PathBuf> {
    use std::path::Component;
    let mut out: Vec<Component> = Vec::new();
    for comp in path.components() {
        match comp {
            Component::ParentDir => match out.last() {
                Some(Component::Normal(_)) => {
                    out.pop();
                }
                // A `..` that cannot be popped (at/above the prefix or root)
                // means the path escapes its base.
                _ => return None,
            },
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    Some(out.iter().collect())
}

/// Build the one-line batch commit header. `<=70` chars (kit policy floor;
/// semantic-commit's hard limit is 100).
fn batch_commit_message(records: &[PreparedRecord]) -> String {
    if records.len() == 1 {
        let r = &records[0];
        let header = format!(
            "archive(evidence): {}-{}",
            r.rollup.skill, r.rollup.outcome.status
        );
        return truncate_header(&header, 70);
    }
    // Summarize by distinct repo or skill.
    let repos: BTreeSet<String> = records.iter().map(|r| r.rollup.repo.repo.clone()).collect();
    let summary = if repos.len() == 1 {
        repos.iter().next().cloned().unwrap_or_default()
    } else {
        format!("{} repos", repos.len())
    };
    let header = format!("archive(evidence): {} runs ({summary})", records.len());
    truncate_header(&header, 70)
}

fn truncate_header(header: &str, max: usize) -> String {
    if header.chars().count() <= max {
        return header.to_string();
    }
    header.chars().take(max).collect()
}

pub(crate) fn run_semantic_commit(repo: &Path, message: &str) -> Result<(), MigrateError> {
    let mut child = ProcCommand::new("semantic-commit")
        .arg("commit")
        .arg("-m")
        .arg(message)
        .arg("--quiet")
        .arg("--no-summary")
        .arg("--repo")
        .arg(repo)
        .spawn()
        .map_err(|e| MigrateError::Subprocess("semantic-commit".to_string(), e.to_string()))?;
    let status = child
        .wait()
        .map_err(|e| MigrateError::Subprocess("semantic-commit".to_string(), e.to_string()))?;
    if !status.success() {
        return Err(MigrateError::Subprocess(
            "semantic-commit".to_string(),
            format!("exit code {:?}", status.code()),
        ));
    }
    Ok(())
}

pub(crate) fn head_sha(repo: &Path) -> Result<String, MigrateError> {
    let out = nils_common::git::run_output_in(repo, &["rev-parse", "HEAD"])
        .map_err(|e| MigrateError::Io(e.to_string()))?;
    if !out.status.success() {
        return Err(MigrateError::Subprocess(
            "git rev-parse".to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

pub(crate) fn push_archive(repo: &Path) -> Result<(), MigrateError> {
    let out = nils_common::git::run_output_in(repo, &["push"])
        .map_err(|e| MigrateError::Io(e.to_string()))?;
    if !out.status.success() {
        return Err(MigrateError::Subprocess(
            "git push (archive)".to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
        ));
    }
    Ok(())
}

fn emit_dry_run(format: OutputFormat, report: &DryRunReport) -> i32 {
    match format {
        OutputFormat::Json => emit_json(report),
        OutputFormat::Text => {
            println!("evidence migrate (dry-run)");
            println!("  source out        : {}", report.source_out);
            println!("  archive           : {}", report.archive);
            println!(
                "  scanned/eligible/skipped/blocked : {}/{}/{}/{}",
                report.scanned,
                report.eligible,
                report.skipped,
                report.blocked.len()
            );
            for rec in &report.records {
                println!(
                    "  - {} [{}/{}/{}]",
                    rec.archive_target.id,
                    rec.rollup.repo.host,
                    rec.rollup.repo.org,
                    rec.rollup.repo.repo
                );
                println!("      skill   : {}", rec.rollup.skill);
                println!("      outcome : {}", rec.rollup.outcome.status);
                println!(
                    "      scrub   : {} match(es) [{}]",
                    rec.scrub.total_matches,
                    rec.scrub.patterns_triggered.join(",")
                );
                if rec.archive_target.exists {
                    println!("      target  : EXISTS (would refuse on --apply)");
                }
                for w in &rec.warnings {
                    eprintln!("      warning : {w}");
                }
            }
            if !report.already_archived.is_empty() {
                println!("  already archived (skipped):");
                for a in &report.already_archived {
                    println!("    - {} ({})", a.source_path, a.source_digest);
                }
            }
            if !report.blocked.is_empty() {
                println!(
                    "  blocked (skipped + reported; unresolvable identity, bad record, or unsupported schema):"
                );
                for b in &report.blocked {
                    println!("    - {} ({})", b.record_path, b.reason);
                }
            }
            println!("  (no files modified; pass --apply to commit)");
            exit::SUCCESS
        }
    }
}

fn emit_apply(format: OutputFormat, report: &ApplyReport) -> i32 {
    match format {
        OutputFormat::Json => emit_json(report),
        OutputFormat::Text => {
            println!("evidence migrate (applied)");
            println!("  archived : {}", report.archived);
            println!("  skipped  : {}", report.skipped);
            println!("  blocked  : {}", report.blocked.len());
            if !report.archive_commit.is_empty() {
                println!("  commit   : {}", report.archive_commit);
            }
            for t in &report.targets {
                println!("    - {t}");
            }
            for b in &report.blocked {
                eprintln!("  blocked : {} ({})", b.record_path, b.reason);
            }
            for w in &report.warnings {
                eprintln!("  warning : {w}");
            }
            exit::SUCCESS
        }
    }
}

fn emit_json<T: Serialize>(data: &T) -> i32 {
    let envelope = Envelope::success(schema_version_for(BINARY, COMMAND, 1), data);
    match serde_json::to_string(&envelope) {
        Ok(s) => {
            let stdout = std::io::stdout();
            let mut handle = stdout.lock();
            if writeln!(handle, "{s}").is_err() {
                return exit::SOFTWARE;
            }
            exit::SUCCESS
        }
        Err(_) => exit::SOFTWARE,
    }
}

fn emit_error(format: OutputFormat, code: &str, message: &str) -> i32 {
    match format {
        OutputFormat::Json => {
            let envelope: Envelope<()> = Envelope::failure(
                schema_version_for(BINARY, COMMAND, 1),
                EnvelopeError::new(code, message),
            );
            if let Ok(s) = serde_json::to_string(&envelope) {
                eprintln!("{s}");
            }
            exit::DATA
        }
        OutputFormat::Text => {
            eprintln!("error [{code}]: {message}");
            exit::DATA
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(skill: &str, status: &str, repo: &str) -> PreparedRecord {
        PreparedRecord {
            rollup: RollupRecord {
                schema: ROLLUP_SCHEMA.to_string(),
                id: "id".into(),
                archived_at: "2026-06-14T10:00:00Z".into(),
                skill: skill.into(),
                intent: String::new(),
                trigger: String::new(),
                repo: RollupRepo {
                    host: "github.com".into(),
                    org: "o".into(),
                    repo: repo.into(),
                },
                cwd: String::new(),
                started_at: "2026-06-14T10:00:00Z".into(),
                ended_at: None,
                outcome: RollupOutcome {
                    status: status.into(),
                    summary: String::new(),
                },
                producer: RollupProducer {
                    tool: "skill-usage".into(),
                    nils_cli_version: Some("1.4.0".into()),
                },
                counts: RollupCounts {
                    validation: 0,
                    failures: 0,
                },
                linked_evidence: Vec::new(),
                source_digest: "sha256:abc".into(),
                promotion: None,
            },
            metadata: MetadataPayload {
                metadata_version: METADATA_VERSION,
                source: MetadataSource {
                    producer: "skill-usage".into(),
                    nils_cli_version: Some("1.4.0".into()),
                    agent_out_path: "p".into(),
                },
                captured_classification: ClassificationSnapshot {
                    host: "github.com".into(),
                    class: HostClass::Personal,
                    employer: None,
                    primary_identity: None,
                    retention: None,
                },
                archived_at: "2026-06-14T10:00:00Z".into(),
                refs: MetadataRefs::default(),
                promotion: None,
            },
            archive_target: ArchiveTarget {
                id: "id".into(),
                absolute_path: "/a".into(),
                relative_path: "evidence/x".into(),
                exists: false,
            },
            files: Vec::new(),
            scrub: ScrubSummary {
                patterns_triggered: Vec::new(),
                total_matches: 0,
            },
            source_path: "s".into(),
            warnings: Vec::new(),
            scrub_matches: Vec::new(),
            child_files: Vec::new(),
        }
    }

    #[test]
    fn single_record_commit_message() {
        let msg = batch_commit_message(&[rec("deliver-pr", "pass", "kit")]);
        assert_eq!(msg, "archive(evidence): deliver-pr-pass");
        assert!(msg.chars().count() <= 70);
    }

    #[test]
    fn multi_record_commit_message_single_repo() {
        let msg = batch_commit_message(&[
            rec("a", "pass", "kit"),
            rec("b", "pass", "kit"),
            rec("c", "fail", "kit"),
        ]);
        assert_eq!(msg, "archive(evidence): 3 runs (kit)");
        assert!(msg.chars().count() <= 70);
    }

    #[test]
    fn multi_record_commit_message_multi_repo() {
        let msg = batch_commit_message(&[rec("a", "pass", "kit"), rec("b", "pass", "cli")]);
        assert_eq!(msg, "archive(evidence): 2 runs (2 repos)");
    }

    #[test]
    fn commit_header_truncated_to_70() {
        let long_skill = "x".repeat(120);
        let msg = batch_commit_message(&[rec(&long_skill, "pass", "kit")]);
        assert!(msg.chars().count() <= 70, "len {}", msg.chars().count());
    }

    #[test]
    fn project_matches_repo_variants() {
        assert!(project_matches_repo(
            "graysurf__agent-runtime-kit",
            "agent-runtime-kit"
        ));
        assert!(project_matches_repo(
            "graysurf__agent-runtime-kit",
            "graysurf__agent-runtime-kit"
        ));
        assert!(!project_matches_repo(
            "graysurf__agent-runtime-kit",
            "nils-cli"
        ));
        // A substring of the repo half must NOT match: `--repo kit` must not
        // pull in `acme__toolkit`. The help promises the full slug or repo name.
        assert!(!project_matches_repo("acme__toolkit", "kit"));
        assert!(!project_matches_repo("acme__toolkit", "tool"));
        // A non-slug project dir matches only on its exact name.
        assert!(project_matches_repo("standalone", "standalone"));
        assert!(!project_matches_repo("standalone", "stand"));
    }

    #[test]
    fn sha256_hex_known_vector() {
        // sha256("") = e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn child_basename_extracts_leaf() {
        assert_eq!(child_basename("a/b/c.json"), "c.json");
        assert_eq!(child_basename("solo.json"), "solo.json");
        assert_eq!(child_basename("dir/"), "dir");
    }

    #[test]
    fn promotion_detected_from_heuristic_inbox_link() {
        let links = vec![crate::record::LinkedRecord {
            record_type: "heuristic-inbox".into(),
            path: "https://example/case/1".into(),
        }];
        let p = promotion_from_linked(&links).expect("promotion");
        assert_eq!(p.heuristic_inbox_case, "https://example/case/1");
    }

    #[test]
    fn sanitize_path_segment_strips_traversal_and_separators() {
        // F1: a malicious record_type cannot become a path escape. The path
        // separators in `../../../etc` collapse to `-`, leaving a single safe
        // segment (no `/`, no standalone `..` component).
        assert_eq!(
            sanitize_path_segment("../../../etc", "evidence"),
            "..-..-..-etc"
        );
        assert_eq!(sanitize_path_segment("/abs/path", "evidence"), "abs-path");
        // A bare `..` or `.` collapses to the fallback (never a real parent dir).
        assert_eq!(sanitize_path_segment("..", "evidence"), "evidence");
        assert_eq!(sanitize_path_segment(".", "evidence"), "evidence");
        assert_eq!(sanitize_path_segment("a/b\\c", "evidence"), "a-b-c");
        assert_eq!(sanitize_path_segment("", "evidence"), "evidence");
        // A scrubbed basename like `[REDACTED]` becomes a safe segment.
        assert_eq!(sanitize_path_segment("[REDACTED]", "evidence"), "REDACTED");
        // Plain names survive intact.
        assert_eq!(
            sanitize_path_segment("review-evidence", "evidence"),
            "review-evidence"
        );
        // The key invariant: a sanitized segment is always a single, in-target
        // path component — never a separator and never a `..`/`.` component.
        for input in ["../x", "a/b", "..\\..", "....//", "/", "..", "."] {
            let seg = sanitize_path_segment(input, "evidence");
            assert!(!seg.contains('/') && !seg.contains('\\'), "seg `{seg}`");
            let rel = PathBuf::from("type").join(&seg);
            assert!(rel_is_descendant(&rel), "escapes target: `{seg}`");
        }
    }

    #[test]
    fn unique_rel_disambiguates_collisions() {
        // F5: two same-type children with the same leaf name get distinct rels.
        let mut used = BTreeSet::new();
        let a = unique_rel(&mut used, "review-evidence", "summary.json");
        let b = unique_rel(&mut used, "review-evidence", "summary.json");
        let c = unique_rel(&mut used, "review-evidence", "summary.json");
        assert_eq!(a, PathBuf::from("review-evidence/summary.json"));
        assert_eq!(b, PathBuf::from("review-evidence/summary-1.json"));
        assert_eq!(c, PathBuf::from("review-evidence/summary-2.json"));
        assert_ne!(a, b);
        assert_ne!(b, c);
        // Extension-less leaf still disambiguates.
        let d = unique_rel(&mut used, "t", "log");
        let e = unique_rel(&mut used, "t", "log");
        assert_eq!(d, PathBuf::from("t/log"));
        assert_eq!(e, PathBuf::from("t/log-1"));
    }

    #[test]
    fn rel_is_descendant_rejects_escapes() {
        assert!(rel_is_descendant(Path::new("review-evidence/summary.json")));
        assert!(!rel_is_descendant(Path::new("../escape")));
        assert!(!rel_is_descendant(Path::new("/abs")));
        assert!(!rel_is_descendant(Path::new("a/../../b")));
        assert!(!rel_is_descendant(Path::new("")));
    }

    #[test]
    fn scrub_cwd_redacts_paths_outside_home() {
        // F4: a cwd that does not start with $HOME must never commit a raw
        // absolute machine path.
        let token = nils_scrub::REDACTION_TOKEN;
        temp_env_home("/Users/someone", || {
            assert_eq!(scrub_cwd("/var/secret/path"), token);
            assert_eq!(scrub_cwd("/Users/someone/Project/x"), "~/Project/x");
            // The home dir itself collapses to bare `~`.
            assert_eq!(scrub_cwd("/Users/someone"), "~");
            // A sibling that merely shares the $HOME byte-prefix is NOT under
            // $HOME and must redact, not leak its tail as `~-work/...`.
            assert_eq!(scrub_cwd("/Users/someone-work/repo"), token);
            assert_eq!(scrub_cwd("/Users/someoneelse"), token);
        });
        assert_eq!(scrub_cwd(""), "");
    }

    #[test]
    fn scrub_skill_path_relativizes_home_and_preserves_relative_ids() {
        let token = nils_scrub::REDACTION_TOKEN;
        temp_env_home("/Users/someone", || {
            // Absolute path under $HOME → ~-relative, so neither the skill field
            // nor the derived id/slug carries a machine home.
            assert_eq!(
                scrub_skill_path("/Users/someone/Project/kit/build/codex/x/SKILL.md"),
                "~/Project/kit/build/codex/x/SKILL.md"
            );
            // Absolute path outside $HOME → redacted, never a raw machine path.
            assert_eq!(scrub_skill_path("/var/lib/x/SKILL.md"), token);
            assert_eq!(scrub_skill_path("/Users/other/x/SKILL.md"), token);
        });
        // A bare skill id or a relative render path carries no home prefix and
        // is left untouched, independent of $HOME.
        assert_eq!(scrub_skill_path("issue-follow-up"), "issue-follow-up");
        assert_eq!(
            scrub_skill_path("build/codex/plugins/pr/skills/deliver-pr/SKILL.md"),
            "build/codex/plugins/pr/skills/deliver-pr/SKILL.md"
        );
        assert_eq!(scrub_skill_path(""), "");
        // is_absolute_path covers the path shapes scrub_skill_path keys on.
        assert!(is_absolute_path("/abs"));
        assert!(is_absolute_path("\\\\unc\\share"));
        assert!(is_absolute_path("C:\\win"));
        assert!(is_absolute_path("C:/win"));
        assert!(!is_absolute_path("rel/path"));
        assert!(!is_absolute_path("bare-id"));
        assert!(!is_absolute_path(""));
    }

    #[test]
    fn resolve_child_source_rejects_absolute_and_escaping_paths() {
        // F2: an absolute path is never copied.
        let dir = tempfile::tempdir().unwrap();
        let run_dir = dir.path().join("run");
        std::fs::create_dir_all(&run_dir).unwrap();
        let record = run_dir.join("skill-usage.record.json");
        std::fs::write(&record, "{}").unwrap();
        // A real file inside the run dir resolves.
        let child = run_dir.join("child.txt");
        std::fs::write(&child, "ok").unwrap();
        assert!(resolve_child_source(&record, "child.txt").is_some());
        // An absolute path (e.g. /etc/hosts) is rejected.
        assert!(resolve_child_source(&record, "/etc/hosts").is_none());
        // A `..` escape out of the run dir is rejected even if the target
        // exists.
        let sibling = dir.path().join("secret.txt");
        std::fs::write(&sibling, "secret").unwrap();
        assert!(resolve_child_source(&record, "../secret.txt").is_none());
        // A URL is reference-only.
        assert!(resolve_child_source(&record, "https://example/x").is_none());
    }

    /// Run `f` with `HOME` set to `value`, restoring it afterwards. Serialized
    /// because env vars are process-global.
    fn temp_env_home<T>(value: &str, f: impl FnOnce() -> T) -> T {
        use std::sync::Mutex;
        static LOCK: Mutex<()> = Mutex::new(());
        let _guard = LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let original = std::env::var_os("HOME");
        // SAFETY: serialized under LOCK; restored before returning.
        unsafe { std::env::set_var("HOME", value) };
        let result = f();
        unsafe {
            match original {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
        }
        result
    }
}
