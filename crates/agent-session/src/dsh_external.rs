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
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
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

/// Converge a record whose liveness-path key was lost between the record write
/// and the extra-key write (a crash in that window). The path is derivable, so
/// a replay repairs it instead of failing permanently.
pub(crate) fn ensure_recorded_liveness_path(
    context: &CliContext,
    record: SessionRecord,
) -> Result<SessionRecord, CliError> {
    // Repair a missing key (a crash between the two writes) and also a shaped
    // but foreign path, so a tampered record converges on the derived path
    // instead of quietly sourcing evidence from somewhere else.
    if !is_external_record(&record) || recorded_liveness_path_is_derived(context, &record) {
        return Ok(record);
    }
    let expected = json!(crate::display_path(&liveness_path(context, &record.id)));
    crate::mutate_session_record(context, &record.id, |current| {
        current
            .extra
            .insert(LIVENESS_PATH_KEY.to_string(), expected.clone());
        Ok(current.clone())
    })
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
/// a usable absolute path (treated as invalid evidence by callers). The path
/// must also be the exact conventional file inside the record's own session
/// directory: a record-supplied path pointing anywhere else is not evidence
/// about this lane.
fn recorded_liveness_path(record: &SessionRecord) -> Option<PathBuf> {
    let raw = record.extra.get(LIVENESS_PATH_KEY)?.as_str()?;
    let path = Path::new(raw);
    if !path.is_absolute() || path.file_name()? != LIVENESS_FILE {
        return None;
    }
    if path.parent()?.file_name()? != record.id.as_str() {
        return None;
    }
    // Reject traversal outright: a normalizing comparison would accept
    // `.../sessions/<id>/../../elsewhere/<id>/<file>` because both sides
    // normalize the same way, so the recorded path must already be canonical.
    if crate::display_path(path) != raw {
        return None;
    }
    if path
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return None;
    }
    Some(path.to_path_buf())
}

/// Whether the record's sidecar path is the exact path this CLI derives for it.
/// Only a context-bearing caller can check this; the context-free reader keeps
/// the shape checks above.
fn recorded_liveness_path_is_derived(context: &CliContext, record: &SessionRecord) -> bool {
    recorded_liveness_path(record)
        .is_some_and(|recorded| recorded == liveness_path(context, &record.id))
}

/// The stop proof a destructive operation requires, which is deliberately
/// stronger than the status projection.
///
/// The sidecar lives in the same-uid state directory a lane's own worker can
/// reach, so sidecar evidence alone is forgeable: a live worker could name a
/// dead pid and have its own lane terminalized. Requiring the lane's broker
/// heartbeat — a separate file maintained by a separate process the plugin
/// owns — to be absent or stale means a forged stop must also stop the real
/// coordination heartbeat, which is itself an observable loss of authority.
/// This raises the bar; it is not an OS boundary, and the same-uid caveat in
/// the spec still stands.
///
/// It holds when either the plugin never attached to this launch, or its
/// published termination is corroborated. Both additionally require the lane's
/// coordination heartbeat to be gone, so a lane that is live enough to hold
/// coordination authority can never be terminalized through an absent or forged
/// sidecar.
///
/// `NeverAttached` is included deliberately: without it a lane whose plugin
/// never took the launch has no terminal disposition at all, and its assignment
/// can be neither cancelled nor reassigned.
pub(crate) fn external_lane_terminal_is_proven(
    context: &CliContext,
    record: &SessionRecord,
    incarnation: &str,
) -> bool {
    if !recorded_liveness_path_is_derived(context, record) {
        return false;
    }
    if !matches!(
        external_lane_disposition_with_broker(context, record),
        ExternalLaneDisposition::NeverAttached | ExternalLaneDisposition::ProvenStopped
    ) {
        return false;
    }
    !coordination::broker::heartbeat_fresh(context, &record.id, incarnation, 0)
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
    // Evidence that gates destructive operations is opened with the same trust
    // bar as the coordination files beside it: never follow a symlink, and
    // accept only an owner-only, single-link regular file.
    let file = match fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return LivenessEvidence::Missing;
        }
        Err(_) => return LivenessEvidence::Invalid,
    };
    let Ok(metadata) = file.metadata() else {
        return LivenessEvidence::Invalid;
    };
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.nlink() != 1
        || metadata.permissions().mode() & 0o077 != 0
        || metadata.len() > MAX_LIVENESS_BYTES
    {
        return LivenessEvidence::Invalid;
    }
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
        match std::io::Error::last_os_error().raw_os_error() {
            Some(libc::ESRCH) => return CoordinationRuntimeStatus::Stopped,
            // EPERM proves some process holds the pid but says nothing about
            // which incarnation, so it still needs the starttime pin below.
            Some(libc::EPERM) => {}
            _ => return CoordinationRuntimeStatus::Unknown,
        }
    }
    match (identity.start_time, linux_process_start_time(identity.pid)) {
        // A different starttime on the same pid proves the pinned incarnation
        // is gone and the pid was recycled.
        (Some(expected), Some(actual)) if expected != actual => CoordinationRuntimeStatus::Stopped,
        (Some(_), Some(_)) => CoordinationRuntimeStatus::Running,
        // Where a starttime source exists, a live pid without a verified pin
        // cannot prove this incarnation, so liveness stays undecided rather
        // than vouching for a possibly recycled pid.
        _ if INCARNATION_PIN_AVAILABLE => CoordinationRuntimeStatus::Unknown,
        // On platforms with no starttime source the pid signal is the only
        // liveness evidence that exists; treating it as undecided would make
        // every lane permanently unterminalizable. Pid reuse therefore remains
        // an accepted residual risk there, exactly as it is for the tmux
        // process-group probe on the same platforms.
        _ => CoordinationRuntimeStatus::Running,
    }
}

/// Whether this platform can pin a process incarnation by starttime. When it
/// can, a liveness claim without a verified pin is undecided rather than live.
#[cfg(target_os = "linux")]
const INCARNATION_PIN_AVAILABLE: bool = true;

#[cfg(not(target_os = "linux"))]
const INCARNATION_PIN_AVAILABLE: bool = false;

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

/// Why a lane's liveness could not be proven. Destructive callers report the
/// reason instead of treating unproven liveness as absence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UnprovenReason {
    /// A sidecar exists but is unreadable, malformed, oversized, or bound to a
    /// different launch than this record's.
    InvalidEvidence,
    /// The sidecar is valid but the pinned harness process state cannot be
    /// decided (permission, unreadable `/proc`, or a missing starttime pin).
    UndecidableHarness,
    /// The sidecar claims the lane is terminated, but the lane's coordination
    /// heartbeat is still fresh: it retains the authority a stopped lane gives
    /// up, so the claim has no second witness.
    LaneRetainsCoordinationAuthority,
}

impl UnprovenReason {
    pub(crate) fn message(self) -> &'static str {
        match self {
            Self::InvalidEvidence => "external dsh runtime liveness evidence is invalid",
            Self::UndecidableHarness => {
                "external dsh runtime liveness could not be decided for the pinned harness"
            }
            Self::LaneRetainsCoordinationAuthority => {
                "external dsh lane termination is uncorroborated while its coordination \
                 heartbeat stays fresh"
            }
        }
    }
}

/// What the lane's own coordination heartbeat says about its authority.
///
/// The plugin releases a lane's heartbeat when it closes that lane, so the
/// heartbeat is a second witness to the sidecar's termination claim, owned by a
/// different process and observed through a different file. A forged stop has
/// to stop the real heartbeat too, which is itself a loss of authority the
/// store can see.
///
/// This witness is deliberately weaker than the harness one, and the spec's
/// "residual risk, stated exactly" paragraph says how: both files are same-uid,
/// so a hostile lane can kill its own heartbeat and keep running, whereas it
/// cannot kill the harness without killing itself. What it gives up in exchange
/// is every authenticated call it could still make.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LaneBrokerWitness {
    /// The heartbeat is fresh: this lane still holds coordination authority.
    Live,
    /// The heartbeat is absent or stale: the authority is gone.
    Released,
    /// The witness could not be read — no context, or no incarnation on the
    /// record. Treated exactly like a live lane so a reader that cannot check
    /// never widens what is provable.
    Unread,
}

fn lane_broker_witness(context: &CliContext, record: &SessionRecord) -> LaneBrokerWitness {
    let Ok(incarnation) = coordination::incarnation(record) else {
        return LaneBrokerWitness::Unread;
    };
    if coordination::broker::heartbeat_fresh(context, &record.id, &incarnation, 0) {
        LaneBrokerWitness::Live
    } else {
        LaneBrokerWitness::Released
    }
}

/// The four lane states a destructive or diagnostic caller must distinguish.
/// Collapsing `Unproven` into "absent" would let a corrupted sidecar authorize
/// destroying the record of a running lane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ExternalLaneDisposition {
    /// No sidecar exists: the external runtime never attached to this launch.
    NeverAttached,
    Running,
    ProvenStopped,
    Unproven(UnprovenReason),
}

/// Classify without a broker witness. Every reader that can supply one should
/// use [`external_lane_disposition_with_broker`]: without it a lane the plugin
/// closed stays unproven until the whole harness exits.
pub(crate) fn external_lane_disposition(record: &SessionRecord) -> ExternalLaneDisposition {
    lane_disposition(record, LaneBrokerWitness::Unread)
}

/// Classify with the lane's coordination heartbeat as a second witness.
pub(crate) fn external_lane_disposition_with_broker(
    context: &CliContext,
    record: &SessionRecord,
) -> ExternalLaneDisposition {
    lane_disposition(record, lane_broker_witness(context, record))
}

fn lane_disposition(record: &SessionRecord, broker: LaneBrokerWitness) -> ExternalLaneDisposition {
    match read_liveness(record) {
        LivenessEvidence::Missing => ExternalLaneDisposition::NeverAttached,
        LivenessEvidence::Invalid => {
            ExternalLaneDisposition::Unproven(UnprovenReason::InvalidEvidence)
        }
        LivenessEvidence::Present(liveness) => {
            let harness = harness_process_status(&liveness.harness);
            if liveness.lane.state == "terminated" {
                // Plugin-asserted termination, deliberately weaker than the
                // tmux kernel-backed proof: the sidecar lives in the same-uid
                // state dir a lane's own worker can reach. It needs a second
                // witness owned by a different process — either the pinned
                // harness proven gone, or this lane's coordination heartbeat
                // released. One of the two is enough; a lane cannot be live
                // enough to hold coordination authority and terminated at the
                // same time.
                return match (harness, broker) {
                    (CoordinationRuntimeStatus::Stopped, _) => {
                        ExternalLaneDisposition::ProvenStopped
                    }
                    // The plugin releases a lane's heartbeat when it closes the
                    // lane, so a released heartbeat corroborates the claim even
                    // while the harness keeps serving its other lanes.
                    (_, LaneBrokerWitness::Released) => ExternalLaneDisposition::ProvenStopped,
                    (_, LaneBrokerWitness::Live) => ExternalLaneDisposition::Unproven(
                        UnprovenReason::LaneRetainsCoordinationAuthority,
                    ),
                    (_, LaneBrokerWitness::Unread) => {
                        ExternalLaneDisposition::Unproven(UnprovenReason::UndecidableHarness)
                    }
                };
            }
            match harness {
                CoordinationRuntimeStatus::Running => ExternalLaneDisposition::Running,
                CoordinationRuntimeStatus::Stopped => ExternalLaneDisposition::ProvenStopped,
                CoordinationRuntimeStatus::Unknown => {
                    ExternalLaneDisposition::Unproven(UnprovenReason::UndecidableHarness)
                }
            }
        }
    }
}

/// `session_status` for external dsh records: `missing` when the runtime never
/// attached, `stopped` when termination is proven, `running` while the lane is
/// open under a live harness (idle lanes stay `running`; cold resume is always
/// available), and `unknown` when liveness cannot be proven.
pub(crate) fn external_session_status(context: &CliContext, record: &SessionRecord) -> String {
    match external_lane_disposition_with_broker(context, record) {
        ExternalLaneDisposition::NeverAttached => "missing".to_string(),
        ExternalLaneDisposition::Running => "running".to_string(),
        ExternalLaneDisposition::ProvenStopped => "stopped".to_string(),
        ExternalLaneDisposition::Unproven(_) => "unknown".to_string(),
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
    context: &CliContext,
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
    // `lane.state` is part of the identity: without it a plugin-asserted
    // termination flip leaves the digest unchanged, so a consumer comparing
    // digests could not see that the evidence changed at all.
    let identity = json!({
        "kind": DSH_RUNTIME_KIND,
        "launch_id": liveness.launch_id,
        "harness": liveness.harness,
        "lane_state": liveness.lane.state,
    });
    let bytes = serde_json::to_vec(&identity).map_err(|_| {
        CliError::runtime(
            "coordination-runtime-unverified",
            "external dsh runtime identity could not be canonicalized",
            None,
        )
    })?;
    // One function owns the corroboration rule. Deriving the status here from
    // the disposition keeps this evidence and the destructive-operation gate
    // from disagreeing about what a plugin-asserted termination proves — which
    // is why it must read the *same* disposition, broker witness included. With
    // the context-free reader here, `worker reconcile-stopped` refused a lane
    // that `agent-session delete` and `session_status` already called stopped.
    let status = match external_lane_disposition_with_broker(context, record) {
        ExternalLaneDisposition::Running => CoordinationRuntimeStatus::Running,
        ExternalLaneDisposition::ProvenStopped => CoordinationRuntimeStatus::Stopped,
        ExternalLaneDisposition::NeverAttached | ExternalLaneDisposition::Unproven(_) => {
            CoordinationRuntimeStatus::Unknown
        }
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
        // Unpinned identity: a live pid alone cannot prove which incarnation
        // holds it, so liveness stays undecided.
        let unpinned = DshHarnessIdentity {
            pid: std::process::id() as i32,
            start_time: None,
        };
        assert_eq!(
            harness_process_status(&unpinned),
            if INCARNATION_PIN_AVAILABLE {
                CoordinationRuntimeStatus::Unknown
            } else {
                CoordinationRuntimeStatus::Running
            },
            "an unpinned pid is undecided wherever a starttime pin exists"
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

    /// Build an external record whose sidecar path points into a scratch dir.
    fn external_record(root: &Path, launch_id: &str) -> SessionRecord {
        let session_dir = root.join("sessions").join("worker-dsh");
        fs::create_dir_all(&session_dir).expect("session dir");
        let mut record: SessionRecord = serde_json::from_value(json!({
            "schema_version": "agent-session.session.v1",
            "id": "worker-dsh",
            "agent": "dsh",
            "mode": "external",
            "title": null,
            "cwd": "/lane/worktree",
            "tmux_session": "",
            "prompt_file": null,
            "log_file": null,
            "created_at": "0",
            "updated_at": "0",
            "runtime": {
                "kind": DSH_RUNTIME_KIND,
                "tmux_session": "",
                "generation": 1,
                "started_at": "0",
                "launch_id": launch_id
            }
        }))
        .expect("external record");
        record.extra.insert(
            LIVENESS_PATH_KEY.to_string(),
            json!(session_dir.join(LIVENESS_FILE).to_string_lossy()),
        );
        record
    }

    fn write_sidecar(record: &SessionRecord, body: &str, mode: u32) {
        let path = recorded_liveness_path(record).expect("recorded path");
        fs::write(&path, body).expect("write sidecar");
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).expect("mode");
    }

    fn pinned_harness() -> serde_json::Value {
        let pid = std::process::id() as i32;
        match linux_process_start_time(pid) {
            Some(start_time) => json!({ "pid": pid, "start_time": start_time }),
            None => json!({ "pid": pid }),
        }
    }

    #[test]
    fn lane_disposition_requires_positive_evidence() {
        let scratch = tempfile::TempDir::new().expect("tempdir");
        let record = external_record(scratch.path(), "launch-1");
        // This lane never writes a heartbeat, so the broker witness is
        // `Released` throughout: every case below turns on sidecar evidence.
        let context = CliContext {
            state_dir: scratch.path().to_path_buf(),
            host: None,
        };

        // No sidecar: the runtime never attached.
        assert_eq!(
            external_lane_disposition(&record),
            ExternalLaneDisposition::NeverAttached
        );
        assert_eq!(external_session_status(&context, &record), "missing");

        // Malformed, schema-mismatched, and launch-mismatched sidecars are all
        // unproven, never a quiet absence.
        for body in [
            "not json".to_string(),
            json!({
                "schema_version": "main-agent.dsh-runtime-liveness.v999",
                "launch_id": "launch-1",
                "harness": pinned_harness(),
                "lane": { "state": "open" }
            })
            .to_string(),
            json!({
                "schema_version": LIVENESS_SCHEMA,
                "launch_id": "launch-2",
                "harness": pinned_harness(),
                "lane": { "state": "open" }
            })
            .to_string(),
        ] {
            write_sidecar(&record, &body, 0o600);
            assert_eq!(
                external_lane_disposition(&record),
                ExternalLaneDisposition::Unproven(UnprovenReason::InvalidEvidence),
                "invalid evidence must stay unproven: {body}"
            );
            assert_eq!(external_session_status(&context, &record), "unknown");
            assert!(external_runtime_evidence(&context, &record).is_err());
            assert!(external_turn_state(&record).is_none());
        }

        // A group-readable sidecar fails the owner-only trust bar.
        write_sidecar(
            &record,
            &json!({
                "schema_version": LIVENESS_SCHEMA,
                "launch_id": "launch-1",
                "harness": pinned_harness(),
                "lane": { "state": "open" }
            })
            .to_string(),
            0o644,
        );
        assert_eq!(
            external_lane_disposition(&record),
            ExternalLaneDisposition::Unproven(UnprovenReason::InvalidEvidence),
            "a group-readable sidecar is not trusted evidence"
        );

        // A dead pid proves the lane stopped.
        write_sidecar(
            &record,
            &json!({
                "schema_version": LIVENESS_SCHEMA,
                "launch_id": "launch-1",
                "harness": { "pid": i32::MAX, "start_time": 1_u64 },
                "lane": { "state": "open" }
            })
            .to_string(),
            0o600,
        );
        assert_eq!(
            external_lane_disposition(&record),
            ExternalLaneDisposition::ProvenStopped
        );
        assert_eq!(external_session_status(&context, &record), "stopped");

        // A terminated lane whose harness is still live is an uncorroborated
        // plugin assertion, not a proof.
        write_sidecar(
            &record,
            &json!({
                "schema_version": LIVENESS_SCHEMA,
                "launch_id": "launch-1",
                "harness": pinned_harness(),
                "lane": { "state": "terminated" }
            })
            .to_string(),
            0o600,
        );
        assert_eq!(
            external_lane_disposition(&record),
            ExternalLaneDisposition::Unproven(UnprovenReason::UndecidableHarness),
            "a context-free reader has no broker witness, so a live harness leaves \
             plugin-asserted termination unproven"
        );
    }

    /// Write the lane's broker heartbeat with a current epoch, the way the
    /// lane's own heartbeat process does while it holds authority.
    ///
    /// Gated with its only caller: proving a *live* harness needs the Linux
    /// starttime pin, so the corroboration test cannot run anywhere else.
    #[cfg(target_os = "linux")]
    fn write_lane_heartbeat(context: &CliContext, record: &SessionRecord, incarnation: &str) {
        let path =
            nils_common::coordination_projection::heartbeat_path(&context.state_dir, &record.id);
        fs::create_dir_all(path.parent().expect("coordination parent")).expect("coordination dir");
        fs::write(
            &path,
            format!("{incarnation}:{}", coordination::now_epoch()),
        )
        .expect("write heartbeat");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("heartbeat mode");
    }

    /// The lane's own coordination heartbeat is the second witness to a
    /// plugin-asserted termination: the plugin releases it when it closes the
    /// lane, so a forged stop has to give up the lane's coordination authority
    /// too — which the store observes independently of the sidecar.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_released_lane_heartbeat_corroborates_plugin_asserted_termination() {
        let scratch = tempfile::TempDir::new().expect("tempdir");
        let record = external_record(scratch.path(), "launch-1");
        let context = CliContext {
            state_dir: scratch.path().to_path_buf(),
            host: None,
        };
        write_sidecar(
            &record,
            &json!({
                "schema_version": LIVENESS_SCHEMA,
                "launch_id": "launch-1",
                "harness": pinned_harness(),
                "lane": { "state": "terminated" }
            })
            .to_string(),
            0o600,
        );

        // This harness is this test process, so it is provably alive. The lane
        // heartbeat is gone, so the assertion is corroborated anyway and the
        // destructive routes open.
        assert!(
            external_lane_terminal_is_proven(&context, &record, "launch-1"),
            "a released lane heartbeat corroborates plugin-asserted termination"
        );
        assert_eq!(
            external_lane_disposition_with_broker(&context, &record),
            ExternalLaneDisposition::ProvenStopped
        );
        assert_eq!(external_session_status(&context, &record), "stopped");
        // Runtime evidence must agree with the projection and the destructive
        // gate. It is the input `worker reconcile-stopped` reads, and while it
        // classified from the context-free disposition it reported `Unknown`
        // here — so a lane `delete` would terminalize refused to reconcile.
        assert_eq!(
            external_runtime_evidence(&context, &record)
                .expect("runtime evidence")
                .status,
            CoordinationRuntimeStatus::Stopped
        );

        // A lane still beating holds coordination authority: nothing is proven,
        // the reason names the retained authority rather than the harness, and
        // the destructive routes stay closed.
        write_lane_heartbeat(&context, &record, "launch-1");
        assert_eq!(
            external_lane_disposition_with_broker(&context, &record),
            ExternalLaneDisposition::Unproven(UnprovenReason::LaneRetainsCoordinationAuthority)
        );
        assert_eq!(external_session_status(&context, &record), "unknown");
        assert!(
            !external_lane_terminal_is_proven(&context, &record, "launch-1"),
            "a lane that still holds coordination authority is not proven stopped"
        );
        assert_eq!(
            external_runtime_evidence(&context, &record)
                .expect("runtime evidence")
                .status,
            CoordinationRuntimeStatus::Unknown,
            "reconcile-stopped must refuse a lane that still holds authority"
        );

        // A heartbeat for a different incarnation is not this lane's authority.
        write_lane_heartbeat(&context, &record, "launch-2");
        assert_eq!(
            external_lane_disposition_with_broker(&context, &record),
            ExternalLaneDisposition::ProvenStopped
        );
    }

    #[test]
    fn recorded_liveness_path_is_confined_to_the_record_session_dir() {
        let scratch = tempfile::TempDir::new().expect("tempdir");
        let mut record = external_record(scratch.path(), "launch-1");
        assert!(recorded_liveness_path(&record).is_some());

        for hostile in [
            scratch.path().join("elsewhere").join(LIVENESS_FILE),
            scratch
                .path()
                .join("sessions")
                .join("worker-dsh")
                .join("other.json"),
            PathBuf::from("relative/dsh-runtime-liveness.json"),
        ] {
            record.extra.insert(
                LIVENESS_PATH_KEY.to_string(),
                json!(hostile.to_string_lossy()),
            );
            assert!(
                recorded_liveness_path(&record).is_none(),
                "path outside the record's own session dir is not evidence: {hostile:?}"
            );
            assert_eq!(
                external_lane_disposition(&record),
                ExternalLaneDisposition::Unproven(UnprovenReason::InvalidEvidence)
            );
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn turn_evidence_yields_authoritative_runtime_activity() {
        let scratch = tempfile::TempDir::new().expect("tempdir");
        let record = external_record(scratch.path(), "launch-1");
        write_sidecar(
            &record,
            &json!({
                "schema_version": LIVENESS_SCHEMA,
                "launch_id": "launch-1",
                "harness": pinned_harness(),
                "lane": { "state": "open" },
                "turn": {
                    "phase": "working",
                    "phase_changed_at": "100",
                    "current_turn": { "started_at": "100", "last_progress_at": "110" },
                    "last_turn": { "completed_at": "90", "outcome": "completed" }
                }
            })
            .to_string(),
            0o600,
        );
        let turn = external_turn_state(&record).expect("turn evidence is present");
        assert_eq!(turn.phase, crate::activity::TurnPhase::Working);
        assert_eq!(turn.source.kind, crate::activity::SourceKind::Runtime);
        assert_eq!(turn.source.provider.as_deref(), Some("dsh"));
        assert_eq!(
            turn.source.confidence,
            crate::activity::Confidence::Authoritative
        );
        assert_eq!(
            turn.current_turn
                .as_ref()
                .and_then(|current| current.last_progress_at.as_deref()),
            Some("110")
        );
        assert_eq!(
            turn.last_turn.as_ref().map(|last| last.outcome.as_str()),
            Some("completed")
        );

        // An unknown phase is not evidence.
        write_sidecar(
            &record,
            &json!({
                "schema_version": LIVENESS_SCHEMA,
                "launch_id": "launch-1",
                "harness": pinned_harness(),
                "lane": { "state": "open" },
                "turn": { "phase": "surprised", "phase_changed_at": "100" }
            })
            .to_string(),
            0o600,
        );
        assert!(external_turn_state(&record).is_none());
    }
}
