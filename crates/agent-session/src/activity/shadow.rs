use std::fs::{self, OpenOptions};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::process::Command as ProcessCommand;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;

use jiff::Timestamp;
use nils_common::fs::{SECRET_FILE_MODE, write_atomic};
use serde::{Deserialize, Serialize};

use super::{
    CliContext, SessionRecord, ShadowObservationView, TurnPhase, TurnState, now, session_dir,
};

pub(crate) const SHADOW_FILE: &str = "activity.shadow.json";
const SHADOW_LOCK_FILE: &str = ".activity.shadow.lock";
const SHADOW_VERSION: &str = "terminal-shadow.v1";
const SHADOW_DOCUMENT_VERSION: &str = "agent-session.activity-shadow.v1";
const SEMANTIC_STALE_AFTER_SECONDS: i64 = 5 * 60;
const SAMPLE_INTERVAL_SECONDS: i64 = 15;
const OBSERVER_TIMEOUT: Duration = Duration::from_millis(250);
const OBSERVER_OUTPUT_LIMIT: usize = 16 * 1024;
const CAPTURE_LINES: &str = "-20";
const MAX_CONCURRENT_SAMPLERS: usize = 4;
static ACTIVE_SAMPLERS: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ShadowDocument {
    schema_version: String,
    runtime_id: String,
    runtime_generation: u64,
    observation: ShadowObservationView,
}

struct ShadowLock(fs::File);

impl Drop for ShadowLock {
    fn drop(&mut self) {
        // SAFETY: flock only observes the valid file descriptor owned by self.
        unsafe {
            libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

struct SamplerPermit;

impl Drop for SamplerPermit {
    fn drop(&mut self) {
        ACTIVE_SAMPLERS.fetch_sub(1, Ordering::AcqRel);
    }
}

pub(crate) fn annotate_for_view(
    context: &CliContext,
    record: &SessionRecord,
    status: &str,
    tmux_bin: &Path,
    mut state: TurnState,
    schedule_sampling: bool,
) -> TurnState {
    if !eligible(&record.agent, status, &state) {
        state.shadow_observation = None;
        return state;
    }

    let dir = session_dir(context, &record.id);
    let path = dir.join(SHADOW_FILE);
    let cached = read_current(&path, record);
    if let Some(cached) = cached.as_ref() {
        state.shadow_observation =
            Some(with_disagreement(cached.observation.clone(), &state.phase));
    }
    if cached
        .as_ref()
        .is_some_and(|cached| is_recent(&cached.observation.observed_at, SAMPLE_INTERVAL_SECONDS))
    {
        return state;
    }

    if schedule_sampling {
        schedule_sample(
            context.clone(),
            record.clone(),
            tmux_bin.to_path_buf(),
            state.phase.clone(),
        );
    }
    state
}

fn schedule_sample(
    context: CliContext,
    record: SessionRecord,
    tmux_bin: std::path::PathBuf,
    phase: TurnPhase,
) {
    let Some(permit) = sampler_permit() else {
        return;
    };
    thread::spawn(move || {
        let _permit = permit;
        let dir = session_dir(&context, &record.id);
        let path = dir.join(SHADOW_FILE);
        let Some(_lock) = acquire_shadow_lock(&dir) else {
            return;
        };
        if read_current(&path, &record).is_some_and(|cached| {
            is_recent(&cached.observation.observed_at, SAMPLE_INTERVAL_SECONDS)
        }) {
            return;
        }
        let observation = sample(&record, &tmux_bin, &phase);
        let Ok(current) = crate::load_session_record(&context, &record.id) else {
            return;
        };
        let Some(runtime) = current.runtime.as_ref() else {
            return;
        };
        let Some(sampled_runtime) = record.runtime.as_ref() else {
            return;
        };
        if runtime.launch_id != sampled_runtime.launch_id
            || runtime.generation != sampled_runtime.generation
            || current.tmux_session != record.tmux_session
        {
            return;
        }
        let document = ShadowDocument {
            schema_version: SHADOW_DOCUMENT_VERSION.to_string(),
            runtime_id: runtime.launch_id.clone(),
            runtime_generation: runtime.generation,
            observation,
        };
        if let Ok(bytes) = serde_json::to_vec_pretty(&document) {
            let _ = write_atomic(&path, &bytes, SECRET_FILE_MODE);
        }
    });
}

fn sampler_permit() -> Option<SamplerPermit> {
    reserve_sampler(&ACTIVE_SAMPLERS, MAX_CONCURRENT_SAMPLERS).then_some(SamplerPermit)
}

fn reserve_sampler(counter: &AtomicUsize, limit: usize) -> bool {
    counter
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
            (active < limit).then_some(active + 1)
        })
        .is_ok()
}

fn acquire_shadow_lock(dir: &Path) -> Option<ShadowLock> {
    let path = dir.join(SHADOW_LOCK_FILE);
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .mode(SECRET_FILE_MODE)
        .open(&path)
        .ok()?;
    fs::set_permissions(&path, fs::Permissions::from_mode(SECRET_FILE_MODE)).ok()?;
    // SAFETY: flock only observes the valid file descriptor owned by file.
    (unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0)
        .then_some(ShadowLock(file))
}

fn eligible(provider: &str, status: &str, state: &TurnState) -> bool {
    if status != "running" || !matches!(provider, "claude" | "codex") {
        return false;
    }
    if state.phase == TurnPhase::Unknown {
        return true;
    }
    state.semantic_event.as_ref().map_or_else(
        || is_older_than(&state.phase_changed_at, SEMANTIC_STALE_AFTER_SECONDS),
        |event| is_older_than(&event.observed_at, SEMANTIC_STALE_AFTER_SECONDS),
    )
}

fn read_current(path: &Path, record: &SessionRecord) -> Option<ShadowDocument> {
    let bytes = fs::read(path).ok()?;
    let document = serde_json::from_slice::<ShadowDocument>(&bytes).ok()?;
    let runtime = record.runtime.as_ref()?;
    (document.schema_version == SHADOW_DOCUMENT_VERSION
        && document.runtime_id == runtime.launch_id
        && document.runtime_generation == runtime.generation
        && valid_observation(&document.observation))
    .then_some(document)
}

fn valid_observation(observation: &ShadowObservationView) -> bool {
    observation.observer_version == SHADOW_VERSION
        && matches!(
            observation.projection.as_str(),
            "working" | "needs_input" | "waiting" | "unknown"
        )
        && observation.observed_at.parse::<Timestamp>().is_ok()
        && observation.rule_id.len() <= 64
        && observation.rule_id.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
        })
}

fn is_recent(value: &str, seconds: i64) -> bool {
    !is_older_than(value, seconds)
}

fn is_older_than(value: &str, seconds: i64) -> bool {
    let Ok(observed) = value.parse::<Timestamp>() else {
        return true;
    };
    Timestamp::now()
        .as_second()
        .saturating_sub(observed.as_second())
        >= seconds
}

fn sample(record: &SessionRecord, tmux_bin: &Path, phase: &TurnPhase) -> ShadowObservationView {
    let target = crate::managed_tmux_pane_target(&record.tmux_session);
    let title = command_output(
        tmux_bin,
        &["display-message", "-p", "-t", &target, "#{pane_title}"],
    );
    let bottom = command_output(
        tmux_bin,
        &["capture-pane", "-p", "-t", &target, "-S", CAPTURE_LINES],
    );
    let (projection, rule_id) = match (title, bottom) {
        (Some(title), Some(bottom)) => classify(&record.agent, &title, &bottom),
        _ => ("unknown", "observer_unavailable"),
    };
    ShadowObservationView {
        observer_version: SHADOW_VERSION.to_string(),
        rule_id: rule_id.to_string(),
        observed_at: now(),
        projection: projection.to_string(),
        disagrees: disagrees(phase, projection),
        extra: serde_json::Map::new(),
    }
}

fn command_output(tmux_bin: &Path, args: &[&str]) -> Option<String> {
    let mut command = ProcessCommand::new(tmux_bin);
    command.args(args);
    let output =
        crate::run_output_with_timeout_and_cap(command, OBSERVER_TIMEOUT, OBSERVER_OUTPUT_LIMIT)
            .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).to_string())
}

fn classify<'a>(provider: &str, title: &str, bottom: &str) -> (&'a str, &'a str) {
    let title = title.to_ascii_lowercase();
    let bottom = bottom.to_ascii_lowercase();
    match provider {
        "codex" if title.contains("action required") => {
            ("needs_input", "codex_action_required_title")
        }
        "codex" if bottom.contains("working (") && bottom.contains("esc to interrupt") => {
            ("working", "codex_working_indicator")
        }
        "codex"
            if bottom.lines().rev().take(3).any(|line| {
                let line = line.trim_start();
                line.starts_with('›') || line.starts_with("> ")
            }) =>
        {
            ("waiting", "codex_prompt_visible")
        }
        "claude"
            if bottom.contains("do you want to proceed")
                || bottom.contains("allow this action") =>
        {
            ("needs_input", "claude_permission_form")
        }
        "claude" if bottom.contains("esc to interrupt") => ("working", "claude_working_indicator"),
        "claude"
            if bottom.lines().rev().take(3).any(|line| {
                let line = line.trim_start();
                line.starts_with('❯') || line.starts_with("> ")
            }) =>
        {
            ("waiting", "claude_prompt_visible")
        }
        "codex" => ("unknown", "codex_unmatched"),
        "claude" => ("unknown", "claude_unmatched"),
        _ => ("unknown", "provider_unsupported"),
    }
}

pub(super) fn disagrees(phase: &TurnPhase, projection: &str) -> bool {
    match projection {
        "working" => *phase != TurnPhase::Working,
        "needs_input" => *phase != TurnPhase::NeedsInput,
        "waiting" => *phase != TurnPhase::Waiting,
        _ => false,
    }
}

fn with_disagreement(
    mut observation: ShadowObservationView,
    phase: &TurnPhase,
) -> ShadowObservationView {
    observation.disagrees = disagrees(phase, &observation.projection);
    observation
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::{Map, json};
    use std::time::Instant;

    fn state(value: serde_json::Value) -> TurnState {
        serde_json::from_value(value).expect("state fixture")
    }

    fn record(id: &str, launch_id: &str, generation: u64) -> SessionRecord {
        serde_json::from_value(json!({
            "schema_version": "agent-session.session.v1",
            "id": id,
            "agent": "codex",
            "mode": "interactive",
            "title": null,
            "cwd": "/tmp",
            "tmux_session": format!("hs-codex-{id}"),
            "prompt_file": null,
            "log_file": null,
            "created_at": "2026-07-10T00:00:00Z",
            "updated_at": "2026-07-10T00:00:00Z",
            "runtime": {
                "kind": "tmux",
                "tmux_session": format!("hs-codex-{id}"),
                "generation": generation,
                "started_at": "2026-07-10T00:00:00Z",
                "launch_id": launch_id
            }
        }))
        .expect("session record")
    }

    fn write_record(context: &CliContext, record: &SessionRecord) {
        let dir = session_dir(context, &record.id);
        fs::create_dir_all(&dir).expect("session dir");
        fs::write(
            dir.join("session.json"),
            serde_json::to_vec_pretty(record).expect("record JSON"),
        )
        .expect("record write");
    }

    fn wait_for_shadow(path: &Path, record: &SessionRecord) -> ShadowDocument {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(document) = read_current(path, record) {
                return document;
            }
            assert!(Instant::now() < deadline, "shadow sample timed out");
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn classifier_uses_bounded_rule_ids_without_returning_content() {
        assert_eq!(
            classify("codex", "Action Required", "private prompt"),
            ("needs_input", "codex_action_required_title")
        );
        assert_eq!(
            classify("claude", "Claude", "Working… esc to interrupt"),
            ("working", "claude_working_indicator")
        );
        assert_eq!(
            classify("codex", "Codex", "unrecognized private output"),
            ("unknown", "codex_unmatched")
        );
    }

    #[test]
    fn exact_semantic_phase_wins_every_shadow_disagreement() {
        assert!(disagrees(&TurnPhase::Working, "waiting"));
        assert!(disagrees(&TurnPhase::NeedsInput, "working"));
        assert!(!disagrees(&TurnPhase::Waiting, "waiting"));
        assert!(!disagrees(&TurnPhase::Working, "unknown"));
    }

    #[test]
    fn sampling_policy_targets_only_uncertain_or_semantically_stale_sessions() {
        let fresh = state(json!({
            "schema_version": "agent-session.turn-state.v1",
            "phase": "working",
            "phase_changed_at": now(),
            "revision": 2,
            "source": {
                "kind": "provider_hook",
                "provider": "codex",
                "confidence": "authoritative"
            },
            "semantic_event": {
                "kind": "progress",
                "observed_at": now()
            }
        }));
        let mut unknown = fresh.clone();
        unknown.phase = TurnPhase::Unknown;
        unknown.semantic_event = None;
        unknown.extra = Map::new();

        assert!(!eligible("codex", "running", &fresh));
        assert!(eligible("codex", "running", &unknown));
        assert!(eligible("claude", "running", &unknown));
        assert!(!eligible("hermes", "running", &unknown));
        assert!(!eligible("codex", "stopped", &unknown));
    }

    #[test]
    fn shadow_sampler_has_a_process_wide_concurrency_cap() {
        let counter = AtomicUsize::new(0);
        for _ in 0..MAX_CONCURRENT_SAMPLERS {
            assert!(reserve_sampler(&counter, MAX_CONCURRENT_SAMPLERS));
        }
        assert!(!reserve_sampler(&counter, MAX_CONCURRENT_SAMPLERS));
    }

    #[test]
    fn shadow_cache_is_bound_to_launch_identity_not_generation_alone() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let path = tmp.path().join(SHADOW_FILE);
        let original = record("same-id", "launch-a", 1);
        let replacement = record("same-id", "launch-b", 1);
        let document = ShadowDocument {
            schema_version: SHADOW_DOCUMENT_VERSION.to_string(),
            runtime_id: "launch-a".to_string(),
            runtime_generation: 1,
            observation: ShadowObservationView {
                observer_version: SHADOW_VERSION.to_string(),
                rule_id: "codex_prompt_visible".to_string(),
                observed_at: now(),
                projection: "waiting".to_string(),
                disagrees: false,
                extra: Map::new(),
            },
        };
        fs::write(&path, serde_json::to_vec(&document).expect("shadow JSON"))
            .expect("shadow write");

        assert!(read_current(&path, &original).is_some());
        assert!(read_current(&path, &replacement).is_none());
    }

    #[test]
    fn list_and_serve_collectors_own_distinct_shadow_sampling_lifecycles() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let context = CliContext {
            state_dir: tmp.path().to_path_buf(),
            host: None,
        };
        let record = record("shadow-integration", "launch-a", 1);
        write_record(&context, &record);
        crate::activity::activate_runtime(&context, &record).expect("activity state");
        let tmux = tmp.path().join("fake-tmux");
        fs::write(
            &tmux,
            format!(
                "#!/bin/sh\ncase \"$1\" in\n  list-windows) printf '%s\\t100\\n' {} ;;\n  display-message) printf 'Codex\\n' ;;\n  capture-pane) printf '› \\n' ;;\nesac\n",
                shell_words::quote(&record.tmux_session),
            ),
        )
        .expect("fake tmux");
        fs::set_permissions(&tmux, fs::Permissions::from_mode(0o700)).expect("tmux mode");
        let path = session_dir(&context, &record.id).join(SHADOW_FILE);

        let one_shot = crate::list_sessions(&context, Some(&tmux)).expect("one-shot list");
        assert_eq!(one_shot.len(), 1);
        thread::sleep(Duration::from_millis(50));
        assert!(!path.exists(), "one-shot view launched detached sampling");

        let immediate =
            crate::list_sessions_for_serve(&context, Some(&tmux)).expect("serve collection");
        assert_eq!(immediate.len(), 1);
        assert!(
            immediate[0]
                .turn_state
                .as_ref()
                .is_some_and(|state| state.shadow_observation.is_none())
        );
        let sampled = wait_for_shadow(&path, &record);
        assert_eq!(sampled.observation.projection, "waiting");
        assert_eq!(sampled.observation.rule_id, "codex_prompt_visible");
        assert_eq!(sampled.runtime_id, "launch-a");
        let cached =
            crate::list_sessions_for_serve(&context, Some(&tmux)).expect("cached collection");
        assert!(
            cached[0]
                .turn_state
                .as_ref()
                .and_then(|state| state.shadow_observation.as_ref())
                .is_some_and(|observation| observation.projection == "waiting")
        );
    }
}
