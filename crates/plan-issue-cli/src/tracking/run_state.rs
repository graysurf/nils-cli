//! Typed local run-state for `plan-issue tracking`.
//!
//! Schema identifier: `plan-issue.execution-run.v1`. File layout under the
//! existing state-dir contract is documented in
//! `docs/source/plan-issue-redesign/plan-tracking-issue-run-state-controller-v1.md`.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::runtime_layout::{self, IssueRoot};

/// Stable schema identifier embedded in every `run-state.json` document.
pub const RUN_STATE_SCHEMA: &str = "plan-issue.execution-run.v1";

const RUNS_DIR: &str = "runs";
const RUN_STATE_FILE: &str = "run-state.json";
const EVENTS_FILE: &str = "events.jsonl";
const INPUTS_DIR: &str = "inputs";
const RENDERED_DIR: &str = "rendered";
const ARTIFACTS_DIR: &str = "artifacts";

/// High-level execution phase. Maps onto FSM states but stays human-friendly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunPhase {
    Initial,
    Implementing,
    Validating,
    Reviewing,
    Blocked,
    ReadyForClose,
    Closed,
}

impl RunPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Initial => "initial",
            Self::Implementing => "implementing",
            Self::Validating => "validating",
            Self::Reviewing => "reviewing",
            Self::Blocked => "blocked",
            Self::ReadyForClose => "ready_for_close",
            Self::Closed => "closed",
        }
    }
}

/// Selected scope (sprint, task, title) recorded in the run.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SelectedScope {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sprint: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

/// Linked PR reference captured in the run state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkedPr {
    #[serde(rename = "ref")]
    pub r#ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
}

/// Compact validation summary captured in run state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationSummary {
    pub overall: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commands: Vec<ValidationCommandRow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waiver: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationCommandRow {
    pub command: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<String>,
}

/// Compact review summary captured in run state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewSummary {
    pub decision: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub findings_disposition: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<String>,
}

/// Captured reconciliation snapshot from a prior FSM evaluation.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LastReconciled {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fsm_state: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dashboard_status: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub latest_comments: BTreeMap<String, String>,
}

/// Pending transition the controller will perform next.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingTransition {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Typed run state document persisted as `run-state.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionRun {
    pub schema: String,
    pub run_id: String,
    pub repo: String,
    pub issue: u64,
    pub profile: String,
    pub phase: RunPhase,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bundle: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_state_file: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_scope: Option<SelectedScope>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr: Option<LinkedPr>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_reconciled: Option<LastReconciled>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_transition: Option<PendingTransition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub validation: Option<ValidationSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<ReviewSummary>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub artifacts: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
    /// Free-form extra fields kept around so future schema additions do not
    /// silently drop on read/write round-trip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<Value>,
}

impl ExecutionRun {
    pub fn new(
        run_id: impl Into<String>,
        repo: impl Into<String>,
        issue: u64,
        profile: impl Into<String>,
        phase: RunPhase,
        at: impl Into<String>,
    ) -> Self {
        let at = at.into();
        Self {
            schema: RUN_STATE_SCHEMA.to_string(),
            run_id: run_id.into(),
            repo: repo.into(),
            issue,
            profile: profile.into(),
            phase,
            created_at: at.clone(),
            updated_at: at,
            bundle: None,
            execution_state_file: None,
            selected_scope: None,
            branch: None,
            worktree: None,
            pr: None,
            last_reconciled: None,
            pending_transition: None,
            validation: None,
            review: None,
            artifacts: BTreeMap::new(),
            notes: Vec::new(),
            extra: None,
        }
    }
}

/// Validation errors emitted by [`parse_run_state`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunStateError {
    SchemaMismatch { actual: String },
    MissingField(&'static str),
    Malformed(String),
}

impl std::fmt::Display for RunStateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SchemaMismatch { actual } => write!(
                f,
                "run-state.json schema mismatch: expected {RUN_STATE_SCHEMA}, got {actual}"
            ),
            Self::MissingField(name) => write!(f, "run-state.json missing required field `{name}`"),
            Self::Malformed(msg) => write!(f, "run-state.json malformed: {msg}"),
        }
    }
}

impl std::error::Error for RunStateError {}

/// Parse a `run-state.json` body and validate the schema id and required
/// fields.
pub fn parse_run_state(raw: &str) -> Result<ExecutionRun, RunStateError> {
    let value: Value = serde_json::from_str(raw).map_err(|err| {
        RunStateError::Malformed(format!("failed to parse JSON: {err}"))
    })?;
    let schema = value
        .get("schema")
        .and_then(Value::as_str)
        .ok_or(RunStateError::MissingField("schema"))?;
    if schema != RUN_STATE_SCHEMA {
        return Err(RunStateError::SchemaMismatch {
            actual: schema.to_string(),
        });
    }
    for required in ["run_id", "repo", "issue", "profile", "phase", "created_at", "updated_at"] {
        if value.get(required).is_none() {
            return Err(RunStateError::MissingField(name_for(required)));
        }
    }
    serde_json::from_value::<ExecutionRun>(value)
        .map_err(|err| RunStateError::Malformed(err.to_string()))
}

fn name_for(field: &str) -> &'static str {
    match field {
        "run_id" => "run_id",
        "repo" => "repo",
        "issue" => "issue",
        "profile" => "profile",
        "phase" => "phase",
        "created_at" => "created_at",
        "updated_at" => "updated_at",
        _ => "unknown",
    }
}

/// Serialize an [`ExecutionRun`] to canonical JSON.
pub fn render_run_state(run: &ExecutionRun) -> Result<String, RunStateError> {
    serde_json::to_string_pretty(run).map_err(|err| RunStateError::Malformed(err.to_string()))
}

/// Read `run-state.json` from disk.
pub fn read_run_state(path: &Path) -> io::Result<ExecutionRun> {
    let raw = fs::read_to_string(path)?;
    parse_run_state(&raw).map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))
}

/// Write `run-state.json` to disk. Creates parent directories as needed.
pub fn write_run_state(path: &Path, run: &ExecutionRun) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        runtime_layout::ensure_dir(parent)?;
    }
    let rendered = render_run_state(run)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    fs::write(path, rendered)
}

/// Issue-scoped run root rooted under the existing
/// [`crate::runtime_layout::IssueRoot`] contract.
#[derive(Debug, Clone)]
pub struct RunRoot {
    issue_root: IssueRoot,
    run_id: String,
}

impl RunRoot {
    /// Build the run root under `<issue-root>/runs/<run-id>/`.
    pub fn new(
        repo_slug: &str,
        issue_number: u64,
        run_id: impl Into<String>,
    ) -> Result<Self, runtime_layout::RuntimeLayoutError> {
        let issue_root = IssueRoot::new(repo_slug, issue_number)?;
        Ok(Self {
            issue_root,
            run_id: run_id.into(),
        })
    }

    pub fn issue_root(&self) -> &IssueRoot {
        &self.issue_root
    }

    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    pub fn root(&self) -> PathBuf {
        self.issue_root.root().join(RUNS_DIR).join(&self.run_id)
    }

    pub fn run_state_path(&self) -> PathBuf {
        self.root().join(RUN_STATE_FILE)
    }

    pub fn events_path(&self) -> PathBuf {
        self.root().join(EVENTS_FILE)
    }

    pub fn inputs_dir(&self) -> PathBuf {
        self.root().join(INPUTS_DIR)
    }

    pub fn rendered_dir(&self) -> PathBuf {
        self.root().join(RENDERED_DIR)
    }

    pub fn artifacts_dir(&self) -> PathBuf {
        self.root().join(ARTIFACTS_DIR)
    }

    /// Ensure the full directory tree exists.
    pub fn ensure_layout(&self) -> io::Result<()> {
        runtime_layout::ensure_dir(&self.root())?;
        runtime_layout::ensure_dir(&self.inputs_dir())?;
        runtime_layout::ensure_dir(&self.rendered_dir())?;
        runtime_layout::ensure_dir(&self.artifacts_dir())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tracking_run_state_round_trips_required_and_recommended_fields() {
        let mut run = ExecutionRun::new(
            "20260526-150405-issue-123",
            "owner/repo",
            123,
            "tracking",
            RunPhase::Implementing,
            "2026-05-26T15:04:05Z",
        );
        run.bundle = Some(PathBuf::from("docs/plans/example"));
        run.execution_state_file = Some(PathBuf::from(
            "docs/plans/example/example-execution-state.md",
        ));
        run.selected_scope = Some(SelectedScope {
            sprint: Some(1),
            task: Some("1.2".to_string()),
            title: Some("visible lint".to_string()),
        });
        run.branch = Some("feat/x".to_string());
        run.pr = Some(LinkedPr {
            r#ref: "owner/repo#456".to_string(),
            url: Some("https://example.com/pr/456".to_string()),
            status: Some("open".to_string()),
        });
        run.validation = Some(ValidationSummary {
            overall: "pass".to_string(),
            commands: vec![ValidationCommandRow {
                command: "cargo test".to_string(),
                status: "pass".to_string(),
                evidence: Some("log.txt".to_string()),
            }],
            waiver: None,
            evidence_path: None,
        });
        let rendered = render_run_state(&run).expect("render");
        let parsed = parse_run_state(&rendered).expect("parse");
        assert_eq!(parsed.run_id, run.run_id);
        assert_eq!(parsed.repo, run.repo);
        assert_eq!(parsed.issue, run.issue);
        assert_eq!(parsed.phase, run.phase);
        assert_eq!(
            parsed
                .selected_scope
                .as_ref()
                .and_then(|s| s.task.clone()),
            Some("1.2".to_string())
        );
        assert_eq!(
            parsed.validation.as_ref().map(|v| v.overall.clone()),
            Some("pass".to_string())
        );
    }

    #[test]
    fn tracking_run_state_rejects_wrong_schema() {
        let raw = json!({
            "schema": "wrong.schema.v1",
            "run_id": "x",
            "repo": "o/r",
            "issue": 1,
            "profile": "tracking",
            "phase": "initial",
            "created_at": "x",
            "updated_at": "x"
        })
        .to_string();
        match parse_run_state(&raw) {
            Err(RunStateError::SchemaMismatch { actual }) => assert_eq!(actual, "wrong.schema.v1"),
            other => panic!("expected SchemaMismatch, got {other:?}"),
        }
    }

    #[test]
    fn tracking_run_state_rejects_missing_required_field() {
        let raw = json!({
            "schema": RUN_STATE_SCHEMA,
            "run_id": "x",
            "issue": 1,
            "profile": "tracking",
            "phase": "initial",
            "created_at": "x",
            "updated_at": "x"
        })
        .to_string();
        match parse_run_state(&raw) {
            Err(RunStateError::MissingField("repo")) => {}
            other => panic!("expected MissingField(repo), got {other:?}"),
        }
    }

    #[test]
    fn tracking_run_state_runroot_layout_under_state_dir() {
        let runroot = RunRoot::new("owner__repo", 123, "20260526-run-1").expect("runroot");
        let expected_root_suffix = "owner__repo/issue-123/runs/20260526-run-1";
        assert!(
            runroot
                .root()
                .to_string_lossy()
                .ends_with(expected_root_suffix),
            "runroot suffix drift: {}",
            runroot.root().display()
        );
        assert!(
            runroot
                .run_state_path()
                .to_string_lossy()
                .ends_with("runs/20260526-run-1/run-state.json")
        );
        assert!(
            runroot
                .events_path()
                .to_string_lossy()
                .ends_with("runs/20260526-run-1/events.jsonl")
        );
    }
}
