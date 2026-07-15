use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};

use nils_common::fs::{SECRET_FILE_MODE, sha256_file, write_atomic};
use serde::{Deserialize, Serialize};

use crate::backend::VerifiedBackend;
use crate::cli::{EvidenceMode, RuntimeMode, ToolProfile};
use crate::error::CliError;
use crate::lock::PeekabooLock;
use crate::test_mode;

pub const JOURNAL_SCHEMA: &str = "macos-agent.journal.v2";
const STEP_SCHEMA: &str = "macos-agent.journal-step.v2";
const SEQUENCE_FILE: &str = ".sequence";
const SEQUENCE_TRANSACTION_FILE: &str = ".sequence.transaction";

#[cfg(test)]
static STEP_SCAN_COUNT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
#[cfg(test)]
static STEP_SCAN_ROOT: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);
const MAX_JOURNAL_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReplayClass {
    Safe,
    Conditional,
    Never,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    Passed,
    Failed,
    Unknown,
    PolicyBlocked,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub schema_version: String,
    pub run_id: String,
    pub adapter_version: String,
    pub peekaboo_tag: String,
    pub peekaboo_commit: String,
    pub backend_digest: String,
    pub runtime: RuntimeMode,
    pub transport: String,
    pub evidence_mode: EvidenceMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_profile: Option<ToolProfile>,
    pub started_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub closed_at: Option<String>,
    pub state: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepRecord {
    pub schema_version: String,
    pub sequence: u64,
    pub id: String,
    pub correlation_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    pub recorded_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub intent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected: Option<String>,
    pub command: String,
    pub argv_shape: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replay_argv: Option<Vec<String>>,
    pub backend_digest: String,
    pub runtime: RuntimeMode,
    pub transport: String,
    pub status: StepStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_class: Option<String>,
    pub duration_ms: u64,
    pub retries: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub precondition_refs: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub postcondition_refs: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot_lineage: Option<String>,
    pub replay_class: ReplayClass,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_refs: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct StepInput {
    pub parent_id: Option<String>,
    pub intent: Option<String>,
    pub expected: Option<String>,
    pub argv: Vec<String>,
    pub status: StepStatus,
    pub failure_class: Option<String>,
    pub duration_ms: u64,
    pub retries: u32,
    pub precondition_refs: Vec<String>,
    pub postcondition_refs: Vec<String>,
    pub snapshot_lineage: Option<String>,
    pub artifact_refs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactIndex {
    pub schema_version: String,
    pub artifacts: Vec<ArtifactRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactRecord {
    pub sha256: String,
    pub mime: String,
    pub kind: String,
    pub producing_step: String,
    pub sensitivity: String,
    pub redaction: String,
    pub retention: String,
    pub relative_path: String,
}

pub struct ArtifactInput<'a> {
    pub step: &'a str,
    pub path: &'a Path,
    pub kind: &'a str,
    pub mime: &'a str,
    pub sensitivity: &'a str,
    pub redaction: &'a str,
    pub retention: &'a str,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Summary {
    pub schema_version: String,
    pub total_steps: usize,
    pub passed: usize,
    pub failed: usize,
    pub unknown: usize,
    pub policy_blocked: usize,
    pub failure_signatures: Vec<FailureSignature>,
    pub replay_candidates: Vec<String>,
    pub defect_candidates: Vec<String>,
    pub assertions: Vec<String>,
    pub residual_user_actions: Vec<String>,
    pub recovered_tail: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailureSignature {
    pub signature: String,
    pub count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Review {
    pub schema_version: String,
    pub candidates: Vec<ReviewCandidate>,
    pub clean: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewCandidate {
    pub signature: String,
    pub count: usize,
    pub significant: bool,
    pub proposed_owner: String,
    pub step_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RedactionReport {
    pub schema_version: String,
    pub rules: Vec<String>,
    pub suppressed_fields: Vec<String>,
    pub failures: Vec<String>,
    pub private_identifier_matches: usize,
    pub secret_matches: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReplayPlan {
    pub steps: Vec<ReplayPlanRow>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReplayPlanRow {
    pub id: String,
    pub replay_class: ReplayClass,
    pub eligible: bool,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub struct ReplayRequest {
    pub parent_id: String,
    pub argv: Vec<String>,
    pub intent: Option<String>,
    pub expected: Option<String>,
    pub runtime: RuntimeMode,
    pub evidence_mode: EvidenceMode,
}

pub struct Journal {
    root: PathBuf,
    manifest: Manifest,
    recovered_tail: bool,
    #[cfg(test)]
    fail_next_sequence_write: bool,
}

struct JournalLock(fs::File);

impl JournalLock {
    fn acquire(root: &Path) -> Result<Self, CliError> {
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .mode(SECRET_FILE_MODE)
            .open(root.join(".journal.lock"))
            .map_err(|_| journal_error("failed to open journal lock"))?;
        // SAFETY: `flock` observes the valid descriptor owned by `file`.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err(journal_error("failed to acquire journal lock"));
        }
        Ok(Self(file))
    }
}

impl Drop for JournalLock {
    fn drop(&mut self) {
        // SAFETY: `flock` observes the valid descriptor owned by this guard.
        let _ = unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}

impl Journal {
    pub fn open(
        root: &Path,
        runtime: RuntimeMode,
        transport: &str,
        evidence_mode: EvidenceMode,
        tool_profile: Option<ToolProfile>,
    ) -> Result<Self, CliError> {
        let lock = PeekabooLock::embedded()?;
        let backend_digest = effective_backend_digest(&lock, true)?;
        Self::open_with_digest(
            root,
            runtime,
            transport,
            evidence_mode,
            tool_profile,
            lock,
            backend_digest,
        )
    }

    pub fn open_for_backend(
        root: &Path,
        runtime: RuntimeMode,
        transport: &str,
        evidence_mode: EvidenceMode,
        tool_profile: Option<ToolProfile>,
        backend: &VerifiedBackend,
    ) -> Result<Self, CliError> {
        let lock = PeekabooLock::embedded()?;
        let backend_digest = backend
            .digest()
            .map_err(|_| journal_error("failed to hash the leased backend"))?;
        Self::open_with_digest(
            root,
            runtime,
            transport,
            evidence_mode,
            tool_profile,
            lock,
            backend_digest,
        )
    }

    fn open_with_digest(
        root: &Path,
        runtime: RuntimeMode,
        transport: &str,
        evidence_mode: EvidenceMode,
        tool_profile: Option<ToolProfile>,
        lock: PeekabooLock,
        backend_digest: String,
    ) -> Result<Self, CliError> {
        validate_root(root)?;
        create_private_dir(root)?;
        let _lock = JournalLock::acquire(root)?;
        create_private_dir(&root.join("artifacts"))?;
        let manifest_path = root.join("manifest.json");
        let manifest = if manifest_path.exists() {
            let manifest: Manifest = read_json(&manifest_path)?;
            if manifest.schema_version != JOURNAL_SCHEMA
                || manifest.backend_digest != backend_digest
                || manifest.runtime != runtime
                || manifest.transport != transport
                || manifest.evidence_mode != evidence_mode
                || manifest.tool_profile != tool_profile
            {
                return Err(journal_error(
                    "journal manifest does not match this execution session",
                ));
            }
            manifest
        } else {
            let started_at = test_mode::timestamp();
            Manifest {
                schema_version: JOURNAL_SCHEMA.into(),
                run_id: format!("run-{}-{}", stable_token(&started_at), std::process::id()),
                adapter_version: env!("CARGO_PKG_VERSION").into(),
                peekaboo_tag: lock.tag,
                peekaboo_commit: lock.commit,
                backend_digest,
                runtime,
                transport: transport.into(),
                evidence_mode,
                tool_profile,
                started_at,
                closed_at: None,
                state: "open".into(),
            }
        };
        write_json(&manifest_path, &manifest)?;
        ensure_index(root)?;
        write_redaction_report(root)?;
        let (steps, recovered_tail) = read_steps_recover(root)?;
        write_sequence(root, steps.len() as u64)?;
        remove_sequence_transaction(root)?;
        Ok(Self {
            root: root.to_path_buf(),
            manifest,
            recovered_tail,
            #[cfg(test)]
            fail_next_sequence_write: false,
        })
    }

    pub fn record_step(&mut self, input: StepInput) -> Result<StepRecord, CliError> {
        let _lock = JournalLock::acquire(&self.root)?;
        recover_sequence_transaction(&self.root)?;
        let sequence = read_sequence(&self.root)? + 1;
        let id = format!("step-{sequence:06}");
        let command = input
            .argv
            .first()
            .map(String::as_str)
            .unwrap_or("unknown")
            .to_ascii_lowercase();
        let replay_class = classify_replay(
            &command,
            input.status,
            &input.argv,
            input.snapshot_lineage.as_deref(),
        );
        let (argv_shape, replay_argv) =
            sanitize_argv(&input.argv, self.manifest.evidence_mode, replay_class);
        let step = StepRecord {
            schema_version: STEP_SCHEMA.into(),
            sequence,
            id: id.clone(),
            correlation_id: format!("{}-{id}", self.manifest.run_id),
            parent_id: input.parent_id,
            recorded_at: test_mode::timestamp(),
            intent: input
                .intent
                .as_deref()
                .map(|value| sanitize_text(value, self.manifest.evidence_mode)),
            expected: input.expected.as_deref().map(suppressed),
            command,
            argv_shape,
            replay_argv,
            backend_digest: self.manifest.backend_digest.clone(),
            runtime: self.manifest.runtime,
            transport: self.manifest.transport.clone(),
            status: input.status,
            failure_class: input.failure_class.map(|value| normalize_failure(&value)),
            duration_ms: input.duration_ms,
            retries: input.retries,
            precondition_refs: sanitize_refs(input.precondition_refs),
            postcondition_refs: sanitize_refs(input.postcondition_refs),
            snapshot_lineage: input.snapshot_lineage.map(|value| safe_reference(&value)),
            replay_class,
            artifact_refs: sanitize_refs(input.artifact_refs),
        };
        write_atomic(
            &self.root.join(SEQUENCE_TRANSACTION_FILE),
            sequence.to_string().as_bytes(),
            SECRET_FILE_MODE,
        )
        .map_err(|_| journal_error("failed to begin journal sequence transaction"))?;
        if let Err(error) = append_step(&self.root, &step) {
            let _ = remove_sequence_transaction(&self.root);
            return Err(error);
        }
        #[cfg(test)]
        if std::mem::take(&mut self.fail_next_sequence_write) {
            return Err(journal_error("injected sequence commit failure"));
        }
        write_sequence(&self.root, sequence)?;
        remove_sequence_transaction(&self.root)?;
        Ok(step)
    }

    #[cfg(test)]
    fn inject_sequence_write_failure(&mut self) {
        self.fail_next_sequence_write = true;
    }

    pub fn register_artifact(&mut self, input: ArtifactInput<'_>) -> Result<String, CliError> {
        let _lock = JournalLock::acquire(&self.root)?;
        let relative = relative_artifact_path(&self.root, input.path)?;
        let digest = format!(
            "sha256:{}",
            sha256_file(input.path)
                .map_err(|_| { journal_error("failed to hash a journal artifact") })?
        );
        let mut index: ArtifactIndex = read_json(&self.root.join("artifacts/index.json"))?;
        index.artifacts.push(ArtifactRecord {
            sha256: digest,
            mime: input.mime.into(),
            kind: input.kind.into(),
            producing_step: safe_reference(input.step),
            sensitivity: input.sensitivity.into(),
            redaction: input.redaction.into(),
            retention: input.retention.into(),
            relative_path: relative.clone(),
        });
        index
            .artifacts
            .sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
        write_json(&self.root.join("artifacts/index.json"), &index)?;
        Ok(relative)
    }

    pub fn close(mut self) -> Result<Summary, CliError> {
        let _lock = JournalLock::acquire(&self.root)?;
        let summary = self.refresh()?;
        self.manifest.state = "closed".into();
        self.manifest.closed_at = Some(test_mode::timestamp());
        write_json(&self.root.join("manifest.json"), &self.manifest)?;
        Ok(summary)
    }

    fn refresh(&mut self) -> Result<Summary, CliError> {
        let (steps, recovered) = read_steps_recover(&self.root)?;
        self.recovered_tail |= recovered;
        let summary = build_summary(&steps, self.recovered_tail);
        write_json(&self.root.join("summary.json"), &summary)?;
        write_redaction_report(&self.root)?;
        Ok(summary)
    }
}

pub fn summarize(root: &Path) -> Result<Summary, CliError> {
    let _lock = JournalLock::acquire(root)?;
    let manifest: Manifest = read_json(&root.join("manifest.json"))?;
    if manifest.schema_version != JOURNAL_SCHEMA {
        return Err(journal_error("unsupported journal schema"));
    }
    let (steps, recovered) = read_steps_recover(root)?;
    let summary = build_summary(&steps, recovered);
    write_json(&root.join("summary.json"), &summary)?;
    Ok(summary)
}

pub fn review(root: &Path) -> Result<Review, CliError> {
    let _lock = JournalLock::acquire(root)?;
    let (steps, _) = read_steps_recover(root)?;
    let mut clusters = BTreeMap::<String, Vec<&StepRecord>>::new();
    for step in &steps {
        if let Some(class) = &step.failure_class {
            clusters
                .entry(format!("{}:{}", step.command, normalize_failure(class)))
                .or_default()
                .push(step);
        }
    }
    let candidates = clusters
        .into_iter()
        .map(|(signature, rows)| {
            let significant = rows.len() >= 2 || rows.iter().any(|row| significant_failure(row));
            ReviewCandidate {
                proposed_owner: proposed_owner(&signature).into(),
                signature,
                count: rows.len(),
                significant,
                step_ids: rows.iter().map(|row| row.id.clone()).collect(),
            }
        })
        .collect::<Vec<_>>();
    let clean = candidates.iter().all(|candidate| !candidate.significant);
    let review = Review {
        schema_version: "macos-agent.journal-review.v1".into(),
        candidates,
        clean,
    };
    write_json(&root.join("review.json"), &review)?;
    Ok(review)
}

pub fn record_system_failure(root: &Path, class: &str) -> Result<(), CliError> {
    let manifest: Manifest = read_json(&root.join("manifest.json"))?;
    let mut journal = Journal::open(
        root,
        manifest.runtime,
        &manifest.transport,
        manifest.evidence_mode,
        manifest.tool_profile,
    )?;
    journal.record_step(StepInput {
        parent_id: None,
        intent: Some("adapter system operation".into()),
        expected: Some("operation completes with verified state".into()),
        argv: vec!["transport".into()],
        status: StepStatus::Failed,
        failure_class: Some(class.into()),
        duration_ms: 0,
        retries: 0,
        precondition_refs: Vec::new(),
        postcondition_refs: Vec::new(),
        snapshot_lineage: None,
        artifact_refs: Vec::new(),
    })?;
    journal.close()?;
    Ok(())
}

pub fn replay_plan(root: &Path, selected: Option<&str>) -> Result<ReplayPlan, CliError> {
    let _lock = JournalLock::acquire(root)?;
    let (steps, _) = read_steps_recover(root)?;
    let rows = steps
        .into_iter()
        .filter(|step| selected.is_none_or(|selected| selected == step.id))
        .map(|step| {
            let (eligible, reason) = replay_eligibility(&step, false, None, None);
            ReplayPlanRow {
                id: step.id,
                replay_class: step.replay_class,
                eligible,
                reason,
            }
        })
        .collect::<Vec<_>>();
    if selected.is_some() && rows.is_empty() {
        return Err(
            CliError::usage("journal step was not found").with_operation("journal.replay-plan")
        );
    }
    Ok(ReplayPlan { steps: rows })
}

pub fn prepare_replay(
    root: &Path,
    step_id: &str,
    confirm_conditional: bool,
    current_snapshot: Option<&str>,
    fresh_expected: Option<&str>,
) -> Result<ReplayRequest, CliError> {
    let _lock = JournalLock::acquire(root)?;
    let manifest: Manifest = read_json(&root.join("manifest.json"))?;
    let lock = PeekabooLock::embedded()?;
    if manifest.backend_digest != effective_backend_digest(&lock, false)? {
        return Err(CliError::policy("journal backend digest is stale")
            .with_operation("journal.replay-step"));
    }
    let (steps, _) = read_steps_recover(root)?;
    let step = steps
        .into_iter()
        .find(|step| step.id == step_id)
        .ok_or_else(|| CliError::usage("journal step was not found"))?;
    validate_replay_record(&step, manifest.evidence_mode)?;
    let (eligible, reason) =
        replay_eligibility(&step, confirm_conditional, current_snapshot, fresh_expected);
    if !eligible {
        return Err(CliError::policy(reason).with_operation("journal.replay-step"));
    }
    Ok(ReplayRequest {
        parent_id: step.id,
        argv: step
            .replay_argv
            .ok_or_else(|| CliError::policy("step has no retained replay arguments"))?,
        intent: step.intent,
        expected: (step.replay_class == ReplayClass::Conditional)
            .then(|| fresh_expected.map(str::to_string))
            .flatten(),
        runtime: manifest.runtime,
        evidence_mode: manifest.evidence_mode,
    })
}

fn effective_backend_digest(
    lock: &PeekabooLock,
    allow_uninstalled: bool,
) -> Result<String, CliError> {
    if let Some(path) = test_mode::peekaboo_bin_override() {
        let digest = sha256_file(&path)
            .map_err(|_| journal_error("failed to hash the effective test backend"))?;
        return Ok(format!("sha256:{digest}"));
    }
    if let Ok(backend) = crate::backend::acquire_verified_backend() {
        let digest = sha256_file(backend.path())
            .map_err(|_| journal_error("failed to hash the effective backend"))?;
        return Ok(format!("sha256:{digest}"));
    }
    if allow_uninstalled || cfg!(test) {
        Ok(format!("sha256:{}", lock.cli_asset().sha256))
    } else {
        Err(journal_error("effective backend is unavailable for replay"))
    }
}

fn replay_eligibility(
    step: &StepRecord,
    confirm_conditional: bool,
    current_snapshot: Option<&str>,
    fresh_expected: Option<&str>,
) -> (bool, String) {
    if step.replay_argv.is_none() {
        return (
            false,
            "sanitized step has no replayable argument set".into(),
        );
    }
    match step.replay_class {
        ReplayClass::Safe => (
            true,
            "read-only/setup step is replayable after backend validation".into(),
        ),
        ReplayClass::Conditional if !confirm_conditional => (
            false,
            "conditional replay requires --confirm-conditional".into(),
        ),
        ReplayClass::Conditional
            if fresh_expected.is_none_or(|expected| expected.trim().is_empty()) =>
        {
            (
                false,
                "conditional replay requires a fresh --expected observable postcondition".into(),
            )
        }
        ReplayClass::Conditional if step.snapshot_lineage.is_none() => (
            false,
            "conditional replay requires a retained pre-action snapshot reference".into(),
        ),
        ReplayClass::Conditional
            if current_snapshot.map(safe_reference).as_deref()
                != step.snapshot_lineage.as_deref() =>
        {
            (
                false,
                "conditional replay current state does not match the retained snapshot".into(),
            )
        }
        ReplayClass::Conditional => (
            true,
            "conditional replay confirmed with a fresh observable postcondition".into(),
        ),
        ReplayClass::Never => (false, "step is permanently classified never-replay".into()),
    }
}

fn validate_replay_record(step: &StepRecord, mode: EvidenceMode) -> Result<(), CliError> {
    let argv = step
        .replay_argv
        .as_ref()
        .ok_or_else(|| CliError::policy("step has no retained replay arguments"))?;
    crate::policy::validate_exec_argv(argv).map_err(|_| {
        CliError::policy("retained replay arguments no longer satisfy execution policy")
    })?;
    let command = argv
        .first()
        .map(|value| value.to_ascii_lowercase())
        .unwrap_or_else(|| "unknown".into());
    let replay_class = classify_replay(
        &command,
        step.status,
        argv,
        step.snapshot_lineage.as_deref(),
    );
    let (argv_shape, replay_argv) = sanitize_argv(argv, mode, replay_class);
    if step.command != command
        || step.replay_class != replay_class
        || step.argv_shape != argv_shape
        || replay_argv.as_ref() != Some(argv)
    {
        return Err(CliError::policy(
            "journal replay metadata does not match the retained arguments",
        ));
    }
    Ok(())
}

fn classify_replay(
    command: &str,
    status: StepStatus,
    argv: &[String],
    snapshot_lineage: Option<&str>,
) -> ReplayClass {
    if matches!(status, StepStatus::Unknown | StepStatus::PolicyBlocked)
        || argv.iter().any(|value| sensitive_flag(value))
    {
        return ReplayClass::Never;
    }
    match command {
        "see" | "list" | "inspect-ui" | "screenshot" | "sleep" | "tools" | "bridge" => {
            ReplayClass::Safe
        }
        "click" | "press" | "hotkey" | "scroll" | "swipe" | "drag" | "move" | "set-value"
        | "perform-action" | "window" | "app" | "menu" | "menubar"
            if snapshot_lineage.is_some() =>
        {
            ReplayClass::Conditional
        }
        _ => ReplayClass::Never,
    }
}

fn sanitize_argv(
    argv: &[String],
    mode: EvidenceMode,
    replay_class: ReplayClass,
) -> (Vec<String>, Option<Vec<String>>) {
    let mut shape = Vec::with_capacity(argv.len());
    let mut replay = Vec::with_capacity(argv.len());
    let mut suppress_next = false;
    let mut replayable = replay_class != ReplayClass::Never;
    let command = argv.first().map(|value| normalize_command(value));
    for (index, value) in argv.iter().enumerate() {
        if suppress_next {
            shape.push(suppressed(value));
            replayable = false;
            suppress_next = false;
            continue;
        }
        let normalized = value.to_ascii_lowercase();
        if command.as_deref().is_some_and(|command| {
            matches!(command, "type" | "paste" | "set_value")
                && index == 1
                && !value.starts_with('-')
        }) {
            shape.push(suppressed(value));
            replayable = false;
            continue;
        }
        if sensitive_flag(&normalized) {
            shape.push(value.clone());
            replay.push(value.clone());
            suppress_next = true;
            continue;
        }
        if normalized.starts_with("--text=")
            || normalized.starts_with("--value=")
            || normalized.starts_with("--password=")
            || normalized.starts_with("--token=")
        {
            let flag = value.split('=').next().unwrap_or(value);
            shape.push(format!("{flag}=<suppressed>"));
            replayable = false;
            continue;
        }
        let sanitized = sanitize_text(value, mode);
        if sanitized != *value {
            replayable = false;
        } else {
            replay.push(value.clone());
        }
        shape.push(sanitized);
    }
    if suppress_next {
        replayable = false;
    }
    (shape, replayable.then_some(replay))
}

fn sensitive_flag(value: &str) -> bool {
    matches!(
        value,
        "--text"
            | "--value"
            | "--password"
            | "--token"
            | "--api-key"
            | "--clipboard"
            | "--paste"
            | "--data-base64"
            | "--also-text"
            | "--file-path"
            | "--image-path"
    )
}

fn normalize_command(value: &str) -> String {
    value.trim().to_ascii_lowercase().replace('-', "_")
}

fn sanitize_text(value: &str, mode: EvidenceMode) -> String {
    if mode == EvidenceMode::Sensitive {
        return suppressed(value);
    }
    let home = std::env::var("HOME").unwrap_or_default();
    let mut sanitized = if !home.is_empty() {
        value.replace(&home, "$HOME")
    } else {
        value.to_string()
    };
    sanitized = redact_home_shape(&sanitized, "/Users/");
    sanitized = redact_home_shape(&sanitized, "/home/");
    if looks_like_secret(&sanitized) {
        return suppressed(value);
    }
    sanitized
}

pub fn sanitize_output(value: &str, mode: EvidenceMode) -> String {
    sanitize_text(value, mode)
}

pub fn sanitize_json(value: &serde_json::Value, mode: EvidenceMode) -> serde_json::Value {
    sanitize_json_inner(value, mode, None, true)
}

pub fn sanitize_result_json(value: &serde_json::Value, mode: EvidenceMode) -> serde_json::Value {
    sanitize_json_inner(value, mode, None, mode == EvidenceMode::Sensitive)
}

fn sanitize_json_inner(
    value: &serde_json::Value,
    mode: EvidenceMode,
    key: Option<&str>,
    suppress_payload_fields: bool,
) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => serde_json::Value::Object(
            map.iter()
                .map(|(child_key, child)| {
                    (
                        child_key.clone(),
                        sanitize_json_inner(child, mode, Some(child_key), suppress_payload_fields),
                    )
                })
                .collect(),
        ),
        serde_json::Value::Array(values) => serde_json::Value::Array(
            values
                .iter()
                .map(|child| sanitize_json_inner(child, mode, key, suppress_payload_fields))
                .collect(),
        ),
        serde_json::Value::String(text)
            if key.is_some_and(|key| {
                let key = key.to_ascii_lowercase();
                key.contains("token")
                    || key.contains("secret")
                    || key.contains("password")
                    || key.contains("path")
                    || key.contains("host")
                    || key.contains("user")
                    || (suppress_payload_fields
                        && (key.contains("text")
                            || key.contains("value")
                            || key.contains("clipboard")
                            || key.contains("title")))
            }) =>
        {
            serde_json::Value::String(suppressed(text))
        }
        serde_json::Value::String(text) => serde_json::Value::String(sanitize_text(text, mode)),
        other => other.clone(),
    }
}

fn redact_home_shape(value: &str, prefix: &str) -> String {
    let mut result = value.to_string();
    let mut search_from = 0usize;
    while let Some(offset) = result[search_from..].find(prefix) {
        let start = search_from + offset;
        let user_start = start + prefix.len();
        let Some(relative_end) = result[user_start..].find('/') else {
            break;
        };
        let end = user_start + relative_end;
        result.replace_range(user_start..end, "$USER");
        search_from = user_start + "$USER".len();
    }
    result
}

fn looks_like_secret(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.contains("sk-")
        || lower.contains("bearer ")
        || lower.contains("private key")
        || lower.contains("password=")
        || lower.contains("token=")
}

fn suppressed(_value: &str) -> String {
    "<suppressed>".into()
}

fn build_summary(steps: &[StepRecord], recovered_tail: bool) -> Summary {
    let mut signatures = BTreeMap::<String, usize>::new();
    for step in steps {
        if let Some(class) = &step.failure_class {
            *signatures
                .entry(format!("{}:{}", step.command, normalize_failure(class)))
                .or_default() += 1;
        }
    }
    let defect_candidates = steps
        .iter()
        .filter(|step| significant_failure(step))
        .map(|step| step.id.clone())
        .collect();
    Summary {
        schema_version: "macos-agent.journal-summary.v1".into(),
        total_steps: steps.len(),
        passed: steps
            .iter()
            .filter(|step| step.status == StepStatus::Passed)
            .count(),
        failed: steps
            .iter()
            .filter(|step| step.status == StepStatus::Failed)
            .count(),
        unknown: steps
            .iter()
            .filter(|step| step.status == StepStatus::Unknown)
            .count(),
        policy_blocked: steps
            .iter()
            .filter(|step| step.status == StepStatus::PolicyBlocked)
            .count(),
        failure_signatures: signatures
            .into_iter()
            .map(|(signature, count)| FailureSignature { signature, count })
            .collect(),
        replay_candidates: steps
            .iter()
            .filter(|step| step.replay_class != ReplayClass::Never)
            .map(|step| step.id.clone())
            .collect(),
        defect_candidates,
        assertions: steps
            .iter()
            .filter_map(|step| step.expected.clone())
            .collect(),
        residual_user_actions: Vec::new(),
        recovered_tail,
    }
}

fn significant_failure(step: &StepRecord) -> bool {
    let class = step.failure_class.as_deref().unwrap_or_default();
    matches!(
        class,
        "privacy_redaction"
            | "wrong_target"
            | "false_success"
            | "unknown_mutation"
            | "held_input"
            | "remote_cleanup"
            | "journal_integrity"
            | "replay_integrity"
            | "backend_drift"
            | "permission_drift"
    ) || step.status == StepStatus::Unknown
}

fn proposed_owner(signature: &str) -> &'static str {
    if signature.contains("permission") {
        "tcc_environment"
    } else if signature.contains("backend") || signature.contains("upstream") {
        "peekaboo_or_adapter"
    } else if signature.contains("policy") || signature.contains("wrong_target") {
        "runtime_skill_policy"
    } else {
        "adapter"
    }
}

fn normalize_failure(value: &str) -> String {
    let normalized = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect::<String>()
        .split('_')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("_");
    match normalized.as_str() {
        "policy"
        | "backend_drift"
        | "permission"
        | "permission_drift"
        | "upstream"
        | "upstream_malformed_json"
        | "upstream_mcp"
        | "upstream_scenario"
        | "upstream_signal"
        | "upstream_timeout"
        | "unknown_mutation"
        | "privacy_redaction"
        | "wrong_target"
        | "false_success"
        | "held_input"
        | "remote_cleanup"
        | "journal_integrity"
        | "replay_integrity" => normalized,
        _ => "other".into(),
    }
}

fn sanitize_refs(values: Vec<String>) -> Vec<String> {
    values
        .into_iter()
        .map(|value| safe_reference(&value))
        .collect()
}

fn safe_reference(value: &str) -> String {
    value
        .chars()
        .filter(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.' | '/')
        })
        .take(240)
        .collect()
}

fn relative_artifact_path(root: &Path, path: &Path) -> Result<String, CliError> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| journal_error("artifact is outside the journal root"))?;
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(journal_error("artifact path is unsafe"));
    }
    let mut current = root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(component) = component else {
            return Err(journal_error("artifact path is unsafe"));
        };
        current.push(component);
        let metadata = fs::symlink_metadata(&current)
            .map_err(|_| journal_error("artifact path is unavailable"))?;
        if metadata.file_type().is_symlink() {
            return Err(journal_error("artifact path contains a symlink"));
        }
    }
    if !current.is_file() {
        return Err(journal_error("artifact is not a regular file"));
    }
    Ok(relative.to_string_lossy().replace('\\', "/"))
}

fn ensure_index(root: &Path) -> Result<(), CliError> {
    let index_path = root.join("artifacts/index.json");
    if !index_path.exists() {
        write_json(
            &index_path,
            &ArtifactIndex {
                schema_version: "macos-agent.artifact-index.v1".into(),
                artifacts: Vec::new(),
            },
        )?;
    }
    Ok(())
}

fn write_redaction_report(root: &Path) -> Result<(), CliError> {
    write_json(
        &root.join("redaction.json"),
        &RedactionReport {
            schema_version: "macos-agent.redaction.v1".into(),
            rules: vec![
                "home-path".into(),
                "ssh-identity-not-recorded".into(),
                "typed-and-clipboard-values".into(),
                "credential-patterns".into(),
                "sensitive-mode-full-value-suppression".into(),
            ],
            suppressed_fields: vec![
                "host".into(),
                "typed_text".into(),
                "clipboard".into(),
                "credentials".into(),
                "raw_remote_request".into(),
            ],
            failures: Vec::new(),
            private_identifier_matches: 0,
            secret_matches: 0,
        },
    )
}

fn read_sequence(root: &Path) -> Result<u64, CliError> {
    let raw = fs::read_to_string(root.join(SEQUENCE_FILE))
        .map_err(|_| journal_error("journal sequence state is unavailable"))?;
    raw.trim()
        .parse()
        .map_err(|_| journal_error("journal sequence state is malformed"))
}

fn write_sequence(root: &Path, sequence: u64) -> Result<(), CliError> {
    write_atomic(
        &root.join(SEQUENCE_FILE),
        sequence.to_string().as_bytes(),
        SECRET_FILE_MODE,
    )
    .map_err(|_| journal_error("failed to persist journal sequence state"))
}

fn recover_sequence_transaction(root: &Path) -> Result<(), CliError> {
    if !root.join(SEQUENCE_TRANSACTION_FILE).exists() {
        return Ok(());
    }
    let (steps, _) = read_steps_recover(root)?;
    write_sequence(root, steps.len() as u64)?;
    remove_sequence_transaction(root)
}

fn remove_sequence_transaction(root: &Path) -> Result<(), CliError> {
    match fs::remove_file(root.join(SEQUENCE_TRANSACTION_FILE)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(journal_error(
            "failed to finalize journal sequence transaction",
        )),
    }
}

fn append_step(root: &Path, step: &StepRecord) -> Result<(), CliError> {
    let path = root.join("steps.jsonl");
    let existing = path.metadata().map(|metadata| metadata.len()).unwrap_or(0);
    if existing >= MAX_JOURNAL_BYTES {
        return Err(journal_error("journal step log exceeded its size bound"));
    }
    let mut body =
        serde_json::to_vec(step).map_err(|_| journal_error("failed to encode journal step"))?;
    body.push(b'\n');
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(SECRET_FILE_MODE)
        .open(&path)
        .map_err(|_| journal_error("failed to open journal step log"))?;
    file.write_all(&body)
        .map_err(|_| journal_error("failed to append journal step"))?;
    file.sync_all()
        .map_err(|_| journal_error("failed to sync journal step"))
}

fn read_steps_recover(root: &Path) -> Result<(Vec<StepRecord>, bool), CliError> {
    #[cfg(test)]
    if STEP_SCAN_ROOT.lock().expect("scan root lock").as_deref() == Some(root) {
        STEP_SCAN_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    let path = root.join("steps.jsonl");
    let mut raw = match fs::File::open(&path) {
        Ok(mut file) => {
            if file
                .metadata()
                .map_err(|_| journal_error("failed to inspect journal step log"))?
                .len()
                > MAX_JOURNAL_BYTES
            {
                return Err(journal_error("journal step log exceeded its size bound"));
            }
            let mut raw = Vec::new();
            file.read_to_end(&mut raw)
                .map_err(|_| journal_error("failed to read journal step log"))?;
            raw
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((Vec::new(), false));
        }
        Err(_) => return Err(journal_error("failed to open journal step log")),
    };
    let mut steps = Vec::new();
    let mut valid_end = 0usize;
    let mut cursor = 0usize;
    while cursor < raw.len() {
        let next = raw[cursor..]
            .iter()
            .position(|byte| *byte == b'\n')
            .map(|offset| cursor + offset + 1)
            .unwrap_or(raw.len());
        let line = &raw[cursor..next];
        let trimmed = line.strip_suffix(b"\n").unwrap_or(line);
        let complete = line.ends_with(b"\n");
        match serde_json::from_slice::<StepRecord>(trimmed) {
            Ok(step)
                if step.schema_version == STEP_SCHEMA
                    && step.sequence == steps.len() as u64 + 1 =>
            {
                steps.push(step);
                valid_end = next;
                cursor = next;
            }
            _ if !complete && next == raw.len() => break,
            _ => {
                return Err(journal_error(
                    "journal contains a malformed or out-of-sequence complete record",
                ));
            }
        }
    }
    if valid_end == raw.len() {
        return Ok((steps, false));
    }
    let tail = raw.split_off(valid_end);
    let quarantine = root.join("quarantine");
    create_private_dir(&quarantine)?;
    let tail_path = quarantine.join(format!(
        "steps-tail-{}.bin",
        stable_token(&test_mode::timestamp())
    ));
    write_atomic(&tail_path, &tail, SECRET_FILE_MODE)
        .map_err(|_| journal_error("failed to quarantine incomplete journal tail"))?;
    write_atomic(&path, &raw, SECRET_FILE_MODE)
        .map_err(|_| journal_error("failed to recover valid journal prefix"))?;
    Ok((steps, true))
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, CliError> {
    let raw =
        fs::read(path).map_err(|_| journal_error("required journal record is unavailable"))?;
    serde_json::from_slice(&raw).map_err(|_| journal_error("journal record is malformed"))
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<(), CliError> {
    let body = serde_json::to_vec_pretty(value)
        .map_err(|_| journal_error("failed to encode journal record"))?;
    write_atomic(path, &body, SECRET_FILE_MODE)
        .map_err(|_| journal_error("failed to atomically write journal record"))
}

fn validate_root(root: &Path) -> Result<(), CliError> {
    if root.as_os_str().is_empty()
        || root
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(CliError::usage("journal output directory is unsafe"));
    }
    if let Ok(metadata) = fs::symlink_metadata(root)
        && metadata.file_type().is_symlink()
    {
        return Err(CliError::policy(
            "journal output directory must not be a symlink",
        ));
    }
    Ok(())
}

fn create_private_dir(path: &Path) -> Result<(), CliError> {
    fs::create_dir_all(path).map_err(|_| journal_error("failed to create journal directory"))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|_| journal_error("failed to secure journal directory"))
}

fn stable_token(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .take(32)
        .collect()
}

fn journal_error(message: impl Into<String>) -> CliError {
    CliError::journal(message).with_operation("journal")
}

#[cfg(test)]
mod tests {
    use std::fs::{self, OpenOptions};
    use std::io::Write;
    use std::sync::{Arc, Barrier};
    use std::thread;

    use tempfile::TempDir;

    use super::{
        EvidenceMode, Journal, ReplayClass, RuntimeMode, STEP_SCAN_COUNT, STEP_SCAN_ROOT,
        StepInput, StepStatus, prepare_replay, read_steps_recover, replay_plan, review,
        sanitize_argv,
    };
    use std::sync::atomic::Ordering;

    fn open(root: &std::path::Path, mode: EvidenceMode) -> Journal {
        Journal::open(root, RuntimeMode::App, "local", mode, None).expect("journal")
    }

    #[test]
    fn sensitive_values_never_enter_steps_or_replay() {
        let root = TempDir::new().expect("root");
        let mut journal = open(root.path(), EvidenceMode::Debug);
        let step = journal
            .record_step(StepInput {
                parent_id: None,
                intent: Some("type fixture".into()),
                expected: Some("field changes".into()),
                argv: vec!["type".into(), "--text".into(), "canary-secret".into()],
                status: StepStatus::Passed,
                failure_class: None,
                duration_ms: 1,
                retries: 0,
                precondition_refs: vec![],
                postcondition_refs: vec!["assertion-1".into()],
                snapshot_lineage: None,
                artifact_refs: vec![],
            })
            .expect("step");
        journal.close().expect("close");
        let raw = fs::read_to_string(root.path().join("steps.jsonl")).expect("steps");
        assert!(!raw.contains("canary-secret"));
        assert_eq!(step.replay_class, ReplayClass::Never);
        assert!(step.replay_argv.is_none());
    }

    #[test]
    fn real_positional_payload_grammar_is_never_retained_or_replayed() {
        for argv in [
            vec!["type".into(), "type-positional-canary".into()],
            vec!["paste".into(), "paste-positional-canary".into()],
            vec![
                "set-value".into(),
                "ax-value-positional-canary".into(),
                "--on".into(),
                "Search".into(),
            ],
        ] {
            let (shape, replay) =
                sanitize_argv(&argv, EvidenceMode::Debug, ReplayClass::Conditional);
            let encoded = serde_json::to_string(&shape).expect("shape");
            assert!(!encoded.contains("positional-canary"), "{encoded}");
            assert!(replay.is_none());
        }
    }

    #[test]
    fn partial_tail_is_quarantined_without_losing_valid_steps() {
        let root = TempDir::new().expect("root");
        let mut journal = open(root.path(), EvidenceMode::Minimal);
        journal
            .record_step(StepInput {
                parent_id: None,
                intent: None,
                expected: None,
                argv: vec!["see".into(), "--json".into()],
                status: StepStatus::Passed,
                failure_class: None,
                duration_ms: 1,
                retries: 0,
                precondition_refs: vec![],
                postcondition_refs: vec![],
                snapshot_lineage: None,
                artifact_refs: vec![],
            })
            .expect("step");
        OpenOptions::new()
            .append(true)
            .open(root.path().join("steps.jsonl"))
            .expect("open")
            .write_all(b"{partial")
            .expect("append");
        let (steps, recovered) = read_steps_recover(root.path()).expect("recover");
        assert!(recovered);
        assert_eq!(steps.len(), 1);
        assert_eq!(
            fs::read_dir(root.path().join("quarantine"))
                .expect("quarantine")
                .count(),
            1
        );
    }

    #[test]
    fn complete_middle_corruption_fails_closed_without_truncating_later_evidence() {
        let root = TempDir::new().expect("root");
        let mut journal = open(root.path(), EvidenceMode::Minimal);
        for command in ["see", "list"] {
            journal
                .record_step(StepInput {
                    parent_id: None,
                    intent: None,
                    expected: None,
                    argv: vec![command.into()],
                    status: StepStatus::Passed,
                    failure_class: None,
                    duration_ms: 1,
                    retries: 0,
                    precondition_refs: vec![],
                    postcondition_refs: vec![],
                    snapshot_lineage: None,
                    artifact_refs: vec![],
                })
                .expect("step");
        }
        let path = root.path().join("steps.jsonl");
        let raw = fs::read_to_string(&path).expect("steps");
        let mut lines = raw.lines();
        let first = lines.next().expect("first");
        let second = lines.next().expect("second");
        let corrupted = format!("{first}\n{{malformed-complete-record}}\n{second}\n");
        fs::write(&path, &corrupted).expect("corrupt");

        assert!(read_steps_recover(root.path()).is_err());
        assert_eq!(fs::read_to_string(&path).expect("unchanged"), corrupted);
    }

    #[test]
    fn valid_final_record_without_newline_is_accepted_without_recovery() {
        let root = TempDir::new().expect("root");
        let mut journal = open(root.path(), EvidenceMode::Minimal);
        journal
            .record_step(StepInput {
                parent_id: None,
                intent: None,
                expected: None,
                argv: vec!["see".into()],
                status: StepStatus::Passed,
                failure_class: None,
                duration_ms: 1,
                retries: 0,
                precondition_refs: vec![],
                postcondition_refs: vec![],
                snapshot_lineage: None,
                artifact_refs: vec![],
            })
            .expect("step");
        let path = root.path().join("steps.jsonl");
        let mut raw = fs::read(&path).expect("steps");
        assert_eq!(raw.pop(), Some(b'\n'));
        fs::write(&path, raw).expect("remove newline");

        let (steps, recovered) = read_steps_recover(root.path()).expect("read");
        assert_eq!(steps.len(), 1);
        assert!(!recovered);
    }

    #[test]
    fn replay_enforces_conditional_confirmation_and_backend_binding() {
        let root = TempDir::new().expect("root");
        let mut journal = open(root.path(), EvidenceMode::Minimal);
        let step = journal
            .record_step(StepInput {
                parent_id: None,
                intent: Some("bounded click".into()),
                expected: Some("button toggles".into()),
                argv: vec!["click".into(), "--id".into(), "B1".into()],
                status: StepStatus::Passed,
                failure_class: None,
                duration_ms: 2,
                retries: 0,
                precondition_refs: vec!["snapshot-1".into()],
                postcondition_refs: vec!["assertion-1".into()],
                snapshot_lineage: Some("snapshot-1".into()),
                artifact_refs: vec![],
            })
            .expect("step");
        journal.close().expect("close");
        assert!(prepare_replay(root.path(), &step.id, false, None, None).is_err());
        assert!(
            prepare_replay(
                root.path(),
                &step.id,
                true,
                Some("stale"),
                Some("button is toggled")
            )
            .is_err()
        );
        assert!(prepare_replay(root.path(), &step.id, true, Some("snapshot-1"), None).is_err());
        let replay = prepare_replay(
            root.path(),
            &step.id,
            true,
            Some("snapshot-1"),
            Some("button is toggled"),
        )
        .expect("conditional replay");
        assert_eq!(replay.expected.as_deref(), Some("button is toggled"));
        let plan = replay_plan(root.path(), Some(&step.id)).expect("plan");
        assert_eq!(plan.steps.len(), 1);
    }

    #[test]
    fn suppressed_values_are_constant_and_not_dictionary_oracles() {
        let root = TempDir::new().expect("root");
        let mut journal = open(root.path(), EvidenceMode::Debug);
        journal
            .record_step(StepInput {
                parent_id: None,
                intent: None,
                expected: Some("1234".into()),
                argv: vec!["type".into(), "1234".into()],
                status: StepStatus::Passed,
                failure_class: None,
                duration_ms: 1,
                retries: 0,
                precondition_refs: vec![],
                postcondition_refs: vec![],
                snapshot_lineage: None,
                artifact_refs: vec![],
            })
            .expect("step");
        journal.close().expect("close");
        let raw = fs::read_to_string(root.path().join("steps.jsonl")).expect("steps");
        assert!(!raw.contains("1234"));
        assert!(!raw.contains("03ac674216f3e15c"));
        assert!(!raw.contains("length="));
        assert!(raw.contains("<suppressed>"));
    }

    #[test]
    fn replay_recomputes_derived_metadata_before_execution() {
        let root = TempDir::new().expect("root");
        let mut journal = open(root.path(), EvidenceMode::Minimal);
        let step = journal
            .record_step(StepInput {
                parent_id: None,
                intent: None,
                expected: None,
                argv: vec!["see".into(), "--json".into()],
                status: StepStatus::Passed,
                failure_class: None,
                duration_ms: 1,
                retries: 0,
                precondition_refs: vec![],
                postcondition_refs: vec![],
                snapshot_lineage: None,
                artifact_refs: vec![],
            })
            .expect("step");
        journal.close().expect("close");
        let path = root.path().join("steps.jsonl");
        let mut row: serde_json::Value =
            serde_json::from_str(fs::read_to_string(&path).expect("steps").trim())
                .expect("step JSON");
        row["replay_argv"] = serde_json::json!(["click", "--on", "B1"]);
        row["argv_shape"] = serde_json::json!(["click", "--on", "B1"]);
        row["expected"] = serde_json::json!("<suppressed>");
        fs::write(
            &path,
            format!("{}\n", serde_json::to_string(&row).expect("row")),
        )
        .expect("rewrite journal");
        assert!(prepare_replay(root.path(), &step.id, true, None, Some("changed")).is_err());
    }

    #[test]
    fn post_append_sequence_failure_is_recovered_by_a_preopened_peer() {
        let root = TempDir::new().expect("root");
        let input = || StepInput {
            parent_id: None,
            intent: None,
            expected: None,
            argv: vec!["see".into()],
            status: StepStatus::Passed,
            failure_class: None,
            duration_ms: 1,
            retries: 0,
            precondition_refs: vec![],
            postcondition_refs: vec![],
            snapshot_lineage: None,
            artifact_refs: vec![],
        };
        let mut journal = open(root.path(), EvidenceMode::Minimal);
        let mut peer = open(root.path(), EvidenceMode::Minimal);
        journal.inject_sequence_write_failure();
        assert!(journal.record_step(input()).is_err());
        let step = peer
            .record_step(input())
            .expect("peer recovers append state");
        assert_eq!(step.sequence, 2);
        let step = journal
            .record_step(input())
            .expect("original writer resumes");
        assert_eq!(step.sequence, 3);
        journal.close().expect("close");
    }

    #[test]
    fn repeated_and_mandatory_failures_become_review_candidates() {
        let root = TempDir::new().expect("root");
        let mut journal = open(root.path(), EvidenceMode::Minimal);
        for class in ["upstream", "upstream", "privacy_redaction"] {
            journal
                .record_step(StepInput {
                    parent_id: None,
                    intent: None,
                    expected: None,
                    argv: vec!["see".into()],
                    status: StepStatus::Failed,
                    failure_class: Some(class.into()),
                    duration_ms: 1,
                    retries: 0,
                    precondition_refs: vec![],
                    postcondition_refs: vec![],
                    snapshot_lineage: None,
                    artifact_refs: vec![],
                })
                .expect("step");
        }
        journal.close().expect("close");
        let review = review(root.path()).expect("review");
        assert!(!review.clean);
        assert_eq!(review.candidates.len(), 2);
        assert!(
            review
                .candidates
                .iter()
                .all(|candidate| candidate.significant)
        );
    }

    #[test]
    fn appending_steps_does_not_rescan_the_full_journal() {
        let root = TempDir::new().expect("root");
        let mut journal = open(root.path(), EvidenceMode::Minimal);
        *STEP_SCAN_ROOT.lock().expect("scan root") = Some(root.path().to_path_buf());
        STEP_SCAN_COUNT.store(0, Ordering::Relaxed);
        for _ in 0..64 {
            journal
                .record_step(StepInput {
                    parent_id: None,
                    intent: None,
                    expected: None,
                    argv: vec!["see".into(), "--json".into()],
                    status: StepStatus::Passed,
                    failure_class: None,
                    duration_ms: 1,
                    retries: 0,
                    precondition_refs: vec![],
                    postcondition_refs: vec![],
                    snapshot_lineage: None,
                    artifact_refs: vec![],
                })
                .expect("append");
        }
        assert_eq!(
            STEP_SCAN_COUNT.load(Ordering::Relaxed),
            0,
            "append path rescanned prior journal history"
        );
        journal.close().expect("close");
        assert_eq!(STEP_SCAN_COUNT.load(Ordering::Relaxed), 1);
        *STEP_SCAN_ROOT.lock().expect("scan root") = None;
    }

    #[test]
    fn home_paths_are_redacted_from_replay() {
        let home = std::env::var("HOME").expect("HOME");
        let (shape, replay) = sanitize_argv(
            &["see".into(), format!("{home}/private.png")],
            EvidenceMode::Minimal,
            ReplayClass::Safe,
        );
        assert!(shape[1].contains("$HOME"));
        assert!(replay.is_none());
    }

    #[test]
    fn concurrent_appenders_receive_unique_monotonic_sequences() {
        let root = TempDir::new().expect("root");
        let root = Arc::new(root);
        let barrier = Arc::new(Barrier::new(8));
        let threads = (0..8)
            .map(|_| {
                let root = Arc::clone(&root);
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    let mut journal = open(root.path(), EvidenceMode::Minimal);
                    barrier.wait();
                    journal
                        .record_step(StepInput {
                            parent_id: None,
                            intent: None,
                            expected: None,
                            argv: vec!["see".into()],
                            status: StepStatus::Passed,
                            failure_class: None,
                            duration_ms: 1,
                            retries: 0,
                            precondition_refs: vec![],
                            postcondition_refs: vec![],
                            snapshot_lineage: None,
                            artifact_refs: vec![],
                        })
                        .expect("append");
                })
            })
            .collect::<Vec<_>>();
        for thread in threads {
            thread.join().expect("thread");
        }
        let (steps, recovered) = read_steps_recover(root.path()).expect("steps");
        assert!(!recovered);
        assert_eq!(steps.len(), 8);
        assert_eq!(
            steps.iter().map(|step| step.sequence).collect::<Vec<_>>(),
            (1..=8).collect::<Vec<_>>()
        );
    }
}
