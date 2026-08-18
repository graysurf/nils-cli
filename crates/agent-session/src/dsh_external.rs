//! External-runtime support for `dsh` worker sessions.
//!
//! DSH workers are never tmux-launched: the dsh-runtime-kit bundle owns the
//! worker agent's lifecycle and maintains a liveness sidecar under the session
//! state directory. This module owns the sidecar contract and the dsh arms of
//! `session_status` / `coordination_runtime_evidence`.
//!
//! Contract: `docs/specs/main-agent-dsh-external-runtime-v1.md`.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    CliContext, CliError, CoordinationRuntimeEvidence, CoordinationRuntimeStatus, SessionRecord,
    coordination,
};

/// `SessionRecord.runtime.kind` for external dsh worker sessions.
pub(crate) const DSH_RUNTIME_KIND: &str = "dsh_external";

/// Versioned schema of the plugin-maintained liveness sidecar.
pub(crate) const LIVENESS_SCHEMA: &str = "main-agent.dsh-runtime-liveness.v1";

/// Sidecar file name inside the session state directory.
pub(crate) const LIVENESS_FILE: &str = "dsh-runtime-liveness.json";

/// `SessionRecord.extra` key naming the absolute sidecar path. Written once at
/// external record creation, trusted exactly like the persisted tmux runtime
/// identity that lives beside it in `extra`.
pub(crate) const LIVENESS_PATH_KEY: &str = "dsh_liveness_file";

/// Bounded sidecar read: the file is a small JSON document; anything larger
/// is treated as invalid evidence rather than parsed.
const MAX_LIVENESS_BYTES: u64 = 64 * 1024;

/// Process identity of the DSH harness process, written by the plugin.
/// `start_time` is the Linux `/proc/<pid>/stat` starttime (clock ticks); when
/// present it pins the exact process incarnation, so a recycled pid cannot
/// masquerade as a live harness.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct DshHarnessIdentity {
    pub(crate) pid: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) start_time: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct DshLaneState {
    /// `open` while the lane may still run turns (idle-but-resumable lanes
    /// stay open: cold resume is always available while the harness lives);
    /// `terminated` once the plugin has permanently stopped the lane.
    pub(crate) state: String,
}

/// Optional turn evidence for the lane, written by the plugin from the
/// harness's own agent status and lifecycle events. When absent, activity
/// evidence stays unknown and diagnosis degrades conservatively.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DshTurnEvidence {
    /// `working` while a turn is running; `waiting` while the lane is idle
    /// but resumable.
    pub(crate) phase: String,
    pub(crate) phase_changed_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) current_turn: Option<DshCurrentTurn>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) last_turn: Option<DshLastTurn>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DshCurrentTurn {
    pub(crate) started_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) last_progress_at: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DshLastTurn {
    pub(crate) completed_at: String,
    /// `completed`, `failed`, or `interrupted`.
    pub(crate) outcome: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DshRuntimeLiveness {
    pub(crate) schema_version: String,
    /// Must equal the session record's `runtime.launch_id`.
    pub(crate) launch_id: String,
    pub(crate) harness: DshHarnessIdentity,
    pub(crate) lane: DshLaneState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) turn: Option<DshTurnEvidence>,
    /// Informational refresh marker; not used as a liveness proof.
    #[serde(default)]
    pub(crate) updated_at: Option<Value>,
}

/// Create the durable session record for an external dsh worker. No process
/// is spawned and no prompt is delivered: the dsh-runtime-kit plugin owns the
/// worker agent's lifecycle. The record carries `runtime.kind = "dsh_external"`
/// (derived from the agent kind at creation) and the absolute liveness sidecar
/// path, so later status and evidence probes need no CLI context.
pub(crate) fn create_external_worker_record(
    context: &CliContext,
    cwd: &std::path::Path,
    worker_session_id: &str,
    prompt: &str,
    coordination_mode: crate::cli::CoordinationMode,
    title: Option<&str>,
    create_guard: Option<&mut dyn FnMut() -> Result<(), CliError>>,
) -> Result<SessionRecord, CliError> {
    let mut created = crate::create_record_with_guard(
        crate::RecordRequest {
            context,
            agent: crate::cli::AgentKind::Dsh,
            // Not "interactive": external records own no startup projection
            // and are never CLI-resumable.
            mode: "external",
            coordination_mode,
            title,
            title_state: None,
            explicit_id: Some(worker_session_id),
            cwd,
            prompt: Some(prompt),
            log_file_name: None,
            provider_resume: None,
            agent_args: Vec::new(),
            agent_bin: None,
        },
        create_guard,
    )?;
    created.record.extra.insert(
        LIVENESS_PATH_KEY.to_string(),
        json!(crate::display_path(&liveness_path(
            context,
            worker_session_id
        ))),
    );
    if let Err(error) = crate::write_session_record(context, &created.record) {
        crate::cleanup_created_record(context, &created);
        return Err(error);
    }
    Ok(created.record.clone())
}

pub(crate) fn is_external_record(record: &SessionRecord) -> bool {
    record
        .runtime
        .as_ref()
        .is_some_and(|runtime| runtime.kind == DSH_RUNTIME_KIND)
}

pub(crate) fn liveness_path(context: &CliContext, session_id: &str) -> PathBuf {
    crate::session_dir(context, session_id).join(LIVENESS_FILE)
}

/// The record-declared sidecar path, or `None` when the record does not carry
/// a usable absolute path (treated as invalid evidence by callers).
fn recorded_liveness_path(record: &SessionRecord) -> Option<PathBuf> {
    let path = record.extra.get(LIVENESS_PATH_KEY)?.as_str()?;
    let path = Path::new(path);
    path.is_absolute().then(|| path.to_path_buf())
}

/// Typed outcome of reading the sidecar against a specific session record.
pub(crate) enum LivenessEvidence {
    /// No sidecar exists: the external runtime has not attached (or evidence
    /// was cleaned); nothing is proven.
    Missing,
    /// A sidecar exists but is unreadable, malformed, oversized, schema- or
    /// launch-mismatched; nothing is proven for this launch.
    Invalid,
    /// A structurally valid sidecar for this exact launch.
    Present(Box<DshRuntimeLiveness>),
}

pub(crate) fn read_liveness(record: &SessionRecord) -> LivenessEvidence {
    let Some(path) = recorded_liveness_path(record) else {
        return LivenessEvidence::Invalid;
    };
    let file = match fs::File::open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return LivenessEvidence::Missing;
        }
        Err(_) => return LivenessEvidence::Invalid,
    };
    let mut bytes = Vec::new();
    if file
        .take(MAX_LIVENESS_BYTES + 1)
        .read_to_end(&mut bytes)
        .is_err()
        || bytes.len() as u64 > MAX_LIVENESS_BYTES
    {
        return LivenessEvidence::Invalid;
    }
    let Ok(liveness) = serde_json::from_slice::<DshRuntimeLiveness>(&bytes) else {
        return LivenessEvidence::Invalid;
    };
    let current_launch_id = record
        .runtime
        .as_ref()
        .map(|runtime| runtime.launch_id.as_str())
        .filter(|value| !value.is_empty());
    if liveness.schema_version != LIVENESS_SCHEMA
        || liveness.harness.pid <= 1
        || liveness.harness.start_time == Some(0)
        || !matches!(liveness.lane.state.as_str(), "open" | "terminated")
        || current_launch_id != Some(liveness.launch_id.as_str())
    {
        return LivenessEvidence::Invalid;
    }
    LivenessEvidence::Present(Box::new(liveness))
}

/// Liveness of the harness process named by the sidecar. `Stopped` requires a
/// proof (ESRCH, or a start-time mismatch proving pid reuse); permission and
/// read errors stay `Unknown`, matching the conservative tmux behavior.
pub(crate) fn harness_process_status(identity: &DshHarnessIdentity) -> CoordinationRuntimeStatus {
    let alive = unsafe { libc::kill(identity.pid, 0) };
    if alive != 0 {
        return match std::io::Error::last_os_error().raw_os_error() {
            Some(libc::ESRCH) => CoordinationRuntimeStatus::Stopped,
            Some(libc::EPERM) => CoordinationRuntimeStatus::Running,
            _ => CoordinationRuntimeStatus::Unknown,
        };
    }
    match (identity.start_time, linux_process_start_time(identity.pid)) {
        (Some(expected), Some(actual)) if expected != actual => CoordinationRuntimeStatus::Stopped,
        (Some(_), None) => CoordinationRuntimeStatus::Unknown,
        _ => CoordinationRuntimeStatus::Running,
    }
}

#[cfg(target_os = "linux")]
fn linux_process_start_time(pid: i32) -> Option<u64> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // Field 22 (starttime) counted after the parenthesized comm field, which
    // may itself contain spaces and parentheses.
    let after_comm = stat.rsplit_once(')')?.1;
    after_comm.split_whitespace().nth(19)?.parse().ok()
}

#[cfg(not(target_os = "linux"))]
fn linux_process_start_time(_pid: i32) -> Option<u64> {
    None
}

/// `session_status` for external dsh records: `missing` when nothing is
/// proven for this launch, `stopped` when the lane is terminated or the
/// harness process is proven gone, `running` while the lane is open under a
/// live harness (idle lanes stay `running`; cold resume is always available).
pub(crate) fn external_session_status(record: &SessionRecord) -> String {
    match read_liveness(record) {
        LivenessEvidence::Missing | LivenessEvidence::Invalid => "missing".to_string(),
        LivenessEvidence::Present(liveness) => {
            if liveness.lane.state == "terminated" {
                return "stopped".to_string();
            }
            match harness_process_status(&liveness.harness) {
                CoordinationRuntimeStatus::Running => "running".to_string(),
                CoordinationRuntimeStatus::Stopped => "stopped".to_string(),
                CoordinationRuntimeStatus::Unknown => "missing".to_string(),
            }
        }
    }
}

/// Synthesized activity evidence for an external dsh lane. `Some` only when
/// the sidecar is valid for this launch, the lane is open, the harness is
/// proven live, and the plugin supplied turn evidence with a known phase;
/// everything else stays `None` so diagnosis degrades conservatively.
pub(crate) fn external_turn_state(record: &SessionRecord) -> Option<crate::activity::TurnState> {
    let LivenessEvidence::Present(liveness) = read_liveness(record) else {
        return None;
    };
    if liveness.lane.state != "open"
        || harness_process_status(&liveness.harness) != CoordinationRuntimeStatus::Running
    {
        return None;
    }
    let turn = liveness.turn.as_ref()?;
    if !matches!(turn.phase.as_str(), "working" | "waiting") {
        return None;
    }
    serde_json::from_value(json!({
        "schema_version": crate::activity::TURN_STATE_VERSION,
        "phase": turn.phase,
        "phase_changed_at": turn.phase_changed_at,
        "revision": 0,
        "source": {
            "kind": "runtime",
            "provider": "dsh",
            "confidence": "authoritative"
        },
        "current_turn": turn.current_turn.as_ref().map(|current| json!({
            "started_at": current.started_at,
            "last_progress_at": current.last_progress_at
        })),
        "last_turn": turn.last_turn.as_ref().map(|last| json!({
            "completed_at": last.completed_at,
            "outcome": last.outcome
        }))
    }))
    .ok()
}

/// `coordination_runtime_evidence` for external dsh records. The identity is
/// the sidecar's launch-bound harness identity; missing or invalid sidecars
/// fail with the same typed error as an unavailable tmux runtime identity.
pub(crate) fn external_runtime_evidence(
    record: &SessionRecord,
) -> Result<CoordinationRuntimeEvidence, CliError> {
    let liveness = match read_liveness(record) {
        LivenessEvidence::Present(liveness) => liveness,
        LivenessEvidence::Missing => {
            return Err(CliError::runtime(
                "coordination-runtime-unverified",
                "external dsh runtime liveness evidence is unavailable",
                None,
            ));
        }
        LivenessEvidence::Invalid => {
            return Err(CliError::runtime(
                "coordination-runtime-unverified",
                "external dsh runtime liveness evidence is invalid",
                None,
            ));
        }
    };
    let identity = json!({
        "kind": DSH_RUNTIME_KIND,
        "launch_id": liveness.launch_id,
        "harness": liveness.harness,
    });
    let bytes = serde_json::to_vec(&identity).map_err(|_| {
        CliError::runtime(
            "coordination-runtime-unverified",
            "external dsh runtime identity could not be canonicalized",
            None,
        )
    })?;
    let status = if liveness.lane.state == "terminated" {
        CoordinationRuntimeStatus::Stopped
    } else {
        harness_process_status(&liveness.harness)
    };
    Ok(CoordinationRuntimeEvidence {
        identity_digest: coordination::digest_bytes(&bytes),
        identity,
        status,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn liveness_json(launch_id: &str, pid: i32, lane: &str) -> String {
        json!({
            "schema_version": LIVENESS_SCHEMA,
            "launch_id": launch_id,
            "harness": { "pid": pid },
            "lane": { "state": lane },
            "updated_at": "0"
        })
        .to_string()
    }

    #[test]
    fn liveness_parses_only_the_exact_versioned_shape() {
        let valid: DshRuntimeLiveness =
            serde_json::from_str(&liveness_json("launch-1", 4242, "open")).expect("valid sidecar");
        assert_eq!(valid.schema_version, LIVENESS_SCHEMA);
        assert_eq!(valid.launch_id, "launch-1");
        assert_eq!(valid.harness.pid, 4242);
        assert_eq!(valid.lane.state, "open");

        let unknown_field = serde_json::from_str::<DshRuntimeLiveness>(
            &json!({
                "schema_version": LIVENESS_SCHEMA,
                "launch_id": "launch-1",
                "harness": { "pid": 4242 },
                "lane": { "state": "open" },
                "surprise": true
            })
            .to_string(),
        );
        assert!(unknown_field.is_err(), "unknown fields are rejected");

        let unknown_harness_field = serde_json::from_str::<DshRuntimeLiveness>(
            &json!({
                "schema_version": LIVENESS_SCHEMA,
                "launch_id": "launch-1",
                "harness": { "pid": 4242, "argv": ["node"] },
                "lane": { "state": "open" }
            })
            .to_string(),
        );
        assert!(
            unknown_harness_field.is_err(),
            "nested unknown fields are rejected"
        );
    }

    #[test]
    fn harness_status_distinguishes_live_dead_and_recycled_processes() {
        let live = DshHarnessIdentity {
            pid: std::process::id() as i32,
            start_time: None,
        };
        assert_eq!(
            harness_process_status(&live),
            CoordinationRuntimeStatus::Running
        );

        #[cfg(target_os = "linux")]
        {
            let pinned = DshHarnessIdentity {
                pid: std::process::id() as i32,
                start_time: linux_process_start_time(std::process::id() as i32),
            };
            assert!(pinned.start_time.is_some(), "own start time is readable");
            assert_eq!(
                harness_process_status(&pinned),
                CoordinationRuntimeStatus::Running
            );
            let recycled = DshHarnessIdentity {
                pid: std::process::id() as i32,
                start_time: Some(pinned.start_time.expect("pinned start time") + 1),
            };
            assert_eq!(
                harness_process_status(&recycled),
                CoordinationRuntimeStatus::Stopped,
                "a start-time mismatch proves pid reuse"
            );
        }
    }
}
