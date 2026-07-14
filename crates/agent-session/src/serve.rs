//! `agent-session serve`: a per-machine control plane (HTTP) plus PTY attach
//! (WebSocket) exposed over loopback for the agent-console edge.
//!
//! The rest of the crate is synchronous; this module builds its own tokio
//! runtime inside the `serve` subcommand and calls the existing synchronous
//! lifecycle functions from `tokio::task::spawn_blocking`, so there is no
//! duplicate state model. Session reads are open on loopback; activity
//! streaming, writes, and the WebSocket attach require a bearer token (fail
//! closed when no token is configured).
//! Every response carries the daemon's `machine` identity so the edge can
//! aggregate multiple machines. Literal keystroke text is never echoed.

use std::collections::{HashMap, VecDeque};
use std::convert::Infallible;
use std::ffi::CString;
use std::fmt;
use std::fs::OpenOptions;
use std::io::{self, Read};
use std::net::SocketAddr;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, Stdio};
#[cfg(test)]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use axum::Json;
use axum::Router;
use axum::body::{Body, Bytes, to_bytes};
use axum::extract::rejection::JsonRejection;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path as AxPath, Query, State};
use axum::http::header::{AUTHORIZATION, CONTENT_TYPE};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post};
use futures_util::{SinkExt, StreamExt};
use nils_common::cli_contract::{exit, schema_version_for};
use nils_common::provider_usage::{ProviderUsageReason, prefer_reason};
use nils_common::usage_time::{
    epoch_seconds_from_f64, normalize_epoch_seconds, reset_epoch_seconds_from_str,
};
use notify::{Event as NotifyEvent, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, broadcast, mpsc, watch};
use tokio::task::JoinSet;

use crate::auto_resume::{self, UsageSnapshot};
use crate::cli::{self, AgentKind, SpecialKey};
use crate::codex_app_server::{self, ControlHandle};
use crate::provider_prompt::{
    MAX_PROVIDER_PROMPT_BYTES, PROVIDER_PROMPT_CAPABILITY, ProviderKind, ProviderPromptEvent,
    ProviderPromptSource, ProviderPromptTail,
};
use crate::{
    BINARY, CliContext, CliError, ProviderResumeImportArgs, SessionRecord, SessionView,
    WorkdirSearchOptions, delete_session, glance_session, list_sessions, load_session_record,
    non_empty_env, repo_remote_url_from_cwd, resolve_tmux_bin, resume_session_by_id,
    search_workdirs, send_auto_resume_input, send_input_serialized, session_clipboard_buffer,
    session_dir, session_status, short_hostname, start_provider_resume_session, start_session,
    update_session_title_if_revision, write_session_attachment,
};

const ATTACH_LIVE_FIFO_NAME: &str = "attach-live.fifo";
const ATTACH_BROADCAST_CAPACITY: usize = 128;
const ATTACH_PIPE_STARTUP_GRACE: Duration = Duration::from_secs(2);
const ATTACH_FIFO_POLL_INTERVAL: Duration = Duration::from_millis(100);
const ATTACH_READER_STOP_TIMEOUT: Duration = Duration::from_millis(250);
const ATTACH_TMUX_COMMAND_TIMEOUT: Duration = Duration::from_secs(2);
const ATTACH_SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(2);
const ATTACH_RESIZE_TIMEOUT: Duration = Duration::from_secs(5);
const ATTACH_WEBSOCKET_SEND_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_ATTACHMENT_BYTES: usize = 25 * 1024 * 1024;
const MAX_STDIN_TOKEN_BYTES: u64 = 8 * 1024;
const USAGE_SCHEMA_VERSION: &str = "agent-session.usage.v1";
const DEFAULT_USAGE_TIMEOUT_MS: u64 = 45_000;
const CLAUDE_USAGE_CLEANUP_SLACK_SECONDS: u64 = 5;
const ATTACH_TERMINAL_QUEUE_CAPACITY: usize = 64;
const ATTACH_CONTROL_QUEUE_CAPACITY: usize = 8;
const ATTACH_TERMINAL_BURST: usize = 32;
const ATTACH_HANDOFF_BUFFER_CAPACITY: usize = ATTACH_BROADCAST_CAPACITY;
const ATTACH_HANDOFF_MAX_RECAPTURES: usize = 1;
const PROVIDER_PROMPT_POLL_INTERVAL: Duration = Duration::from_millis(100);
const PROVIDER_PROMPT_PENDING_POLL_INTERVAL: Duration = Duration::from_millis(500);
const PROVIDER_PROMPT_PENDING_TIMEOUT: Duration = Duration::from_secs(2 * 60);
const STRUCTURED_PROMPT_CONTROL_WAIT: Duration = Duration::from_secs(3);
const STRUCTURED_PROMPT_CONTROL_POLL: Duration = Duration::from_millis(50);
const CODEX_ACCOUNT_BINDING_WAIT: Duration = Duration::from_secs(30);
const PROVIDER_PROMPT_DISCOVERY_MAX_BACKOFF: Duration = Duration::from_secs(30);
const PROVIDER_PROMPT_DISCOVERY_MAX_ENTRIES: usize = 64;
const PROVIDER_PROMPT_DISCOVERY_MAX_CONCURRENT_SCANS: usize = 4;
const MAX_CONCURRENT_AUTO_RESUME_TICKS: usize = 4;
const ATTACH_SCHEMA_VERSION: &str = "agent-session.attach.v1";
const ATTACH_EVENT_SCHEMA_VERSION: &str = "agent-session.attach.event.v1";
const ACTIVITY_STREAM_EVENT_SCHEMA_VERSION: &str = "agent-session.activity-stream.event.v1";
const ACTIVITY_STREAM_BROADCAST_CAPACITY: usize = 1;
const ACTIVITY_STREAM_REPLAY_CAPACITY: usize = 128;
const ACTIVITY_STREAM_REPLAY_BYTE_CAPACITY: usize = 512 * 1024;
const ACTIVITY_STREAM_SUBSCRIBER_CAPACITY: usize = 64;
const ACTIVITY_STREAM_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);
const ACTIVITY_STREAM_DEBOUNCE: Duration = Duration::from_millis(25);
const ACTIVITY_STREAM_MAX_REFRESH_CADENCE: Duration = Duration::from_millis(250);
const ACTIVITY_STREAM_OVERSIZED_REASON: &str = "oversized_snapshot";
const RESET_AT_KEYS: &[&str] = &["reset_at", "resetAt", "resets_at", "resetsAt"];
const RESET_AT_EPOCH_KEYS: &[&str] = &[
    "reset_at_epoch",
    "resetAtEpoch",
    "resets_at_epoch",
    "resetsAtEpoch",
    "reset_at",
    "resetAt",
    "resets_at",
    "resetsAt",
];

/// Shared daemon state handed to every request handler.
type SessionCollector =
    Arc<dyn Fn(&CliContext, &Path) -> Result<Vec<SessionView>, CliError> + Send + Sync>;

struct ServeState {
    context: CliContext,
    machine: String,
    token: Option<String>,
    tmux_bin: PathBuf,
    attach_brokers: AttachBrokerRegistry,
    provider_prompt_discovery: Arc<ProviderPromptDiscoveryRegistry>,
    activity_broker: Arc<ActivityBroker>,
    codex_controls: Arc<StdMutex<HashMap<String, CodexControlEntry>>>,
    codex_account_switches: CodexAccountSwitchRegistry,
    session_collector: SessionCollector,
}

#[derive(Clone)]
struct CodexControlEntry {
    launch_id: String,
    handle: ControlHandle,
}

#[derive(Clone, Default)]
struct CodexAccountSwitchRegistry {
    entries: Arc<tokio::sync::Mutex<HashMap<String, std::sync::Weak<tokio::sync::Mutex<()>>>>>,
}

impl CodexAccountSwitchRegistry {
    async fn lock(&self, session_id: &str) -> tokio::sync::OwnedMutexGuard<()> {
        let slot = {
            let mut entries = self.entries.lock().await;
            entries.retain(|_, entry| entry.strong_count() > 0);
            if let Some(slot) = entries.get(session_id).and_then(std::sync::Weak::upgrade) {
                slot
            } else {
                let slot = Arc::new(tokio::sync::Mutex::new(()));
                entries.insert(session_id.to_string(), Arc::downgrade(&slot));
                slot
            }
        };
        slot.lock_owned().await
    }
}

fn default_session_collector() -> SessionCollector {
    Arc::new(|context, tmux_bin| list_sessions(context, Some(tmux_bin)))
}

pub fn run_serve(context: &CliContext, args: cli::ServeArgs) -> i32 {
    let bind: SocketAddr = match args.bind.parse() {
        Ok(addr) => addr,
        Err(err) => {
            eprintln!("error: invalid --bind {}: {err}", args.bind);
            return exit::USAGE;
        }
    };
    if !bind.ip().is_loopback() && !args.allow_non_loopback {
        eprintln!(
            "error: refusing to bind a non-loopback address ({bind}); this endpoint drives a \
             remote shell. Keep it tailnet-only behind `tailscale serve`, or pass \
             --allow-non-loopback to override deliberately."
        );
        return exit::USAGE;
    }
    if !bind.ip().is_loopback() {
        eprintln!(
            "warning: binding a non-loopback address ({bind}) exposes a remote shell; \
             keep it tailnet-only behind `tailscale serve` and off the public internet"
        );
    }

    let token = match resolve_serve_token(&args) {
        Ok(token) => token,
        Err(err) => {
            eprintln!("error: {err}");
            return exit::USAGE;
        }
    };
    if token.is_none() {
        eprintln!(
            "warning: no --token / --token-stdin / AGENT_SESSION_TOKEN set; authenticated endpoints are disabled"
        );
    }

    let machine = args
        .machine
        .clone()
        .or_else(|| non_empty_env("AGENT_SESSION_MACHINE"))
        .or_else(|| context.host.clone())
        .or_else(short_hostname)
        .unwrap_or_else(|| "unknown".to_string());

    let tmux_bin = resolve_tmux_bin(args.tmux_bin.as_deref());
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("error: failed to build async runtime: {err}");
            return exit::RUNTIME;
        }
    };

    runtime.block_on(async move {
        if let Err(err) = fence_codex_controls_before_listen(context, &tmux_bin) {
            eprintln!(
                "error: failed to fence Codex account controls: {}",
                err.code()
            );
            return exit::RUNTIME;
        }
        let session_collector = default_session_collector();
        let activity_broker = ActivityBroker::start(
            context.clone(),
            machine.clone(),
            tmux_bin.clone(),
            session_collector.clone(),
        )
        .await;
        let state = Arc::new(ServeState {
            context: context.clone(),
            machine,
            token,
            tmux_bin,
            attach_brokers: AttachBrokerRegistry::default(),
            provider_prompt_discovery: Arc::new(ProviderPromptDiscoveryRegistry::default()),
            activity_broker,
            codex_controls: Arc::new(StdMutex::new(HashMap::new())),
            codex_account_switches: CodexAccountSwitchRegistry::default(),
            session_collector,
        });
        let listener = match tokio::net::TcpListener::bind(bind).await {
            Ok(listener) => listener,
            Err(err) => {
                eprintln!("error: failed to bind {bind}: {err}");
                return exit::RUNTIME;
            }
        };
        eprintln!(
            "agent-session serve listening on http://{bind} (machine={})",
            state.machine
        );
        let app = router(state.clone());
        let codex_control_task = tokio::spawn(codex_control_loop(state.clone()));
        let auto_resume_task = tokio::spawn(auto_resume_loop(state.clone()));
        let result = axum::serve(listener, app)
            .with_graceful_shutdown(shutdown_signal())
            .await;
        auto_resume_task.abort();
        let _ = auto_resume_task.await;
        codex_control_task.abort();
        let _ = codex_control_task.await;
        state.attach_brokers.shutdown_all().await;
        match result {
            Ok(()) => exit::SUCCESS,
            Err(err) => {
                eprintln!("error: serve failed: {err}");
                exit::RUNTIME
            }
        }
    })
}

fn router(state: Arc<ServeState>) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/sessions", get(list_handler).post(create_handler))
        .route("/codex/accounts", get(codex_accounts_handler))
        .route("/activity/events", get(activity_events_handler))
        .route("/usage", get(usage_handler))
        .route("/workdirs", get(workdirs_handler))
        .route("/repos/remote-url", get(repo_remote_url_handler))
        .route("/sessions/{id}/glance", get(glance_handler))
        .route("/sessions/{id}/buffer", get(buffer_handler))
        .route("/sessions/{id}/send", post(send_handler))
        .route("/sessions/{id}/prompt", post(structured_prompt_handler))
        .route("/sessions/{id}/resume", post(resume_handler))
        .route(
            "/sessions/{id}/account",
            axum::routing::put(codex_account_handler),
        )
        .route(
            "/sessions/{id}/auto-resume",
            get(auto_resume_status_handler)
                .put(auto_resume_set_handler)
                .delete(auto_resume_cancel_handler),
        )
        .route(
            "/sessions/{id}/attachments",
            post(upload_attachment_handler),
        )
        .route("/sessions/{id}/attach", get(attach_handler))
        .route(
            "/sessions/{id}",
            patch(update_session_handler).delete(delete_handler),
        )
        .with_state(state)
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

#[derive(Debug)]
enum ServeTokenError {
    EmptyStdin,
    MultipleStdinTokens,
    StdinTooLarge,
    ReadStdin(io::Error),
}

impl fmt::Display for ServeTokenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyStdin => write!(f, "--token-stdin received an empty token"),
            Self::MultipleStdinTokens => write!(f, "--token-stdin expects exactly one token"),
            Self::StdinTooLarge => write!(f, "--token-stdin input exceeds 8192 bytes"),
            Self::ReadStdin(err) => write!(f, "failed to read --token-stdin input: {err}"),
        }
    }
}

fn resolve_serve_token(args: &cli::ServeArgs) -> Result<Option<String>, ServeTokenError> {
    if args.token_stdin {
        return read_token_from_stdin(io::stdin().lock()).map(Some);
    }
    Ok(sanitize_token(args.token.clone()).or_else(|| non_empty_env("AGENT_SESSION_TOKEN")))
}

fn read_token_from_stdin<R: Read>(reader: R) -> Result<String, ServeTokenError> {
    let mut input = String::new();
    let mut limited = reader.take(MAX_STDIN_TOKEN_BYTES + 1);
    limited
        .read_to_string(&mut input)
        .map_err(ServeTokenError::ReadStdin)?;
    if input.len() as u64 > MAX_STDIN_TOKEN_BYTES {
        return Err(ServeTokenError::StdinTooLarge);
    }

    let token = input.trim();
    if token.is_empty() {
        return Err(ServeTokenError::EmptyStdin);
    }
    if token.contains(['\n', '\r']) {
        return Err(ServeTokenError::MultipleStdinTokens);
    }
    Ok(token.to_string())
}

/// Treat an empty/whitespace explicit `--token` as "no token" (fail closed),
/// matching the env-var sanitization in `non_empty_env`.
fn sanitize_token(token: Option<String>) -> Option<String> {
    token.filter(|value| !value.trim().is_empty())
}

// --- activity stream ---------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Serialize)]
struct ActivityStreamSession {
    id: String,
    turn_state: Option<crate::activity::StreamTurnState>,
}

#[derive(Clone, Debug, Serialize)]
struct ActivityStreamEvent {
    schema_version: &'static str,
    #[serde(rename = "type")]
    kind: &'static str,
    stream_id: String,
    sequence: u64,
    machine: String,
    observed_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sessions: Option<Vec<ActivityStreamSession>>,
}

impl ActivityStreamEvent {
    fn sse_id(&self) -> String {
        format!("{}:{}", self.stream_id, self.sequence)
    }
}

#[derive(Debug)]
struct ActivityStreamFrame {
    kind: &'static str,
    sequence: u64,
    #[cfg(test)]
    event: ActivityStreamEvent,
    sse: Bytes,
    wire_bytes: usize,
}

#[derive(Default)]
struct ActivityFrameEncoder {
    #[cfg(test)]
    count: AtomicUsize,
}

impl ActivityFrameEncoder {
    fn encode(&self, event: ActivityStreamEvent) -> Arc<ActivityStreamFrame> {
        let payload = serde_json::to_vec(&event).expect("activity stream event serializes");
        #[cfg(test)]
        self.count.fetch_add(1, Ordering::SeqCst);
        let id = event.sse_id();
        let mut sse = Vec::with_capacity(id.len() + event.kind.len() + payload.len() + 24);
        sse.extend_from_slice(b"id: ");
        sse.extend_from_slice(id.as_bytes());
        sse.extend_from_slice(b"\nevent: ");
        sse.extend_from_slice(event.kind.as_bytes());
        sse.extend_from_slice(b"\ndata: ");
        sse.extend_from_slice(&payload);
        sse.extend_from_slice(b"\n\n");
        let wire_bytes = sse.len();
        Arc::new(ActivityStreamFrame {
            kind: event.kind,
            sequence: event.sequence,
            #[cfg(test)]
            event,
            sse: Bytes::from(sse),
            wire_bytes,
        })
    }

    #[cfg(test)]
    fn count(&self) -> usize {
        self.count.load(Ordering::SeqCst)
    }
}

impl ActivityStreamFrame {
    fn replay_bytes(&self) -> usize {
        self.wire_bytes
    }
}

#[cfg(test)]
impl std::ops::Deref for ActivityStreamFrame {
    type Target = ActivityStreamEvent;

    fn deref(&self) -> &Self::Target {
        &self.event
    }
}

struct ActivityEventState {
    history: VecDeque<Arc<ActivityStreamFrame>>,
    history_bytes: usize,
    latest_sessions: Vec<ActivityStreamSession>,
    latest_observed_at: String,
    latest_snapshot_oversized: bool,
    cached_snapshot: Option<Arc<ActivityStreamFrame>>,
    cached_reset: Option<Arc<ActivityStreamFrame>>,
}

struct ActivityEventLog {
    stream_id: String,
    machine: String,
    encoder: ActivityFrameEncoder,
    sequence: AtomicU64,
    state: StdMutex<ActivityEventState>,
    sender: broadcast::Sender<Arc<ActivityStreamFrame>>,
    lifecycle: watch::Sender<ActivityBrokerLifecycle>,
}

impl ActivityEventLog {
    #[cfg(test)]
    fn new(machine: String, sessions: Vec<ActivityStreamSession>) -> Arc<Self> {
        let (lifecycle, _) = watch::channel(ActivityBrokerLifecycle::Ready);
        Self::new_with_lifecycle(machine, sessions, lifecycle)
    }

    fn new_with_lifecycle(
        machine: String,
        sessions: Vec<ActivityStreamSession>,
        lifecycle: watch::Sender<ActivityBrokerLifecycle>,
    ) -> Arc<Self> {
        let stream_id = uuid::Uuid::new_v4().to_string();
        let observed_at = activity_observed_at();
        let encoder = ActivityFrameEncoder::default();
        let initial = encoder.encode(ActivityStreamEvent {
            schema_version: ACTIVITY_STREAM_EVENT_SCHEMA_VERSION,
            kind: "snapshot",
            stream_id: stream_id.clone(),
            sequence: 1,
            machine: machine.clone(),
            observed_at: observed_at.clone(),
            reason: None,
            sessions: Some(sessions.clone()),
        });
        let latest_snapshot_oversized =
            initial.replay_bytes() > ACTIVITY_STREAM_REPLAY_BYTE_CAPACITY;
        let initial = if latest_snapshot_oversized {
            encoder.encode(ActivityStreamEvent {
                schema_version: ACTIVITY_STREAM_EVENT_SCHEMA_VERSION,
                kind: "reset",
                stream_id: stream_id.clone(),
                sequence: 1,
                machine: machine.clone(),
                observed_at: observed_at.clone(),
                reason: Some(ACTIVITY_STREAM_OVERSIZED_REASON),
                sessions: None,
            })
        } else {
            initial
        };
        let mut history = VecDeque::with_capacity(ACTIVITY_STREAM_REPLAY_CAPACITY);
        let initial_bytes = initial.replay_bytes();
        let history_bytes = if initial_bytes <= ACTIVITY_STREAM_REPLAY_BYTE_CAPACITY {
            history.push_back(initial.clone());
            initial_bytes
        } else {
            0
        };
        let (sender, _) = broadcast::channel(ACTIVITY_STREAM_BROADCAST_CAPACITY);
        Arc::new(Self {
            stream_id,
            machine,
            encoder,
            sequence: AtomicU64::new(1),
            state: StdMutex::new(ActivityEventState {
                history,
                history_bytes,
                latest_sessions: sessions,
                latest_observed_at: observed_at,
                latest_snapshot_oversized,
                cached_snapshot: Some(initial.clone()),
                cached_reset: (initial.kind == "reset").then(|| initial.clone()),
            }),
            sender,
            lifecycle,
        })
    }

    fn publish_snapshot(&self, sessions: Vec<ActivityStreamSession>) {
        let mut state = self.state.lock().expect("activity event state lock");
        let unchanged = state.latest_sessions == sessions;
        if unchanged {
            state.latest_observed_at = activity_observed_at();
            if !state.latest_snapshot_oversized {
                state.cached_snapshot = None;
                state.cached_reset = None;
            }
            return;
        }
        let sequence = self.sequence.load(Ordering::SeqCst) + 1;
        let observed_at = activity_observed_at();
        let event = self.encoder.encode(ActivityStreamEvent {
            schema_version: ACTIVITY_STREAM_EVENT_SCHEMA_VERSION,
            kind: "snapshot",
            stream_id: self.stream_id.clone(),
            sequence,
            machine: self.machine.clone(),
            observed_at: observed_at.clone(),
            reason: None,
            sessions: Some(sessions.clone()),
        });
        let oversized = event.replay_bytes() > ACTIVITY_STREAM_REPLAY_BYTE_CAPACITY;
        let was_oversized = state.latest_snapshot_oversized;
        state.latest_sessions = sessions;
        state.latest_observed_at = observed_at;
        state.latest_snapshot_oversized = oversized;
        if oversized && was_oversized {
            return;
        }
        self.sequence.store(sequence, Ordering::SeqCst);
        state.cached_snapshot = None;
        state.cached_reset = None;
        let event = if oversized {
            let reset = self.content_free_reset_frame(sequence, &state.latest_observed_at);
            state.cached_snapshot = Some(reset.clone());
            state.cached_reset = Some(reset.clone());
            reset
        } else {
            if event.kind == "snapshot" {
                state.cached_snapshot = Some(event.clone());
            }
            event
        };
        self.retain_and_broadcast_locked(&mut state, event);
    }

    fn publish_heartbeat(&self) {
        let mut state = self.state.lock().expect("activity event state lock");
        let sequence = self.sequence.fetch_add(1, Ordering::SeqCst) + 1;
        let event = self.encoder.encode(ActivityStreamEvent {
            schema_version: ACTIVITY_STREAM_EVENT_SCHEMA_VERSION,
            kind: "heartbeat",
            stream_id: self.stream_id.clone(),
            sequence,
            machine: self.machine.clone(),
            observed_at: activity_observed_at(),
            reason: None,
            sessions: None,
        });
        state.cached_snapshot = None;
        state.cached_reset = None;
        self.retain_and_broadcast_locked(&mut state, event);
    }

    fn retain_and_broadcast_locked(
        &self,
        state: &mut ActivityEventState,
        event: Arc<ActivityStreamFrame>,
    ) {
        state.history_bytes = state.history_bytes.saturating_add(event.replay_bytes());
        state.history.push_back(event.clone());
        while state.history.len() > ACTIVITY_STREAM_REPLAY_CAPACITY
            || state.history_bytes > ACTIVITY_STREAM_REPLAY_BYTE_CAPACITY
        {
            if let Some(evicted) = state.history.pop_front() {
                state.history_bytes = state.history_bytes.saturating_sub(evicted.replay_bytes());
            }
        }
        // A bounded broadcast is deliberately lossy. A lagged subscriber gets
        // a full reset and polling remains the convergence path.
        let _ = self.sender.send(event);
    }

    fn subscribe(self: &Arc<Self>, last_event_id: Option<&str>) -> ActivitySubscription {
        // Subscribe before taking the replay snapshot. Events racing with the
        // snapshot may be present in both sources; the sequence filter removes
        // those duplicates without losing an event.
        let receiver = self.sender.subscribe();
        let mut state = self.state.lock().expect("activity event state lock");
        let current_sequence = self.sequence.load(Ordering::SeqCst);
        let pending = match last_event_id.and_then(parse_activity_event_id) {
            None if last_event_id.is_none() => VecDeque::from([state
                .cached_snapshot
                .clone()
                .filter(|frame| frame.sequence == current_sequence)
                .unwrap_or_else(|| self.publish_full_event_locked(&mut state, "snapshot"))]),
            Some((stream_id, sequence))
                if stream_id == self.stream_id && sequence <= current_sequence =>
            {
                let replay: VecDeque<_> = state
                    .history
                    .iter()
                    .filter(|event| event.sequence > sequence)
                    .cloned()
                    .collect();
                let contiguous = if sequence == current_sequence {
                    replay.is_empty()
                } else {
                    replay
                        .front()
                        .is_some_and(|event| event.sequence == sequence.saturating_add(1))
                        && replay
                            .back()
                            .is_some_and(|event| event.sequence == current_sequence)
                        && replay
                            .iter()
                            .zip(replay.iter().skip(1))
                            .all(|(left, right)| right.sequence == left.sequence.saturating_add(1))
                };
                if contiguous {
                    replay
                } else {
                    VecDeque::from([self.publish_full_event_locked(&mut state, "reset")])
                }
            }
            _ => VecDeque::from([self.publish_full_event_locked(&mut state, "reset")]),
        };
        ActivitySubscription {
            log: self.clone(),
            pending,
            receiver,
            lifecycle: self.lifecycle.subscribe(),
            last_sequence: last_event_id
                .and_then(parse_activity_event_id)
                .filter(|(stream_id, sequence)| {
                    stream_id == &self.stream_id && *sequence <= current_sequence
                })
                .map_or(0, |(_, sequence)| sequence),
            reconciliation_sent: false,
            _subscriber_permit: None,
        }
    }

    fn reset_event(&self) -> Arc<ActivityStreamFrame> {
        let mut state = self.state.lock().expect("activity event state lock");
        let current_sequence = self.sequence.load(Ordering::SeqCst);
        state
            .cached_reset
            .clone()
            .filter(|frame| frame.sequence == current_sequence)
            .unwrap_or_else(|| self.publish_full_event_locked(&mut state, "reset"))
    }

    fn degraded_reset_event(&self) -> Arc<ActivityStreamFrame> {
        let state = self.state.lock().expect("activity event state lock");
        let sequence = self.sequence.fetch_add(1, Ordering::SeqCst) + 1;
        self.uncached_full_event_locked(&state, sequence, "reset")
    }

    fn publish_full_event_locked(
        &self,
        state: &mut ActivityEventState,
        kind: &'static str,
    ) -> Arc<ActivityStreamFrame> {
        let sequence = self.sequence.fetch_add(1, Ordering::SeqCst) + 1;
        state.cached_snapshot = None;
        state.cached_reset = None;
        let frame = self.uncached_full_event_locked(state, sequence, kind);
        if kind == "snapshot" {
            state.cached_snapshot = Some(frame.clone());
        } else {
            state.cached_reset = Some(frame.clone());
        }
        if state.latest_snapshot_oversized {
            state.cached_snapshot = Some(frame.clone());
            state.cached_reset = Some(frame.clone());
        }
        self.retain_and_broadcast_locked(state, frame.clone());
        frame
    }

    fn uncached_full_event_locked(
        &self,
        state: &ActivityEventState,
        sequence: u64,
        kind: &'static str,
    ) -> Arc<ActivityStreamFrame> {
        if state.latest_snapshot_oversized {
            return self.content_free_reset_frame(sequence, &state.latest_observed_at);
        }
        self.encoder.encode(ActivityStreamEvent {
            schema_version: ACTIVITY_STREAM_EVENT_SCHEMA_VERSION,
            kind,
            stream_id: self.stream_id.clone(),
            sequence,
            machine: self.machine.clone(),
            observed_at: state.latest_observed_at.clone(),
            reason: None,
            sessions: Some(state.latest_sessions.clone()),
        })
    }

    fn content_free_reset_frame(
        &self,
        sequence: u64,
        observed_at: &str,
    ) -> Arc<ActivityStreamFrame> {
        self.encoder.encode(ActivityStreamEvent {
            schema_version: ACTIVITY_STREAM_EVENT_SCHEMA_VERSION,
            kind: "reset",
            stream_id: self.stream_id.clone(),
            sequence,
            machine: self.machine.clone(),
            observed_at: observed_at.to_string(),
            reason: Some(ACTIVITY_STREAM_OVERSIZED_REASON),
            sessions: None,
        })
    }
}

struct ActivitySubscription {
    log: Arc<ActivityEventLog>,
    pending: VecDeque<Arc<ActivityStreamFrame>>,
    receiver: broadcast::Receiver<Arc<ActivityStreamFrame>>,
    lifecycle: watch::Receiver<ActivityBrokerLifecycle>,
    last_sequence: u64,
    reconciliation_sent: bool,
    _subscriber_permit: Option<OwnedSemaphorePermit>,
}

impl ActivitySubscription {
    fn reconcile_or_stop(&mut self) -> Option<Arc<ActivityStreamFrame>> {
        if self.reconciliation_sent {
            None
        } else {
            self.reconciliation_sent = true;
            Some(self.log.degraded_reset_event())
        }
    }

    async fn next_event(&mut self) -> Option<Arc<ActivityStreamFrame>> {
        loop {
            if *self.lifecycle.borrow() != ActivityBrokerLifecycle::Ready {
                return self.reconcile_or_stop();
            }
            if let Some(event) = self.pending.pop_front() {
                if event.sequence > self.last_sequence || event.kind == "reset" {
                    self.last_sequence = event.sequence;
                    return Some(event);
                }
                continue;
            }
            tokio::select! {
                biased;
                lifecycle = self.lifecycle.changed() => {
                    if lifecycle.is_err()
                        || *self.lifecycle.borrow() != ActivityBrokerLifecycle::Ready
                    {
                        return self.reconcile_or_stop();
                    }
                }
                received = self.receiver.recv() => match received {
                    Ok(event) if event.sequence > self.last_sequence => {
                        self.last_sequence = event.sequence;
                        return Some(event);
                    }
                    Ok(_) => continue,
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        let event = self.log.reset_event();
                        self.last_sequence = event.sequence;
                        return Some(event);
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        return self.reconcile_or_stop();
                    }
                }
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ActivityBrokerLifecycle {
    Starting,
    Ready,
    Degraded,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ActivitySubscribeError {
    Unavailable,
    Capacity,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ActivityChange {
    Refresh,
    Rescan,
    RootLost,
}

type ActivityWatchRearm = Arc<dyn Fn() -> bool + Send + Sync>;

#[derive(Default)]
struct ActivityChangeLoopControls {
    refresh_started: Option<mpsc::UnboundedSender<tokio::time::Instant>>,
    watch_rearm: Option<ActivityWatchRearm>,
}

fn create_activity_watcher(
    change_tx: mpsc::Sender<ActivityChange>,
    lifecycle: watch::Sender<ActivityBrokerLifecycle>,
    sessions_root: PathBuf,
) -> Option<RecommendedWatcher> {
    let callback_root = sessions_root.clone();
    let mut watcher = notify::recommended_watcher(move |result: notify::Result<NotifyEvent>| {
        activity_watch_callback(result, &change_tx, &lifecycle, &callback_root);
    })
    .ok()?;
    watcher
        .watch(&sessions_root, RecursiveMode::Recursive)
        .ok()?;
    Some(watcher)
}

struct ActivityBroker {
    log: Arc<ActivityEventLog>,
    lifecycle: watch::Sender<ActivityBrokerLifecycle>,
    subscribers: Arc<Semaphore>,
    _watcher: Arc<StdMutex<Option<RecommendedWatcher>>>,
}

impl ActivityBroker {
    async fn start(
        context: CliContext,
        machine: String,
        tmux_bin: PathBuf,
        session_collector: SessionCollector,
    ) -> Arc<Self> {
        let (lifecycle, _) = watch::channel(ActivityBrokerLifecycle::Starting);
        let (change_tx, change_rx) = mpsc::channel(1);
        let sessions_root = context.state_dir.join("sessions");
        let watcher = std::fs::create_dir_all(&sessions_root).ok().and_then(|()| {
            create_activity_watcher(change_tx.clone(), lifecycle.clone(), sessions_root.clone())
        });
        if watcher.is_none() {
            eprintln!(
                "warning: activity filesystem notifications unavailable; session polling will reconcile"
            );
        }
        let initial_context = context.clone();
        let initial_tmux = tmux_bin.clone();
        let initial_collector = session_collector.clone();
        let (initial, snapshot_available) = match tokio::task::spawn_blocking(move || {
            collect_activity_stream_sessions(&initial_collector, &initial_context, &initial_tmux)
        })
        .await
        {
            Ok(Ok(sessions)) => (sessions, true),
            Ok(Err(_)) | Err(_) => {
                eprintln!(
                    "warning: initial activity stream snapshot unavailable; session polling will reconcile"
                );
                (Vec::new(), false)
            }
        };
        let log = ActivityEventLog::new_with_lifecycle(machine, initial, lifecycle.clone());
        let ready = watcher.is_some()
            && snapshot_available
            && *lifecycle.borrow() != ActivityBrokerLifecycle::Degraded;
        lifecycle.send_replace(if ready {
            ActivityBrokerLifecycle::Ready
        } else {
            ActivityBrokerLifecycle::Degraded
        });
        let watcher = Arc::new(StdMutex::new(watcher));
        let broker = Arc::new(Self {
            log,
            lifecycle,
            subscribers: Arc::new(Semaphore::new(ACTIVITY_STREAM_SUBSCRIBER_CAPACITY)),
            _watcher: watcher.clone(),
        });
        if ready {
            let rearm_watcher = watcher;
            let rearm_tx = change_tx;
            let rearm_lifecycle = broker.lifecycle.clone();
            let rearm_root = sessions_root;
            let watch_rearm: ActivityWatchRearm = Arc::new(move || {
                if std::fs::create_dir_all(&rearm_root).is_err() {
                    return false;
                }
                let Some(rearmed) = create_activity_watcher(
                    rearm_tx.clone(),
                    rearm_lifecycle.clone(),
                    rearm_root.clone(),
                ) else {
                    return false;
                };
                *rearm_watcher.lock().expect("activity watcher lock") = Some(rearmed);
                true
            });
            tokio::spawn(activity_change_loop_inner(
                broker.log.clone(),
                broker.lifecycle.clone(),
                session_collector,
                context,
                tmux_bin,
                change_rx,
                ActivityChangeLoopControls {
                    refresh_started: None,
                    watch_rearm: Some(watch_rearm),
                },
            ));
            tokio::spawn(activity_heartbeat_loop(
                broker.log.clone(),
                broker.lifecycle.clone(),
            ));
        }
        broker
    }

    #[cfg(test)]
    fn for_test(machine: &str) -> Arc<Self> {
        Self::for_test_with_subscriber_limit(machine, ACTIVITY_STREAM_SUBSCRIBER_CAPACITY)
    }

    #[cfg(test)]
    fn for_test_with_subscriber_limit(machine: &str, subscriber_limit: usize) -> Arc<Self> {
        let watcher = notify::recommended_watcher(|_: notify::Result<NotifyEvent>| {})
            .expect("test filesystem watcher");
        let log = ActivityEventLog::new(machine.to_string(), Vec::new());
        let lifecycle = log.lifecycle.clone();
        Arc::new(Self {
            log,
            lifecycle,
            subscribers: Arc::new(Semaphore::new(subscriber_limit)),
            _watcher: Arc::new(StdMutex::new(Some(watcher))),
        })
    }

    #[cfg(test)]
    fn for_test_with_session_collector(
        machine: &str,
        context: &CliContext,
        tmux_bin: &Path,
        session_collector: SessionCollector,
    ) -> Arc<Self> {
        let sessions = collect_activity_stream_sessions(&session_collector, context, tmux_bin)
            .expect("test activity snapshot source");
        let watcher = notify::recommended_watcher(|_: notify::Result<NotifyEvent>| {})
            .expect("test filesystem watcher");
        let log = ActivityEventLog::new(machine.to_string(), sessions);
        let lifecycle = log.lifecycle.clone();
        Arc::new(Self {
            log,
            lifecycle,
            subscribers: Arc::new(Semaphore::new(ACTIVITY_STREAM_SUBSCRIBER_CAPACITY)),
            _watcher: Arc::new(StdMutex::new(Some(watcher))),
        })
    }

    fn subscribe(
        &self,
        last_event_id: Option<&str>,
    ) -> Result<ActivitySubscription, ActivitySubscribeError> {
        let lifecycle = self.lifecycle.subscribe();
        if *lifecycle.borrow() != ActivityBrokerLifecycle::Ready {
            return Err(ActivitySubscribeError::Unavailable);
        }
        let permit = self
            .subscribers
            .clone()
            .try_acquire_owned()
            .map_err(|_| ActivitySubscribeError::Capacity)?;
        if *lifecycle.borrow() != ActivityBrokerLifecycle::Ready {
            return Err(ActivitySubscribeError::Unavailable);
        }
        let mut subscription = self.log.subscribe(last_event_id);
        subscription.lifecycle = lifecycle;
        subscription._subscriber_permit = Some(permit);
        Ok(subscription)
    }
}

fn activity_observed_at() -> String {
    jiff::Timestamp::now().to_string()
}

fn parse_activity_event_id(raw: &str) -> Option<(String, u64)> {
    let (stream_id, sequence) = raw.rsplit_once(':')?;
    if stream_id.is_empty() {
        return None;
    }
    Some((stream_id.to_string(), sequence.parse().ok()?))
}

fn degrade_activity_broker(lifecycle: &watch::Sender<ActivityBrokerLifecycle>, warning: &str) {
    if lifecycle.send_replace(ActivityBrokerLifecycle::Degraded)
        != ActivityBrokerLifecycle::Degraded
    {
        eprintln!("warning: {warning}; session polling will reconcile");
    }
}

fn activity_watch_callback(
    result: notify::Result<NotifyEvent>,
    change_tx: &mpsc::Sender<ActivityChange>,
    lifecycle: &watch::Sender<ActivityBrokerLifecycle>,
    sessions_root: &Path,
) {
    match result {
        Ok(event) if activity_notify_event_root_lost(&event, sessions_root) => {
            if change_tx.try_send(ActivityChange::RootLost).is_err() {
                degrade_activity_broker(
                    lifecycle,
                    "activity sessions root invalidation could not be queued; activity stream disabled",
                );
            }
        }
        Ok(event) if event.need_rescan() => match change_tx.try_send(ActivityChange::Rescan) {
            Ok(()) | Err(mpsc::error::TrySendError::Full(_)) => {}
            Err(mpsc::error::TrySendError::Closed(_)) => degrade_activity_broker(
                lifecycle,
                "activity rescan could not be queued; activity stream disabled",
            ),
        },
        Ok(event) if activity_notify_event_relevant(&event) => {
            match change_tx.try_send(ActivityChange::Refresh) {
                Ok(()) | Err(mpsc::error::TrySendError::Full(_)) => {}
                Err(mpsc::error::TrySendError::Closed(_)) => degrade_activity_broker(
                    lifecycle,
                    "activity refresh could not be queued; activity stream disabled",
                ),
            }
        }
        Ok(_) => {}
        Err(_) => degrade_activity_broker(
            lifecycle,
            "activity filesystem notification failed; activity stream disabled",
        ),
    }
}

fn activity_notify_event_root_lost(event: &NotifyEvent, sessions_root: &Path) -> bool {
    matches!(
        event.kind,
        EventKind::Remove(_) | EventKind::Modify(notify::event::ModifyKind::Name(_))
    ) && event.paths.iter().any(|path| path == sessions_root)
}

fn activity_notify_event_relevant(event: &NotifyEvent) -> bool {
    let known_snapshot_path = event.paths.iter().any(|path| {
        path.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| matches!(name, "activity.json" | "session.json"))
    });
    // Linux inotify normally names the removed file, while macOS FSEvents can
    // coalesce a rename/removal to its watched directory. All paths delivered
    // here are already scoped beneath the sessions root, so treat rename and
    // remove kinds as lifecycle invalidations without broadening modify noise
    // from terminal FIFOs or attachment writes.
    known_snapshot_path
        || matches!(
            event.kind,
            EventKind::Remove(_) | EventKind::Modify(notify::event::ModifyKind::Name(_))
        )
}

fn collect_activity_stream_sessions(
    session_collector: &SessionCollector,
    context: &CliContext,
    tmux_bin: &Path,
) -> Result<Vec<ActivityStreamSession>, CliError> {
    session_collector(context, tmux_bin).map(|sessions| {
        sessions
            .into_iter()
            .map(|session| ActivityStreamSession {
                id: session.id,
                turn_state: session
                    .turn_state
                    .as_ref()
                    .map(crate::activity::stream_projection),
            })
            .collect()
    })
}

async fn activity_refresh_snapshot(
    log: Arc<ActivityEventLog>,
    lifecycle: watch::Sender<ActivityBrokerLifecycle>,
    session_collector: SessionCollector,
    context: CliContext,
    tmux_bin: PathBuf,
) -> bool {
    match tokio::task::spawn_blocking(move || {
        collect_activity_stream_sessions(&session_collector, &context, &tmux_bin)
    })
    .await
    {
        Ok(Ok(sessions)) => {
            log.publish_snapshot(sessions);
            true
        }
        Ok(Err(_)) | Err(_) => {
            degrade_activity_broker(
                &lifecycle,
                "activity stream snapshot refresh failed; activity stream disabled",
            );
            false
        }
    }
}

#[cfg(test)]
async fn activity_change_loop(
    log: Arc<ActivityEventLog>,
    lifecycle: watch::Sender<ActivityBrokerLifecycle>,
    session_collector: SessionCollector,
    context: CliContext,
    tmux_bin: PathBuf,
    changes: mpsc::Receiver<ActivityChange>,
) {
    activity_change_loop_inner(
        log,
        lifecycle,
        session_collector,
        context,
        tmux_bin,
        changes,
        ActivityChangeLoopControls::default(),
    )
    .await;
}

async fn activity_change_loop_inner(
    log: Arc<ActivityEventLog>,
    lifecycle: watch::Sender<ActivityBrokerLifecycle>,
    session_collector: SessionCollector,
    context: CliContext,
    tmux_bin: PathBuf,
    mut changes: mpsc::Receiver<ActivityChange>,
    controls: ActivityChangeLoopControls,
) {
    let mut lifecycle_rx = lifecycle.subscribe();
    let mut last_refresh_started_at = None;
    loop {
        let change = tokio::select! {
            biased;
            changed = lifecycle_rx.changed() => {
                if changed.is_err()
                    || *lifecycle_rx.borrow() != ActivityBrokerLifecycle::Ready
                {
                    return;
                }
                continue;
            }
            change = changes.recv() => change,
        };
        let Some(change) = change else {
            degrade_activity_broker(
                &lifecycle,
                "activity filesystem notification channel closed; activity stream disabled",
            );
            return;
        };
        if change == ActivityChange::RootLost
            && !controls.watch_rearm.as_ref().is_some_and(|rearm| rearm())
        {
            degrade_activity_broker(
                &lifecycle,
                "activity sessions root watch could not be re-armed; activity stream disabled",
            );
            return;
        }
        let batch_started_at = tokio::time::Instant::now();
        let minimum_start_at = last_refresh_started_at
            .map(|started_at| started_at + ACTIVITY_STREAM_MAX_REFRESH_CADENCE)
            .unwrap_or(batch_started_at);
        let quiet = tokio::time::sleep_until(
            (batch_started_at + ACTIVITY_STREAM_DEBOUNCE).max(minimum_start_at),
        );
        let cadence = tokio::time::sleep_until(if minimum_start_at > batch_started_at {
            minimum_start_at
        } else {
            batch_started_at + ACTIVITY_STREAM_MAX_REFRESH_CADENCE
        });
        tokio::pin!(quiet);
        tokio::pin!(cadence);
        loop {
            tokio::select! {
                biased;
                changed = lifecycle_rx.changed() => {
                    if changed.is_err()
                        || *lifecycle_rx.borrow() != ActivityBrokerLifecycle::Ready
                    {
                        return;
                    }
                }
                _ = &mut cadence => break,
                _ = &mut quiet => break,
                change = changes.recv() => match change {
                    None => {
                        degrade_activity_broker(
                            &lifecycle,
                            "activity filesystem notification channel closed; activity stream disabled",
                        );
                        return;
                    }
                    Some(change) => {
                    if change == ActivityChange::RootLost
                        && !controls
                            .watch_rearm
                            .as_ref()
                            .is_some_and(|rearm| rearm())
                    {
                        degrade_activity_broker(
                            &lifecycle,
                            "activity sessions root watch could not be re-armed; activity stream disabled",
                        );
                        return;
                    }
                    quiet
                        .as_mut()
                        .reset(
                            (tokio::time::Instant::now() + ACTIVITY_STREAM_DEBOUNCE)
                                .max(minimum_start_at),
                        );
                    }
                }
            }
        }
        let started_at = tokio::time::Instant::now();
        last_refresh_started_at = Some(started_at);
        if let Some(observer) = &controls.refresh_started {
            let _ = observer.send(started_at);
        }
        if !activity_refresh_snapshot(
            log.clone(),
            lifecycle.clone(),
            session_collector.clone(),
            context.clone(),
            tmux_bin.clone(),
        )
        .await
        {
            return;
        }
    }
}

fn activity_publish_heartbeat_if_ready(
    log: &ActivityEventLog,
    lifecycle: &watch::Sender<ActivityBrokerLifecycle>,
) -> bool {
    if *lifecycle.borrow() != ActivityBrokerLifecycle::Ready {
        return false;
    }
    log.publish_heartbeat();
    true
}

async fn activity_heartbeat_loop(
    log: Arc<ActivityEventLog>,
    lifecycle: watch::Sender<ActivityBrokerLifecycle>,
) {
    let mut interval = tokio::time::interval(ACTIVITY_STREAM_HEARTBEAT_INTERVAL);
    let mut lifecycle_rx = lifecycle.subscribe();
    if *lifecycle_rx.borrow() != ActivityBrokerLifecycle::Ready {
        return;
    }
    interval.tick().await;
    loop {
        tokio::select! {
            biased;
            changed = lifecycle_rx.changed() => {
                if changed.is_err()
                    || *lifecycle_rx.borrow() != ActivityBrokerLifecycle::Ready
                {
                    return;
                }
            }
            _ = interval.tick() => {
                if !activity_publish_heartbeat_if_ready(&log, &lifecycle) {
                    return;
                }
            }
        }
    }
}

// --- response helpers ---------------------------------------------------------

// Every serve response uses one wire contract, `cli.agent-session.serve.v1`,
// distinct from the CLI stdout envelopes: the `data` shape ({machine, ...}) is
// serve-specific, so it must NOT reuse the CLI's per-command schema versions.
fn serve_schema() -> String {
    schema_version_for(BINARY, "serve", 1)
}

fn envelope_ok(data: Value) -> Response {
    (
        StatusCode::OK,
        Json(json!({
            "schema_version": serve_schema(),
            "ok": true,
            "data": data,
        })),
    )
        .into_response()
}

fn envelope_err(err: CliError) -> Response {
    let data = err.into_inner();
    let status = match data.code.as_str() {
        "session-not-found" => StatusCode::NOT_FOUND,
        "session-exists"
        | "title-revision-conflict"
        | "title-state-conflict"
        | "session-incarnation-conflict"
        | "codex-account-session-incarnation-conflict"
        | "codex-account-session-busy" => StatusCode::CONFLICT,
        _ => match data.exit_code {
            exit::USAGE => StatusCode::BAD_REQUEST,
            exit::DATA => StatusCode::UNPROCESSABLE_ENTITY,
            exit::UNAVAILABLE => StatusCode::SERVICE_UNAVAILABLE,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        },
    };
    let mut error = json!({ "code": data.code, "message": data.message });
    if let Some(details) = data.details
        && let Some(map) = error.as_object_mut()
    {
        map.insert("details".to_string(), details);
    }
    (
        status,
        Json(json!({
            "schema_version": serve_schema(),
            "ok": false,
            "error": error,
        })),
    )
        .into_response()
}

fn status_json(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        Json(json!({
            "schema_version": serve_schema(),
            "ok": false,
            "error": { "code": code, "message": message },
        })),
    )
        .into_response()
}

fn join_err() -> Response {
    envelope_err(CliError::runtime(
        "serve-task-failed",
        "internal task failed",
        None,
    ))
}

#[derive(Debug, Serialize)]
struct UsageReport {
    schema_version: String,
    ok: bool,
    providers: Vec<UsageProvider>,
}

#[derive(Debug, Serialize)]
struct UsageProvider {
    id: String,
    label: String,
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason_code: Option<ProviderUsageReason>,
    windows: Vec<UsageWindow>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<UsageProviderError>,
    #[serde(skip)]
    fresh_authoritative: bool,
}

#[derive(Debug, Serialize)]
struct UsageWindow {
    label: String,
    used_percent: i64,
    remaining_percent: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    reset_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reset_at_epoch: Option<i64>,
}

#[derive(Debug, Serialize)]
struct UsageProviderError {
    code: String,
    message: String,
}

struct UsageHelperOutput {
    status_code: Option<i32>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    timed_out: bool,
}

async fn usage_handler(State(state): State<Arc<ServeState>>) -> Response {
    let timeout = usage_timeout();
    let codex = tokio::task::spawn_blocking(move || collect_codex_usage(timeout));
    let claude = tokio::task::spawn_blocking(move || collect_claude_usage(timeout));
    let (codex, claude) = tokio::join!(codex, claude);

    let providers = vec![
        codex.unwrap_or_else(|_| provider_internal_error("codex", "Codex")),
        claude.unwrap_or_else(|_| provider_internal_error("claude", "Claude")),
    ];
    let ok = providers.iter().all(|provider| provider.ok);
    envelope_ok(json!({
        "machine": state.machine,
        "usage": UsageReport {
            schema_version: USAGE_SCHEMA_VERSION.to_string(),
            ok,
            providers,
        },
    }))
}

fn usage_timeout() -> Duration {
    std::env::var("AGENT_SESSION_USAGE_TIMEOUT_MS")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .filter(|millis| *millis > 0)
        .map(Duration::from_millis)
        .unwrap_or_else(|| Duration::from_millis(DEFAULT_USAGE_TIMEOUT_MS))
}

fn collect_codex_usage(timeout: Duration) -> UsageProvider {
    match run_usage_helper(
        "codex-cli",
        &[
            "diag",
            "rate-limits",
            "--all",
            "--format",
            "json",
            "--no-refresh-auth",
        ],
        &[],
        timeout,
    ) {
        Ok(output) => normalize_codex_usage(output),
        Err(message) => provider_error("codex", "Codex", "helper-spawn-failed", message),
    }
}

fn collect_claude_usage(timeout: Duration) -> UsageProvider {
    let explicit_timeout = non_empty_env("CLAUDE_PROMPT_SEGMENT_CLAUDE_TIMEOUT_SECONDS");
    let timeout_seconds =
        claude_inner_timeout_seconds(timeout, explicit_timeout.as_deref()).to_string();
    match run_usage_helper(
        "claude-cli",
        &["usage", "--format", "json", "--source", "auto"],
        &[(
            "CLAUDE_PROMPT_SEGMENT_CLAUDE_TIMEOUT_SECONDS",
            timeout_seconds.as_str(),
        )],
        timeout,
    ) {
        Ok(output) => normalize_claude_usage(output),
        Err(message) => provider_error("claude", "Claude", "helper-spawn-failed", message),
    }
}

fn claude_inner_timeout_seconds(timeout: Duration, explicit: Option<&str>) -> u64 {
    let max_inner_seconds = timeout
        .checked_sub(Duration::from_secs(CLAUDE_USAGE_CLEANUP_SLACK_SECONDS))
        .map(|remaining| remaining.as_secs())
        .unwrap_or(0)
        .max(1);
    explicit
        .and_then(|raw| raw.parse::<u64>().ok())
        .filter(|seconds| *seconds > 0)
        .unwrap_or(max_inner_seconds)
        .min(max_inner_seconds)
}

fn run_usage_helper(
    program: &str,
    args: &[&str],
    envs: &[(&str, &str)],
    timeout: Duration,
) -> Result<UsageHelperOutput, String> {
    let mut child = ProcessCommand::new(program)
        .args(args)
        .envs(envs.iter().copied())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(|err| format!("failed to start {program}: {err}"))?;

    let start = Instant::now();
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status.code().unwrap_or(1)),
            Ok(None) => {
                if start.elapsed() >= timeout {
                    timed_out = true;
                    unsafe {
                        let pgid = -(child.id() as libc::pid_t);
                        let _ = libc::kill(pgid, libc::SIGKILL);
                    }
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(err) => {
                let _ = child.kill();
                return Err(format!("failed to wait for {program}: {err}"));
            }
        }
    };

    let mut stdout = Vec::new();
    if let Some(mut pipe) = child.stdout.take() {
        let _ = pipe.read_to_end(&mut stdout);
    }
    let mut stderr = Vec::new();
    if let Some(mut pipe) = child.stderr.take() {
        let _ = pipe.read_to_end(&mut stderr);
    }

    Ok(UsageHelperOutput {
        status_code: status,
        stdout,
        stderr,
        timed_out,
    })
}

fn normalize_codex_usage(output: UsageHelperOutput) -> UsageProvider {
    if output.timed_out {
        return provider_error(
            "codex",
            "Codex",
            "helper-timeout",
            "codex usage helper timed out".to_string(),
        );
    }

    let value: Value = match serde_json::from_slice(&output.stdout) {
        Ok(value) => value,
        Err(_) => {
            return provider_error(
                "codex",
                "Codex",
                "helper-invalid-json",
                helper_failure_message("codex usage unavailable", &output),
            );
        }
    };

    let reason_code = reason_from_helper_json(&value);
    let results = value
        .get("results")
        .and_then(Value::as_array)
        .cloned()
        .or_else(|| value.get("result").map(|result| vec![result.clone()]))
        .unwrap_or_default();

    let mut windows = Vec::new();
    for result in results.iter().filter(|result| {
        result
            .get("ok")
            .and_then(Value::as_bool)
            .unwrap_or_else(|| result.get("status").and_then(Value::as_str) == Some("ok"))
    }) {
        windows.extend(windows_from_value(result, None));
        if windows.is_empty()
            && let Some(summary) = result.get("summary")
        {
            windows.extend(windows_from_codex_summary(summary));
        }
    }

    if !windows.is_empty() {
        return UsageProvider {
            id: "codex".to_string(),
            label: "Codex".to_string(),
            ok: true,
            source: Some("codex-cli".to_string()),
            reason_code,
            windows,
            error: None,
            fresh_authoritative: false,
        };
    }

    let (code, message) =
        error_from_helper_json(&value, output.status_code, "codex usage unavailable");
    provider_error_with_reason("codex", "Codex", &code, message, reason_code)
}

fn normalize_claude_usage(output: UsageHelperOutput) -> UsageProvider {
    if output.timed_out {
        return provider_error(
            "claude",
            "Claude",
            "helper-timeout",
            "claude usage helper timed out".to_string(),
        );
    }

    let value: Value = match serde_json::from_slice(&output.stdout) {
        Ok(value) => value,
        Err(_) => {
            return provider_error(
                "claude",
                "Claude",
                "helper-invalid-json",
                helper_failure_message("claude usage unavailable", &output),
            );
        }
    };

    let result = value.get("result").unwrap_or(&value);
    let reason_code = reason_from_helper_json(&value);
    let fresh_authoritative = output.status_code == Some(0)
        && value.get("ok").and_then(Value::as_bool) == Some(true)
        && result
            .get("source")
            .and_then(Value::as_str)
            .is_some_and(|source| matches!(source, "oauth" | "cli"))
        && result.get("stale").and_then(Value::as_bool) == Some(false)
        && reason_code.is_none();
    let reference_epoch = i64_field(result, &["updated_at", "updatedAt"]);
    let windows = windows_from_value(result, reference_epoch);
    if !windows.is_empty() {
        return UsageProvider {
            id: "claude".to_string(),
            label: "Claude".to_string(),
            ok: true,
            source: Some("claude-cli".to_string()),
            reason_code,
            windows,
            error: None,
            fresh_authoritative,
        };
    }

    let (code, message) =
        error_from_helper_json(&value, output.status_code, "claude usage unavailable");
    provider_error_with_reason("claude", "Claude", &code, message, reason_code)
}

fn windows_from_value(value: &Value, reference_epoch: Option<i64>) -> Vec<UsageWindow> {
    value
        .get("windows")
        .and_then(Value::as_array)
        .map(|windows| {
            windows
                .iter()
                .filter_map(|window| usage_window_from_value(window, reference_epoch))
                .collect()
        })
        .unwrap_or_default()
}

fn usage_window_from_value(value: &Value, reference_epoch: Option<i64>) -> Option<UsageWindow> {
    let label = value.get("label").and_then(Value::as_str)?.to_string();
    let used_percent = i64_field(value, &["used_percent"])?;
    let remaining_percent = i64_field(value, &["remaining_percent"])
        .unwrap_or_else(|| (100 - used_percent).clamp(0, 100));
    let reset_at = reset_at_text_field(value, RESET_AT_KEYS);
    let reset_at_epoch = epoch_field(value, RESET_AT_EPOCH_KEYS, reference_epoch);
    Some(UsageWindow {
        label,
        used_percent,
        remaining_percent,
        reset_at,
        reset_at_epoch,
    })
}

fn windows_from_codex_summary(summary: &Value) -> Vec<UsageWindow> {
    let mut windows = Vec::new();
    if let Some(remaining) = i64_field(summary, &["non_weekly_remaining"]) {
        windows.push(UsageWindow {
            label: summary
                .get("non_weekly_label")
                .and_then(Value::as_str)
                .unwrap_or("Non-weekly")
                .to_string(),
            used_percent: (100 - remaining).clamp(0, 100),
            remaining_percent: remaining,
            reset_at: None,
            reset_at_epoch: i64_field(
                summary,
                &["non_weekly_reset_epoch", "non_weekly_reset_at_epoch"],
            ),
        });
    }
    if let Some(remaining) = i64_field(summary, &["weekly_remaining"]) {
        windows.push(UsageWindow {
            label: "Weekly".to_string(),
            used_percent: (100 - remaining).clamp(0, 100),
            remaining_percent: remaining,
            reset_at: None,
            reset_at_epoch: i64_field(summary, &["weekly_reset_epoch", "weekly_reset_at_epoch"]),
        });
    }
    windows
}

fn reset_at_text_field(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        value
            .get(*key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|raw| !raw.is_empty())
            .filter(|raw| safe_reset_at_text(raw))
            .map(ToString::to_string)
    })
}

fn safe_reset_at_text(raw: &str) -> bool {
    if raw.len() > 256 || raw.chars().any(char::is_control) {
        return false;
    }
    let lower = raw.to_ascii_lowercase();
    if lower.contains("access_token")
        || lower.contains("refresh_token")
        || lower.contains("authorization")
        || lower.contains("bearer")
        || lower.contains("sk-")
        || lower.contains("account_id")
        || lower.contains("account-id")
        || lower.contains("acct_")
        || raw.contains('@')
    {
        return false;
    }
    !(raw.starts_with('/')
        || raw.starts_with("~/")
        || raw.starts_with("$HOME/")
        || raw.contains("/home/")
        || raw.contains("/Users/"))
}

fn epoch_field(value: &Value, keys: &[&str], reference_epoch: Option<i64>) -> Option<i64> {
    keys.iter().find_map(|key| {
        value
            .get(*key)
            .and_then(|value| epoch_seconds_from_value(value, reference_epoch))
    })
}

fn epoch_seconds_from_value(value: &Value, reference_epoch: Option<i64>) -> Option<i64> {
    match value {
        Value::Number(number) => number
            .as_i64()
            .or_else(|| number.as_f64().and_then(epoch_seconds_from_f64))
            .map(normalize_epoch_seconds),
        Value::String(raw) => reset_epoch_seconds_from_str(raw, reference_epoch),
        _ => None,
    }
}

fn i64_field(value: &Value, keys: &[&str]) -> Option<i64> {
    keys.iter().find_map(|key| {
        value.get(*key).and_then(|value| {
            value
                .as_i64()
                .or_else(|| value.as_f64().map(|n| n.round() as i64))
        })
    })
}

fn error_from_helper_json(
    value: &Value,
    status_code: Option<i32>,
    fallback: &str,
) -> (String, String) {
    let code = value
        .get("error")
        .and_then(|error| error.get("code"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            if status_code.unwrap_or(0) == 0 {
                "usage-unavailable"
            } else {
                "helper-failed"
            }
        })
        .to_string();
    let message = value
        .get("error")
        .and_then(|error| error.get("message"))
        .and_then(Value::as_str)
        .unwrap_or(fallback);
    (code, sanitize_helper_message(message))
}

fn reason_from_helper_json(value: &Value) -> Option<ProviderUsageReason> {
    fn direct(value: &Value) -> Option<ProviderUsageReason> {
        let direct = value
            .get("reason_code")
            .and_then(Value::as_str)
            .and_then(ProviderUsageReason::from_code);
        let details = value
            .get("error")
            .and_then(|error| error.get("details"))
            .and_then(|details| details.get("reason_code"))
            .and_then(Value::as_str)
            .and_then(ProviderUsageReason::from_code);
        match (direct, details) {
            (Some(first), Some(second)) => Some(prefer_reason(first, second)),
            (Some(reason), None) | (None, Some(reason)) => Some(reason),
            (None, None) => None,
        }
    }

    let mut reason = direct(value);
    let mut merge = |candidate: Option<ProviderUsageReason>| {
        if let Some(candidate) = candidate {
            reason = Some(
                reason
                    .map(|current| prefer_reason(current, candidate))
                    .unwrap_or(candidate),
            );
        }
    };
    merge(value.get("result").and_then(direct));
    if let Some(results) = value.get("results").and_then(Value::as_array) {
        for result in results {
            merge(direct(result));
        }
    }
    reason
}

fn helper_failure_message(prefix: &str, output: &UsageHelperOutput) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let message = stderr
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or(prefix);
    sanitize_helper_message(message)
}

fn provider_internal_error(id: &str, label: &str) -> UsageProvider {
    provider_error(
        id,
        label,
        "serve-task-failed",
        "usage provider task failed".to_string(),
    )
}

fn provider_error(id: &str, label: &str, code: &str, message: String) -> UsageProvider {
    let reason_code = match code {
        "helper-timeout" => Some(ProviderUsageReason::Timeout),
        "helper-spawn-failed" | "helper-invalid-json" | "serve-task-failed" => {
            Some(ProviderUsageReason::ServiceUnavailable)
        }
        _ => None,
    };
    provider_error_with_reason(id, label, code, message, reason_code)
}

fn provider_error_with_reason(
    id: &str,
    label: &str,
    code: &str,
    message: String,
    reason_code: Option<ProviderUsageReason>,
) -> UsageProvider {
    UsageProvider {
        id: id.to_string(),
        label: label.to_string(),
        ok: false,
        source: None,
        reason_code,
        windows: Vec::new(),
        error: Some(UsageProviderError {
            code: code.to_string(),
            message: sanitize_helper_message(&message),
        }),
        fresh_authoritative: false,
    }
}

fn sanitize_helper_message(message: &str) -> String {
    let normalized = message.replace(['\n', '\r', '\t'], " ");
    let mut cleaned = Vec::new();
    let mut redact_next = false;
    for token in normalized.split_whitespace() {
        let lower = token.to_ascii_lowercase();
        let should_redact = redact_next
            || lower.contains("access_token")
            || lower.contains("refresh_token")
            || lower.contains("authorization")
            || lower.contains("bearer")
            || lower.contains("sk-")
            || lower.contains("account_id")
            || lower.contains("account-id")
            || lower.contains("acct_")
            || token.contains('@');
        redact_next = lower.contains("bearer") || lower.contains("authorization:");

        if should_redact {
            cleaned.push("[redacted]".to_string());
        } else if token.starts_with('/')
            || token.starts_with("~/")
            || token.starts_with("$HOME/")
            || token.contains("/home/")
            || token.contains("/Users/")
        {
            cleaned.push("[path]".to_string());
        } else {
            cleaned.push(token.to_string());
        }
        if cleaned.len() >= 24 {
            break;
        }
    }
    if cleaned.is_empty() {
        "usage unavailable".to_string()
    } else {
        cleaned.join(" ")
    }
}

/// XOR-accumulating byte comparison so a correct-length wrong token is compared
/// in constant time (no early return on the first differing byte). The length
/// check is intentionally NOT constant-time — the token length is not treated as
/// a secret; the token itself is the high-entropy secret. Never echoed anywhere.
fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Enforce the bearer token on authenticated endpoints. Returns `Some(denial)`
/// to reject (401), or `Some(503)` when the daemon has no token configured
/// (fail closed); returns `None` when the request is authorized.
fn deny_unauthorized(state: &ServeState, headers: &HeaderMap) -> Option<Response> {
    let Some(expected) = state.token.as_deref() else {
        return Some(status_json(
            StatusCode::SERVICE_UNAVAILABLE,
            "token-not-configured",
            "server has no token configured; authenticated endpoints are disabled",
        ));
    };
    let provided = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    match provided {
        Some(token) if constant_time_eq(token, expected) => None,
        _ => Some(status_json(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "missing or invalid bearer token",
        )),
    }
}

// --- request bodies -----------------------------------------------------------

#[derive(Debug, Deserialize)]
struct GlanceQuery {
    tail: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct CreateBody {
    agent: String,
    cwd: Option<String>,
    title: Option<String>,
    id: Option<String>,
    prompt: Option<String>,
    #[serde(default, alias = "resume_id")]
    provider_resume_id: Option<String>,
    #[serde(default)]
    agent_args: Vec<String>,
    #[serde(default)]
    codex_account: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SendBody {
    text: Option<String>,
    #[serde(default)]
    keys: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct StructuredPromptBody {
    text: String,
}

#[derive(Debug, Deserialize)]
struct AutoResumeBody {
    enabled: bool,
}

#[derive(Debug, Deserialize)]
struct CodexAccountBody {
    account: String,
    expected_session_incarnation: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AttachmentQuery {
    filename: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WorkdirQuery {
    q: Option<String>,
    limit: Option<usize>,
    #[serde(default)]
    git_only: bool,
    #[serde(default)]
    exclude_worktrees: bool,
}

#[derive(Debug, Deserialize)]
struct RepoRemoteUrlQuery {
    cwd: Option<String>,
}

// --- handlers -----------------------------------------------------------------

async fn healthz(State(state): State<Arc<ServeState>>) -> Response {
    envelope_ok(json!({ "status": "ok", "machine": state.machine }))
}

async fn list_handler(State(state): State<Arc<ServeState>>) -> Response {
    let context = state.context.clone();
    let tmux = state.tmux_bin.clone();
    let collect = state.session_collector.clone();
    match tokio::task::spawn_blocking(move || collect(&context, &tmux)).await {
        Ok(Ok(sessions)) => envelope_ok(json!({
            "machine": state.machine,
            "observed_at": activity_observed_at(),
            "sessions": sessions,
        })),
        Ok(Err(err)) => envelope_err(err),
        Err(_) => join_err(),
    }
}

async fn codex_accounts_handler(
    State(state): State<Arc<ServeState>>,
    headers: HeaderMap,
) -> Response {
    if let Some(response) = deny_unauthorized(&state, &headers) {
        return response;
    }
    match tokio::task::spawn_blocking(crate::codex_account::list_accounts).await {
        Ok(Ok(accounts)) => envelope_ok(json!({ "machine": state.machine, "accounts": accounts })),
        Ok(Err(err)) => envelope_err(err),
        Err(_) => join_err(),
    }
}

async fn activity_events_handler(
    State(state): State<Arc<ServeState>>,
    headers: HeaderMap,
) -> Response {
    if let Some(response) = deny_unauthorized(&state, &headers) {
        return response;
    }
    let last_event_id = headers
        .get("last-event-id")
        .and_then(|value| value.to_str().ok());
    let subscription = match state.activity_broker.subscribe(last_event_id) {
        Ok(subscription) => subscription,
        Err(ActivitySubscribeError::Unavailable) => {
            return status_json(
                StatusCode::SERVICE_UNAVAILABLE,
                "activity-stream-unavailable",
                "activity streaming is degraded or unavailable; use session polling",
            );
        }
        Err(ActivitySubscribeError::Capacity) => {
            return status_json(
                StatusCode::TOO_MANY_REQUESTS,
                "activity-stream-capacity",
                "activity stream subscriber capacity reached; retry with polling fallback",
            );
        }
    };
    let stream = futures_util::stream::unfold(subscription, |mut subscription| async move {
        subscription
            .next_event()
            .await
            .map(|event| (Ok::<_, Infallible>(event.sse.clone()), subscription))
    });
    let mut response = Response::new(Body::from_stream(stream));
    response.headers_mut().insert(
        CONTENT_TYPE,
        "text/event-stream".parse().expect("static header"),
    );
    response.headers_mut().insert(
        "cache-control",
        "no-cache, no-transform".parse().expect("static header"),
    );
    response
        .headers_mut()
        .insert("x-accel-buffering", "no".parse().expect("static header"));
    response
}

async fn glance_handler(
    State(state): State<Arc<ServeState>>,
    AxPath(id): AxPath<String>,
    Query(query): Query<GlanceQuery>,
) -> Response {
    let context = state.context.clone();
    let args = cli::GlanceArgs {
        id,
        tail: query.tail.unwrap_or(cli::DEFAULT_GLANCE_TAIL),
        tmux_bin: Some(state.tmux_bin.clone()),
        format: nils_common::cli_contract::OutputFormat::Json,
    };
    match tokio::task::spawn_blocking(move || glance_session(&context, args)).await {
        Ok(Ok(glance)) => envelope_ok(json!({ "machine": state.machine, "glance": glance })),
        Ok(Err(err)) => envelope_err(err),
        Err(_) => join_err(),
    }
}

// Return the session server's tmux paste buffer (`tmux show-buffer`) — the text a
// mouse-selection-copying TUI (e.g. Claude Code) placed there. Read-only, open on
// loopback like `glance`; the browser edge uses it so a right-click Copy can reach
// the on-screen selection that a live TUI never exposes to the DOM.
async fn buffer_handler(
    State(state): State<Arc<ServeState>>,
    AxPath(id): AxPath<String>,
) -> Response {
    let context = state.context.clone();
    let tmux = state.tmux_bin.clone();
    match tokio::task::spawn_blocking(move || session_clipboard_buffer(&context, &id, &tmux)).await
    {
        Ok(Ok(text)) => envelope_ok(json!({ "machine": state.machine, "text": text })),
        Ok(Err(err)) => envelope_err(err),
        Err(_) => join_err(),
    }
}

async fn create_handler(
    State(state): State<Arc<ServeState>>,
    headers: HeaderMap,
    Json(body): Json<CreateBody>,
) -> Response {
    if let Some(resp) = deny_unauthorized(&state, &headers) {
        return resp;
    }
    let Some(agent) = AgentKind::from_name(&body.agent) else {
        return envelope_err(CliError::usage(
            "invalid-agent",
            format!("unknown agent: {}", body.agent),
            None,
        ));
    };
    let context = state.context.clone();
    if body.codex_account.is_some() && agent != AgentKind::Codex {
        return envelope_err(CliError::usage(
            "codex-account-agent-conflict",
            "codex_account is supported only for Codex sessions",
            None,
        ));
    }
    if let Some(provider_resume_id) = body.provider_resume_id {
        if body.codex_account.is_some() {
            return envelope_err(CliError::usage(
                "codex-account-provider-resume-conflict",
                "create the resumable Codex session first, then bind its account",
                None,
            ));
        }
        if body.cwd.is_some() {
            return envelope_err(CliError::usage(
                "provider-resume-cwd-conflict",
                "provider_resume_id mode resolves cwd from provider history; omit cwd",
                None,
            ));
        }
        if body.prompt.is_some() {
            return envelope_err(CliError::usage(
                "provider-resume-prompt-conflict",
                "provider_resume_id mode imports an existing provider session; omit prompt",
                None,
            ));
        }
        let args = ProviderResumeImportArgs {
            agent,
            provider_resume_id,
            title: body.title,
            id: body.id,
            tmux_bin: Some(state.tmux_bin.clone()),
            agent_bin: None,
            agent_args: body.agent_args,
            format: nils_common::cli_contract::OutputFormat::Json,
        };
        return match tokio::task::spawn_blocking(move || {
            start_provider_resume_session(&context, args)
        })
        .await
        {
            Ok(Ok(view)) => {
                envelope_ok(json!({ "machine": state.machine, "session": view.result }))
            }
            Ok(Err(err)) => envelope_err(err),
            Err(_) => join_err(),
        };
    }
    let selected_account = body.codex_account;
    let prompt = body.prompt;
    let deferred_prompt = if selected_account.is_some() {
        prompt.clone()
    } else {
        None
    };
    let args = cli::StartArgs {
        app_server_managed: true,
        initial_codex_account: selected_account.clone(),
        agent,
        cwd: body.cwd.map(PathBuf::from),
        title: body.title,
        id: body.id,
        prompt: if selected_account.is_some() {
            None
        } else {
            prompt
        },
        prompt_file: None,
        prompt_stdin: false,
        tmux_bin: Some(state.tmux_bin.clone()),
        agent_bin: None,
        agent_args: body.agent_args,
        paste_delay_ms: cli::DEFAULT_PASTE_DELAY_MS,
        format: nils_common::cli_contract::OutputFormat::Json,
    };
    match tokio::task::spawn_blocking(move || start_session(&context, args)).await {
        Ok(Ok(mut view)) => {
            if selected_account.is_some() {
                let Some(launch_id) = view
                    .result
                    .session_incarnation
                    .as_deref()
                    .map(str::to_string)
                else {
                    return status_json(
                        StatusCode::CONFLICT,
                        "codex-account-unsupported",
                        "created Codex session does not support account binding",
                    );
                };
                let requested_account = selected_account.as_deref().unwrap_or_default();
                let binding = match wait_for_account_binding(
                    &state,
                    &view.result.id,
                    &launch_id,
                    requested_account,
                    1,
                )
                .await
                {
                    Ok(binding) => binding,
                    Err(response) => return response,
                };
                view.result.codex_account = binding;
                if let Some(prompt) = deferred_prompt {
                    let Some(handle) =
                        wait_for_codex_control(&state, &view.result.id, &launch_id).await
                    else {
                        return status_json(
                            StatusCode::CONFLICT,
                            "structured-prompt-unavailable",
                            "structured prompt control is not ready for this session",
                        );
                    };
                    if let Err(response) = submit_structured_prompt_locked(
                        &state,
                        &view.result.id,
                        &launch_id,
                        Some((requested_account, 1)),
                        &handle,
                        &prompt,
                    )
                    .await
                    {
                        return response;
                    }
                }
            }
            envelope_ok(json!({ "machine": state.machine, "session": view.result }))
        }
        Ok(Err(err)) => envelope_err(err),
        Err(_) => join_err(),
    }
}

async fn wait_for_codex_control(
    state: &ServeState,
    id: &str,
    launch_id: &str,
) -> Option<ControlHandle> {
    let deadline = Instant::now() + STRUCTURED_PROMPT_CONTROL_WAIT;
    loop {
        let handle = state.codex_controls.lock().ok().and_then(|controls| {
            controls
                .get(id)
                .filter(|entry| entry.launch_id == launch_id)
                .map(|entry| entry.handle.clone())
        });
        if handle.is_some() || Instant::now() >= deadline {
            return handle;
        }
        tokio::time::sleep(STRUCTURED_PROMPT_CONTROL_POLL).await;
    }
}

async fn wait_for_account_binding(
    state: &ServeState,
    id: &str,
    launch_id: &str,
    requested_account: &str,
    requested_revision: u64,
) -> Result<crate::codex_account::CodexAccountView, Response> {
    let deadline = Instant::now() + CODEX_ACCOUNT_BINDING_WAIT;
    let mut poll_delay = STRUCTURED_PROMPT_CONTROL_POLL;
    loop {
        let context = state.context.clone();
        let load_id = id.to_string();
        let record = match tokio::task::spawn_blocking(move || {
            load_session_record(&context, &load_id)
        })
        .await
        {
            Ok(Ok(record)) => record,
            Ok(Err(err)) => return Err(envelope_err(err)),
            Err(_) => return Err(join_err()),
        };
        if record
            .runtime
            .as_ref()
            .is_none_or(|runtime| runtime.launch_id != launch_id)
        {
            return Err(status_json(
                StatusCode::CONFLICT,
                "codex-account-runtime-changed",
                "Codex session runtime changed while binding its account",
            ));
        }
        let view = crate::codex_account::view_for_record(&record);
        let requested_binding = view.selected_account.as_deref() == Some(requested_account)
            && view.revision == requested_revision;
        match view.state {
            "bound"
                if requested_binding && view.applied_runtime_id.as_deref() == Some(launch_id) =>
            {
                return Ok(view);
            }
            "bound" | "pending" | "failed" if !requested_binding => {
                return Err(status_json(
                    StatusCode::CONFLICT,
                    "codex-account-binding-superseded",
                    "the requested Codex account binding was superseded",
                ));
            }
            "bound" => {
                return Err(status_json(
                    StatusCode::CONFLICT,
                    "codex-account-binding-superseded",
                    "the requested Codex account binding was applied to another runtime",
                ));
            }
            "failed" | "unsupported" => {
                return Err(status_json(
                    StatusCode::BAD_GATEWAY,
                    "codex-account-binding-failed",
                    "Codex account binding failed",
                ));
            }
            _ if Instant::now() >= deadline => {
                return Err(status_json(
                    StatusCode::GATEWAY_TIMEOUT,
                    "codex-account-binding-timeout",
                    "Codex account binding timed out",
                ));
            }
            _ => {
                tokio::time::sleep(poll_delay).await;
                poll_delay = (poll_delay * 2).min(Duration::from_millis(500));
            }
        }
    }
}

async fn submit_structured_prompt_locked(
    state: &ServeState,
    id: &str,
    launch_id: &str,
    expected_binding: Option<(&str, u64)>,
    handle: &ControlHandle,
    text: &str,
) -> Result<(), Response> {
    let lock_context = state.context.clone();
    let lock_id = id.to_string();
    let expected_launch_id = launch_id.to_string();
    let expected_binding =
        expected_binding.map(|(account, revision)| (account.to_string(), revision));
    let record_lock = match tokio::task::spawn_blocking(move || {
        let record_lock = crate::acquire_session_record_lock(&lock_context, &lock_id)?;
        let mut current = load_session_record(&lock_context, &lock_id)?;
        if current
            .runtime
            .as_ref()
            .is_none_or(|runtime| runtime.launch_id != expected_launch_id)
        {
            return Err(CliError::data(
                "session-incarnation-conflict",
                "session runtime changed before prompt submission",
                Some(json!({ "id": current.id })),
            ));
        }
        if let Some((account, revision)) = expected_binding
            && crate::codex_account::binding_snapshot(&current)
                != (crate::codex_account::BindingSnapshot::Bound { account, revision })
        {
            return Err(CliError::data(
                "codex-account-binding-superseded",
                "the requested Codex account binding was superseded before prompt submission",
                Some(json!({ "id": current.id })),
            ));
        }
        crate::codex_account::authorize_input_locked(&lock_context, &mut current)?;
        auto_resume::cancel_for_manual_input_locked(
            &lock_context,
            &current.id,
            &jiff::Timestamp::now().to_string(),
        )?;
        crate::codex_account::ensure_input_allowed(&current)?;
        Ok::<_, CliError>(record_lock)
    })
    .await
    {
        Ok(Ok(record_lock)) => record_lock,
        Ok(Err(err))
            if matches!(
                err.code(),
                "codex-account-binding-superseded" | "session-incarnation-conflict"
            ) =>
        {
            return Err(status_json(
                StatusCode::CONFLICT,
                err.code(),
                "session account or runtime changed before prompt submission",
            ));
        }
        Ok(Err(err)) => return Err(envelope_err(err)),
        Err(_) => return Err(join_err()),
    };

    let response = handle.submit_prompt(text).await.map(|_| ()).map_err(|_| {
        status_json(
            StatusCode::INTERNAL_SERVER_ERROR,
            "structured-prompt-outcome-unknown",
            "structured prompt submission outcome is unknown",
        )
    });
    drop(record_lock);
    response
}

async fn codex_account_handler(
    State(state): State<Arc<ServeState>>,
    headers: HeaderMap,
    AxPath(id): AxPath<String>,
    Json(body): Json<CodexAccountBody>,
) -> Response {
    if let Some(response) = deny_unauthorized(&state, &headers) {
        return response;
    }
    let Some(expected_session_incarnation) = body
        .expected_session_incarnation
        .as_deref()
        .filter(|value| !value.is_empty() && value.len() <= 128)
    else {
        return status_json(
            StatusCode::BAD_REQUEST,
            "invalid-session-incarnation",
            "expected_session_incarnation is required",
        );
    };
    let _switch_guard = state.codex_account_switches.lock(&id).await;
    let context = state.context.clone();
    let load_id = id.clone();
    let record =
        match tokio::task::spawn_blocking(move || load_session_record(&context, &load_id)).await {
            Ok(Ok(record)) => record,
            Ok(Err(err)) => return envelope_err(err),
            Err(_) => return join_err(),
        };
    if !crate::codex_account::view_for_record(&record).supported {
        return status_json(
            StatusCode::CONFLICT,
            "codex-account-unsupported",
            "this session does not support Codex account switching",
        );
    }
    let Some(launch_id) = record
        .runtime
        .as_ref()
        .map(|runtime| runtime.launch_id.clone())
    else {
        return status_json(
            StatusCode::CONFLICT,
            "codex-account-unsupported",
            "this session does not support Codex account switching",
        );
    };
    if launch_id != expected_session_incarnation {
        return status_json(
            StatusCode::CONFLICT,
            "codex-account-session-incarnation-conflict",
            "session was replaced before its Codex account switch was applied",
        );
    }
    let Some(handle) = wait_for_codex_control(&state, &id, &launch_id).await else {
        return status_json(
            StatusCode::CONFLICT,
            "codex-account-control-unavailable",
            "Codex account control is not ready for this session",
        );
    };
    let begin_context = state.context.clone();
    let begin_id = id.clone();
    let begin_launch_id = launch_id.clone();
    let begin_account = body.account.clone();
    let revision = match tokio::task::spawn_blocking(move || {
        crate::codex_account::begin_switch_binding(
            &begin_context,
            &begin_id,
            &begin_launch_id,
            &begin_account,
        )
    })
    .await
    {
        Ok(Ok(revision)) => revision,
        Ok(Err(err)) => return envelope_err(err),
        Err(_) => return join_err(),
    };
    match handle.bind_account(&body.account, revision).await {
        Ok(view) => envelope_ok(json!({ "machine": state.machine, "codex_account": view })),
        Err(_) => {
            let finish_context = state.context.clone();
            let finish_id = id.clone();
            let finish_launch_id = launch_id.clone();
            let finish_account = body.account.clone();
            let _ = tokio::task::spawn_blocking(move || {
                crate::codex_account::finish_binding(
                    &finish_context,
                    &finish_id,
                    &finish_launch_id,
                    &finish_account,
                    revision,
                    Err("apply_failed"),
                )
            })
            .await;
            status_json(
                StatusCode::BAD_GATEWAY,
                "codex-account-binding-failed",
                "Codex account binding failed",
            )
        }
    }
}

async fn send_handler(
    State(state): State<Arc<ServeState>>,
    headers: HeaderMap,
    AxPath(id): AxPath<String>,
    Json(body): Json<SendBody>,
) -> Response {
    if let Some(resp) = deny_unauthorized(&state, &headers) {
        return resp;
    }
    let mut keys = Vec::with_capacity(body.keys.len());
    for name in &body.keys {
        match SpecialKey::from_name(name) {
            Some(key) => keys.push(key),
            None => {
                return envelope_err(CliError::usage(
                    "invalid-key",
                    format!("unknown key: {name}"),
                    None,
                ));
            }
        }
    }
    let context = state.context.clone();
    let args = cli::SendArgs {
        id,
        text: body.text,
        text_stdin: false,
        keys,
        tmux_bin: Some(state.tmux_bin.clone()),
        format: nils_common::cli_contract::OutputFormat::Json,
    };
    match tokio::task::spawn_blocking(move || crate::send_to_session(&context, args)).await {
        Ok(Ok(sent)) => envelope_ok(json!({ "machine": state.machine, "sent": sent })),
        Ok(Err(err)) => envelope_err(err),
        Err(_) => join_err(),
    }
}

async fn structured_prompt_handler(
    State(state): State<Arc<ServeState>>,
    headers: HeaderMap,
    AxPath(id): AxPath<String>,
    Json(body): Json<StructuredPromptBody>,
) -> Response {
    if let Some(resp) = deny_unauthorized(&state, &headers) {
        return resp;
    }
    if body.text.trim().is_empty() {
        return status_json(
            StatusCode::BAD_REQUEST,
            "empty-prompt",
            "structured prompt text must not be blank",
        );
    }
    if body.text.len() > MAX_PROVIDER_PROMPT_BYTES {
        return status_json(
            StatusCode::PAYLOAD_TOO_LARGE,
            "prompt-too-large",
            "structured prompt exceeds the maximum supported size",
        );
    }
    if structured_prompt_has_unsafe_control(&body.text) {
        return status_json(
            StatusCode::BAD_REQUEST,
            "unsafe-prompt-control",
            "structured prompt contains an unsupported control character",
        );
    }

    let context = state.context.clone();
    let load_id = id.clone();
    let record =
        match tokio::task::spawn_blocking(move || load_session_record(&context, &load_id)).await {
            Ok(Ok(record)) => record,
            Ok(Err(err)) => return envelope_err(err),
            Err(_) => return join_err(),
        };
    if !codex_app_server::runtime_is_supported(&record) {
        return status_json(
            StatusCode::CONFLICT,
            "structured-prompt-unsupported",
            "this session does not provide structured prompt submission",
        );
    }
    let Some(launch_id) = record
        .runtime
        .as_ref()
        .map(|runtime| runtime.launch_id.clone())
    else {
        return status_json(
            StatusCode::CONFLICT,
            "structured-prompt-unsupported",
            "this session does not provide structured prompt submission",
        );
    };

    let deadline = Instant::now() + STRUCTURED_PROMPT_CONTROL_WAIT;
    let handle = loop {
        let handle = state.codex_controls.lock().ok().and_then(|controls| {
            controls
                .get(&id)
                .filter(|entry| entry.launch_id == launch_id)
                .map(|entry| entry.handle.clone())
        });
        if handle.is_some() || Instant::now() >= deadline {
            break handle;
        }
        tokio::time::sleep(STRUCTURED_PROMPT_CONTROL_POLL).await;
    };
    let Some(handle) = handle else {
        return status_json(
            StatusCode::CONFLICT,
            "structured-prompt-unavailable",
            "structured prompt control is not ready for this session",
        );
    };

    match submit_structured_prompt_locked(&state, &record.id, &launch_id, None, &handle, &body.text)
        .await
    {
        Ok(()) => envelope_ok(json!({ "machine": state.machine, "submitted": true })),
        Err(response) => response,
    }
}

fn structured_prompt_has_unsafe_control(text: &str) -> bool {
    text.chars().any(|character| {
        character <= '\u{0009}'
            || ('\u{000b}'..='\u{001f}').contains(&character)
            || character == '\u{007f}'
    })
}

async fn auto_resume_status_handler(
    State(state): State<Arc<ServeState>>,
    AxPath(id): AxPath<String>,
) -> Response {
    let context = state.context.clone();
    match tokio::task::spawn_blocking(move || {
        let record = load_session_record(&context, &id)?;
        Ok::<_, CliError>(auto_resume::view_for_record(&context, &record))
    })
    .await
    {
        Ok(Ok(view)) => envelope_ok(json!({ "machine": state.machine, "auto_resume": view })),
        Ok(Err(err)) => envelope_err(err),
        Err(_) => join_err(),
    }
}

async fn auto_resume_set_handler(
    State(state): State<Arc<ServeState>>,
    headers: HeaderMap,
    AxPath(id): AxPath<String>,
    body: Result<Json<AutoResumeBody>, JsonRejection>,
) -> Response {
    if let Some(response) = deny_unauthorized(&state, &headers) {
        return response;
    }
    let Json(body) = match body {
        Ok(body) => body,
        Err(_) => {
            return envelope_err(CliError::usage(
                "invalid-json-body",
                "request body must be JSON with a boolean enabled field",
                None,
            ));
        }
    };
    let context = state.context.clone();
    let now = jiff::Timestamp::now().to_string();
    match tokio::task::spawn_blocking(move || {
        auto_resume::set_enabled(&context, &id, body.enabled, &now)
    })
    .await
    {
        Ok(Ok(view)) => envelope_ok(json!({ "machine": state.machine, "auto_resume": view })),
        Ok(Err(err)) => envelope_err(err),
        Err(_) => join_err(),
    }
}

async fn auto_resume_cancel_handler(
    State(state): State<Arc<ServeState>>,
    headers: HeaderMap,
    AxPath(id): AxPath<String>,
) -> Response {
    if let Some(response) = deny_unauthorized(&state, &headers) {
        return response;
    }
    let context = state.context.clone();
    let now = jiff::Timestamp::now().to_string();
    match tokio::task::spawn_blocking(move || auto_resume::cancel(&context, &id, &now)).await {
        Ok(Ok(view)) => envelope_ok(json!({ "machine": state.machine, "auto_resume": view })),
        Ok(Err(err)) => envelope_err(err),
        Err(_) => join_err(),
    }
}

async fn auto_resume_loop(state: Arc<ServeState>) {
    const POLL_INTERVAL: Duration = Duration::from_secs(15);
    loop {
        let now_epoch = jiff::Timestamp::now().as_second();
        let context = state.context.clone();
        let pending = match tokio::task::spawn_blocking(move || {
            auto_resume::pending_sessions(&context, now_epoch)
        })
        .await
        {
            Ok(Ok(pending)) => pending,
            Ok(Err(err)) => {
                eprintln!("warning: auto-resume discovery failed: {}", err.code());
                tokio::time::sleep(POLL_INTERVAL).await;
                continue;
            }
            Err(_) => {
                eprintln!("warning: auto-resume discovery worker failed");
                tokio::time::sleep(POLL_INTERVAL).await;
                continue;
            }
        };

        for code in &pending.error_codes {
            eprintln!("warning: auto-resume session state skipped: {code}");
        }

        if !pending.recovery_ids.is_empty() {
            process_auto_resume_ids(
                state.clone(),
                pending.recovery_ids,
                UsageSnapshot {
                    authoritative: false,
                    has_exhausted_windows: false,
                    exhausted_reset_epochs: Vec::new(),
                },
            )
            .await;
        }
        if !pending.usage_ids.is_empty() {
            let (codex_ids, claude_ids) =
                partition_auto_resume_ids(&state, pending.usage_ids).await;
            if !codex_ids.is_empty() {
                process_codex_auto_resume_ids(state.clone(), codex_ids).await;
            }
            if claude_ids.is_empty() {
                tokio::time::sleep(POLL_INTERVAL).await;
                continue;
            }
            let timeout = usage_timeout();
            let provider = tokio::task::spawn_blocking(move || collect_claude_usage(timeout))
                .await
                .unwrap_or_else(|_| provider_internal_error("claude", "Claude"));
            let usage = UsageSnapshot {
                authoritative: provider.fresh_authoritative,
                has_exhausted_windows: provider
                    .windows
                    .iter()
                    .any(|window| window.used_percent >= 100),
                exhausted_reset_epochs: provider
                    .windows
                    .iter()
                    .filter(|window| window.used_percent >= 100)
                    .filter_map(|window| window.reset_at_epoch)
                    .collect(),
            };
            process_auto_resume_ids(state.clone(), claude_ids, usage).await;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

#[derive(Clone)]
struct CodexAutoResumeTarget {
    id: String,
    launch_id: String,
    binding: crate::codex_account::BindingSnapshot,
}

async fn partition_auto_resume_ids(
    state: &ServeState,
    ids: Vec<String>,
) -> (Vec<CodexAutoResumeTarget>, Vec<String>) {
    let context = state.context.clone();
    tokio::task::spawn_blocking(move || {
        let mut codex = Vec::new();
        let mut other = Vec::new();
        for id in ids {
            let target = load_session_record(&context, &id)
                .ok()
                .filter(codex_app_server::runtime_is_supported)
                .and_then(|record| {
                    let binding = crate::codex_account::binding_snapshot(&record);
                    record.runtime.map(|runtime| CodexAutoResumeTarget {
                        id: record.id,
                        launch_id: runtime.launch_id,
                        binding,
                    })
                });
            if let Some(target) = target {
                codex.push(target);
            } else {
                other.push(id);
            }
        }
        (codex, other)
    })
    .await
    .unwrap_or_default()
}

async fn process_codex_auto_resume_ids(
    state: Arc<ServeState>,
    targets: Vec<CodexAutoResumeTarget>,
) {
    let mut tasks = JoinSet::new();
    for target in targets {
        let state = state.clone();
        tasks.spawn(async move { process_codex_auto_resume_id(state, target).await });
        if tasks.len() >= MAX_CONCURRENT_AUTO_RESUME_TICKS {
            report_auto_resume_join(tasks.join_next().await);
        }
    }
    while !tasks.is_empty() {
        report_auto_resume_join(tasks.join_next().await);
    }
}

async fn process_codex_auto_resume_id(state: Arc<ServeState>, target: CodexAutoResumeTarget) {
    let control = state
        .codex_controls
        .lock()
        .ok()
        .and_then(|controls| controls.get(&target.id).cloned());
    let Some(control) = control else {
        record_codex_scheduler_error_for_runtime(
            state.context.clone(),
            target.id,
            target.launch_id,
            "usage_unavailable",
        )
        .await;
        return;
    };
    if control.launch_id != target.launch_id {
        record_codex_scheduler_error_for_runtime(
            state.context.clone(),
            target.id,
            target.launch_id,
            "usage_unavailable",
        )
        .await;
        return;
    }
    let usage = match control.handle.usage().await {
        Ok(usage) => usage,
        Err(_) => {
            record_codex_scheduler_error_for_runtime(
                state.context.clone(),
                target.id,
                target.launch_id,
                "usage_unavailable",
            )
            .await;
            return;
        }
    };
    let context = state.context.clone();
    let runtime = tokio::runtime::Handle::current();
    let handle = control.handle.clone();
    let expected_launch_id = target.launch_id;
    let id = target.id;
    tokio::task::spawn_blocking(move || {
        let now_epoch = jiff::Timestamp::now().as_second();
        if let Err(err) = auto_resume::tick_for_runtime_and_binding(
            &context,
            &id,
            &expected_launch_id,
            &target.binding,
            now_epoch,
            &usage,
            |_| {
                runtime
                    .block_on(handle.submit(auto_resume::CONTINUATION_MESSAGE))
                    .map(|_| ())
                    .map_err(|_| {
                        CliError::runtime(
                            "codex-app-server-submit-unknown",
                            "Codex continuation submission outcome is unknown",
                            Some(json!({ "id": id })),
                        )
                    })
            },
        ) {
            eprintln!("warning: Codex auto-resume tick failed: {}", err.code());
        }
    })
    .await
    .ok();
}

async fn record_codex_scheduler_error_for_runtime(
    context: CliContext,
    id: String,
    expected_launch_id: String,
    reason: &'static str,
) {
    tokio::task::spawn_blocking(move || {
        let now_epoch = jiff::Timestamp::now().as_second();
        if let Err(err) = auto_resume::record_scheduler_error_for_runtime(
            &context,
            &id,
            &expected_launch_id,
            now_epoch,
            reason,
        ) {
            eprintln!(
                "warning: Codex auto-resume failure checkpoint failed: {}",
                err.code()
            );
        }
    })
    .await
    .ok();
}

async fn codex_control_loop(state: Arc<ServeState>) {
    const RECONCILE_INTERVAL: Duration = Duration::from_secs(2);
    loop {
        let context = state.context.clone();
        let tmux = state.tmux_bin.clone();
        let records = tokio::task::spawn_blocking(move || discover_codex_controls(&context, &tmux))
            .await
            .unwrap_or_default();
        let live: std::collections::HashSet<(String, String)> = records
            .iter()
            .filter_map(|record| {
                record
                    .runtime
                    .as_ref()
                    .map(|runtime| (record.id.clone(), runtime.launch_id.clone()))
            })
            .collect();
        if let Ok(mut controls) = state.codex_controls.lock() {
            controls.retain(|id, entry| live.contains(&(id.clone(), entry.launch_id.clone())));
        }
        for record in records {
            let Some(launch_id) = record
                .runtime
                .as_ref()
                .map(|runtime| runtime.launch_id.clone())
            else {
                continue;
            };
            let already_running = state.codex_controls.lock().ok().is_some_and(|controls| {
                controls
                    .get(&record.id)
                    .is_some_and(|entry| entry.launch_id == launch_id)
            });
            if already_running {
                continue;
            }
            let prepare_context = state.context.clone();
            let prepare_id = record.id.clone();
            let prepare_launch_id = launch_id.clone();
            let record = match tokio::task::spawn_blocking(move || {
                crate::codex_account::prepare_control_reconnect(
                    &prepare_context,
                    &prepare_id,
                    &prepare_launch_id,
                )
            })
            .await
            {
                Ok(Ok(record)) => record,
                Ok(Err(err)) => {
                    eprintln!(
                        "warning: Codex account reconnect fence failed: {}",
                        err.code()
                    );
                    continue;
                }
                Err(_) => continue,
            };
            let (handle, commands) = codex_app_server::control_channel();
            if let Ok(mut controls) = state.codex_controls.lock() {
                controls.insert(
                    record.id.clone(),
                    CodexControlEntry {
                        launch_id: launch_id.clone(),
                        handle,
                    },
                );
            }
            let registry = state.codex_controls.clone();
            let context = state.context.clone();
            tokio::spawn(async move {
                if let Err(err) =
                    codex_app_server::run_control(context, record.clone(), commands).await
                {
                    eprintln!("warning: Codex app-server control ended: {err}");
                }
                if let Ok(mut controls) = registry.lock()
                    && controls
                        .get(&record.id)
                        .is_some_and(|entry| entry.launch_id == launch_id)
                {
                    controls.remove(&record.id);
                }
            });
        }
        tokio::time::sleep(RECONCILE_INTERVAL).await;
    }
}

fn fence_codex_controls_before_listen(context: &CliContext, tmux: &Path) -> Result<(), CliError> {
    let root = context.state_dir.join("sessions");
    let candidates: Vec<SessionRecord> = match std::fs::read_dir(root) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter_map(|id| load_session_record(context, &id).ok())
            .filter(|record| {
                codex_app_server::runtime_is_supported(record)
                    && crate::codex_account::binding_is_present(record)
            })
            .collect(),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(_) => {
            return Err(CliError::runtime(
                "codex-account-reconnect-fence-unavailable",
                "failed to inspect session records before serving",
                None,
            ));
        }
    };
    if candidates.is_empty() {
        return Ok(());
    }
    let snapshots = crate::tmux_session_snapshots(tmux).ok_or_else(|| {
        CliError::runtime(
            "codex-account-reconnect-fence-unavailable",
            "failed to inspect live sessions before fencing Codex account controls",
            None,
        )
    })?;
    for record in candidates
        .into_iter()
        .filter(|record| snapshots.contains_key(&record.tmux_session))
    {
        let Some(launch_id) = record
            .runtime
            .as_ref()
            .map(|runtime| runtime.launch_id.as_str())
        else {
            continue;
        };
        crate::codex_account::prepare_control_reconnect(context, &record.id, launch_id)?;
    }
    Ok(())
}

fn discover_codex_controls(context: &CliContext, tmux: &Path) -> Vec<SessionRecord> {
    let Some(tmux_snapshots) = crate::tmux_session_snapshots(tmux) else {
        return Vec::new();
    };
    discover_codex_controls_from_snapshots(context, &tmux_snapshots)
}

fn discover_codex_controls_from_snapshots(
    context: &CliContext,
    tmux_snapshots: &std::collections::BTreeMap<String, crate::TmuxSessionSnapshot>,
) -> Vec<SessionRecord> {
    let root = context.state_dir.join("sessions");
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter_map(|id| load_session_record(context, &id).ok())
        .filter(|record| {
            codex_app_server::runtime_is_supported(record)
                && tmux_snapshots.contains_key(&record.tmux_session)
        })
        .collect()
}

async fn process_auto_resume_ids(state: Arc<ServeState>, ids: Vec<String>, usage: UsageSnapshot) {
    let mut tasks = JoinSet::new();
    for id in ids {
        let context = state.context.clone();
        let tmux = state.tmux_bin.clone();
        let usage = usage.clone();
        tasks.spawn_blocking(move || {
            let now_epoch = jiff::Timestamp::now().as_second();
            if let Err(err) = auto_resume::tick(&context, &id, now_epoch, &usage, |record| {
                send_auto_resume_input(&context, record, auto_resume::CONTINUATION_MESSAGE, &tmux)
            }) {
                eprintln!("warning: auto-resume tick failed: {}", err.code());
                if let Err(record_err) =
                    auto_resume::record_scheduler_error(&context, &id, now_epoch, "scheduler_error")
                {
                    eprintln!(
                        "warning: auto-resume failure checkpoint failed: {}",
                        record_err.code()
                    );
                }
            }
        });
        if tasks.len() >= MAX_CONCURRENT_AUTO_RESUME_TICKS {
            report_auto_resume_join(tasks.join_next().await);
        }
    }
    while !tasks.is_empty() {
        report_auto_resume_join(tasks.join_next().await);
    }
}

fn report_auto_resume_join(result: Option<Result<(), tokio::task::JoinError>>) {
    if result.is_some_and(|result| result.is_err()) {
        eprintln!("warning: auto-resume worker failed");
    }
}

async fn resume_handler(
    State(state): State<Arc<ServeState>>,
    headers: HeaderMap,
    AxPath(id): AxPath<String>,
) -> Response {
    if let Some(resp) = deny_unauthorized(&state, &headers) {
        return resp;
    }
    let context = state.context.clone();
    let tmux = state.tmux_bin.clone();
    match tokio::task::spawn_blocking(move || resume_session_by_id(&context, &id, &tmux)).await {
        Ok(Ok(session)) => envelope_ok(json!({ "machine": state.machine, "session": session })),
        Ok(Err(err)) => envelope_err(err),
        Err(_) => join_err(),
    }
}

async fn update_session_handler(
    State(state): State<Arc<ServeState>>,
    headers: HeaderMap,
    AxPath(id): AxPath<String>,
    Json(body): Json<Value>,
) -> Response {
    if let Some(resp) = deny_unauthorized(&state, &headers) {
        return resp;
    }
    let Some(object) = body.as_object() else {
        return envelope_err(CliError::usage(
            "invalid-session-update",
            "session update body must be a JSON object",
            None,
        ));
    };
    let title = match object.get("title") {
        Some(Value::String(title)) => Some(title.clone()),
        Some(Value::Null) => None,
        Some(_) => {
            return envelope_err(CliError::usage(
                "invalid-title",
                "session title must be a string or null",
                Some(json!({ "field": "title" })),
            ));
        }
        None => {
            return envelope_err(CliError::usage(
                "missing-title",
                "session update requires a title field",
                Some(json!({ "field": "title" })),
            ));
        }
    };
    let expected_title_revision = match object.get("expected_title_revision") {
        Some(Value::Number(revision)) => match revision.as_u64() {
            Some(revision) => Some(revision),
            None => {
                return envelope_err(CliError::usage(
                    "invalid-title-revision",
                    "expected_title_revision must be an unsigned integer",
                    Some(json!({ "field": "expected_title_revision" })),
                ));
            }
        },
        Some(_) => {
            return envelope_err(CliError::usage(
                "invalid-title-revision",
                "expected_title_revision must be an unsigned integer",
                Some(json!({ "field": "expected_title_revision" })),
            ));
        }
        None => None,
    };
    let expected_session_created_at = match object.get("expected_session_created_at") {
        Some(Value::String(created_at))
            if !created_at.trim().is_empty() && created_at.len() <= 128 =>
        {
            Some(created_at.clone())
        }
        Some(_) => {
            return envelope_err(CliError::usage(
                "invalid-session-incarnation",
                "expected_session_created_at must be a non-empty string",
                Some(json!({ "field": "expected_session_created_at" })),
            ));
        }
        None => None,
    };
    let expected_session_incarnation = match object.get("expected_session_incarnation") {
        Some(Value::String(incarnation))
            if !incarnation.trim().is_empty() && incarnation.len() <= 128 =>
        {
            Some(incarnation.clone())
        }
        Some(_) => {
            return envelope_err(CliError::usage(
                "invalid-session-incarnation",
                "expected_session_incarnation must be a non-empty string",
                Some(json!({ "field": "expected_session_incarnation" })),
            ));
        }
        None => None,
    };
    let expected_session_title = match object.get("expected_session_title") {
        // This is observed state, not a proposed title. Older clients could persist
        // raw prompt titles beyond the current normalized-title limit, and upgraded
        // writers must still be able to replace those values safely.
        Some(Value::String(title)) => Some(Some(title.clone())),
        Some(Value::Null) => Some(None),
        Some(_) => {
            return envelope_err(CliError::usage(
                "invalid-expected-session-title",
                "expected_session_title must be a string or null",
                Some(json!({ "field": "expected_session_title" })),
            ));
        }
        None => None,
    };
    let context = state.context.clone();
    let tmux = state.tmux_bin.clone();
    match tokio::task::spawn_blocking(move || {
        update_session_title_if_revision(
            &context,
            &id,
            title,
            crate::TitleUpdatePreconditions {
                title_revision: expected_title_revision,
                session_created_at: expected_session_created_at,
                session_incarnation: expected_session_incarnation,
                session_title: expected_session_title,
            },
            &tmux,
        )
    })
    .await
    {
        Ok(Ok(session)) => envelope_ok(json!({ "machine": state.machine, "session": session })),
        Ok(Err(err)) => envelope_err(err),
        Err(_) => join_err(),
    }
}

async fn upload_attachment_handler(
    State(state): State<Arc<ServeState>>,
    headers: HeaderMap,
    AxPath(id): AxPath<String>,
    Query(query): Query<AttachmentQuery>,
    body: Body,
) -> Response {
    if let Some(resp) = deny_unauthorized(&state, &headers) {
        return resp;
    }
    let content_type = headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let bytes = match to_bytes(body, MAX_ATTACHMENT_BYTES + 1).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return status_json(
                StatusCode::PAYLOAD_TOO_LARGE,
                "attachment-too-large",
                "attachment exceeds the maximum allowed size",
            );
        }
    };
    if bytes.len() > MAX_ATTACHMENT_BYTES {
        return status_json(
            StatusCode::PAYLOAD_TOO_LARGE,
            "attachment-too-large",
            "attachment exceeds the maximum allowed size",
        );
    }
    let context = state.context.clone();
    let filename = query.filename.clone();
    let bytes = bytes.to_vec();
    match tokio::task::spawn_blocking(move || {
        write_session_attachment(&context, &id, filename.as_deref(), content_type, &bytes)
    })
    .await
    {
        Ok(Ok(attachment)) => {
            envelope_ok(json!({ "machine": state.machine, "attachment": attachment }))
        }
        Ok(Err(err)) => envelope_err(err),
        Err(_) => join_err(),
    }
}

async fn workdirs_handler(
    State(state): State<Arc<ServeState>>,
    headers: HeaderMap,
    Query(query): Query<WorkdirQuery>,
) -> Response {
    if let Some(resp) = deny_unauthorized(&state, &headers) {
        return resp;
    }
    let q = query.q.clone();
    let limit = query.limit;
    let options = WorkdirSearchOptions {
        git_only: query.git_only,
        exclude_worktrees: query.exclude_worktrees,
    };
    let context = state.context.clone();
    match tokio::task::spawn_blocking(move || {
        search_workdirs(&context, q.as_deref(), limit, options)
    })
    .await
    {
        Ok(Ok(workdirs)) => envelope_ok(json!({ "machine": state.machine, "workdirs": workdirs })),
        Ok(Err(err)) => envelope_err(err),
        Err(_) => join_err(),
    }
}

async fn repo_remote_url_handler(
    State(state): State<Arc<ServeState>>,
    headers: HeaderMap,
    Query(query): Query<RepoRemoteUrlQuery>,
) -> Response {
    if let Some(resp) = deny_unauthorized(&state, &headers) {
        return resp;
    }
    let Some(cwd) = query
        .cwd
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return envelope_err(CliError::usage(
            "missing-cwd",
            "missing cwd query parameter",
            None,
        ));
    };
    let cwd = cwd.to_string();
    match tokio::task::spawn_blocking(move || repo_remote_url_from_cwd(&cwd)).await {
        Ok(url) => envelope_ok(json!({ "machine": state.machine, "url": url })),
        Err(_) => join_err(),
    }
}

async fn delete_handler(
    State(state): State<Arc<ServeState>>,
    headers: HeaderMap,
    AxPath(id): AxPath<String>,
) -> Response {
    if let Some(resp) = deny_unauthorized(&state, &headers) {
        return resp;
    }
    let context = state.context.clone();
    let tmux = state.tmux_bin.clone();
    let delete_id = id.clone();
    match tokio::task::spawn_blocking(move || delete_session(&context, &delete_id, tmux)).await {
        Ok(Ok(result)) => {
            if let Ok(mut controls) = state.codex_controls.lock() {
                controls.remove(&id);
            }
            state.attach_brokers.shutdown_session(&id).await;
            state.provider_prompt_discovery.evict_session(&id).await;
            envelope_ok(json!({ "machine": state.machine, "deleted": result }))
        }
        Ok(Err(err)) => envelope_err(err),
        Err(_) => join_err(),
    }
}

async fn attach_handler(
    State(state): State<Arc<ServeState>>,
    headers: HeaderMap,
    AxPath(id): AxPath<String>,
    ws: WebSocketUpgrade,
) -> Response {
    if let Some(resp) = deny_unauthorized(&state, &headers) {
        return resp;
    }
    let context = state.context.clone();
    let tmux = state.tmux_bin.clone();
    let lookup = tokio::task::spawn_blocking({
        let context = context.clone();
        let id = id.clone();
        let tmux = tmux.clone();
        move || {
            let record = load_session_record(&context, &id)?;
            let status = session_status(&tmux, &record);
            Ok::<_, CliError>((record, status))
        }
    })
    .await;
    let (record, status) = match lookup {
        Ok(Ok(result)) => result,
        Ok(Err(err)) => return envelope_err(err),
        Err(_) => return join_err(),
    };
    if status != "running" {
        return envelope_err(attach_unavailable_error(&record, &status));
    }
    ws.on_upgrade(move |socket| attach_socket(socket, state, record))
}

fn attach_unavailable_error(record: &crate::SessionRecord, status: &str) -> CliError {
    if status == "unknown" {
        return CliError::runtime(
            "session-status-unknown",
            format!("session status could not be checked: {}", record.id),
            Some(json!({ "id": record.id.clone() })),
        );
    }
    CliError::data(
        "session-not-running",
        format!("session is not running: {}", record.id),
        Some(json!({ "id": record.id.clone(), "status": status })),
    )
}

// --- websocket attach ---------------------------------------------------------

#[derive(Clone, Debug)]
enum AttachEvent {
    Output(Bytes),
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AttachWriterExit {
    Drained,
    SendFailed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AttachPumpExit {
    BrokerClosed,
    Lagged,
    WriterClosed,
}

struct AttachHandoff {
    snapshot: Option<String>,
    live: VecDeque<Bytes>,
    broker_closed: bool,
}

struct AbortOnDropTask<T> {
    handle: Option<tokio::task::JoinHandle<T>>,
}

impl<T> AbortOnDropTask<T> {
    fn new(handle: tokio::task::JoinHandle<T>) -> Self {
        Self {
            handle: Some(handle),
        }
    }

    fn handle_mut(&mut self) -> &mut tokio::task::JoinHandle<T> {
        self.handle.as_mut().expect("task handle is present")
    }

    fn abort(&mut self) {
        if let Some(handle) = &self.handle {
            handle.abort();
        }
    }

    async fn abort_and_wait(mut self) {
        let Some(handle) = self.handle.take() else {
            return;
        };
        handle.abort();
        let _ = handle.await;
    }
}

impl<T> Drop for AbortOnDropTask<T> {
    fn drop(&mut self) {
        self.abort();
    }
}

struct AttachSubscription {
    receiver: Option<tokio::sync::broadcast::Receiver<AttachEvent>>,
    lease: Option<AttachLease>,
    resize_lock: Arc<tokio::sync::Mutex<()>>,
}

struct AttachLease {
    slot: Arc<tokio::sync::Mutex<AttachBrokerSlot>>,
    generation: u64,
    released: bool,
}

#[derive(Default)]
struct AttachBrokerRegistry {
    entries: tokio::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<AttachBrokerSlot>>>>,
}

struct ProviderPromptDiscoveryRegistry {
    entries: tokio::sync::Mutex<
        HashMap<ProviderPromptDiscoveryKey, Arc<tokio::sync::Mutex<ProviderPromptDiscoverySlot>>>,
    >,
    resolver: Arc<ProviderPromptSourceResolver>,
    scan_permits: Arc<tokio::sync::Semaphore>,
}

type ProviderPromptSourceResolver =
    dyn Fn(&crate::SessionRecord) -> Option<ProviderPromptSource> + Send + Sync;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ProviderPromptDiscoveryKey {
    session_id: String,
    provider: String,
    provider_session_id: String,
    generation: u64,
    launch_id: String,
    tmux_session: String,
}

struct ProviderPromptDiscoverySlot {
    source: Option<ProviderPromptSource>,
    in_flight: bool,
    progress: tokio::sync::watch::Sender<u64>,
    next_scan_at: Option<Instant>,
    backoff: Duration,
    scan_attempts: usize,
}

struct AttachBrokerSlot {
    accepting: bool,
    next_generation: u64,
    active: Option<AttachBrokerGeneration>,
}

struct AttachBrokerGeneration {
    generation: u64,
    subscribers: usize,
    broker: AttachBroker,
}

struct AttachBroker {
    target: String,
    fifo_path: PathBuf,
    tmux: PathBuf,
    sender: tokio::sync::broadcast::Sender<AttachEvent>,
    reader_task: tokio::task::JoinHandle<()>,
    closed: Arc<AtomicBool>,
    resize_lock: Arc<tokio::sync::Mutex<()>>,
}

struct AttachFifoCleanup {
    path: PathBuf,
    armed: bool,
}

impl AttachFifoCleanup {
    fn new(path: PathBuf) -> Self {
        Self { path, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for AttachFifoCleanup {
    fn drop(&mut self) {
        if self.armed {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

impl Default for AttachBrokerSlot {
    fn default() -> Self {
        Self {
            accepting: true,
            next_generation: 1,
            active: None,
        }
    }
}

impl Default for ProviderPromptDiscoverySlot {
    fn default() -> Self {
        let (progress, _) = tokio::sync::watch::channel(0);
        Self {
            source: None,
            in_flight: false,
            progress,
            next_scan_at: None,
            backoff: PROVIDER_PROMPT_PENDING_POLL_INTERVAL,
            scan_attempts: 0,
        }
    }
}

impl Default for ProviderPromptDiscoveryRegistry {
    fn default() -> Self {
        Self {
            entries: tokio::sync::Mutex::new(HashMap::new()),
            resolver: Arc::new(ProviderPromptTail::resolve_source),
            scan_permits: Arc::new(tokio::sync::Semaphore::new(
                PROVIDER_PROMPT_DISCOVERY_MAX_CONCURRENT_SCANS,
            )),
        }
    }
}

impl ProviderPromptDiscoveryKey {
    fn from_record(record: &crate::SessionRecord) -> Option<Self> {
        let runtime = record.runtime.as_ref()?;
        let resume = record.provider_resume.as_ref()?;
        Some(Self {
            session_id: record.id.clone(),
            provider: resume.provider.clone(),
            provider_session_id: resume.session_id.clone(),
            generation: runtime.generation,
            launch_id: runtime.launch_id.clone(),
            tmux_session: runtime.tmux_session.clone(),
        })
    }
}

impl ProviderPromptDiscoveryRegistry {
    #[cfg(test)]
    fn with_resolver<F>(resolver: F) -> Self
    where
        F: Fn(&crate::SessionRecord) -> Option<ProviderPromptSource> + Send + Sync + 'static,
    {
        Self {
            entries: tokio::sync::Mutex::new(HashMap::new()),
            resolver: Arc::new(resolver),
            scan_permits: Arc::new(tokio::sync::Semaphore::new(
                PROVIDER_PROMPT_DISCOVERY_MAX_CONCURRENT_SCANS,
            )),
        }
    }

    async fn resolve_source(&self, record: &crate::SessionRecord) -> Option<ProviderPromptSource> {
        let key = ProviderPromptDiscoveryKey::from_record(record)?;
        let slot = {
            let mut entries = self.entries.lock().await;
            entries.retain(|existing, _| existing.session_id != key.session_id || existing == &key);
            if !entries.contains_key(&key) && entries.len() >= PROVIDER_PROMPT_DISCOVERY_MAX_ENTRIES
            {
                let evicted = entries.iter().find_map(|(existing, slot)| {
                    let state = slot.try_lock().ok()?;
                    (!state.in_flight && existing != &key).then(|| existing.clone())
                });
                let evicted = evicted?;
                entries.remove(&evicted);
            }
            entries
                .entry(key.clone())
                .or_insert_with(|| {
                    Arc::new(tokio::sync::Mutex::new(
                        ProviderPromptDiscoverySlot::default(),
                    ))
                })
                .clone()
        };
        loop {
            let mut state = slot.lock().await;
            if let Some(source) = state.source.clone() {
                return Some(source);
            }
            let now = Instant::now();
            if state.next_scan_at.is_some_and(|next| now < next) {
                return None;
            }
            let mut progress = state.progress.subscribe();
            if state.in_flight {
                drop(state);
                let _ = progress.changed().await;
                continue;
            }
            state.in_flight = true;
            state.scan_attempts = state.scan_attempts.saturating_add(1);
            drop(state);

            let resolver = self.resolver.clone();
            let scan_permits = self.scan_permits.clone();
            let candidate = record.clone();
            let task_slot = slot.clone();
            tokio::spawn(async move {
                let source = match scan_permits.acquire_owned().await {
                    Ok(_permit) => tokio::task::spawn_blocking(move || resolver(&candidate))
                        .await
                        .ok()
                        .flatten(),
                    Err(_) => None,
                };
                let mut state = task_slot.lock().await;
                state.in_flight = false;
                if let Some(source) = source {
                    state.source = Some(source);
                    state.next_scan_at = None;
                } else {
                    state.next_scan_at = Some(Instant::now() + state.backoff);
                    state.backoff = next_provider_prompt_discovery_backoff(state.backoff);
                }
                state.progress.send_modify(|version| {
                    *version = version.wrapping_add(1);
                });
            });
            let _ = progress.changed().await;
        }
    }

    async fn invalidate_source(&self, record: &crate::SessionRecord) {
        let Some(key) = ProviderPromptDiscoveryKey::from_record(record) else {
            return;
        };
        let slot = self.entries.lock().await.get(&key).cloned();
        if let Some(slot) = slot {
            let mut state = slot.lock().await;
            state.source = None;
            state.next_scan_at = None;
            state.backoff = PROVIDER_PROMPT_PENDING_POLL_INTERVAL;
        }
    }

    async fn evict_session(&self, session_id: &str) {
        self.entries
            .lock()
            .await
            .retain(|key, _| key.session_id != session_id);
    }

    #[cfg(test)]
    async fn scan_attempts(&self, record: &crate::SessionRecord) -> usize {
        let Some(key) = ProviderPromptDiscoveryKey::from_record(record) else {
            return 0;
        };
        let slot = self.entries.lock().await.get(&key).cloned();
        let Some(slot) = slot else { return 0 };
        slot.lock().await.scan_attempts
    }

    #[cfg(test)]
    async fn entry_count(&self) -> usize {
        self.entries.lock().await.len()
    }

    #[cfg(test)]
    async fn in_flight_count(&self) -> usize {
        let slots: Vec<_> = self.entries.lock().await.values().cloned().collect();
        let mut count = 0;
        for slot in slots {
            if slot.lock().await.in_flight {
                count += 1;
            }
        }
        count
    }
}

fn next_provider_prompt_discovery_backoff(current: Duration) -> Duration {
    current
        .saturating_mul(2)
        .min(PROVIDER_PROMPT_DISCOVERY_MAX_BACKOFF)
}

impl AttachSubscription {
    fn receiver_mut(&mut self) -> &mut tokio::sync::broadcast::Receiver<AttachEvent> {
        self.receiver.as_mut().expect("attach receiver is present")
    }

    fn take_receiver(&mut self) -> tokio::sync::broadcast::Receiver<AttachEvent> {
        self.receiver.take().expect("attach receiver is present")
    }

    async fn release(mut self) {
        if let Some(lease) = self.lease.take() {
            lease.release().await;
        }
    }
}

impl Drop for AttachSubscription {
    fn drop(&mut self) {
        let Some(lease) = self.lease.take() else {
            return;
        };
        drop(lease);
    }
}

impl AttachLease {
    async fn release(mut self) {
        let cleanup = self.schedule_release();
        self.released = true;
        if let Some(cleanup) = cleanup {
            let _ = cleanup.await;
        }
    }

    fn schedule_release(&self) -> Option<tokio::task::JoinHandle<()>> {
        let handle = tokio::runtime::Handle::try_current().ok()?;
        let slot = self.slot.clone();
        let generation = self.generation;
        Some(handle.spawn(async move {
            let mut slot = slot.lock().await;
            let Some(active) = slot.active.as_mut() else {
                return;
            };
            if active.generation != generation {
                return;
            }
            if active.subscribers > 1 {
                active.subscribers -= 1;
                return;
            }
            if let Some(active) = slot.active.take() {
                // This supervisor task owns teardown independently of the
                // caller, so cancellation cannot strand the pipe or FIFO.
                // Keep the per-session lock so an old generation cannot close
                // a replacement pane pipe for the same session.
                active.broker.stop().await;
            }
        }))
    }
}

impl Drop for AttachLease {
    fn drop(&mut self) {
        if self.released {
            return;
        }
        let _ = self.schedule_release();
    }
}

impl AttachBrokerRegistry {
    async fn subscribe(
        &self,
        context: &CliContext,
        tmux: &Path,
        record: &crate::SessionRecord,
    ) -> io::Result<AttachSubscription> {
        let slot = {
            let mut entries = self.entries.lock().await;
            entries
                .entry(record.id.clone())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(AttachBrokerSlot::default())))
                .clone()
        };
        let mut slot_state = slot.lock().await;
        if !slot_state.accepting {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "attach broker is shutting down",
            ));
        }

        let target = format!("{}:0.0", record.tmux_session);
        if let Some(active) = slot_state.active.as_mut()
            && active.broker.target == target
        {
            // Subscribe before checking `closed`. If the reader exits between
            // these operations, this receiver observes its terminal event.
            let receiver = active.broker.sender.subscribe();
            if !active.broker.closed.load(Ordering::Acquire) {
                active.subscribers += 1;
                return Ok(AttachSubscription {
                    receiver: Some(receiver),
                    lease: Some(AttachLease {
                        slot: slot.clone(),
                        generation: active.generation,
                        released: false,
                    }),
                    resize_lock: active.broker.resize_lock.clone(),
                });
            }
        }

        if let Some(active) = slot_state.active.take() {
            active.broker.stop().await;
        }
        let generation = slot_state.next_generation;
        slot_state.next_generation = slot_state.next_generation.wrapping_add(1).max(1);
        let (broker, receiver) = AttachBroker::start(context, tmux, record).await?;
        let resize_lock = broker.resize_lock.clone();
        slot_state.active = Some(AttachBrokerGeneration {
            generation,
            subscribers: 1,
            broker,
        });
        Ok(AttachSubscription {
            receiver: Some(receiver),
            lease: Some(AttachLease {
                slot: slot.clone(),
                generation,
                released: false,
            }),
            resize_lock,
        })
    }

    async fn shutdown_all(&self) {
        let mut entries = self.entries.lock().await;
        let slots: Vec<_> = entries.drain().map(|(_, slot)| slot).collect();
        drop(entries);
        for slot in slots {
            let mut slot = slot.lock().await;
            slot.accepting = false;
            if let Some(active) = slot.active.take() {
                active.broker.stop().await;
            }
        }
    }

    async fn shutdown_session(&self, session_id: &str) {
        let slot = self.entries.lock().await.remove(session_id);
        if let Some(slot) = slot {
            let mut slot = slot.lock().await;
            slot.accepting = false;
            if let Some(active) = slot.active.take() {
                active.broker.stop().await;
            }
        }
    }

    #[cfg(test)]
    async fn subscriber_count(&self, session_id: &str) -> usize {
        let slot = self.entries.lock().await.get(session_id).cloned();
        let Some(slot) = slot else { return 0 };
        let slot = slot.lock().await;
        slot.active.as_ref().map_or(0, |active| active.subscribers)
    }
}

impl AttachBroker {
    async fn start(
        context: &CliContext,
        tmux: &Path,
        record: &crate::SessionRecord,
    ) -> io::Result<(Self, tokio::sync::broadcast::Receiver<AttachEvent>)> {
        Self::start_with_fifo_opener(context, tmux, record, |fifo_path| {
            let fifo = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(fifo_path)?;
            tokio::io::unix::AsyncFd::new(fifo)
        })
        .await
    }

    async fn start_with_fifo_opener<F>(
        context: &CliContext,
        tmux: &Path,
        record: &crate::SessionRecord,
        open_fifo: F,
    ) -> io::Result<(Self, tokio::sync::broadcast::Receiver<AttachEvent>)>
    where
        F: FnOnce(&Path) -> io::Result<tokio::io::unix::AsyncFd<std::fs::File>>,
    {
        let target = format!("{}:0.0", record.tmux_session);
        let fifo_path = session_dir(context, &record.id).join(ATTACH_LIVE_FIFO_NAME);
        create_private_fifo(&fifo_path)?;
        let mut fifo_cleanup = AttachFifoCleanup::new(fifo_path.clone());
        let fifo = open_fifo(&fifo_path)?;

        let (sender, receiver) = tokio::sync::broadcast::channel(ATTACH_BROADCAST_CAPACITY);
        enable_tmux_pipe(tmux, &target, &fifo_path).await?;

        let reader_sender = sender.clone();
        let closed = Arc::new(AtomicBool::new(false));
        let reader_closed = closed.clone();
        let reader_task = tokio::spawn(async move {
            read_attach_fifo(fifo, reader_sender, reader_closed).await;
        });
        let resize_lock = Arc::new(tokio::sync::Mutex::new(()));
        fifo_cleanup.disarm();
        Ok((
            Self {
                target,
                fifo_path,
                tmux: tmux.to_path_buf(),
                sender,
                reader_task,
                closed,
                resize_lock,
            },
            receiver,
        ))
    }

    async fn stop(self) {
        let _ = close_tmux_pipe(&self.tmux, &self.target).await;
        let mut reader_task = self.reader_task;
        if tokio::time::timeout(ATTACH_READER_STOP_TIMEOUT, &mut reader_task)
            .await
            .is_err()
        {
            reader_task.abort();
            let _ = reader_task.await;
        }
        let _ = std::fs::remove_file(&self.fifo_path);
    }
}

fn create_private_fifo(path: &Path) -> io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => std::fs::remove_file(path)?,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => return Err(err),
    }
    let path = CString::new(path.as_os_str().as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "attach FIFO path contains a NUL byte",
        )
    })?;
    // SAFETY: `path` is a valid, NUL-terminated C string and mode 0600 keeps
    // terminal bytes private to the daemon user.
    if unsafe { libc::mkfifo(path.as_ptr(), 0o600) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

async fn enable_tmux_pipe(tmux: &Path, target: &str, fifo_path: &Path) -> io::Result<()> {
    let tmux = tmux.to_path_buf();
    let target = target.to_string();
    let fifo_path = fifo_path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let mut command = ProcessCommand::new(&tmux);
        command.arg("pipe-pane").arg("-t").arg(&target).arg(format!(
            "cat > {}",
            shell_words::quote(&fifo_path.to_string_lossy())
        ));
        let status = run_process_with_timeout(&mut command, ATTACH_TMUX_COMMAND_TIMEOUT)?;
        if status.success() {
            Ok(())
        } else {
            Err(io::Error::other(format!(
                "tmux pipe-pane failed with status {status}"
            )))
        }
    })
    .await
    .map_err(|err| io::Error::other(format!("tmux pipe task failed: {err}")))?
}

async fn close_tmux_pipe(tmux: &Path, target: &str) -> io::Result<()> {
    let tmux = tmux.to_path_buf();
    let target = target.to_string();
    tokio::task::spawn_blocking(move || {
        let mut command = ProcessCommand::new(&tmux);
        command.arg("pipe-pane").arg("-t").arg(&target);
        run_process_with_timeout(&mut command, ATTACH_TMUX_COMMAND_TIMEOUT).map(|_| ())
    })
    .await
    .map_err(|err| io::Error::other(format!("tmux close task failed: {err}")))?
}

fn run_process_with_timeout(
    command: &mut ProcessCommand,
    timeout: Duration,
) -> io::Result<std::process::ExitStatus> {
    let mut child = command.spawn()?;
    wait_process_with_timeout(&mut child, timeout)
}

fn run_process_output_with_timeout(
    command: &mut ProcessCommand,
    timeout: Duration,
) -> io::Result<std::process::Output> {
    command.stdout(Stdio::piped()).stderr(Stdio::null());
    let mut child = command.spawn()?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("child stdout pipe is unavailable"))?;
    let stdout_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).map(|_| bytes)
    });
    let status = wait_process_with_timeout(&mut child, timeout);
    let stdout = stdout_reader
        .join()
        .map_err(|_| io::Error::other("child stdout reader panicked"))??;
    Ok(std::process::Output {
        status: status?,
        stdout,
        stderr: Vec::new(),
    })
}

fn wait_process_with_timeout(
    child: &mut std::process::Child,
    timeout: Duration,
) -> io::Result<std::process::ExitStatus> {
    let started_at = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if started_at.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("process exceeded {} ms", timeout.as_millis()),
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

async fn capture_attach_snapshot(
    tmux: &Path,
    record: &crate::SessionRecord,
) -> io::Result<Option<String>> {
    capture_attach_snapshot_with_timeout(tmux, record, ATTACH_SNAPSHOT_TIMEOUT).await
}

async fn capture_attach_snapshot_with_timeout(
    tmux: &Path,
    record: &crate::SessionRecord,
    timeout: Duration,
) -> io::Result<Option<String>> {
    let tmux = tmux.to_path_buf();
    let tmux_session = record.tmux_session.clone();
    tokio::task::spawn_blocking(move || {
        let mut command = ProcessCommand::new(&tmux);
        command
            .arg("capture-pane")
            .arg("-p")
            .arg("-t")
            .arg(&tmux_session)
            .arg("-S")
            .arg("-200");
        let output = run_process_output_with_timeout(&mut command, timeout)?;
        if output.status.success() {
            Ok(Some(String::from_utf8_lossy(&output.stdout).to_string()))
        } else {
            Ok(None)
        }
    })
    .await
    .map_err(|err| io::Error::other(format!("tmux capture task failed: {err}")))?
}

async fn capture_attach_handoff(
    tmux: &Path,
    record: &crate::SessionRecord,
    receiver: &mut tokio::sync::broadcast::Receiver<AttachEvent>,
) -> io::Result<AttachHandoff> {
    for recaptures in 0..=ATTACH_HANDOFF_MAX_RECAPTURES {
        let capture = capture_attach_snapshot(tmux, record);
        tokio::pin!(capture);
        let mut live = VecDeque::with_capacity(ATTACH_HANDOFF_BUFFER_CAPACITY);
        let mut overflowed = false;
        let mut broker_closed = false;

        let snapshot = loop {
            tokio::select! {
                snapshot = &mut capture => break snapshot?,
                event = receiver.recv(), if !broker_closed => {
                    match event {
                        Ok(AttachEvent::Output(bytes)) => {
                            if !overflowed && live.len() < ATTACH_HANDOFF_BUFFER_CAPACITY {
                                live.push_back(bytes);
                            } else {
                                // Continue draining so output generated by the
                                // daemon's own snapshot handoff cannot make this
                                // client appear slow. A fresh snapshot below
                                // becomes the new terminal-state baseline.
                                overflowed = true;
                                live.clear();
                            }
                        }
                        Ok(AttachEvent::Closed)
                        | Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            broker_closed = true;
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                            overflowed = true;
                            live.clear();
                        }
                    }
                }
            }
        };

        if !overflowed {
            return Ok(AttachHandoff {
                snapshot,
                live,
                broker_closed,
            });
        }
        if recaptures == ATTACH_HANDOFF_MAX_RECAPTURES {
            return Err(io::Error::other(
                "attach snapshot handoff overflowed after bounded recapture",
            ));
        }
    }
    unreachable!("bounded snapshot handoff loop returns")
}

async fn pump_attach_events(
    mut receiver: tokio::sync::broadcast::Receiver<AttachEvent>,
    terminal_tx: mpsc::Sender<Message>,
) -> AttachPumpExit {
    loop {
        match receiver.recv().await {
            Ok(AttachEvent::Output(bytes)) => {
                if terminal_tx.send(Message::Binary(bytes)).await.is_err() {
                    return AttachPumpExit::WriterClosed;
                }
            }
            Ok(AttachEvent::Closed) | Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                return AttachPumpExit::BrokerClosed;
            }
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                return AttachPumpExit::Lagged;
            }
        }
    }
}

async fn read_attach_fifo(
    fifo: tokio::io::unix::AsyncFd<std::fs::File>,
    sender: tokio::sync::broadcast::Sender<AttachEvent>,
    closed: Arc<AtomicBool>,
) {
    let mut buf = vec![0u8; 8192];
    let started_at = Instant::now();
    let mut writer_seen = false;
    loop {
        let read = match tokio::time::timeout(ATTACH_FIFO_POLL_INTERVAL, fifo.readable()).await {
            Ok(Ok(mut ready)) => match ready.try_io(|inner| {
                let mut file = inner.get_ref();
                file.read(&mut buf)
            }) {
                Ok(read) => read,
                Err(_would_block) => continue,
            },
            Ok(Err(err)) => Err(err),
            Err(_) => {
                // Some kqueue implementations do not emit another readable
                // notification when a FIFO's final writer disappears. The
                // descriptor is nonblocking, so a low-frequency direct read
                // distinguishes an idle writer (WouldBlock) from EOF without
                // tying up the async runtime.
                let mut file = fifo.get_ref();
                file.read(&mut buf)
            }
        };
        match read {
            Ok(0) if !writer_seen && started_at.elapsed() < ATTACH_PIPE_STARTUP_GRACE => {
                // `pipe-pane` starts its shell asynchronously. A nonblocking
                // FIFO reader briefly sees EOF until that writer opens.
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            Ok(0) => break,
            Ok(n) => {
                writer_seen = true;
                let _ = sender.send(AttachEvent::Output(Bytes::copy_from_slice(&buf[..n])));
            }
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {}
            Err(_) => break,
        }
    }
    closed.store(true, Ordering::Release);
    let _ = sender.send(AttachEvent::Closed);
}

#[cfg(test)]
fn attach_event_bytes(
    event: Result<AttachEvent, tokio::sync::broadcast::error::RecvError>,
) -> Option<Bytes> {
    match event {
        Ok(AttachEvent::Output(bytes)) => Some(bytes),
        Ok(AttachEvent::Closed)
        | Err(tokio::sync::broadcast::error::RecvError::Closed)
        | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => None,
    }
}

async fn send_attach_message<S>(sender: &mut S, message: Message, timeout: Duration) -> bool
where
    S: futures_util::Sink<Message> + Unpin,
{
    matches!(
        tokio::time::timeout(timeout, sender.send(message)).await,
        Ok(Ok(()))
    )
}

async fn attach_socket(
    socket: WebSocket,
    state: Arc<ServeState>,
    mut record: crate::SessionRecord,
) {
    let target = format!("{}:0.0", record.tmux_session);
    let (sender, mut receiver) = socket.split();
    let (terminal_tx, terminal_rx) = mpsc::channel(ATTACH_TERMINAL_QUEUE_CAPACITY);
    let (control_tx, control_rx) = mpsc::channel(ATTACH_CONTROL_QUEUE_CAPACITY);
    let mut writer_task = Some(AbortOnDropTask::new(tokio::spawn(outbound_writer(
        sender,
        terminal_rx,
        control_rx,
        ATTACH_WEBSOCKET_SEND_TIMEOUT,
    ))));

    // Subscribe before capturing the initial screen so pane output produced
    // during capture is buffered instead of silently lost. Snapshot and live
    // bytes may overlap; preserving every byte is preferable to a gap.
    let mut subscription = match state
        .attach_brokers
        .subscribe(&state.context, &state.tmux_bin, &record)
        .await
    {
        Ok(subscription) => subscription,
        Err(err) => {
            eprintln!(
                "warning: failed to start live attach broker for {}: {err}",
                record.id
            );
            return;
        }
    };
    let handoff =
        match capture_attach_handoff(&state.tmux_bin, &record, subscription.receiver_mut()).await {
            Ok(handoff) => handoff,
            Err(err) => {
                eprintln!(
                    "warning: failed to establish attach snapshot handoff for {}: {err}",
                    record.id
                );
                subscription.release().await;
                if let Some(task) = writer_task.take() {
                    task.abort_and_wait().await;
                }
                return;
            }
        };
    if let Some(text) = handoff.snapshot
        && terminal_tx
            .send(Message::Binary(text.into_bytes().into()))
            .await
            .is_err()
    {
        subscription.release().await;
        if let Some(task) = writer_task.take() {
            task.abort_and_wait().await;
        }
        return;
    }
    for bytes in handoff.live {
        if terminal_tx.send(Message::Binary(bytes)).await.is_err() {
            subscription.release().await;
            if let Some(task) = writer_task.take() {
                task.abort_and_wait().await;
            }
            return;
        }
    }

    let mut pump_task = (!handoff.broker_closed).then(|| {
        AbortOnDropTask::new(tokio::spawn(pump_attach_events(
            subscription.take_receiver(),
            terminal_tx.clone(),
        )))
    });

    // Client -> pane: JSON control frames { text?, key?, keys?, resize{cols,rows} }.
    // The first resize after attach forces a full-frame repaint (see resize_pane).
    let mut initial_repaint_pending = true;
    let resize_lock = subscription.resize_lock.clone();
    let mut provider_prompt_task: Option<AbortOnDropTask<()>> = None;
    let mut provider_open_task: Option<
        AbortOnDropTask<(crate::SessionRecord, Option<ProviderPromptTail>)>,
    > = None;
    let mut provider_refresh_pending = false;
    let mut provider_pending_deadline: Option<Instant> = None;
    let mut drain_writer = handoff.broker_closed;
    while pump_task.is_some() {
        tokio::select! {
            writer_result = async {
                writer_task
                    .as_mut()
                    .expect("writer task select guard")
                    .handle_mut()
                    .await
            }, if writer_task.is_some() => {
                writer_task.take();
                if matches!(writer_result, Ok(AttachWriterExit::SendFailed) | Err(_)) {
                    eprintln!(
                        "warning: outbound attach writer stopped for {}",
                        record.id
                    );
                }
                break;
            }
            pump_result = async {
                pump_task
                    .as_mut()
                    .expect("attach pump select guard")
                    .handle_mut()
                    .await
            }, if pump_task.is_some() => {
                pump_task.take();
                drain_writer = matches!(pump_result, Ok(AttachPumpExit::BrokerClosed));
                break;
            }
            opened = async {
                provider_open_task
                    .as_mut()
                    .expect("provider open task select guard")
                    .handle_mut()
                    .await
            }, if provider_open_task.is_some() => {
                provider_open_task.take();
                let prompt_tail = match opened {
                    Ok((updated_record, tail)) => {
                        record = updated_record;
                        tail
                    }
                    Err(_) => None,
                };
                let capability = ProviderPromptCapabilityState {
                    supported: prompt_tail.is_some(),
                    provider: prompt_tail.as_ref().map(ProviderPromptTail::provider),
                };
                if control_tx
                    .send(Message::Text(
                        provider_prompt_capability_frame(capability).into(),
                    ))
                    .await
                    .is_err()
                {
                    break;
                }

                if provider_refresh_pending && prompt_tail.is_none() {
                    provider_refresh_pending = false;
                    provider_open_task = Some(open_provider_prompt_task(
                        state.context.clone(),
                        state.provider_prompt_discovery.clone(),
                        record.clone(),
                        *provider_pending_deadline
                            .get_or_insert_with(|| Instant::now() + PROVIDER_PROMPT_PENDING_TIMEOUT),
                    ));
                } else if let Some(tail) = prompt_tail {
                    provider_refresh_pending = false;
                    provider_prompt_task = Some(AbortOnDropTask::new(tokio::spawn(
                        provider_prompt_loop(tail, control_tx.clone()),
                    )));
                }
            }
            message = receiver.next() => {
                let Some(Ok(message)) = message else { break; };
                match message {
                    Message::Text(text) => {
                        if provider_prompt_subscription_requested(text.as_str()) {
                            if let Some(mut task) = provider_prompt_task.take() {
                                task.abort();
                            }
                            if provider_open_task.is_some() {
                                // Preserve the old serial refresh behavior without
                                // pausing broker consumption or launching overlapping
                                // transcript scans for repeated subscriptions.
                                provider_refresh_pending = true;
                            } else {
                                provider_open_task = Some(open_provider_prompt_task(
                                    state.context.clone(),
                                    state.provider_prompt_discovery.clone(),
                                    record.clone(),
                                    *provider_pending_deadline.get_or_insert_with(|| {
                                        Instant::now() + PROVIDER_PROMPT_PENDING_TIMEOUT
                                    }),
                                ));
                            }
                            continue;
                        }
                        if let Err(err) = handle_input(
                            &state.context,
                            &state.tmux_bin,
                            &record,
                            &target,
                            text.as_str(),
                            &mut initial_repaint_pending,
                            &resize_lock,
                        )
                        .await
                        {
                            eprintln!(
                                "warning: terminal input failed for {}: {}",
                                record.id,
                                err.code()
                            );
                            break;
                        }
                    }
                    Message::Close(_) => break,
                    _ => {}
                }
            }
        }
    }

    if let Some(task) = pump_task {
        task.abort_and_wait().await;
    }
    if let Some(task) = provider_prompt_task {
        task.abort_and_wait().await;
    }
    if let Some(task) = provider_open_task {
        task.abort_and_wait().await;
    }
    subscription.release().await;
    drop(terminal_tx);
    drop(control_tx);
    if let Some(task) = writer_task {
        finish_outbound_writer(task, drain_writer).await;
    }
}

fn open_provider_prompt_task(
    context: CliContext,
    discovery: Arc<ProviderPromptDiscoveryRegistry>,
    record: crate::SessionRecord,
    pending_deadline: Instant,
) -> AbortOnDropTask<(crate::SessionRecord, Option<ProviderPromptTail>)> {
    AbortOnDropTask::new(tokio::spawn(resolve_provider_prompt_tail(
        context,
        discovery,
        record,
        pending_deadline,
    )))
}

async fn resolve_provider_prompt_tail(
    context: CliContext,
    discovery: Arc<ProviderPromptDiscoveryRegistry>,
    record: crate::SessionRecord,
    pending_deadline: Instant,
) -> (crate::SessionRecord, Option<ProviderPromptTail>) {
    let pending_fresh_runtime = provider_prompt_pending_fresh_runtime(&record);
    let initial_source = discovery.resolve_source(&record).await;
    let had_initial_source = initial_source.is_some();
    let mut opened = if let Some(source) = initial_source {
        tokio::task::spawn_blocking(move || ProviderPromptTail::open_source_at_eof(source))
            .await
            .ok()
            .flatten()
    } else {
        None
    };
    if had_initial_source && opened.is_none() {
        discovery.invalidate_source(&record).await;
        if let Some(source) = discovery.resolve_source(&record).await {
            opened =
                tokio::task::spawn_blocking(move || ProviderPromptTail::open_source_at_eof(source))
                    .await
                    .ok()
                    .flatten();
        }
    }
    if opened.is_some() || !pending_fresh_runtime {
        return (record, opened);
    }

    let mut current = record;
    while Instant::now() < pending_deadline {
        tokio::time::sleep(PROVIDER_PROMPT_PENDING_POLL_INTERVAL).await;
        let load_context = context.clone();
        let id = current.id.clone();
        let loaded =
            tokio::task::spawn_blocking(move || load_session_record(&load_context, &id)).await;
        let Ok(Ok(updated)) = loaded else {
            continue;
        };
        if !same_provider_prompt_runtime(&current, &updated) {
            return (updated, None);
        }
        current = updated;
        if !provider_prompt_new_runtime_source_authorized(&current) {
            continue;
        }
        let source = discovery.resolve_source(&current).await;
        let had_source = source.is_some();
        let tail = if let Some(source) = source {
            tokio::task::spawn_blocking(move || ProviderPromptTail::open_new_runtime_source(source))
                .await
                .ok()
                .flatten()
        } else {
            None
        };
        if tail.is_some() {
            return (current, tail);
        }
        if had_source {
            discovery.invalidate_source(&current).await;
        }
    }
    (current, None)
}

fn provider_prompt_new_runtime_source_authorized(record: &crate::SessionRecord) -> bool {
    let Some(resume) = record.provider_resume.as_ref() else {
        return false;
    };
    match AgentKind::from_name(&record.agent) {
        Some(AgentKind::Codex) => {
            resume.provider == "codex" && resume.capture_method == "codex-user-prompt-submit-hook"
        }
        Some(AgentKind::Claude) => {
            resume.provider == "claude" && resume.capture_method == "claude-explicit-session-id"
        }
        Some(AgentKind::Hermes) | None => false,
    }
}

fn provider_prompt_pending_fresh_runtime(record: &crate::SessionRecord) -> bool {
    if record
        .runtime
        .as_ref()
        .is_none_or(|runtime| runtime.generation != 1)
    {
        return false;
    }
    match AgentKind::from_name(&record.agent) {
        Some(AgentKind::Codex) => record.provider_resume.is_none(),
        Some(AgentKind::Claude) => record.provider_resume.as_ref().is_some_and(|resume| {
            resume.provider == "claude"
                && resume.capture_method == "claude-explicit-session-id"
                && !resume.session_id.trim().is_empty()
        }),
        Some(AgentKind::Hermes) | None => false,
    }
}

fn same_provider_prompt_runtime(
    expected: &crate::SessionRecord,
    current: &crate::SessionRecord,
) -> bool {
    match (&expected.runtime, &current.runtime) {
        (Some(expected), Some(current)) => {
            expected.generation == current.generation
                && expected.launch_id == current.launch_id
                && expected.tmux_session == current.tmux_session
        }
        _ => false,
    }
}

async fn finish_outbound_writer(mut writer_task: AbortOnDropTask<AttachWriterExit>, drain: bool) {
    if drain
        && tokio::time::timeout(ATTACH_WEBSOCKET_SEND_TIMEOUT, writer_task.handle_mut())
            .await
            .is_ok()
    {
        return;
    }
    writer_task.abort_and_wait().await;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ProviderPromptCapabilityState {
    supported: bool,
    provider: Option<ProviderKind>,
}

fn provider_prompt_subscription_requested(frame: &str) -> bool {
    serde_json::from_str::<Value>(frame)
        .ok()
        .and_then(|value| value.get("subscribe").and_then(Value::as_array).cloned())
        .is_some_and(|capabilities| {
            capabilities
                .iter()
                .any(|capability| capability.as_str() == Some(PROVIDER_PROMPT_CAPABILITY))
        })
}

fn provider_prompt_capability_frame(capability: ProviderPromptCapabilityState) -> String {
    json!({
        "schema_version": ATTACH_SCHEMA_VERSION,
        "type": "capability",
        "capability": PROVIDER_PROMPT_CAPABILITY,
        "supported": capability.supported,
        "provider": capability.provider.map(ProviderKind::as_str),
        "prompt_max_bytes": MAX_PROVIDER_PROMPT_BYTES,
    })
    .to_string()
}

fn provider_prompt_event_frame(provider: ProviderKind, event: ProviderPromptEvent) -> String {
    json!({
        "schema_version": ATTACH_EVENT_SCHEMA_VERSION,
        "type": "prompt_submitted",
        "event_id": event.id,
        "provider": provider.as_str(),
        "submitted_at": event.submitted_at,
        "text": event.prompt,
        "truncated": event.truncated,
    })
    .to_string()
}

async fn provider_prompt_loop(mut tail: ProviderPromptTail, sender: mpsc::Sender<Message>) {
    let provider = tail.provider();
    let mut interval = tokio::time::interval(PROVIDER_PROMPT_POLL_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = sender.closed() => break,
            _ = interval.tick() => {}
        }
        let polled = tokio::task::spawn_blocking(move || {
            let result = tail.poll();
            (tail, result)
        })
        .await;
        let Ok((returned_tail, result)) = polled else {
            break;
        };
        tail = returned_tail;
        let Ok(events) = result else {
            break;
        };
        for event in events {
            let frame = Message::Text(provider_prompt_event_frame(provider, event).into());
            match sender.try_send(frame) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(_)) => {
                    // Prompt events are advisory. Queue saturation must never
                    // apply backpressure to terminal output.
                    break;
                }
                Err(mpsc::error::TrySendError::Closed(_)) => return,
            }
        }
    }
}

async fn outbound_writer<S>(
    mut sender: S,
    mut terminal_rx: mpsc::Receiver<Message>,
    mut control_rx: mpsc::Receiver<Message>,
    send_timeout: Duration,
) -> AttachWriterExit
where
    S: futures_util::Sink<Message> + Unpin,
{
    let mut terminal_burst = 0;
    while let Some(message) =
        next_outbound_message(&mut terminal_rx, &mut control_rx, &mut terminal_burst).await
    {
        if !send_attach_message(&mut sender, message, send_timeout).await {
            return AttachWriterExit::SendFailed;
        }
    }
    AttachWriterExit::Drained
}

async fn next_outbound_message(
    terminal_rx: &mut mpsc::Receiver<Message>,
    control_rx: &mut mpsc::Receiver<Message>,
    terminal_burst: &mut usize,
) -> Option<Message> {
    if *terminal_burst >= ATTACH_TERMINAL_BURST
        && let Ok(message) = control_rx.try_recv()
    {
        *terminal_burst = 0;
        return Some(message);
    }
    if let Ok(message) = terminal_rx.try_recv() {
        *terminal_burst += 1;
        return Some(message);
    }
    if let Ok(message) = control_rx.try_recv() {
        *terminal_burst = 0;
        return Some(message);
    }
    tokio::select! {
        biased;
        message = terminal_rx.recv() => {
            match message {
                Some(message) => {
                    *terminal_burst += 1;
                    Some(message)
                }
                None => {
                    let message = control_rx.recv().await;
                    if message.is_some() {
                        *terminal_burst = 0;
                    }
                    message
                },
            }
        }
        message = control_rx.recv() => {
            match message {
                Some(message) => {
                    *terminal_burst = 0;
                    Some(message)
                }
                None => {
                    let message = terminal_rx.recv().await;
                    if message.is_some() {
                        *terminal_burst += 1;
                    }
                    message
                },
            }
        }
    }
}

/// After a fresh (re)attach the client rebuilds its terminal emulator from
/// scratch and sends its real size as the first resize frame. A `resize-window`
/// to dimensions the tmux pane already has is a no-op — no SIGWINCH — so a
/// full-screen agent (codex/claude/hermes) never repaints, and the client is
/// left rendering the stale pre-attach snapshot mis-wrapped against its new
/// grid. Force exactly one guaranteed size change on that first resize so the
/// agent redraws its whole frame into the fresh grid. The short pause lets the
/// agent observe the intermediate size, so the two changes are not coalesced
/// into "no net change" (which would skip the redraw).
const INITIAL_REPAINT_NUDGE_DELAY: Duration = Duration::from_millis(150);

/// Apply a client resize to the pane. On the first resize after attach
/// (`force_repaint`), nudge to an off-by-one height first so the final resize to
/// `rows` is always a real change that repaints the agent's frame; afterwards a
/// single `resize-window` is issued so ordinary mid-session resizes don't flicker.
async fn resize_pane(tmux: &Path, target: &str, cols: u64, rows: u64, force_repaint: bool) {
    resize_pane_with_timeout(
        tmux,
        target,
        cols,
        rows,
        force_repaint,
        ATTACH_RESIZE_TIMEOUT,
    )
    .await;
}

async fn resize_pane_with_timeout(
    tmux: &Path,
    target: &str,
    cols: u64,
    rows: u64,
    force_repaint: bool,
    timeout: Duration,
) {
    if force_repaint {
        let nudge_rows = if rows > 1 { rows - 1 } else { rows + 1 };
        let _ = run_resize_window(tmux, target, cols, nudge_rows, timeout).await;
        tokio::time::sleep(INITIAL_REPAINT_NUDGE_DELAY).await;
    }
    let _ = run_resize_window(tmux, target, cols, rows, timeout).await;
}

async fn run_resize_window(
    tmux: &Path,
    target: &str,
    cols: u64,
    rows: u64,
    timeout: Duration,
) -> io::Result<()> {
    let tmux = tmux.to_path_buf();
    let target = target.to_string();
    tokio::task::spawn_blocking(move || {
        let mut command = ProcessCommand::new(&tmux);
        command
            .arg("resize-window")
            .arg("-t")
            .arg(&target)
            .arg("-x")
            .arg(cols.to_string())
            .arg("-y")
            .arg(rows.to_string());
        run_process_with_timeout(&mut command, timeout).map(|_| ())
    })
    .await
    .map_err(|err| io::Error::other(format!("tmux resize task failed: {err}")))?
}

async fn handle_input(
    context: &CliContext,
    tmux: &Path,
    record: &crate::SessionRecord,
    target: &str,
    frame: &str,
    initial_repaint_pending: &mut bool,
    resize_lock: &tokio::sync::Mutex<()>,
) -> Result<(), CliError> {
    let Ok(value) = serde_json::from_str::<Value>(frame) else {
        return Ok(());
    };

    if let Some(resize) = value.get("resize") {
        let cols = resize.get("cols").and_then(Value::as_u64);
        let rows = resize.get("rows").and_then(Value::as_u64);
        if let (Some(cols), Some(rows)) = (cols, rows) {
            let force_repaint = std::mem::take(initial_repaint_pending);
            // Concurrent attach clients share one tmux pane. Keep each resize
            // sequence atomic; the last completed client resize wins.
            let _guard = resize_lock.lock().await;
            resize_pane(tmux, target, cols, rows, force_repaint).await;
        }
        return Ok(());
    }

    let text = value
        .get("text")
        .and_then(Value::as_str)
        .map(str::to_string);
    let mut keys: Vec<SpecialKey> = value
        .get("keys")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .filter_map(SpecialKey::from_name)
                .collect()
        })
        .unwrap_or_default();
    if let Some(key) = value
        .get("key")
        .and_then(Value::as_str)
        .and_then(SpecialKey::from_name)
    {
        keys.push(key);
    }
    if text.is_none() && keys.is_empty() {
        return Ok(());
    }

    let context = context.clone();
    let tmux = tmux.to_path_buf();
    let record = record.clone();
    let id = record.id.clone();
    tokio::task::spawn_blocking(move || {
        send_input_serialized(&context, &record, text.as_deref(), &keys, &tmux)
    })
    .await
    .map_err(|_| {
        CliError::runtime(
            "attach-input-worker-failed",
            "the terminal input worker failed",
            Some(json!({ "id": id })),
        )
    })?
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use nils_test_support::{EnvGuard, GlobalStateLock};
    use std::fs;
    use std::io::Write;
    use std::os::unix::fs::{FileTypeExt, PermissionsExt, symlink};
    use tokio_tungstenite::connect_async;
    use tokio_tungstenite::tungstenite::{Message as ClientMessage, client::IntoClientRequest};
    use tower::ServiceExt;

    const MACHINE: &str = "test-machine";
    const TOKEN: &str = "s3cr3t-token";

    #[test]
    fn provider_prompt_subscription_requires_the_versioned_capability() {
        assert!(provider_prompt_subscription_requested(
            r#"{"subscribe":["provider-prompt.v1"]}"#
        ));
        assert!(provider_prompt_subscription_requested(
            r#"{"subscribe":["other.v1","provider-prompt.v1"]}"#
        ));
        assert!(!provider_prompt_subscription_requested(
            r#"{"subscribe":["provider-prompt.v2"]}"#
        ));
        assert!(!provider_prompt_subscription_requested(
            r#"{"text":"provider-prompt.v1"}"#
        ));
        assert!(!provider_prompt_subscription_requested("not json"));
    }

    #[test]
    fn provider_prompt_frames_use_the_versioned_text_contract() {
        let capability = provider_prompt_capability_frame(ProviderPromptCapabilityState {
            supported: true,
            provider: Some(ProviderKind::Codex),
        });
        let capability: Value = serde_json::from_str(&capability).expect("capability json");
        assert_eq!(
            capability,
            json!({
                "schema_version": ATTACH_SCHEMA_VERSION,
                "type": "capability",
                "capability": PROVIDER_PROMPT_CAPABILITY,
                "supported": true,
                "provider": "codex",
                "prompt_max_bytes": MAX_PROVIDER_PROMPT_BYTES,
            })
        );
        let unsupported: Value = serde_json::from_str(&provider_prompt_capability_frame(
            ProviderPromptCapabilityState {
                supported: false,
                provider: None,
            },
        ))
        .expect("unsupported capability json");
        assert_eq!(
            unsupported,
            json!({
                "schema_version": ATTACH_SCHEMA_VERSION,
                "type": "capability",
                "capability": PROVIDER_PROMPT_CAPABILITY,
                "supported": false,
                "provider": null,
                "prompt_max_bytes": MAX_PROVIDER_PROMPT_BYTES,
            })
        );

        let event = provider_prompt_event_frame(
            ProviderKind::Claude,
            ProviderPromptEvent {
                id: "pp-opaque".to_string(),
                prompt: "submitted prompt".to_string(),
                submitted_at: "2099-01-01T00:00:00Z".to_string(),
                truncated: false,
            },
        );
        let event: Value = serde_json::from_str(&event).expect("event json");
        assert_eq!(
            event,
            json!({
                "schema_version": ATTACH_EVENT_SCHEMA_VERSION,
                "type": "prompt_submitted",
                "event_id": "pp-opaque",
                "provider": "claude",
                "submitted_at": "2099-01-01T00:00:00Z",
                "text": "submitted prompt",
                "truncated": false,
            })
        );
        assert!(!event.to_string().contains("/home/"));
    }

    #[test]
    fn pending_provider_prompt_runtime_guard_rejects_replacement_identity() {
        let mut expected = test_record("pending-runtime", "hs-pending-runtime");
        expected.runtime = Some(crate::RuntimeInfo {
            kind: "tmux".to_string(),
            tmux_session: "hs-pending-runtime".to_string(),
            generation: 1,
            started_at: "2026-07-11T00:00:00Z".to_string(),
            launch_id: "launch-1".to_string(),
            extra: std::collections::BTreeMap::new(),
        });
        let mut current = expected.clone();
        assert!(same_provider_prompt_runtime(&expected, &current));

        current.runtime.as_mut().expect("runtime").launch_id = "launch-2".to_string();
        assert!(!same_provider_prompt_runtime(&expected, &current));
        current = expected.clone();
        current.runtime.as_mut().expect("runtime").generation = 2;
        assert!(!same_provider_prompt_runtime(&expected, &current));
        current = expected.clone();
        current.runtime.as_mut().expect("runtime").tmux_session = "hs-replaced".to_string();
        assert!(!same_provider_prompt_runtime(&expected, &current));
    }

    #[test]
    fn fresh_provider_prompt_byte_zero_requires_authoritative_provenance() {
        let mut record = provider_discovery_record(
            "prompt-provenance",
            "hs-prompt-provenance",
            "launch-prompt-provenance",
            1,
        );
        assert!(provider_prompt_new_runtime_source_authorized(&record));

        record.agent = "codex".to_string();
        let resume = record.provider_resume.as_mut().expect("provider resume");
        resume.provider = "codex".to_string();
        resume.capture_method = "codex-session-meta".to_string();
        assert!(!provider_prompt_new_runtime_source_authorized(&record));
        record
            .provider_resume
            .as_mut()
            .expect("provider resume")
            .capture_method = "codex-user-prompt-submit-hook".to_string();
        assert!(provider_prompt_new_runtime_source_authorized(&record));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn pending_provider_prompt_resolver_rejects_runtime_replacement_without_replay() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let state_dir = tmp.path().join("state");
        let cwd = tmp.path().join("repo");
        std::fs::create_dir_all(&cwd).expect("cwd");
        seed_fresh_provider_session(
            &state_dir,
            "resolver-runtime-replaced",
            "codex",
            "hs-resolver-runtime-replaced",
            &cwd,
            None,
        );
        let context = CliContext {
            state_dir: state_dir.clone(),
            host: None,
        };
        let record = load_session_record(&context, "resolver-runtime-replaced")
            .expect("initial session record");
        let discovery = Arc::new(ProviderPromptDiscoveryRegistry::default());
        let task = tokio::spawn(resolve_provider_prompt_tail(
            context.clone(),
            discovery,
            record,
            Instant::now() + Duration::from_secs(2),
        ));

        tokio::time::sleep(Duration::from_millis(50)).await;
        let mut replacement = load_session_record(&context, "resolver-runtime-replaced")
            .expect("replacement session record");
        let runtime = replacement.runtime.as_mut().expect("runtime");
        runtime.generation = 2;
        runtime.launch_id = "replacement-launch".to_string();
        runtime.tmux_session = "hs-resolver-runtime-replacement".to_string();
        replacement.provider_resume = Some(crate::ProviderResume {
            provider: "codex".to_string(),
            session_id: "replacement-provider-id".to_string(),
            captured_at: "2026-07-11T00:00:00Z".to_string(),
            capture_method: "codex-user-prompt-submit-hook".to_string(),
            resume_args: vec!["resume".to_string(), "replacement-provider-id".to_string()],
            extra: std::collections::BTreeMap::new(),
        });
        crate::write_session_record(&context, &replacement).expect("persist replacement runtime");

        let (resolved_record, tail) = task.await.expect("resolver task");
        assert_eq!(
            resolved_record
                .runtime
                .as_ref()
                .expect("resolved runtime")
                .generation,
            2
        );
        assert!(
            tail.is_none(),
            "runtime replacement must not open either runtime transcript"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn pending_provider_prompt_discovery_coalesces_concurrent_scans() {
        let lock = GlobalStateLock::new();
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let claude_config = tmp.path().join("claude-config");
        std::fs::create_dir_all(claude_config.join("projects/-pending"))
            .expect("claude projects root");
        let _claude_config =
            EnvGuard::set(&lock, "CLAUDE_CONFIG_DIR", claude_config.to_str().unwrap());
        let mut record = test_record("pending-shared-scan", "hs-pending-shared-scan");
        record.agent = "claude".to_string();
        record.provider_resume = Some(crate::ProviderResume {
            provider: "claude".to_string(),
            session_id: "pending-shared-provider-id".to_string(),
            captured_at: "2026-07-11T00:00:00Z".to_string(),
            capture_method: "claude-explicit-session-id".to_string(),
            resume_args: vec![
                "--resume".to_string(),
                "pending-shared-provider-id".to_string(),
            ],
            extra: std::collections::BTreeMap::new(),
        });
        record.runtime = Some(crate::RuntimeInfo {
            kind: "tmux".to_string(),
            tmux_session: "hs-pending-shared-scan".to_string(),
            generation: 1,
            started_at: "2026-07-11T00:00:00Z".to_string(),
            launch_id: "launch-shared-scan".to_string(),
            extra: std::collections::BTreeMap::new(),
        });
        let registry = Arc::new(ProviderPromptDiscoveryRegistry::default());
        let mut tasks = tokio::task::JoinSet::new();
        for _ in 0..8 {
            let registry = registry.clone();
            let record = record.clone();
            tasks.spawn(async move { registry.resolve_source(&record).await });
        }
        while tasks.join_next().await.is_some() {}

        assert_eq!(
            registry.scan_attempts(&record).await,
            1,
            "concurrent clients must share one provider-history scan"
        );
    }

    #[test]
    fn provider_prompt_discovery_backoff_doubles_and_caps() {
        let mut backoff = PROVIDER_PROMPT_PENDING_POLL_INTERVAL;
        let mut observed = Vec::new();
        for _ in 0..8 {
            backoff = next_provider_prompt_discovery_backoff(backoff);
            observed.push(backoff);
        }
        assert_eq!(observed[0], Duration::from_secs(1));
        assert_eq!(observed[1], Duration::from_secs(2));
        assert_eq!(observed[2], Duration::from_secs(4));
        assert_eq!(observed[5], PROVIDER_PROMPT_DISCOVERY_MAX_BACKOFF);
        assert_eq!(observed[7], PROVIDER_PROMPT_DISCOVERY_MAX_BACKOFF);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn pending_provider_prompt_scan_survives_lead_waiter_cancellation() {
        let started = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let max_active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let release = Arc::new(AtomicBool::new(false));
        let registry = Arc::new(ProviderPromptDiscoveryRegistry::with_resolver({
            let started = started.clone();
            let active = active.clone();
            let max_active = max_active.clone();
            let release = release.clone();
            move |_| {
                started.fetch_add(1, Ordering::SeqCst);
                let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                max_active.fetch_max(current, Ordering::SeqCst);
                while !release.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(5));
                }
                active.fetch_sub(1, Ordering::SeqCst);
                None
            }
        }));
        let record = provider_discovery_record(
            "cancelled-lead-scan",
            "hs-cancelled-lead-scan",
            "launch-cancelled-lead-scan",
            1,
        );
        let lead = {
            let registry = registry.clone();
            let record = record.clone();
            tokio::spawn(async move { registry.resolve_source(&record).await })
        };
        while started.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
        lead.abort();

        let mut replacements = tokio::task::JoinSet::new();
        for _ in 0..4 {
            let registry = registry.clone();
            let record = record.clone();
            replacements.spawn(async move { registry.resolve_source(&record).await });
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(started.load(Ordering::SeqCst), 1);
        assert_eq!(max_active.load(Ordering::SeqCst), 1);
        release.store(true, Ordering::SeqCst);
        while replacements.join_next().await.is_some() {}
        assert_eq!(registry.scan_attempts(&record).await, 1);
        assert!(registry.resolve_source(&record).await.is_none());
        assert_eq!(registry.scan_attempts(&record).await, 1);
    }

    #[tokio::test]
    async fn provider_prompt_discovery_prunes_old_runtimes_and_deleted_sessions() {
        let registry = ProviderPromptDiscoveryRegistry::with_resolver(|_| None);
        let first = provider_discovery_record(
            "discovery-lifecycle",
            "hs-discovery-lifecycle",
            "launch-1",
            1,
        );
        assert!(registry.resolve_source(&first).await.is_none());
        assert_eq!(registry.entry_count().await, 1);

        let second = provider_discovery_record(
            "discovery-lifecycle",
            "hs-discovery-lifecycle",
            "launch-2",
            2,
        );
        assert!(registry.resolve_source(&second).await.is_none());
        assert_eq!(registry.entry_count().await, 1);

        registry.evict_session("discovery-lifecycle").await;
        assert_eq!(registry.entry_count().await, 0);
    }

    #[tokio::test]
    async fn provider_prompt_discovery_bounds_distinct_session_churn() {
        let registry = ProviderPromptDiscoveryRegistry::with_resolver(|_| None);
        for index in 0..(PROVIDER_PROMPT_DISCOVERY_MAX_ENTRIES + 10) {
            let id = format!("discovery-churn-{index}");
            let record =
                provider_discovery_record(&id, &format!("hs-{id}"), &format!("launch-{index}"), 1);
            assert!(registry.resolve_source(&record).await.is_none());
        }
        assert_eq!(
            registry.entry_count().await,
            PROVIDER_PROMPT_DISCOVERY_MAX_ENTRIES
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn provider_prompt_discovery_does_not_evict_active_slots_and_bounds_scans() {
        let started = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let max_active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let release = Arc::new(AtomicBool::new(false));
        let registry = Arc::new(ProviderPromptDiscoveryRegistry::with_resolver({
            let started = started.clone();
            let active = active.clone();
            let max_active = max_active.clone();
            let release = release.clone();
            move |_| {
                started.fetch_add(1, Ordering::SeqCst);
                let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                max_active.fetch_max(current, Ordering::SeqCst);
                while !release.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(5));
                }
                active.fetch_sub(1, Ordering::SeqCst);
                None
            }
        }));
        let records: Vec<_> = (0..PROVIDER_PROMPT_DISCOVERY_MAX_ENTRIES)
            .map(|index| {
                let id = format!("active-discovery-{index}");
                provider_discovery_record(&id, &format!("hs-{id}"), &format!("launch-{index}"), 1)
            })
            .collect();
        let mut tasks = tokio::task::JoinSet::new();
        for record in &records {
            let registry = registry.clone();
            let record = record.clone();
            tasks.spawn(async move { registry.resolve_source(&record).await });
        }
        while registry.in_flight_count().await < PROVIDER_PROMPT_DISCOVERY_MAX_ENTRIES {
            tokio::task::yield_now().await;
        }
        while started.load(Ordering::SeqCst) < PROVIDER_PROMPT_DISCOVERY_MAX_CONCURRENT_SCANS {
            tokio::task::yield_now().await;
        }
        for index in 0..10 {
            let id = format!("overflow-discovery-{index}");
            let record = provider_discovery_record(
                &id,
                &format!("hs-{id}"),
                &format!("overflow-launch-{index}"),
                1,
            );
            assert!(registry.resolve_source(&record).await.is_none());
        }
        let duplicate = {
            let registry = registry.clone();
            let record = records[0].clone();
            tokio::spawn(async move { registry.resolve_source(&record).await })
        };
        tokio::time::sleep(Duration::from_millis(25)).await;
        assert_eq!(registry.scan_attempts(&records[0]).await, 1);
        assert_eq!(
            max_active.load(Ordering::SeqCst),
            PROVIDER_PROMPT_DISCOVERY_MAX_CONCURRENT_SCANS
        );

        release.store(true, Ordering::SeqCst);
        while tasks.join_next().await.is_some() {}
        duplicate.await.expect("duplicate waiter");
        assert_eq!(
            registry.entry_count().await,
            PROVIDER_PROMPT_DISCOVERY_MAX_ENTRIES
        );
        assert!(
            max_active.load(Ordering::SeqCst) <= PROVIDER_PROMPT_DISCOVERY_MAX_CONCURRENT_SCANS
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn provider_prompt_resolver_evicts_stale_cache_and_rediscovers_exact_source() {
        let lock = GlobalStateLock::new();
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let claude_config = tmp.path().join("claude-config");
        let _claude_config =
            EnvGuard::set(&lock, "CLAUDE_CONFIG_DIR", claude_config.to_str().unwrap());
        let record =
            provider_discovery_record("stale-cache", "hs-stale-cache", "launch-stale-cache", 2);
        let provider_id = record
            .provider_resume
            .as_ref()
            .expect("provider resume")
            .session_id
            .clone();
        let first = claude_config
            .join("projects/-first")
            .join(format!("{provider_id}.jsonl"));
        std::fs::create_dir_all(first.parent().expect("first parent")).expect("first dir");
        std::fs::write(
            &first,
            format!(
                "{}\n",
                json!({
                    "type":"user",
                    "sessionId":provider_id,
                    "cwd":record.cwd.clone(),
                    "message":{"role":"user","content":"first"}
                })
            ),
        )
        .expect("first transcript");
        let registry = Arc::new(ProviderPromptDiscoveryRegistry::default());
        assert!(registry.resolve_source(&record).await.is_some());

        std::fs::write(
            &first,
            format!(
                "{}\n",
                json!({
                    "type":"user",
                    "sessionId":"different-provider-id",
                    "cwd":record.cwd.clone(),
                    "message":{"role":"user","content":"mismatch"}
                })
            ),
        )
        .expect("replace cached transcript");
        let second = claude_config
            .join("projects/-second")
            .join(format!("{provider_id}.jsonl"));
        std::fs::create_dir_all(second.parent().expect("second parent")).expect("second dir");
        std::fs::write(
            &second,
            format!(
                "{}\n",
                json!({
                    "type":"user",
                    "sessionId":provider_id,
                    "cwd":record.cwd.clone(),
                    "message":{"role":"user","content":"replacement"}
                })
            ),
        )
        .expect("replacement transcript");

        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let (_, tail) = resolve_provider_prompt_tail(
            context,
            registry.clone(),
            record.clone(),
            Instant::now() + Duration::from_secs(1),
        )
        .await;
        assert!(tail.is_some(), "stale cache must be rediscovered exactly");
        assert_eq!(registry.scan_attempts(&record).await, 2);
    }

    #[tokio::test]
    async fn outbound_writer_prioritizes_terminal_bytes_over_control_events() {
        let (terminal_tx, mut terminal_rx) = mpsc::channel(2);
        let (control_tx, mut control_rx) = mpsc::channel(2);
        control_tx
            .send(Message::Text("control".into()))
            .await
            .expect("queue control");
        terminal_tx
            .send(Message::Binary(vec![1, 2, 3].into()))
            .await
            .expect("queue terminal");

        let mut terminal_burst = 0;
        let first = next_outbound_message(&mut terminal_rx, &mut control_rx, &mut terminal_burst)
            .await
            .expect("first frame");
        let second = next_outbound_message(&mut terminal_rx, &mut control_rx, &mut terminal_burst)
            .await
            .expect("second frame");
        assert!(matches!(first, Message::Binary(_)));
        assert!(matches!(second, Message::Text(_)));
    }

    #[tokio::test]
    async fn outbound_writer_delivers_control_within_a_bounded_terminal_burst() {
        let (terminal_tx, mut terminal_rx) = mpsc::channel(ATTACH_TERMINAL_QUEUE_CAPACITY);
        let (control_tx, mut control_rx) = mpsc::channel(2);
        for byte in 0..ATTACH_TERMINAL_QUEUE_CAPACITY {
            terminal_tx
                .send(Message::Binary(vec![byte as u8].into()))
                .await
                .expect("queue terminal");
        }
        control_tx
            .send(Message::Text("capability".into()))
            .await
            .expect("queue control");

        let mut selected_control = false;
        let mut terminal_burst = 0;
        for _ in 0..=32 {
            let message =
                next_outbound_message(&mut terminal_rx, &mut control_rx, &mut terminal_burst)
                    .await
                    .expect("frame");
            if matches!(message, Message::Text(_)) {
                selected_control = true;
                break;
            }
        }
        assert!(
            selected_control,
            "control frame exceeded the terminal burst bound"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn websocket_subscription_emits_capability_and_new_provider_prompt_only() {
        let lock = GlobalStateLock::new();
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let state_dir = tmp.path().join("state");
        let codex_home = tmp.path().join("codex-home");
        let transcript = codex_home.join("sessions/2026/07/session.jsonl");
        let cwd = tmp.path().join("repo");
        std::fs::create_dir_all(transcript.parent().expect("transcript parent"))
            .expect("transcript dir");
        std::fs::create_dir_all(&cwd).expect("cwd");
        std::fs::write(
            &transcript,
            format!(
                "{}\n{}\n",
                json!({
                    "timestamp":"2099-01-01T00:00:00Z",
                    "type":"session_meta",
                    "payload":{
                        "id":"resume-session-id",
                        "session_id":"resume-session-id",
                        "cwd":cwd.to_string_lossy(),
                        "source":"cli",
                        "timestamp":"2099-01-01T00:00:00Z"
                    }
                }),
                json!({
                    "type":"event_msg",
                    "payload":{"type":"user_message","message":"pre-subscription prompt"}
                })
            ),
        )
        .expect("transcript");
        let _codex_home = EnvGuard::set(&lock, "CODEX_HOME", codex_home.to_str().unwrap());
        seed_resumable_session(
            &state_dir,
            "ws-prompt",
            "codex",
            "hs-codex-ws-prompt",
            &cwd,
            &["resume", "resume-session-id"],
        );
        let tmux = minimal_tmux(tmp.path());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener");
        let addr = listener.local_addr().expect("address");
        let server_state_dir = state_dir.clone();
        let server_state = state(&server_state_dir, Some(TOKEN), tmux);
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, router(server_state)).await;
        });

        let mut request = format!("ws://{addr}/sessions/ws-prompt/attach")
            .into_client_request()
            .expect("request");
        request.headers_mut().insert(
            AUTHORIZATION,
            format!("Bearer {TOKEN}").parse().expect("authorization"),
        );
        let (mut socket, _) = connect_async(request).await.expect("connect");
        let snapshot = tokio::time::timeout(Duration::from_secs(2), socket.next())
            .await
            .expect("snapshot timeout")
            .expect("snapshot frame")
            .expect("snapshot message");
        assert!(matches!(snapshot, ClientMessage::Binary(_)));

        socket
            .send(ClientMessage::Text(
                json!({"subscribe":[PROVIDER_PROMPT_CAPABILITY]})
                    .to_string()
                    .into(),
            ))
            .await
            .expect("subscribe");
        let capability = tokio::time::timeout(Duration::from_secs(2), socket.next())
            .await
            .expect("capability timeout")
            .expect("capability frame")
            .expect("capability message");
        let ClientMessage::Text(capability) = capability else {
            panic!("capability must be a text frame");
        };
        let capability: Value = serde_json::from_str(capability.as_str()).expect("capability json");
        assert_eq!(capability["type"], "capability");
        assert_eq!(capability["supported"], true);

        OpenOptions::new()
            .append(true)
            .open(&transcript)
            .expect("append transcript")
            .write_all(
                format!(
                    "{}\n",
                    json!({
                        "type":"event_msg",
                        "payload":{"type":"user_message","message":"websocket prompt"}
                    })
                )
                .as_bytes(),
            )
            .expect("write prompt");
        let event = tokio::time::timeout(Duration::from_secs(2), socket.next())
            .await
            .expect("event timeout")
            .expect("event frame")
            .expect("event message");
        let ClientMessage::Text(event) = event else {
            panic!("prompt event must be a text frame");
        };
        let event: Value = serde_json::from_str(event.as_str()).expect("event json");
        assert_eq!(event["type"], "prompt_submitted");
        assert_eq!(event["text"], "websocket prompt");
        assert!(event["event_id"].is_string());
        assert!(event["submitted_at"].is_string());
        assert_eq!(event["provider"], "codex");
        assert!(
            !event
                .to_string()
                .contains(transcript.to_string_lossy().as_ref())
        );

        let rotated = transcript.with_extension("jsonl.old");
        std::fs::rename(&transcript, &rotated).expect("rotate transcript");
        std::fs::write(
            &transcript,
            format!(
                "{}\n",
                json!({
                    "timestamp":"2099-01-01T00:00:00Z",
                    "type":"session_meta",
                    "payload":{
                        "id":"different-session-id",
                        "cwd":cwd.to_string_lossy(),
                        "source":"cli",
                        "timestamp":"2099-01-01T00:00:00Z"
                    }
                })
            ),
        )
        .expect("mismatched replacement");
        socket
            .send(ClientMessage::Text(
                json!({"subscribe":[PROVIDER_PROMPT_CAPABILITY]})
                    .to_string()
                    .into(),
            ))
            .await
            .expect("resubscribe");
        let capability = tokio::time::timeout(Duration::from_secs(2), socket.next())
            .await
            .expect("resubscribe timeout")
            .expect("resubscribe frame")
            .expect("resubscribe message");
        let ClientMessage::Text(capability) = capability else {
            panic!("capability must be a text frame");
        };
        let capability: Value = serde_json::from_str(capability.as_str()).expect("capability json");
        assert_eq!(capability["supported"], false);

        let _ = socket.close(None).await;
        let session_dir = state_dir.join("sessions/ws-prompt");
        let mut cleaned = false;
        for _ in 0..20 {
            cleaned = std::fs::read_dir(&session_dir)
                .expect("session dir")
                .flatten()
                .all(|entry| !entry.file_name().to_string_lossy().starts_with("attach-"));
            if cleaned {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert!(cleaned, "disconnect must remove the private attach pipe");
        server.abort();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn websocket_subscription_recovers_fresh_codex_identity_and_first_prompt() {
        let lock = GlobalStateLock::new();
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let state_dir = tmp.path().join("state");
        let codex_home = tmp.path().join("codex-home");
        let cwd = tmp.path().join("repo");
        std::fs::create_dir_all(&cwd).expect("cwd");
        let created_at = seed_fresh_provider_session(
            &state_dir,
            "ws-codex-pending",
            "codex",
            "hs-codex-pending",
            &cwd,
            None,
        );
        let _codex_home = EnvGuard::set(&lock, "CODEX_HOME", codex_home.to_str().unwrap());
        let tmux = minimal_tmux(tmp.path());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener");
        let addr = listener.local_addr().expect("address");
        let server_state_dir = state_dir.clone();
        let server_state = state(&server_state_dir, Some(TOKEN), tmux);
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, router(server_state)).await;
        });

        let mut request = format!("ws://{addr}/sessions/ws-codex-pending/attach")
            .into_client_request()
            .expect("request");
        request.headers_mut().insert(
            AUTHORIZATION,
            format!("Bearer {TOKEN}").parse().expect("authorization"),
        );
        let (mut socket, _) = connect_async(request).await.expect("connect");
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(2), socket.next())
                .await
                .expect("snapshot timeout")
                .expect("snapshot frame")
                .expect("snapshot message"),
            ClientMessage::Binary(_)
        ));
        socket
            .send(ClientMessage::Text(
                json!({"subscribe":[PROVIDER_PROMPT_CAPABILITY]})
                    .to_string()
                    .into(),
            ))
            .await
            .expect("subscribe before identity");

        tokio::time::sleep(Duration::from_millis(50)).await;
        let transcript = codex_home.join("sessions/2026/07/pending.jsonl");
        std::fs::create_dir_all(transcript.parent().expect("transcript parent"))
            .expect("transcript dir");
        std::fs::write(
            &transcript,
            format!(
                "{}\n{}\n",
                json!({
                    "timestamp":created_at,
                    "type":"session_meta",
                    "payload":{
                        "id":"fresh-codex-id",
                        "session_id":"fresh-codex-id",
                        "cwd":cwd.to_string_lossy(),
                        "source":"cli",
                        "timestamp":created_at
                    }
                }),
                json!({
                    "timestamp":created_at,
                    "type":"event_msg",
                    "payload":{"type":"user_message","message":"fresh codex first prompt"}
                })
            ),
        )
        .expect("fresh transcript");
        persist_provider_resume_identity(
            &state_dir,
            "ws-codex-pending",
            "codex",
            "fresh-codex-id",
            "codex-user-prompt-submit-hook",
        );

        assert_supported_provider_prompt(&mut socket, "codex").await;
        assert_provider_prompt(&mut socket, "codex", "fresh codex first prompt").await;
        assert!(
            tokio::time::timeout(Duration::from_millis(250), socket.next())
                .await
                .is_err(),
            "fresh first prompt must be emitted exactly once"
        );
        let _ = socket.close(None).await;

        let mut request = format!("ws://{addr}/sessions/ws-codex-pending/attach")
            .into_client_request()
            .expect("reconnect request");
        request.headers_mut().insert(
            AUTHORIZATION,
            format!("Bearer {TOKEN}").parse().expect("authorization"),
        );
        let (mut reconnect, _) = connect_async(request).await.expect("reconnect");
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(2), reconnect.next())
                .await
                .expect("reconnect snapshot timeout")
                .expect("reconnect snapshot frame")
                .expect("reconnect snapshot message"),
            ClientMessage::Binary(_)
        ));
        reconnect
            .send(ClientMessage::Text(
                json!({"subscribe":[PROVIDER_PROMPT_CAPABILITY]})
                    .to_string()
                    .into(),
            ))
            .await
            .expect("reconnect subscribe");
        assert_supported_provider_prompt(&mut reconnect, "codex").await;
        assert!(
            tokio::time::timeout(Duration::from_millis(250), reconnect.next())
                .await
                .is_err(),
            "reconnect must baseline at EOF instead of replaying the recovered prompt"
        );
        let _ = reconnect.close(None).await;
        server.abort();
        let _ = server.await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn websocket_pending_codex_rejects_unrelated_same_cwd_transcript() {
        let lock = GlobalStateLock::new();
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let state_dir = tmp.path().join("state");
        let codex_home = tmp.path().join("codex-home");
        let cwd = tmp.path().join("repo");
        std::fs::create_dir_all(&cwd).expect("cwd");
        let created_at = seed_fresh_provider_session(
            &state_dir,
            "ws-codex-unrelated",
            "codex",
            "hs-codex-unrelated",
            &cwd,
            None,
        );
        let _codex_home = EnvGuard::set(&lock, "CODEX_HOME", codex_home.to_str().unwrap());
        let tmux = minimal_tmux(tmp.path());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener");
        let addr = listener.local_addr().expect("address");
        let server_state_dir = state_dir.clone();
        let server_state = state(&server_state_dir, Some(TOKEN), tmux);
        let list_state = server_state.clone();
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, router(server_state)).await;
        });

        let mut request = format!("ws://{addr}/sessions/ws-codex-unrelated/attach")
            .into_client_request()
            .expect("request");
        request.headers_mut().insert(
            AUTHORIZATION,
            format!("Bearer {TOKEN}").parse().expect("authorization"),
        );
        let (mut socket, _) = connect_async(request).await.expect("connect");
        let _snapshot = tokio::time::timeout(Duration::from_secs(2), socket.next())
            .await
            .expect("snapshot timeout")
            .expect("snapshot frame")
            .expect("snapshot message");
        socket
            .send(ClientMessage::Text(
                json!({"subscribe":[PROVIDER_PROMPT_CAPABILITY]})
                    .to_string()
                    .into(),
            ))
            .await
            .expect("subscribe");

        let transcript = codex_home.join("sessions/2026/07/unrelated.jsonl");
        std::fs::create_dir_all(transcript.parent().expect("transcript parent"))
            .expect("transcript dir");
        std::fs::write(
            &transcript,
            format!(
                "{}\n{}\n",
                json!({
                    "timestamp":created_at,
                    "type":"session_meta",
                    "payload":{
                        "id":"unrelated-codex-id",
                        "session_id":"unrelated-codex-id",
                        "cwd":cwd.to_string_lossy(),
                        "source":"cli",
                        "timestamp":created_at
                    }
                }),
                json!({
                    "timestamp":created_at,
                    "type":"event_msg",
                    "payload":{"type":"user_message","message":"unrelated private prompt"}
                })
            ),
        )
        .expect("unrelated transcript");

        let (status, _) = call(router(list_state), get("/sessions")).await;
        assert_eq!(status, StatusCode::OK, "list must remain a read-only probe");

        if let Ok(Some(Ok(ClientMessage::Text(frame)))) =
            tokio::time::timeout(Duration::from_millis(500), socket.next()).await
        {
            let frame: Value = serde_json::from_str(frame.as_str()).expect("control json");
            assert_eq!(
                frame["supported"], false,
                "unrelated transcript must not resolve capability"
            );
        }
        let record: Value = serde_json::from_slice(
            &std::fs::read(state_dir.join("sessions/ws-codex-unrelated/session.json"))
                .expect("session record"),
        )
        .expect("session json");
        assert!(
            record.get("provider_resume").is_none(),
            "pending provider discovery must not persist same-cwd history"
        );
        let _ = socket.close(None).await;

        server.abort();
        let _ = server.await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn websocket_subscription_recovers_fresh_claude_transcript_and_first_prompt() {
        let lock = GlobalStateLock::new();
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let state_dir = tmp.path().join("state");
        let claude_config = tmp.path().join("claude-config");
        let cwd = tmp.path().join("repo");
        let provider_session_id = "fresh-claude-id";
        std::fs::create_dir_all(&cwd).expect("cwd");
        let created_at = seed_fresh_provider_session(
            &state_dir,
            "ws-claude-pending",
            "claude",
            "hs-claude-pending",
            &cwd,
            Some((provider_session_id, "claude-explicit-session-id")),
        );
        let _claude_config =
            EnvGuard::set(&lock, "CLAUDE_CONFIG_DIR", claude_config.to_str().unwrap());
        let tmux = minimal_tmux(tmp.path());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener");
        let addr = listener.local_addr().expect("address");
        let server_state_dir = state_dir.clone();
        let server = tokio::spawn(async move {
            let _ = axum::serve(
                listener,
                router(state(&server_state_dir, Some(TOKEN), tmux)),
            )
            .await;
        });

        let mut request = format!("ws://{addr}/sessions/ws-claude-pending/attach")
            .into_client_request()
            .expect("request");
        request.headers_mut().insert(
            AUTHORIZATION,
            format!("Bearer {TOKEN}").parse().expect("authorization"),
        );
        let (mut socket, _) = connect_async(request).await.expect("connect");
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(2), socket.next())
                .await
                .expect("snapshot timeout")
                .expect("snapshot frame")
                .expect("snapshot message"),
            ClientMessage::Binary(_)
        ));
        socket
            .send(ClientMessage::Text(
                json!({"subscribe":[PROVIDER_PROMPT_CAPABILITY]})
                    .to_string()
                    .into(),
            ))
            .await
            .expect("subscribe before transcript");

        tokio::time::sleep(Duration::from_millis(50)).await;
        let transcript = claude_config
            .join("projects/-pending")
            .join(format!("{provider_session_id}.jsonl"));
        std::fs::create_dir_all(transcript.parent().expect("transcript parent"))
            .expect("transcript dir");
        std::fs::write(
            &transcript,
            format!(
                "{}\n{}\n",
                json!({
                    "type":"user",
                    "uuid":"claude-turn-1",
                    "sessionId":provider_session_id,
                    "cwd":cwd.to_string_lossy(),
                    "timestamp":created_at,
                    "message":{"role":"user","content":"fresh claude first prompt"}
                }),
                json!({
                    "type":"last-prompt",
                    "sessionId":provider_session_id,
                    "leafUuid":"claude-turn-1",
                    "timestamp":created_at,
                    "lastPrompt":"fresh claude first prompt"
                })
            ),
        )
        .expect("fresh transcript");

        assert_supported_provider_prompt(&mut socket, "claude").await;
        assert_provider_prompt(&mut socket, "claude", "fresh claude first prompt").await;
        assert!(
            tokio::time::timeout(Duration::from_millis(250), socket.next())
                .await
                .is_err(),
            "fresh first prompt must be emitted exactly once"
        );
        let _ = socket.close(None).await;

        let mut request = format!("ws://{addr}/sessions/ws-claude-pending/attach")
            .into_client_request()
            .expect("reconnect request");
        request.headers_mut().insert(
            AUTHORIZATION,
            format!("Bearer {TOKEN}").parse().expect("authorization"),
        );
        let (mut reconnect, _) = connect_async(request).await.expect("reconnect");
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(2), reconnect.next())
                .await
                .expect("reconnect snapshot timeout")
                .expect("reconnect snapshot frame")
                .expect("reconnect snapshot message"),
            ClientMessage::Binary(_)
        ));
        reconnect
            .send(ClientMessage::Text(
                json!({"subscribe":[PROVIDER_PROMPT_CAPABILITY]})
                    .to_string()
                    .into(),
            ))
            .await
            .expect("reconnect subscribe");
        assert_supported_provider_prompt(&mut reconnect, "claude").await;
        assert!(
            tokio::time::timeout(Duration::from_millis(250), reconnect.next())
                .await
                .is_err(),
            "Claude reconnect must baseline the cached source at EOF"
        );
        let _ = reconnect.close(None).await;
        server.abort();
        let _ = server.await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn websocket_unsubscribed_client_gets_binary_only_and_unsupported_ack() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let state_dir = tmp.path().join("state");
        seed_session(&state_dir, "ws-unsupported", "hermes", "hs-hermes-ws");
        let tmux = minimal_tmux(tmp.path());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener");
        let addr = listener.local_addr().expect("address");
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, router(state(&state_dir, Some(TOKEN), tmux))).await;
        });

        let mut request = format!("ws://{addr}/sessions/ws-unsupported/attach")
            .into_client_request()
            .expect("request");
        request.headers_mut().insert(
            AUTHORIZATION,
            format!("Bearer {TOKEN}").parse().expect("authorization"),
        );
        let (mut socket, _) = connect_async(request).await.expect("connect");
        let snapshot = tokio::time::timeout(Duration::from_secs(2), socket.next())
            .await
            .expect("snapshot timeout")
            .expect("snapshot frame")
            .expect("snapshot message");
        assert!(matches!(snapshot, ClientMessage::Binary(_)));
        assert!(
            tokio::time::timeout(Duration::from_millis(200), socket.next())
                .await
                .is_err(),
            "unsubscribed clients must not receive text events"
        );

        socket
            .send(ClientMessage::Text(
                json!({"subscribe":[PROVIDER_PROMPT_CAPABILITY]})
                    .to_string()
                    .into(),
            ))
            .await
            .expect("subscribe");
        let capability = tokio::time::timeout(Duration::from_secs(2), socket.next())
            .await
            .expect("capability timeout")
            .expect("capability frame")
            .expect("capability message");
        let ClientMessage::Text(capability) = capability else {
            panic!("capability must be a text frame");
        };
        let capability: Value = serde_json::from_str(capability.as_str()).expect("capability json");
        assert_eq!(capability["supported"], false);
        let _ = socket.close(None).await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        server.abort();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn websocket_subscription_rejects_heuristic_same_cwd_codex_backfill() {
        let lock = GlobalStateLock::new();
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let state_dir = tmp.path().join("state");
        let codex_home = tmp.path().join("codex-home");
        let transcript = codex_home.join("sessions/2000/01/session.jsonl");
        std::fs::create_dir_all(transcript.parent().expect("parent")).expect("dirs");
        std::fs::write(
            &transcript,
            format!(
                "{}\n",
                json!({
                    "timestamp":"2000-01-01T00:00:00Z",
                    "type":"session_meta",
                    "payload":{
                        "id":"backfill-resume-id",
                        "cwd":"/tmp",
                        "source":"cli",
                        "timestamp":"2000-01-01T00:00:00Z"
                    }
                })
            ),
        )
        .expect("transcript");
        let _codex_home = EnvGuard::set(&lock, "CODEX_HOME", codex_home.to_str().unwrap());
        seed_session(&state_dir, "ws-backfill", "codex", "hs-codex-backfill");
        let record_path = state_dir.join("sessions/ws-backfill/session.json");
        let tmux = minimal_tmux(tmp.path());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener");
        let addr = listener.local_addr().expect("address");
        let server_state_dir = state_dir.clone();
        let server = tokio::spawn(async move {
            let _ = axum::serve(
                listener,
                router(state(&server_state_dir, Some(TOKEN), tmux)),
            )
            .await;
        });

        let mut request = format!("ws://{addr}/sessions/ws-backfill/attach")
            .into_client_request()
            .expect("request");
        request.headers_mut().insert(
            AUTHORIZATION,
            format!("Bearer {TOKEN}").parse().expect("authorization"),
        );
        let (mut socket, _) = connect_async(request).await.expect("connect");
        let _snapshot = tokio::time::timeout(Duration::from_secs(2), socket.next())
            .await
            .expect("snapshot timeout")
            .expect("snapshot frame")
            .expect("snapshot message");
        let before: Value = serde_json::from_slice(
            &std::fs::read(&record_path).expect("record before subscription"),
        )
        .expect("record json");
        assert!(before.get("provider_resume").is_none());

        socket
            .send(ClientMessage::Text(
                json!({"subscribe":[PROVIDER_PROMPT_CAPABILITY]})
                    .to_string()
                    .into(),
            ))
            .await
            .expect("subscribe");
        let capability = tokio::time::timeout(Duration::from_secs(2), socket.next())
            .await
            .expect("capability timeout")
            .expect("capability frame")
            .expect("capability message");
        let ClientMessage::Text(capability) = capability else {
            panic!("capability must be a text frame");
        };
        let capability: Value = serde_json::from_str(capability.as_str()).expect("capability json");
        assert_eq!(capability["supported"], false);
        let after: Value = serde_json::from_slice(
            &std::fs::read(&record_path).expect("record after subscription"),
        )
        .expect("record json");
        assert!(after.get("provider_resume").is_none());

        OpenOptions::new()
            .append(true)
            .open(&transcript)
            .expect("open unrelated transcript")
            .write_all(
                format!(
                    "{}\n",
                    json!({
                        "timestamp":"2099-01-01T00:00:00Z",
                        "type":"event_msg",
                        "payload":{
                            "type":"user_message",
                            "message":"unrelated same-cwd prompt"
                        }
                    })
                )
                .as_bytes(),
            )
            .expect("append unrelated prompt");
        assert!(
            tokio::time::timeout(Duration::from_millis(250), socket.next())
                .await
                .is_err(),
            "an unrelated same-cwd transcript must not emit prompt events"
        );
        let _ = socket.close(None).await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        server.abort();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn provider_prompt_loop_drops_events_when_control_queue_is_saturated() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let transcript = tmp.path().join("codex.jsonl");
        std::fs::write(&transcript, "").expect("baseline");
        let tail = ProviderPromptTail::open_path(
            ProviderKind::Codex,
            "codex-id",
            transcript.clone(),
            Duration::ZERO,
        )
        .expect("tail");
        let (control_tx, mut control_rx) = mpsc::channel(1);
        control_tx
            .send(Message::Text("capability".into()))
            .await
            .expect("fill queue");
        let task = tokio::spawn(provider_prompt_loop(tail, control_tx));
        OpenOptions::new()
            .append(true)
            .open(&transcript)
            .expect("open transcript")
            .write_all(
                format!(
                    "{}\n",
                    json!({
                        "timestamp":"2099-01-01T00:00:00Z",
                        "type":"event_msg",
                        "payload":{"type":"user_message","message":"dropped prompt"}
                    })
                )
                .as_bytes(),
            )
            .expect("append prompt");
        tokio::time::sleep(Duration::from_millis(250)).await;
        assert_eq!(
            control_rx.recv().await,
            Some(Message::Text("capability".into()))
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(150), control_rx.recv())
                .await
                .is_err(),
            "advisory event should be dropped while the queue is full"
        );
        task.abort();
    }

    #[tokio::test]
    async fn provider_prompt_loop_stops_when_control_receiver_closes() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let transcript = tmp.path().join("codex.jsonl");
        std::fs::write(&transcript, "").expect("baseline");
        let tail = ProviderPromptTail::open_path(
            ProviderKind::Codex,
            "codex-id",
            transcript,
            Duration::ZERO,
        )
        .expect("tail");
        let (control_tx, control_rx) = mpsc::channel(1);
        let task = tokio::spawn(provider_prompt_loop(tail, control_tx));
        drop(control_rx);

        tokio::time::timeout(Duration::from_millis(250), task)
            .await
            .expect("provider watcher must stop when its writer queue closes")
            .expect("provider watcher task");
    }

    fn state(state_dir: &Path, token: Option<&str>, tmux_bin: PathBuf) -> Arc<ServeState> {
        state_with_activity_broker(
            state_dir,
            token,
            tmux_bin,
            ActivityBroker::for_test(MACHINE),
        )
    }

    fn state_with_activity_broker(
        state_dir: &Path,
        token: Option<&str>,
        tmux_bin: PathBuf,
        activity_broker: Arc<ActivityBroker>,
    ) -> Arc<ServeState> {
        state_with_dependencies(
            state_dir,
            token,
            tmux_bin,
            activity_broker,
            default_session_collector(),
        )
    }

    fn state_with_session_collector(
        state_dir: &Path,
        token: Option<&str>,
        tmux_bin: PathBuf,
        session_collector: SessionCollector,
    ) -> Arc<ServeState> {
        state_with_dependencies(
            state_dir,
            token,
            tmux_bin,
            ActivityBroker::for_test(MACHINE),
            session_collector,
        )
    }

    fn state_with_dependencies(
        state_dir: &Path,
        token: Option<&str>,
        tmux_bin: PathBuf,
        activity_broker: Arc<ActivityBroker>,
        session_collector: SessionCollector,
    ) -> Arc<ServeState> {
        Arc::new(ServeState {
            context: CliContext {
                state_dir: state_dir.to_path_buf(),
                host: None,
            },
            machine: MACHINE.to_string(),
            token: token.map(str::to_string),
            tmux_bin,
            attach_brokers: AttachBrokerRegistry::default(),
            provider_prompt_discovery: Arc::new(ProviderPromptDiscoveryRegistry::default()),
            activity_broker,
            codex_controls: Arc::new(StdMutex::new(HashMap::new())),
            codex_account_switches: CodexAccountSwitchRegistry::default(),
            session_collector,
        })
    }

    fn minimal_tmux(dir: &Path) -> PathBuf {
        let bin = dir.join("tmux");
        std::fs::write(
            &bin,
            "#!/usr/bin/env sh\ncase \"$1\" in\n  has-session) exit 0 ;;\n  capture-pane) printf 'pane\\n'; exit 0 ;;\n  show-buffer) printf 'buffered selection\\n'; exit 0 ;;\n  *) exit 0 ;;\nesac\n",
        )
        .unwrap();
        let mut perms = std::fs::metadata(&bin).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&bin, perms).unwrap();
        bin
    }

    fn seed_session(state_dir: &Path, id: &str, agent: &str, tmux_session: &str) {
        let dir = state_dir.join("sessions").join(id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("session.json"),
            format!(
                r#"{{"schema_version":"agent-session.session.v1","id":"{id}","agent":"{agent}","mode":"interactive","title":null,"cwd":"/tmp","tmux_session":"{tmux_session}","prompt_file":null,"log_file":null,"created_at":"2000-01-01T00:00:00Z","updated_at":"2000-01-01T00:00:00Z"}}"#
            ),
        )
        .unwrap();
    }

    fn seed_session_with_runtime(state_dir: &Path, id: &str, agent: &str, tmux_session: &str) {
        seed_session(state_dir, id, agent, tmux_session);
        let record_path = state_dir.join("sessions").join(id).join("session.json");
        let mut record: Value =
            serde_json::from_str(&std::fs::read_to_string(&record_path).unwrap()).unwrap();
        record["runtime"] = json!({
            "kind": "tmux",
            "tmux_session": tmux_session,
            "generation": 1,
            "started_at": "2000-01-01T00:00:00Z",
            "launch_id": format!("launch-{id}")
        });
        std::fs::write(record_path, serde_json::to_vec_pretty(&record).unwrap()).unwrap();
    }

    fn seed_codex_app_server_session(state_dir: &Path, id: &str) -> String {
        seed_session_with_runtime(state_dir, id, "codex", &format!("hs-codex-{id}"));
        let record_path = state_dir.join("sessions").join(id).join("session.json");
        let mut record: Value =
            serde_json::from_str(&std::fs::read_to_string(&record_path).unwrap()).unwrap();
        let launch_id = format!("launch-{id}");
        record["runtime"]["kind"] = json!(codex_app_server::RUNTIME_KIND);
        record["runtime"][codex_app_server::PROTOCOL_KEY] =
            json!(codex_app_server::PROTOCOL_VERSION);
        for (key, suffix) in [
            (codex_app_server::SOCKET_KEY, "sock"),
            (codex_app_server::PROXY_KEY, "proxy"),
            (codex_app_server::THREAD_HANDOFF_KEY, "thread"),
            (codex_app_server::THREAD_ATTACHED_KEY, "attached"),
        ] {
            record["runtime"][key] = json!(state_dir.join(format!("{id}.{suffix}")));
        }
        std::fs::write(record_path, serde_json::to_vec_pretty(&record).unwrap()).unwrap();
        launch_id
    }

    fn seed_resumable_session(
        state_dir: &Path,
        id: &str,
        agent: &str,
        tmux_session: &str,
        cwd: &Path,
        resume_args: &[&str],
    ) {
        let dir = state_dir.join("sessions").join(id);
        std::fs::create_dir_all(&dir).unwrap();
        let resume_args = serde_json::to_string(resume_args).unwrap();
        std::fs::write(
            dir.join("session.json"),
            format!(
                r#"{{
  "schema_version": "agent-session.session.v1",
  "id": "{id}",
  "agent": "{agent}",
  "mode": "interactive",
  "title": null,
  "cwd": "{}",
  "tmux_session": "{tmux_session}",
  "prompt_file": null,
  "log_file": null,
  "created_at": "2000-01-01T00:00:00Z",
  "updated_at": "2000-01-01T00:00:00Z",
  "provider_resume": {{
    "provider": "{agent}",
    "session_id": "resume-session-id",
    "captured_at": "2000-01-01T00:00:00Z",
    "capture_method": "fixture",
    "resume_args": {resume_args}
  }},
  "runtime": {{
    "kind": "tmux",
    "tmux_session": "{tmux_session}",
    "generation": 1,
    "started_at": "2000-01-01T00:00:00Z"
  }},
  "agent_args": []
}}"#,
                cwd.to_string_lossy()
            ),
        )
        .unwrap();
    }

    fn seed_fresh_provider_session(
        state_dir: &Path,
        id: &str,
        agent: &str,
        tmux_session: &str,
        cwd: &Path,
        provider_resume: Option<(&str, &str)>,
    ) -> String {
        let created_at = jiff::Timestamp::now().to_string();
        let dir = state_dir.join("sessions").join(id);
        std::fs::create_dir_all(&dir).expect("session dir");
        let mut record = json!({
            "schema_version":"agent-session.session.v1",
            "id":id,
            "agent":agent,
            "mode":"interactive",
            "title":null,
            "cwd":cwd.to_string_lossy(),
            "tmux_session":tmux_session,
            "prompt_file":null,
            "log_file":null,
            "created_at":created_at,
            "updated_at":created_at,
            "runtime":{
                "kind":"tmux",
                "tmux_session":tmux_session,
                "generation":1,
                "started_at":created_at,
                "launch_id":format!("launch-{id}")
            },
            "agent_args":[]
        });
        if let Some((session_id, capture_method)) = provider_resume {
            record["provider_resume"] = json!({
                "provider":agent,
                "session_id":session_id,
                "captured_at":created_at,
                "capture_method":capture_method,
                "resume_args":[]
            });
        }
        std::fs::write(
            dir.join("session.json"),
            serde_json::to_vec_pretty(&record).expect("session json"),
        )
        .expect("session record");
        created_at
    }

    fn persist_provider_resume_identity(
        state_dir: &Path,
        id: &str,
        provider: &str,
        session_id: &str,
        capture_method: &str,
    ) {
        let path = state_dir.join("sessions").join(id).join("session.json");
        let mut record: Value =
            serde_json::from_slice(&std::fs::read(&path).expect("session record"))
                .expect("session json");
        record["provider_resume"] = json!({
            "provider":provider,
            "session_id":session_id,
            "captured_at":jiff::Timestamp::now().to_string(),
            "capture_method":capture_method,
            "resume_args":[]
        });
        std::fs::write(
            path,
            serde_json::to_vec_pretty(&record).expect("session json"),
        )
        .expect("persist provider identity");
    }

    async fn assert_supported_provider_prompt(
        socket: &mut tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        provider: &str,
    ) {
        let frame = tokio::time::timeout(Duration::from_secs(3), socket.next())
            .await
            .expect("capability timeout")
            .expect("capability frame")
            .expect("capability message");
        let ClientMessage::Text(frame) = frame else {
            panic!("capability must be a text frame");
        };
        let frame: Value = serde_json::from_str(frame.as_str()).expect("capability json");
        assert_eq!(frame["type"], "capability");
        assert_eq!(frame["capability"], PROVIDER_PROMPT_CAPABILITY);
        assert_eq!(frame["supported"], true);
        assert_eq!(frame["provider"], provider);
    }

    async fn assert_provider_prompt(
        socket: &mut tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        provider: &str,
        prompt: &str,
    ) {
        let frame = tokio::time::timeout(Duration::from_secs(3), socket.next())
            .await
            .expect("prompt timeout")
            .expect("prompt frame")
            .expect("prompt message");
        let ClientMessage::Text(frame) = frame else {
            panic!("prompt must be a text frame");
        };
        let frame: Value = serde_json::from_str(frame.as_str()).expect("prompt json");
        assert_eq!(frame["type"], "prompt_submitted");
        assert_eq!(frame["provider"], provider);
        assert_eq!(frame["text"], prompt);
        assert!(frame["event_id"].is_string());
    }

    fn add_provider_resume_extra(state_dir: &Path, id: &str) {
        let path = state_dir.join("sessions").join(id).join("session.json");
        let mut record: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        record["provider_resume"]["storage_only"] = json!({ "keep": true });
        std::fs::write(&path, serde_json::to_string(&record).unwrap()).unwrap();
    }

    fn executable(path: &Path, body: &str) -> PathBuf {
        std::fs::write(path, body).unwrap();
        let mut perms = std::fs::metadata(path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms).unwrap();
        path.to_path_buf()
    }

    fn resume_tmux(dir: &Path, log: &Path) -> PathBuf {
        executable(
            &dir.join("tmux"),
            &format!(
                "#!/usr/bin/env sh\nprintf '%s\\n' \"$*\" >> {}\ncase \"$1\" in\n  has-session) exit 1 ;;\n  *) exit 0 ;;\nesac\n",
                shell_words::quote(&log.to_string_lossy())
            ),
        )
    }

    fn fake_agent(dir: &Path, name: &str) -> PathBuf {
        executable(
            &dir.join(name),
            "#!/usr/bin/env sh\nprintf 'fake agent started\\n'\n",
        )
    }

    fn init_git_remote(repo: &Path, remote: &str) {
        std::fs::create_dir_all(repo).unwrap();
        let init = ProcessCommand::new("git")
            .arg("-C")
            .arg(repo)
            .arg("init")
            .output()
            .unwrap();
        assert!(
            init.status.success(),
            "git init failed: {}",
            String::from_utf8_lossy(&init.stderr)
        );
        let add_remote = ProcessCommand::new("git")
            .arg("-C")
            .arg(repo)
            .args(["remote", "add", "origin", remote])
            .output()
            .unwrap();
        assert!(
            add_remote.status.success(),
            "git remote add failed: {}",
            String::from_utf8_lossy(&add_remote.stderr)
        );
    }

    #[test]
    fn normalize_claude_usage_preserves_reset_aliases() {
        let provider = normalize_claude_usage(UsageHelperOutput {
            status_code: Some(0),
            stdout: br#"{
  "schema_version": "claude-cli.usage.v1",
  "command": "usage",
  "ok": true,
  "result": {
    "source": "cli",
    "stale": false,
    "updated_at": 1783666800,
    "windows": [
      { "label": "5h", "used_percent": 3, "remaining_percent": 97, "resets_at": "2030-01-01T00:00:00Z" },
      { "label": "Weekly", "used_percent": 0, "remaining_percent": 100, "resetsAtEpoch": 1805000000 },
      { "label": "Month", "used_percent": 9, "remaining_percent": 91, "resetsAt": "2030-01-01T00:00:00.950339+00:00" },
      { "label": "Year", "used_percent": 1, "remaining_percent": 99, "resets_at_epoch": "1893456000000" },
      { "label": "Human", "used_percent": 4, "remaining_percent": 96, "resets_at": "Jul 12, 9pm (Asia/Taipei)" },
      { "label": "Invalid", "used_percent": 2, "remaining_percent": 98, "resets_at": "/Users/terry/.claude/token" }
    ]
  }
}"#
            .to_vec(),
            stderr: Vec::new(),
            timed_out: false,
        });

        assert!(provider.fresh_authoritative);
        let value = serde_json::to_value(provider).unwrap();
        assert_eq!(value["ok"], true);
        assert_eq!(value["windows"][0]["reset_at"], "2030-01-01T00:00:00Z");
        assert_eq!(value["windows"][0]["reset_at_epoch"], 1_893_456_000);
        assert_eq!(value["windows"][1]["reset_at_epoch"], 1_805_000_000);
        assert!(value["windows"][1].get("reset_at").is_none());
        assert_eq!(
            value["windows"][2]["reset_at"],
            "2030-01-01T00:00:00.950339+00:00"
        );
        assert_eq!(value["windows"][2]["reset_at_epoch"], 1_893_456_000);
        assert_eq!(value["windows"][3]["reset_at_epoch"], 1_893_456_000);
        assert_eq!(value["windows"][4]["reset_at"], "Jul 12, 9pm (Asia/Taipei)");
        assert_eq!(value["windows"][4]["reset_at_epoch"], 1_783_861_200);
        assert!(value["windows"][5].get("reset_at").is_none());
        assert!(value["windows"][5].get("reset_at_epoch").is_none());
    }

    #[test]
    fn normalize_claude_usage_preserves_provider_reason_code() {
        let provider = normalize_claude_usage(UsageHelperOutput {
            status_code: Some(0),
            stdout: br#"{
  "schema_version": "claude-cli.usage.v1",
  "command": "usage",
  "ok": true,
  "result": {
    "source": "none",
    "stale": true,
    "windows": [],
    "reason_code": "organization_disabled"
  }
}"#
            .to_vec(),
            stderr: Vec::new(),
            timed_out: false,
        });

        let value = serde_json::to_value(provider).unwrap();
        assert_eq!(value["ok"], false);
        assert_eq!(value["reason_code"], "organization_disabled");
    }

    #[test]
    fn normalize_claude_usage_preserves_reason_with_cached_windows() {
        let provider = normalize_claude_usage(UsageHelperOutput {
            status_code: Some(0),
            stdout: br#"{
  "schema_version": "claude-cli.usage.v1",
  "command": "usage",
  "ok": true,
  "result": {
    "source": "cache",
    "stale": true,
    "reason_code": "organization_disabled",
    "windows": [{"label": "5h", "used_percent": 20, "remaining_percent": 80}]
  }
}"#
            .to_vec(),
            stderr: Vec::new(),
            timed_out: false,
        });

        let value = serde_json::to_value(provider).unwrap();
        assert_eq!(value["ok"], true);
        assert_eq!(value["windows"][0]["used_percent"], 20);
        assert_eq!(value["reason_code"], "organization_disabled");
    }

    #[test]
    fn normalize_claude_usage_never_authorizes_stale_cached_windows() {
        let provider = normalize_claude_usage(UsageHelperOutput {
            status_code: Some(0),
            stdout: br#"{
  "schema_version": "claude-cli.usage.v1",
  "command": "usage",
  "ok": false,
  "result": {
    "source": "cache",
    "stale": true,
    "windows": [{"label": "5h", "used_percent": 20, "remaining_percent": 80}]
  }
}"#
            .to_vec(),
            stderr: Vec::new(),
            timed_out: false,
        });

        assert!(!provider.fresh_authoritative);
        assert_eq!(provider.windows.len(), 1);
    }

    #[test]
    fn normalize_codex_usage_preserves_failure_reason_with_healthy_windows() {
        let provider = normalize_codex_usage(UsageHelperOutput {
            status_code: Some(1),
            stdout: br#"{
  "schema_version": "codex-cli.diag.rate-limits.v1",
  "command": "diag rate-limits",
  "ok": false,
  "results": [
    {
      "provider": "codex",
      "name": "healthy",
      "status": "ok",
      "ok": true,
      "summary": {"non_weekly_remaining": 75}
    },
    {
      "provider": "codex",
      "name": "billing",
      "status": "error",
      "ok": false,
      "reason_code": "billing_past_due"
    }
  ]
}"#
            .to_vec(),
            stderr: Vec::new(),
            timed_out: false,
        });

        let value = serde_json::to_value(provider).unwrap();
        assert_eq!(value["ok"], true);
        assert_eq!(value["windows"][0]["remaining_percent"], 75);
        assert_eq!(value["reason_code"], "billing_past_due");
    }

    #[test]
    fn normalize_codex_usage_preserves_provider_reason_code() {
        let provider = normalize_codex_usage(UsageHelperOutput {
            status_code: Some(1),
            stdout: br#"{
  "schema_version": "codex-cli.diag.rate-limits.v1",
  "command": "diag rate-limits",
  "ok": false,
  "results": [{
    "provider": "codex",
    "name": "alpha",
    "status": "error",
    "ok": false,
    "reason_code": "auth_expired",
    "error": {"code": "request-failed", "message": "request failed"}
  }]
}"#
            .to_vec(),
            stderr: Vec::new(),
            timed_out: false,
        });

        let value = serde_json::to_value(provider).unwrap();
        assert_eq!(value["ok"], false);
        assert_eq!(value["reason_code"], "auth_expired");
    }

    #[test]
    fn normalize_codex_usage_prefers_actionable_reason_after_unknown_result() {
        let provider = normalize_codex_usage(UsageHelperOutput {
            status_code: Some(1),
            stdout: br#"{
  "schema_version": "codex-cli.diag.rate-limits.v1",
  "command": "diag rate-limits",
  "ok": false,
  "results": [
    {
      "provider": "codex",
      "name": "alpha",
      "status": "error",
      "ok": false,
      "reason_code": "unknown"
    },
    {
      "provider": "codex",
      "name": "beta",
      "status": "error",
      "ok": false,
      "reason_code": "billing_past_due"
    }
  ]
}"#
            .to_vec(),
            stderr: Vec::new(),
            timed_out: false,
        });

        let value = serde_json::to_value(provider).unwrap();
        assert_eq!(value["reason_code"], "billing_past_due");
    }

    #[test]
    fn default_usage_timeout_covers_slow_claude_usage_helpers() {
        assert!(
            Duration::from_millis(DEFAULT_USAGE_TIMEOUT_MS) >= Duration::from_secs(30),
            "m4 claude-cli usage can take more than 20s before returning reset-bearing windows"
        );
    }

    #[test]
    fn claude_inner_timeout_leaves_cleanup_slack_and_clamps_overrides() {
        let outer = Duration::from_secs(45);
        assert_eq!(claude_inner_timeout_seconds(outer, None), 40);
        assert_eq!(
            claude_inner_timeout_seconds(Duration::from_millis(45_001), None),
            40
        );
        assert_eq!(
            claude_inner_timeout_seconds(Duration::from_millis(1_001), None),
            1
        );
        assert_eq!(claude_inner_timeout_seconds(outer, Some("9")), 9);
        assert_eq!(claude_inner_timeout_seconds(outer, Some("90")), 40);
        assert_eq!(claude_inner_timeout_seconds(outer, Some("invalid")), 40);
    }

    #[test]
    fn usage_helper_timeout_kills_descendant_process_group() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let helper = tmp.path().join("hanging-helper");
        let pid_file = tmp.path().join("descendant.pid");
        fs::write(
            &helper,
            "#!/usr/bin/env sh\nsleep 60 &\nprintf '%s' \"$!\" > \"$PID_FILE\"\nwait\n",
        )
        .expect("write helper");
        fs::set_permissions(&helper, fs::Permissions::from_mode(0o755)).expect("chmod helper");

        let output = run_usage_helper(
            helper.to_str().expect("helper path"),
            &[],
            &[("PID_FILE", pid_file.to_str().expect("pid path"))],
            Duration::from_millis(250),
        )
        .expect("run helper");
        assert!(output.timed_out);

        let pid = fs::read_to_string(&pid_file)
            .expect("descendant pid")
            .parse::<libc::pid_t>()
            .expect("numeric pid");
        let deadline = Instant::now() + Duration::from_secs(2);
        while unsafe { libc::kill(pid, 0) } == 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(25));
        }
        assert_eq!(
            unsafe { libc::kill(pid, 0) },
            -1,
            "timed-out helper descendant must not survive"
        );
        assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::ESRCH));
    }

    fn write_codex_session_meta(codex_home: &Path, session_id: &str, cwd: &Path) {
        write_codex_session_meta_file(codex_home, "session.jsonl", session_id, cwd);
    }

    fn write_codex_session_meta_file(
        codex_home: &Path,
        filename: &str,
        session_id: &str,
        cwd: &Path,
    ) {
        let path = codex_home.join("sessions/2026/07/05").join(filename);
        std::fs::create_dir_all(path.parent().expect("parent")).unwrap();
        std::fs::write(
            path,
            format!(
                r#"{{"timestamp":"2099-01-01T00:00:00Z","type":"session_meta","payload":{{"id":"{session_id}","session_id":"{session_id}","cwd":"{}","source":"cli","timestamp":"2099-01-01T00:00:00Z"}}}}
"#,
                cwd.to_string_lossy()
            ),
        )
        .unwrap();
    }

    fn write_claude_session_transcript(
        claude_config: &Path,
        project_slug: &str,
        session_id: &str,
        cwd: &Path,
    ) {
        let path = claude_config
            .join("projects")
            .join(project_slug)
            .join(format!("{session_id}.jsonl"));
        std::fs::create_dir_all(path.parent().expect("parent")).unwrap();
        std::fs::write(
            path,
            format!(
                r#"{{"type":"mode","mode":"normal","sessionId":"{session_id}"}}
{{"type":"user","isSidechain":false,"cwd":"{}","sessionId":"{session_id}"}}
"#,
                cwd.to_string_lossy()
            ),
        )
        .unwrap();
    }

    async fn call(app: Router, req: Request<Body>) -> (StatusCode, Value) {
        let resp = app.oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap()
        };
        (status, value)
    }

    fn get(uri: &str) -> Request<Body> {
        Request::builder()
            .method("GET")
            .uri(uri)
            .body(Body::empty())
            .unwrap()
    }

    fn get_auth(uri: &str, token: Option<&str>) -> Request<Body> {
        let mut builder = Request::builder().method("GET").uri(uri);
        if let Some(token) = token {
            builder = builder.header("authorization", format!("Bearer {token}"));
        }
        builder.body(Body::empty()).unwrap()
    }

    fn get_activity_events(last_event_id: Option<&str>) -> Request<Body> {
        let mut builder = Request::builder()
            .method("GET")
            .uri("/activity/events")
            .header("authorization", format!("Bearer {TOKEN}"));
        if let Some(last_event_id) = last_event_id {
            builder = builder.header("last-event-id", last_event_id);
        }
        builder.body(Body::empty()).unwrap()
    }

    async fn first_activity_sse_frame(
        state: Arc<ServeState>,
        last_event_id: Option<&str>,
    ) -> String {
        let response = router(state)
            .oneshot(get_activity_events(last_event_id))
            .await
            .expect("activity stream response");
        assert_eq!(response.status(), StatusCode::OK);
        let mut body = response.into_body().into_data_stream();
        let mut bytes = Vec::new();
        for _ in 0..8 {
            let chunk = tokio::time::timeout(Duration::from_millis(250), body.next())
                .await
                .expect("bounded SSE response frame")
                .expect("SSE response data")
                .expect("SSE response chunk");
            bytes.extend_from_slice(&chunk);
            assert!(bytes.len() <= 64 * 1024, "SSE frame exceeded test bound");
            if bytes.windows(2).any(|window| window == b"\n\n") {
                break;
            }
        }
        let frame = String::from_utf8(bytes).expect("UTF-8 SSE frame");
        assert!(frame.ends_with("\n\n"), "incomplete SSE frame: {frame:?}");
        frame
    }

    fn parse_activity_sse_frame(frame: &str) -> (String, String, Value) {
        let id = frame
            .lines()
            .find_map(|line| line.strip_prefix("id: "))
            .expect("SSE id")
            .to_string();
        let event = frame
            .lines()
            .find_map(|line| line.strip_prefix("event: "))
            .expect("SSE event")
            .to_string();
        let data = frame
            .lines()
            .find_map(|line| line.strip_prefix("data: "))
            .expect("SSE data");
        (
            id,
            event,
            serde_json::from_str(data).expect("SSE JSON data"),
        )
    }

    fn assert_degraded_reset_survives_consumer_dedupe(
        previous: &ActivityStreamEvent,
        reset: &ActivityStreamEvent,
    ) {
        let mut consumer_seen = std::collections::HashSet::new();
        assert!(consumer_seen.insert((
            previous.machine.clone(),
            previous.stream_id.clone(),
            previous.sequence,
        )));
        assert!(
            consumer_seen.insert((
                reset.machine.clone(),
                reset.stream_id.clone(),
                reset.sequence,
            )),
            "consumer dedupe discarded the degraded reset"
        );
        assert_eq!(reset.kind, "reset");
        assert_eq!(reset.stream_id, previous.stream_id);
        assert!(reset.sequence > previous.sequence);
    }

    fn test_stream_turn_state(
        revision: u64,
        phase_changed_at: impl Into<String>,
    ) -> crate::activity::StreamTurnState {
        let state: crate::activity::TurnState = serde_json::from_value(json!({
            "schema_version":"agent-session.turn-state.v1",
            "phase":"working",
            "phase_changed_at":phase_changed_at.into(),
            "revision":revision,
            "source":{
                "kind":"provider_hook",
                "provider":"codex",
                "confidence":"authoritative"
            }
        }))
        .expect("test turn state");
        crate::activity::stream_projection(&state)
    }

    fn auth_headers(token: Option<&str>) -> HeaderMap {
        let mut headers = HeaderMap::new();
        if let Some(token) = token {
            headers.insert(AUTHORIZATION, format!("Bearer {token}").parse().unwrap());
        }
        headers
    }

    fn post_json(uri: &str, token: Option<&str>, body: Value) -> Request<Body> {
        let mut builder = Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json");
        if let Some(token) = token {
            builder = builder.header("authorization", format!("Bearer {token}"));
        }
        builder
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap()
    }

    fn patch_json(uri: &str, token: Option<&str>, body: Value) -> Request<Body> {
        let mut builder = Request::builder()
            .method("PATCH")
            .uri(uri)
            .header("content-type", "application/json");
        if let Some(token) = token {
            builder = builder.header("authorization", format!("Bearer {token}"));
        }
        builder
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap()
    }

    fn put_json(uri: &str, token: Option<&str>, body: Value) -> Request<Body> {
        let mut builder = Request::builder()
            .method("PUT")
            .uri(uri)
            .header("content-type", "application/json");
        if let Some(token) = token {
            builder = builder.header("authorization", format!("Bearer {token}"));
        }
        builder
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap()
    }

    fn post_bytes(uri: &str, token: Option<&str>, body: &[u8]) -> Request<Body> {
        let mut builder = Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/octet-stream");
        if let Some(token) = token {
            builder = builder.header("authorization", format!("Bearer {token}"));
        }
        builder.body(Body::from(body.to_vec())).unwrap()
    }

    #[tokio::test]
    async fn healthz_reports_machine() {
        let tmp = tempfile::TempDir::new().unwrap();
        let st = state(tmp.path(), Some(TOKEN), PathBuf::from("tmux"));
        let (status, body) = call(router(st.clone()), get("/healthz")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["ok"], true);
        assert_eq!(body["data"]["machine"], MACHINE);
    }

    #[tokio::test]
    async fn list_empty_reports_machine_and_open_on_reads() {
        let tmp = tempfile::TempDir::new().unwrap();
        let st = state(tmp.path(), Some(TOKEN), PathBuf::from("tmux"));
        // No Authorization header: reads are open on loopback.
        let (status, body) = call(router(st.clone()), get("/sessions")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["schema_version"], "cli.agent-session.serve.v1");
        assert_eq!(body["data"]["machine"], MACHINE);
        let observed_at = body["data"]["observed_at"]
            .as_str()
            .expect("daemon observation anchor");
        observed_at
            .parse::<jiff::Timestamp>()
            .expect("RFC3339 daemon observation anchor");
        assert_eq!(body["data"]["sessions"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn list_observed_at_is_sampled_after_delayed_assembly_and_before_receipt() {
        let tmp = tempfile::TempDir::new().unwrap();
        let collection_finished = Arc::new(StdMutex::new(None));
        let finished = collection_finished.clone();
        let collector: SessionCollector = Arc::new(move |_, _| {
            std::thread::sleep(Duration::from_millis(50));
            *finished.lock().expect("collection timestamp lock") = Some(jiff::Timestamp::now());
            Ok(Vec::new())
        });
        let st =
            state_with_session_collector(tmp.path(), Some(TOKEN), PathBuf::from("tmux"), collector);

        let (status, body) = call(router(st), get("/sessions")).await;
        let received_at = jiff::Timestamp::now();
        assert_eq!(status, StatusCode::OK);
        let observed_at = body["data"]["observed_at"]
            .as_str()
            .unwrap()
            .parse::<jiff::Timestamp>()
            .unwrap();
        let collection_finished = collection_finished
            .lock()
            .expect("collection timestamp lock")
            .expect("collection completed");
        assert!(
            observed_at >= collection_finished,
            "observation preceded collection: observed={observed_at}, finished={collection_finished}"
        );
        assert!(
            observed_at <= received_at,
            "observation followed receipt: observed={observed_at}, receipt={received_at}"
        );
    }

    #[tokio::test]
    async fn activity_stream_requires_auth_and_sets_sse_headers() {
        let tmp = tempfile::TempDir::new().unwrap();
        let st = state(tmp.path(), Some(TOKEN), PathBuf::from("tmux"));

        let (status, body) = call(router(st.clone()), get_auth("/activity/events", None)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["error"]["code"], "unauthorized");

        let response = router(st)
            .oneshot(get_auth("/activity/events", Some(TOKEN)))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("text/event-stream")
        );
        assert_eq!(
            response.headers()["cache-control"],
            "no-cache, no-transform"
        );
        assert_eq!(response.headers()["x-accel-buffering"], "no");
    }

    #[tokio::test]
    async fn activity_stream_route_frames_cover_cursor_and_reset_contract() {
        let tmp = tempfile::TempDir::new().unwrap();
        let st = state(tmp.path(), Some(TOKEN), PathBuf::from("tmux"));
        let stream_id = st.activity_broker.log.stream_id.clone();

        let (id, event, data) =
            parse_activity_sse_frame(&first_activity_sse_frame(st.clone(), None).await);
        assert_eq!(id, format!("{stream_id}:1"));
        assert_eq!(event, "snapshot");
        assert_eq!(data["schema_version"], ACTIVITY_STREAM_EVENT_SCHEMA_VERSION);
        assert_eq!(data["type"], "snapshot");
        assert_eq!(data["stream_id"], stream_id);
        assert_eq!(data["sequence"], 1);
        assert_eq!(data["machine"], MACHINE);
        assert_eq!(data["sessions"], json!([]));

        st.activity_broker.log.publish_heartbeat();
        st.activity_broker
            .log
            .publish_snapshot(vec![ActivityStreamSession {
                id: "cursor-session".to_string(),
                turn_state: Some(test_stream_turn_state(2, "2026-07-11T00:00:02Z")),
            }]);
        let retained_cursor = format!("{stream_id}:1");
        let (id, event, data) = parse_activity_sse_frame(
            &first_activity_sse_frame(st.clone(), Some(&retained_cursor)).await,
        );
        assert_eq!(id, format!("{stream_id}:2"));
        assert_eq!(event, "heartbeat");
        assert_eq!(data["type"], "heartbeat");
        assert!(data.get("sessions").is_none());

        let retained_after_heartbeat = format!("{stream_id}:2");
        let (id, event, data) = parse_activity_sse_frame(
            &first_activity_sse_frame(st.clone(), Some(&retained_after_heartbeat)).await,
        );
        assert_eq!(id, format!("{stream_id}:3"));
        assert_eq!(event, "snapshot");
        assert_eq!(data["type"], "snapshot");
        assert_eq!(data["sessions"][0]["id"], "cursor-session");

        for (offset, cursor) in [
            "malformed".to_string(),
            format!("{stream_id}:999"),
            "foreign-stream:1".to_string(),
        ]
        .into_iter()
        .enumerate()
        {
            let (id, event, data) = parse_activity_sse_frame(
                &first_activity_sse_frame(st.clone(), Some(&cursor)).await,
            );
            let expected_sequence = 4 + offset as u64;
            assert_eq!(id, format!("{stream_id}:{expected_sequence}"));
            assert_eq!(event, "reset");
            assert_eq!(data["type"], "reset");
            assert_eq!(data["sequence"], expected_sequence);
            assert_eq!(data["sessions"][0]["id"], "cursor-session");
        }

        for _ in 0..ACTIVITY_STREAM_REPLAY_CAPACITY {
            st.activity_broker.log.publish_heartbeat();
        }
        let (id, event, data) = parse_activity_sse_frame(
            &first_activity_sse_frame(st.clone(), Some(&retained_cursor)).await,
        );
        assert_eq!(id, format!("{stream_id}:{}", data["sequence"]));
        assert_eq!(event, "reset");
        assert_eq!(data["type"], "reset");
        assert_eq!(data["sessions"][0]["id"], "cursor-session");
    }

    #[test]
    fn activity_stream_projection_is_metadata_only_and_drops_additive_content() {
        let state: crate::activity::TurnState = serde_json::from_value(json!({
            "schema_version":"agent-session.turn-state.v1",
            "phase":"needs_input",
            "phase_changed_at":"2026-07-11T00:00:00Z",
            "revision":7,
            "source":{
                "kind":"provider_hook",
                "provider":"codex",
                "confidence":"authoritative",
                "config":"must-not-stream"
            },
            "current_turn":{
                "provider_turn_id":"opaque-turn",
                "started_at":"2026-07-11T00:00:00Z",
                "last_progress_at":"2026-07-11T00:00:01Z",
                "attention":{
                    "kind":"permission",
                    "requested_at":"2026-07-11T00:00:02Z",
                    "pending_count":1,
                    "tool":"must-not-stream"
                },
                "prompt":"must-not-stream"
            },
            "last_turn":{
                "provider_turn_id":"opaque-prior-turn",
                "started_at":"2026-07-10T23:59:00Z",
                "completed_at":"2026-07-10T23:59:30Z",
                "outcome":"completed",
                "response":"must-not-stream"
            },
            "terminal":"must-not-stream",
            "credential":"must-not-stream"
        }))
        .expect("forward-compatible state");

        let projection = serde_json::to_value(crate::activity::stream_projection(&state)).unwrap();
        assert_eq!(projection["revision"], 7);
        assert_eq!(
            projection["current_turn"]["attention"]["kind"],
            "permission"
        );
        let encoded = projection.to_string();
        for forbidden in [
            "prompt",
            "response",
            "command",
            "tool",
            "terminal",
            "transcript",
            "config",
            "credential",
            "must-not-stream",
        ] {
            assert!(
                !encoded.contains(forbidden),
                "leaked forbidden field: {forbidden}"
            );
        }
    }

    #[test]
    fn activity_stream_multi_session_producer_matches_exact_downstream_fixture() {
        let working: crate::activity::TurnState = serde_json::from_value(json!({
            "schema_version":"agent-session.turn-state.v1",
            "phase":"working",
            "phase_changed_at":"2026-07-11T17:00:00Z",
            "revision":11,
            "source":{
                "kind":"provider_hook",
                "confidence":"authoritative"
            },
            "current_turn":{
                "started_at":"2026-07-11T16:59:00Z"
            }
        }))
        .expect("working fixture state");
        let completed: crate::activity::TurnState = serde_json::from_value(json!({
            "schema_version":"agent-session.turn-state.v1",
            "phase":"waiting",
            "phase_changed_at":"2026-07-11T17:01:00Z",
            "revision":12,
            "source":{
                "kind":"provider_hook",
                "provider":"codex",
                "confidence":"authoritative"
            },
            "last_turn":{
                "completed_at":"2026-07-11T17:01:00Z",
                "outcome":"completed"
            }
        }))
        .expect("completed fixture state");
        let event = ActivityStreamEvent {
            schema_version: ACTIVITY_STREAM_EVENT_SCHEMA_VERSION,
            kind: "snapshot",
            stream_id: "fixture-stream".to_string(),
            sequence: 42,
            machine: "fixture-machine".to_string(),
            observed_at: "2026-07-11T17:01:01Z".to_string(),
            reason: None,
            sessions: Some(vec![
                ActivityStreamSession {
                    id: "working-minimal".to_string(),
                    turn_state: Some(crate::activity::stream_projection(&working)),
                },
                ActivityStreamSession {
                    id: "completed-minimal".to_string(),
                    turn_state: Some(crate::activity::stream_projection(&completed)),
                },
                ActivityStreamSession {
                    id: "no-activity".to_string(),
                    turn_state: None,
                },
            ]),
        };
        let expected: Value = serde_json::from_str(include_str!(
            "../tests/fixtures/activity/activity-stream-v1-multi-session.json"
        ))
        .expect("canonical Agent Console producer fixture");

        assert_eq!(serde_json::to_value(event).unwrap(), expected);
    }

    #[tokio::test]
    async fn activity_stream_replays_in_order_and_resets_invalid_or_evicted_cursors() {
        let initial = vec![ActivityStreamSession {
            id: "session-1".to_string(),
            turn_state: Some(test_stream_turn_state(1, "2026-07-11T00:00:01Z")),
        }];
        let log = ActivityEventLog::new(MACHINE.to_string(), initial.clone());
        let mut first = log.subscribe(None);
        let snapshot = first.next_event().await.unwrap();
        assert_eq!(snapshot.kind, "snapshot");
        assert_eq!(snapshot.sequence, 1);
        assert_eq!(snapshot.sessions.as_ref(), Some(&initial));

        log.publish_heartbeat();
        log.publish_snapshot(vec![ActivityStreamSession {
            id: "session-1".to_string(),
            turn_state: Some(test_stream_turn_state(2, "2026-07-11T00:00:02Z")),
        }]);
        let mut replay = log.subscribe(Some(&format!("{}:1", log.stream_id)));
        assert_eq!(replay.next_event().await.unwrap().sequence, 2);
        let changed = replay.next_event().await.unwrap();
        assert_eq!(changed.sequence, 3);
        assert_eq!(changed.kind, "snapshot");

        let mut live = log.subscribe(Some(&format!("{}:3", log.stream_id)));
        let mut foreign = log.subscribe(Some("other-stream:3"));
        let reset = foreign.next_event().await.unwrap();
        assert_eq!(reset.kind, "reset");
        assert_eq!(reset.sequence, 4);
        assert_eq!(
            reset.sessions.as_ref().unwrap()[0]
                .turn_state
                .as_ref()
                .unwrap()
                .revision,
            2
        );
        let broadcast_reset = tokio::time::timeout(Duration::from_millis(100), live.next_event())
            .await
            .expect("reconciliation reset must reach existing subscribers")
            .expect("activity stream remains open");
        assert_eq!(broadcast_reset.kind, "reset");
        assert_eq!(broadcast_reset.sequence, reset.sequence);

        for _ in 0..ACTIVITY_STREAM_REPLAY_CAPACITY {
            log.publish_heartbeat();
        }
        let mut fresh = log.subscribe(None);
        let current = fresh.next_event().await.unwrap();
        assert_eq!(current.kind, "snapshot");
        assert_eq!(current.sequence, log.sequence.load(Ordering::SeqCst));
        let mut evicted = log.subscribe(Some(&format!("{}:1", log.stream_id)));
        assert_eq!(evicted.next_event().await.unwrap().kind, "reset");
    }

    #[tokio::test]
    async fn activity_stream_replay_bytes_are_bounded_and_high_cardinality_eviction_resets() {
        let log = ActivityEventLog::new(MACHINE.to_string(), Vec::new());
        let previous_sequence = log.sequence.load(Ordering::SeqCst);
        let sessions = (0..128)
            .map(|index| ActivityStreamSession {
                id: format!("session-{index:03}-{}", "x".repeat(128)),
                turn_state: Some(test_stream_turn_state(index, "x".repeat(8192))),
            })
            .collect();
        log.publish_snapshot(sessions);

        {
            let state = log.state.lock().expect("activity event state lock");
            assert!(
                state.history_bytes <= ACTIVITY_STREAM_REPLAY_BYTE_CAPACITY,
                "replay bytes exceeded cap: {} > {}",
                state.history_bytes,
                ACTIVITY_STREAM_REPLAY_BYTE_CAPACITY
            );
        }

        let mut replay = log.subscribe(Some(&format!("{}:{previous_sequence}", log.stream_id)));
        let reset = replay.next_event().await.unwrap();
        assert_eq!(reset.kind, "reset");
        assert_eq!(reset.sequence, log.sequence.load(Ordering::SeqCst));
        assert_eq!(reset.reason, Some(ACTIVITY_STREAM_OVERSIZED_REASON));
        assert!(reset.sessions.is_none());
    }

    #[tokio::test]
    async fn activity_stream_backpressure_never_blocks_producers_and_lag_resets() {
        let log = ActivityEventLog::new(MACHINE.to_string(), Vec::new());
        let cursor = format!("{}:1", log.stream_id);
        let mut slow = log.subscribe(Some(&cursor));
        for _ in 0..(ACTIVITY_STREAM_BROADCAST_CAPACITY + 4) {
            log.publish_heartbeat();
        }
        let reset = tokio::time::timeout(Duration::from_millis(100), slow.next_event())
            .await
            .expect("lagged subscriber reset")
            .expect("lagged subscriber event");
        assert_eq!(reset.kind, "reset");
        assert_eq!(reset.sequence, log.sequence.load(Ordering::SeqCst));
        assert!(reset.sessions.is_some());
    }

    #[tokio::test]
    async fn activity_stream_oversized_max_fanout_and_lag_reconcile_with_bounded_frames() {
        let broker = ActivityBroker::for_test(MACHINE);
        let cursor = format!("{}:1", broker.log.stream_id);
        let mut subscribers = (0..ACTIVITY_STREAM_SUBSCRIBER_CAPACITY)
            .map(|_| {
                broker
                    .subscribe(Some(&cursor))
                    .expect("admitted subscriber")
            })
            .collect::<Vec<_>>();
        assert!(matches!(
            broker.subscribe(Some(&cursor)),
            Err(ActivitySubscribeError::Capacity)
        ));
        let sessions = (0..128)
            .map(|index| ActivityStreamSession {
                id: format!("oversized-{index:03}-{}", "x".repeat(128)),
                turn_state: Some(test_stream_turn_state(index, "x".repeat(8192))),
            })
            .collect();

        let encode_count = broker.log.encoder.count();
        broker.log.publish_snapshot(sessions);
        let mut slow = subscribers.pop().expect("lagging subscriber");
        let first = subscribers[0]
            .next_event()
            .await
            .expect("oversized reconciliation frame");
        assert_eq!(first.kind, "reset");
        assert_eq!(first.reason, Some(ACTIVITY_STREAM_OVERSIZED_REASON));
        assert!(first.sessions.is_none());
        assert!(first.replay_bytes() <= ACTIVITY_STREAM_REPLAY_BYTE_CAPACITY);
        let (_, _, wire) = parse_activity_sse_frame(
            std::str::from_utf8(first.sse.as_ref()).expect("UTF-8 SSE frame"),
        );
        assert_eq!(wire["type"], "reset");
        assert_eq!(wire["reason"], ACTIVITY_STREAM_OVERSIZED_REASON);
        assert!(wire.get("sessions").is_none());
        assert_eq!(
            broker.log.encoder.count(),
            encode_count + 2,
            "oversized payload and its bounded reset each serialize once"
        );
        for subscriber in &mut subscribers[1..] {
            let observed = subscriber
                .next_event()
                .await
                .expect("shared oversized reconciliation frame");
            assert!(Arc::ptr_eq(&first, &observed));
        }
        assert_eq!(
            broker.log.encoder.count(),
            encode_count + 2,
            "maximum subscriber fan-out must share the pre-serialized frame"
        );

        for _ in 0..(ACTIVITY_STREAM_BROADCAST_CAPACITY + 4) {
            broker.log.publish_heartbeat();
        }
        let before_lag_reset = broker.log.encoder.count();
        let lagged = tokio::time::timeout(Duration::from_millis(100), slow.next_event())
            .await
            .expect("lagged subscriber reset")
            .expect("lagged subscriber event");
        assert_eq!(lagged.kind, "reset");
        assert_eq!(lagged.reason, Some(ACTIVITY_STREAM_OVERSIZED_REASON));
        assert!(lagged.sessions.is_none());
        assert!(lagged.replay_bytes() <= ACTIVITY_STREAM_REPLAY_BYTE_CAPACITY);
        assert_eq!(
            broker.log.encoder.count(),
            before_lag_reset + 1,
            "the first lagging subscriber materializes one cached reset frame"
        );
        let second_lagged = subscribers[0]
            .next_event()
            .await
            .expect("second lagging subscriber reset");
        assert!(Arc::ptr_eq(&lagged, &second_lagged));
        assert_eq!(
            broker.log.encoder.count(),
            before_lag_reset + 1,
            "lagging subscribers must share the cached reconciliation frame"
        );
        let state = broker.log.state.lock().expect("activity event state lock");
        assert!(state.history_bytes <= ACTIVITY_STREAM_REPLAY_BYTE_CAPACITY);
        assert_eq!(ACTIVITY_STREAM_BROADCAST_CAPACITY, 1);
    }

    #[tokio::test]
    async fn activity_stream_persistent_oversized_state_emits_one_transition_reset() {
        let log = ActivityEventLog::new(MACHINE.to_string(), Vec::new());
        let cursor = format!("{}:1", log.stream_id);
        let mut subscriber = log.subscribe(Some(&cursor));
        let oversized_sessions = |prefix: &str| {
            (0..128)
                .map(|index| ActivityStreamSession {
                    id: format!("{prefix}-{index:03}-{}", "x".repeat(128)),
                    turn_state: Some(test_stream_turn_state(index, "x".repeat(8192))),
                })
                .collect()
        };

        log.publish_snapshot(oversized_sessions("oversized-first"));
        let transition = subscriber.next_event().await.expect("oversized reset");
        assert_eq!(transition.kind, "reset");
        assert_eq!(transition.reason, Some(ACTIVITY_STREAM_OVERSIZED_REASON));
        let transition_sequence = transition.sequence;

        log.publish_snapshot(oversized_sessions("oversized-still"));
        assert_eq!(
            log.sequence.load(Ordering::SeqCst),
            transition_sequence,
            "persistent oversized snapshots must not create hidden gaps or repeated resets"
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(25), subscriber.next_event())
                .await
                .is_err(),
            "persistent oversized state amplified one transition into another poll reset"
        );

        log.publish_snapshot(vec![ActivityStreamSession {
            id: "bounded-recovery".to_string(),
            turn_state: None,
        }]);
        let recovery = subscriber.next_event().await.expect("bounded recovery");
        assert_eq!(recovery.kind, "snapshot");
        assert_eq!(recovery.sequence, transition_sequence + 1);
        assert_eq!(
            recovery.sessions.as_ref().unwrap()[0].id,
            "bounded-recovery"
        );
    }

    #[tokio::test]
    async fn activity_stream_rejects_subscriber_saturation_and_reclaims_permits() {
        let tmp = tempfile::TempDir::new().unwrap();
        let broker = ActivityBroker::for_test_with_subscriber_limit(MACHINE, 1);
        let st = state_with_activity_broker(tmp.path(), Some(TOKEN), PathBuf::from("tmux"), broker);

        let first = router(st.clone())
            .oneshot(get_auth("/activity/events", Some(TOKEN)))
            .await
            .unwrap();
        assert_eq!(first.status(), StatusCode::OK);

        let (status, body) = call(
            router(st.clone()),
            get_auth("/activity/events", Some(TOKEN)),
        )
        .await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(body["error"]["code"], "activity-stream-capacity");

        drop(first);
        let reclaimed = router(st)
            .oneshot(get_auth("/activity/events", Some(TOKEN)))
            .await
            .unwrap();
        assert_eq!(reclaimed.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn activity_broker_watcher_error_resets_existing_stream_and_disables_heartbeats() {
        let tmp = tempfile::TempDir::new().unwrap();
        let broker = ActivityBroker::for_test(MACHINE);
        let mut subscription = broker.subscribe(None).expect("ready subscription");
        let initial = subscription.next_event().await.expect("initial snapshot");
        assert_eq!(initial.kind, "snapshot");
        let sequence_before_failure = broker.log.sequence.load(Ordering::SeqCst);
        let (change_tx, _change_rx) = mpsc::channel(1);

        activity_watch_callback(
            Err(notify::Error::generic("injected watcher failure")),
            &change_tx,
            &broker.lifecycle,
            &tmp.path().join("sessions"),
        );

        let reset = subscription
            .next_event()
            .await
            .expect("degraded stream reconciliation reset");
        assert_degraded_reset_survives_consumer_dedupe(&initial, &reset);
        assert_eq!(reset.observed_at, initial.observed_at);
        assert!(subscription.next_event().await.is_none());
        assert!(matches!(
            broker.subscribe(None),
            Err(ActivitySubscribeError::Unavailable)
        ));
        assert!(!activity_publish_heartbeat_if_ready(
            &broker.log,
            &broker.lifecycle
        ));
        assert_eq!(
            broker.log.sequence.load(Ordering::SeqCst),
            reset.sequence,
            "a degraded broker must not emit healthy heartbeats"
        );
        assert_eq!(reset.sequence, sequence_before_failure + 1);

        tokio::time::sleep(Duration::from_millis(2)).await;
        let st = state_with_activity_broker(tmp.path(), Some(TOKEN), PathBuf::from("tmux"), broker);
        let (poll_status, poll_body) = call(router(st.clone()), get("/sessions")).await;
        assert_eq!(poll_status, StatusCode::OK);
        let poll_observed_at = poll_body["data"]["observed_at"]
            .as_str()
            .unwrap()
            .parse::<jiff::Timestamp>()
            .unwrap();
        let reset_observed_at = reset.observed_at.parse::<jiff::Timestamp>().unwrap();
        assert!(poll_observed_at > reset_observed_at);
        let (status, body) = call(router(st), get_auth("/activity/events", Some(TOKEN))).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["error"]["code"], "activity-stream-unavailable");
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("session polling")
        );
    }

    #[tokio::test]
    async fn activity_broker_rescan_flag_forces_full_snapshot_refresh() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = CliContext {
            state_dir: tmp.path().to_path_buf(),
            host: None,
        };
        let broker = ActivityBroker::for_test(MACHINE);
        let calls = Arc::new(AtomicUsize::new(0));
        let observed_calls = calls.clone();
        let collector: SessionCollector = Arc::new(move |_, _| {
            observed_calls.fetch_add(1, Ordering::SeqCst);
            Ok(Vec::new())
        });
        let (change_tx, change_rx) = mpsc::channel(1);
        let change_task = tokio::spawn(activity_change_loop(
            broker.log.clone(),
            broker.lifecycle.clone(),
            collector,
            context,
            PathBuf::from("tmux"),
            change_rx,
        ));
        let rescan = NotifyEvent::new(EventKind::Other).set_flag(notify::event::Flag::Rescan);

        activity_watch_callback(
            Ok(rescan),
            &change_tx,
            &broker.lifecycle,
            &tmp.path().join("sessions"),
        );
        tokio::time::timeout(Duration::from_secs(1), async {
            while calls.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("rescan-triggered full refresh");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(*broker.lifecycle.borrow(), ActivityBrokerLifecycle::Ready);

        change_task.abort();
        let _ = change_task.await;
    }

    #[tokio::test]
    async fn activity_broker_root_loss_rearm_failure_degrades_and_stops_stream() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = CliContext {
            state_dir: tmp.path().to_path_buf(),
            host: None,
        };
        let broker = ActivityBroker::for_test(MACHINE);
        let mut subscription = broker.subscribe(None).expect("ready subscription");
        let initial = subscription.next_event().await.expect("initial snapshot");
        let sessions_root = tmp.path().join("sessions");
        std::fs::create_dir_all(&sessions_root).unwrap();
        let (change_tx, change_rx) = mpsc::channel(1);
        let change_task = tokio::spawn(activity_change_loop_inner(
            broker.log.clone(),
            broker.lifecycle.clone(),
            default_session_collector(),
            context,
            PathBuf::from("tmux"),
            change_rx,
            ActivityChangeLoopControls {
                refresh_started: None,
                watch_rearm: Some(Arc::new(|| false)),
            },
        ));
        let removed = NotifyEvent::new(EventKind::Remove(notify::event::RemoveKind::Folder))
            .add_path(sessions_root.clone());

        activity_watch_callback(Ok(removed), &change_tx, &broker.lifecycle, &sessions_root);
        tokio::time::timeout(Duration::from_secs(1), change_task)
            .await
            .expect("root-loss loop terminates")
            .expect("root-loss task");
        assert_eq!(
            *broker.lifecycle.borrow(),
            ActivityBrokerLifecycle::Degraded
        );
        let reset = subscription.next_event().await.expect("degraded reset");
        assert_degraded_reset_survives_consumer_dedupe(&initial, &reset);
        assert!(subscription.next_event().await.is_none());
        assert!(matches!(
            broker.subscribe(None),
            Err(ActivitySubscribeError::Unavailable)
        ));
        assert!(!activity_publish_heartbeat_if_ready(
            &broker.log,
            &broker.lifecycle
        ));
    }

    #[tokio::test]
    async fn activity_broker_collector_failure_degrades_and_stops_existing_stream() {
        let tmp = tempfile::TempDir::new().unwrap();
        let broker = ActivityBroker::for_test(MACHINE);
        let mut subscription = broker.subscribe(None).expect("ready subscription");
        let initial = subscription.next_event().await.expect("initial snapshot");
        let failing_collector: SessionCollector = Arc::new(|_, _| {
            Err(CliError::runtime(
                "activity-snapshot-failed",
                "injected activity snapshot failure",
                None,
            ))
        });
        let context = CliContext {
            state_dir: tmp.path().to_path_buf(),
            host: None,
        };

        assert!(
            !activity_refresh_snapshot(
                broker.log.clone(),
                broker.lifecycle.clone(),
                failing_collector,
                context,
                PathBuf::from("tmux"),
            )
            .await
        );

        let reset = subscription
            .next_event()
            .await
            .expect("collector failure reset");
        assert_degraded_reset_survives_consumer_dedupe(&initial, &reset);
        assert_eq!(reset.observed_at, initial.observed_at);
        assert!(subscription.next_event().await.is_none());
        assert!(matches!(
            broker.subscribe(None),
            Err(ActivitySubscribeError::Unavailable)
        ));
    }

    #[tokio::test]
    async fn sessions_poll_and_activity_broker_share_one_injected_snapshot_source() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = CliContext {
            state_dir: tmp.path().to_path_buf(),
            host: None,
        };
        let tmux = minimal_tmux(tmp.path());
        let cwd = tmp.path().join("repo");
        std::fs::create_dir_all(&cwd).unwrap();
        seed_fresh_provider_session(
            tmp.path(),
            "must-be-hidden-by-injected-source",
            "codex",
            "hs-source-parity",
            &cwd,
            None,
        );
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed_calls = calls.clone();
        let collector: SessionCollector = Arc::new(move |_, _| {
            observed_calls.fetch_add(1, Ordering::SeqCst);
            Ok(Vec::new())
        });
        let broker = ActivityBroker::for_test_with_session_collector(
            MACHINE,
            &context,
            &tmux,
            collector.clone(),
        );
        let st = state_with_dependencies(tmp.path(), Some(TOKEN), tmux, broker.clone(), collector);

        let (status, body) = call(router(st), get("/sessions")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["data"]["sessions"], json!([]));
        let mut subscription = broker.subscribe(None).expect("ready subscription");
        let snapshot = subscription.next_event().await.expect("stream snapshot");
        assert_eq!(snapshot.sessions, Some(Vec::new()));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn activity_broker_initial_collector_failure_starts_degraded_and_returns_poll_fallback() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = CliContext {
            state_dir: tmp.path().to_path_buf(),
            host: None,
        };
        let failing_collector: SessionCollector = Arc::new(|_, _| {
            Err(CliError::runtime(
                "activity-snapshot-failed",
                "injected initial activity snapshot failure",
                None,
            ))
        });
        let broker = ActivityBroker::start(
            context,
            MACHINE.to_string(),
            minimal_tmux(tmp.path()),
            failing_collector.clone(),
        )
        .await;
        assert_eq!(
            *broker.lifecycle.borrow(),
            ActivityBrokerLifecycle::Degraded
        );
        assert!(matches!(
            broker.subscribe(None),
            Err(ActivitySubscribeError::Unavailable)
        ));

        let st = state_with_dependencies(
            tmp.path(),
            Some(TOKEN),
            PathBuf::from("tmux"),
            broker,
            failing_collector,
        );
        let (status, body) = call(router(st), get_auth("/activity/events", Some(TOKEN))).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["error"]["code"], "activity-stream-unavailable");
    }

    #[tokio::test]
    async fn activity_broker_change_channel_close_resets_once_stops_stream_and_heartbeat() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = CliContext {
            state_dir: tmp.path().to_path_buf(),
            host: None,
        };
        let broker = ActivityBroker::for_test(MACHINE);
        let mut subscription = broker.subscribe(None).expect("ready subscription");
        let initial = subscription.next_event().await.expect("initial snapshot");
        assert_eq!(initial.kind, "snapshot");
        let sequence_before_close = broker.log.sequence.load(Ordering::SeqCst);
        let (change_tx, change_rx) = mpsc::channel(1);
        let change_task = tokio::spawn(activity_change_loop(
            broker.log.clone(),
            broker.lifecycle.clone(),
            default_session_collector(),
            context,
            PathBuf::from("tmux"),
            change_rx,
        ));
        let heartbeat_task = tokio::spawn(activity_heartbeat_loop(
            broker.log.clone(),
            broker.lifecycle.clone(),
        ));

        drop(change_tx);
        tokio::time::timeout(Duration::from_millis(250), change_task)
            .await
            .expect("change loop stops after notification channel close")
            .expect("change loop task");
        assert_eq!(
            *broker.lifecycle.borrow(),
            ActivityBrokerLifecycle::Degraded
        );
        let reset = subscription
            .next_event()
            .await
            .expect("channel-close reconciliation");
        assert_degraded_reset_survives_consumer_dedupe(&initial, &reset);
        assert_eq!(reset.observed_at, initial.observed_at);
        assert!(subscription.next_event().await.is_none());
        tokio::time::timeout(Duration::from_millis(250), heartbeat_task)
            .await
            .expect("heartbeat loop stops after degradation")
            .expect("heartbeat loop task");
        assert_eq!(
            broker.log.sequence.load(Ordering::SeqCst),
            reset.sequence,
            "notification-channel failure must not leave a healthy heartbeat running"
        );
        assert_eq!(reset.sequence, sequence_before_close + 1);
    }

    #[tokio::test]
    async fn activity_broker_notification_refresh_matches_stateful_shared_poll_source() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = CliContext {
            state_dir: tmp.path().to_path_buf(),
            host: None,
        };
        let tmux = minimal_tmux(tmp.path());
        let cwd = tmp.path().join("repo");
        std::fs::create_dir_all(&cwd).unwrap();
        for id in ["source-before", "source-after"] {
            seed_fresh_provider_session(tmp.path(), id, "codex", &format!("hs-{id}"), &cwd, None);
        }
        let source_revision = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed_revision = source_revision.clone();
        let collector: SessionCollector = Arc::new(move |context, tmux_bin| {
            let expected_id = if observed_revision.load(Ordering::SeqCst) == 0 {
                "source-before"
            } else {
                "source-after"
            };
            list_sessions(context, Some(tmux_bin)).map(|sessions| {
                sessions
                    .into_iter()
                    .filter(|session| session.id == expected_id)
                    .collect()
            })
        });
        let broker = ActivityBroker::for_test_with_session_collector(
            MACHINE,
            &context,
            &tmux,
            collector.clone(),
        );
        let mut subscription = broker.subscribe(None).expect("ready subscription");
        let initial = subscription.next_event().await.expect("initial snapshot");
        assert_eq!(initial.sessions.as_ref().unwrap()[0].id, "source-before");
        let st = state_with_dependencies(
            tmp.path(),
            Some(TOKEN),
            tmux.clone(),
            broker.clone(),
            collector.clone(),
        );
        source_revision.store(1, Ordering::SeqCst);

        let (status, body) = call(router(st), get("/sessions")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["data"]["sessions"][0]["id"], "source-after");

        let (change_tx, change_rx) = mpsc::channel(1);
        let change_task = tokio::spawn(activity_change_loop(
            broker.log.clone(),
            broker.lifecycle.clone(),
            collector,
            context,
            tmux,
            change_rx,
        ));
        change_tx
            .send(ActivityChange::Refresh)
            .await
            .expect("activity notification");
        let refreshed = tokio::time::timeout(Duration::from_secs(1), subscription.next_event())
            .await
            .expect("notification refresh")
            .expect("refreshed snapshot");
        assert_eq!(refreshed.kind, "snapshot");
        assert_eq!(refreshed.sessions.as_ref().unwrap()[0].id, "source-after");

        change_task.abort();
        let _ = change_task.await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn activity_broker_notification_storm_with_slow_scans_converges_final_revision() {
        const NOTIFICATIONS: usize = 300;
        let tmp = tempfile::TempDir::new().unwrap();
        let context = CliContext {
            state_dir: tmp.path().to_path_buf(),
            host: None,
        };
        let tmux = minimal_tmux(tmp.path());
        let cwd = tmp.path().join("repo");
        std::fs::create_dir_all(&cwd).unwrap();
        for id in ["storm-before", "storm-final"] {
            seed_fresh_provider_session(tmp.path(), id, "codex", &format!("hs-{id}"), &cwd, None);
        }
        let source_revision = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed_revision = source_revision.clone();
        let collector: SessionCollector = Arc::new(move |context, tmux_bin| {
            std::thread::sleep(Duration::from_millis(75));
            let expected_id = if observed_revision.load(Ordering::SeqCst) == NOTIFICATIONS {
                "storm-final"
            } else {
                "storm-before"
            };
            list_sessions(context, Some(tmux_bin)).map(|sessions| {
                sessions
                    .into_iter()
                    .filter(|session| session.id == expected_id)
                    .collect()
            })
        });
        let broker = ActivityBroker::for_test_with_session_collector(
            MACHINE,
            &context,
            &tmux,
            collector.clone(),
        );
        let mut subscription = broker.subscribe(None).expect("ready subscription");
        let initial = subscription.next_event().await.expect("initial snapshot");
        assert_eq!(initial.sessions.as_ref().unwrap()[0].id, "storm-before");
        let (change_tx, change_rx) = mpsc::channel(1);
        let change_task = tokio::spawn(activity_change_loop(
            broker.log.clone(),
            broker.lifecycle.clone(),
            collector,
            context,
            tmux,
            change_rx,
        ));

        for revision in 1..=NOTIFICATIONS {
            source_revision.store(revision, Ordering::SeqCst);
            let _ = change_tx.try_send(ActivityChange::Refresh);
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        // This real-time integration case asserts only liveness and final-state
        // convergence. macOS CI scheduling can legitimately vary the number of
        // scans observed during the wall-clock storm; exact cadence and spacing
        // are covered by the paused-time dense, periodic, and slow-scan tests.
        let final_snapshot = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let event = subscription.next_event().await.expect("activity event");
                if event.sessions.as_ref().is_some_and(|sessions| {
                    sessions.iter().any(|session| session.id == "storm-final")
                }) {
                    break event;
                }
            }
        })
        .await
        .expect("storm converges to final revision");
        assert_eq!(final_snapshot.kind, "snapshot");

        change_task.abort();
        let _ = change_task.await;
    }

    #[tokio::test]
    async fn activity_broker_refresh_starts_respect_the_minimum_cadence() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = CliContext {
            state_dir: tmp.path().to_path_buf(),
            host: None,
        };
        let broker = ActivityBroker::for_test(MACHINE);
        let (started_tx, mut started_rx) = mpsc::unbounded_channel();
        let collector: SessionCollector = Arc::new(move |_, _| {
            let _ = started_tx.send(std::time::Instant::now());
            Ok(Vec::new())
        });
        let (change_tx, change_rx) = mpsc::channel(1);
        let change_task = tokio::spawn(activity_change_loop(
            broker.log.clone(),
            broker.lifecycle.clone(),
            collector,
            context,
            PathBuf::from("tmux"),
            change_rx,
        ));

        change_tx
            .send(ActivityChange::Refresh)
            .await
            .expect("first activity notification");
        let first = tokio::time::timeout(Duration::from_secs(1), started_rx.recv())
            .await
            .expect("first refresh start")
            .expect("first refresh timestamp");
        change_tx
            .send(ActivityChange::Refresh)
            .await
            .expect("second activity notification");
        let second = tokio::time::timeout(Duration::from_secs(1), started_rx.recv())
            .await
            .expect("second refresh start")
            .expect("second refresh timestamp");
        assert!(
            second.duration_since(first) >= ACTIVITY_STREAM_MAX_REFRESH_CADENCE,
            "refreshes started too close together: {:?}",
            second.duration_since(first)
        );

        change_task.abort();
        let _ = change_task.await;
    }

    async fn drive_paused_activity_refresh_pattern(
        interval: Duration,
        notifications: usize,
    ) -> (Vec<Duration>, Vec<usize>) {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = CliContext {
            state_dir: tmp.path().to_path_buf(),
            host: None,
        };
        let broker = ActivityBroker::for_test(MACHINE);
        let source_revision = Arc::new(AtomicUsize::new(0));
        let sampled_revisions = Arc::new(StdMutex::new(Vec::new()));
        let (sampled_tx, mut sampled_rx) = watch::channel(0_usize);
        let observed_revision = source_revision.clone();
        let observed_samples = sampled_revisions.clone();
        let collector: SessionCollector = Arc::new(move |_, _| {
            let revision = observed_revision.load(Ordering::SeqCst);
            observed_samples
                .lock()
                .expect("sampled revision lock")
                .push(revision);
            sampled_tx.send_replace(revision);
            Ok(Vec::new())
        });
        let (change_tx, change_rx) = mpsc::channel(1);
        let (started_tx, mut started_rx) = mpsc::unbounded_channel();
        let origin = tokio::time::Instant::now();
        let change_task = tokio::spawn(activity_change_loop_inner(
            broker.log.clone(),
            broker.lifecycle.clone(),
            collector,
            context,
            PathBuf::from("tmux"),
            change_rx,
            ActivityChangeLoopControls {
                refresh_started: Some(started_tx),
                watch_rearm: None,
            },
        ));

        for revision in 1..=notifications {
            source_revision.store(revision, Ordering::SeqCst);
            let _ = change_tx.try_send(ActivityChange::Refresh);
            for _ in 0..if revision == 1 { 4 } else { 1 } {
                tokio::task::yield_now().await;
            }
            tokio::time::advance(interval).await;
        }
        for _ in 0..4 {
            tokio::task::yield_now().await;
        }
        tokio::time::advance(ACTIVITY_STREAM_MAX_REFRESH_CADENCE + ACTIVITY_STREAM_DEBOUNCE).await;
        let (deadline_tx, mut deadline_rx) = mpsc::unbounded_channel();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(5));
            let _ = deadline_tx.send(());
        });
        tokio::select! {
            _ = async {
                loop {
                    if *sampled_rx.borrow_and_update() == notifications {
                        break;
                    }
                    sampled_rx.changed().await.expect("sampled revision channel");
                }
            } => {}
            _ = deadline_rx.recv() => panic!("activity refresh did not sample revision {notifications}"),
        }

        let mut starts = Vec::new();
        while let Ok(started_at) = started_rx.try_recv() {
            starts.push(started_at.duration_since(origin));
        }
        let samples = sampled_revisions
            .lock()
            .expect("sampled revision lock")
            .clone();
        change_task.abort();
        let _ = change_task.await;
        (starts, samples)
    }

    #[tokio::test(start_paused = true)]
    async fn activity_refresh_manual_time_dense_and_26ms_patterns_are_rate_bounded() {
        let (dense_starts, dense_samples) =
            drive_paused_activity_refresh_pattern(Duration::from_millis(2), 300).await;
        assert_eq!(dense_starts.first(), Some(&Duration::from_millis(250)));
        assert!(dense_starts.len() <= 3, "dense refreshes: {dense_starts:?}");
        assert_eq!(dense_samples.last(), Some(&300));
        assert!(
            dense_starts
                .windows(2)
                .all(|starts| { starts[1] - starts[0] >= ACTIVITY_STREAM_MAX_REFRESH_CADENCE })
        );

        let (periodic_starts, periodic_samples) =
            drive_paused_activity_refresh_pattern(Duration::from_millis(26), 30).await;
        assert!(
            periodic_starts
                .first()
                .is_some_and(|start| *start >= Duration::from_millis(25)
                    && *start <= Duration::from_millis(26)),
            "isolated trailing quiet start: {periodic_starts:?}"
        );
        assert!(
            periodic_starts.len() <= 4,
            "26ms refreshes: {periodic_starts:?}"
        );
        assert_eq!(periodic_samples.last(), Some(&30));
        assert!(
            periodic_starts
                .windows(2)
                .all(|starts| { starts[1] - starts[0] >= ACTIVITY_STREAM_MAX_REFRESH_CADENCE })
        );
    }

    #[tokio::test(start_paused = true)]
    async fn activity_refresh_manual_time_slow_scan_retains_dirty_final_revision() {
        const NOTIFICATIONS: usize = 100;
        let tmp = tempfile::TempDir::new().unwrap();
        let context = CliContext {
            state_dir: tmp.path().to_path_buf(),
            host: None,
        };
        let tmux = minimal_tmux(tmp.path());
        let cwd = tmp.path().join("repo");
        std::fs::create_dir_all(&cwd).unwrap();
        for id in ["slow-before", "slow-final"] {
            seed_fresh_provider_session(tmp.path(), id, "codex", &format!("hs-{id}"), &cwd, None);
        }
        let broker = ActivityBroker::for_test(MACHINE);
        let mut subscription = broker.subscribe(None).expect("ready subscription");
        assert_eq!(
            subscription.next_event().await.unwrap().sessions,
            Some(Vec::new())
        );
        let source_revision = Arc::new(AtomicUsize::new(0));
        let calls = Arc::new(AtomicUsize::new(0));
        let observed_revision = source_revision.clone();
        let observed_calls = calls.clone();
        let (collector_started_tx, mut collector_started_rx) = mpsc::unbounded_channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let release_rx = Arc::new(StdMutex::new(release_rx));
        let observed_release = release_rx.clone();
        let collector: SessionCollector = Arc::new(move |context, tmux_bin| {
            let call = observed_calls.fetch_add(1, Ordering::SeqCst);
            let revision = observed_revision.load(Ordering::SeqCst);
            if call == 0 {
                let _ = collector_started_tx.send(());
                observed_release
                    .lock()
                    .expect("slow collector release lock")
                    .recv()
                    .expect("slow collector release");
            }
            let expected_id = if revision == 0 {
                "slow-before"
            } else {
                "slow-final"
            };
            list_sessions(context, Some(tmux_bin)).map(|sessions| {
                sessions
                    .into_iter()
                    .filter(|session| session.id == expected_id)
                    .collect()
            })
        });
        let (change_tx, change_rx) = mpsc::channel(1);
        let (started_tx, mut started_rx) = mpsc::unbounded_channel();
        let origin = tokio::time::Instant::now();
        let change_task = tokio::spawn(activity_change_loop_inner(
            broker.log.clone(),
            broker.lifecycle.clone(),
            collector,
            context,
            tmux,
            change_rx,
            ActivityChangeLoopControls {
                refresh_started: Some(started_tx),
                watch_rearm: None,
            },
        ));

        change_tx
            .send(ActivityChange::Refresh)
            .await
            .expect("initial activity notification");
        for _ in 0..4 {
            tokio::task::yield_now().await;
        }
        tokio::time::advance(ACTIVITY_STREAM_DEBOUNCE).await;
        assert_eq!(
            started_rx.recv().await.unwrap().duration_since(origin),
            Duration::from_millis(25)
        );
        collector_started_rx
            .recv()
            .await
            .expect("slow collector started");
        for revision in 1..=NOTIFICATIONS {
            source_revision.store(revision, Ordering::SeqCst);
            let _ = change_tx.try_send(ActivityChange::Refresh);
            tokio::time::advance(Duration::from_millis(2)).await;
        }
        release_tx.send(()).expect("release slow collector");
        let before = subscription.next_event().await.expect("pre-storm snapshot");
        assert_eq!(before.sessions.as_ref().unwrap()[0].id, "slow-before");
        tokio::time::advance(Duration::from_millis(50)).await;
        let second_started = started_rx.recv().await.expect("final refresh start");
        assert_eq!(
            second_started.duration_since(origin),
            Duration::from_millis(275)
        );
        let final_snapshot = subscription.next_event().await.expect("final snapshot");
        assert_eq!(
            final_snapshot.sessions.as_ref().unwrap()[0].id,
            "slow-final"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2);

        change_task.abort();
        let _ = change_task.await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn activity_sessions_root_delete_recreate_delivers_transition_or_degrades() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tmux = minimal_tmux(tmp.path());
        let context = CliContext {
            state_dir: tmp.path().to_path_buf(),
            host: None,
        };
        let broker = ActivityBroker::start(
            context.clone(),
            MACHINE.to_string(),
            tmux,
            default_session_collector(),
        )
        .await;
        let mut subscription = broker.subscribe(None).expect("ready activity broker");
        let initial = subscription.next_event().await.expect("initial snapshot");
        let sessions_root = tmp.path().join("sessions");
        std::fs::remove_dir_all(&sessions_root).expect("remove watched sessions root");
        let _ = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if *broker.lifecycle.borrow() == ActivityBrokerLifecycle::Degraded
                    || sessions_root.is_dir()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;

        if *broker.lifecycle.borrow() == ActivityBrokerLifecycle::Degraded {
            let reset = subscription.next_event().await.expect("root-loss reset");
            assert_degraded_reset_survives_consumer_dedupe(&initial, &reset);
            assert!(subscription.next_event().await.is_none());
            assert!(matches!(
                broker.subscribe(None),
                Err(ActivitySubscribeError::Unavailable)
            ));
            assert!(!activity_publish_heartbeat_if_ready(
                &broker.log,
                &broker.lifecycle
            ));
            return;
        }

        let cwd = tmp.path().join("repo");
        std::fs::create_dir_all(&cwd).unwrap();
        seed_fresh_provider_session(
            tmp.path(),
            "root-rearmed-session",
            "codex",
            "hs-root-rearmed-session",
            &cwd,
            None,
        );
        let record = load_session_record(&context, "root-rearmed-session").expect("session record");
        crate::activity::activate_runtime(&context, &record).expect("activity snapshot");
        let delivered = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let event = subscription.next_event().await.expect("activity event");
                if event.sessions.as_ref().is_some_and(|sessions| {
                    sessions
                        .iter()
                        .any(|session| session.id == "root-rearmed-session")
                }) {
                    break event;
                }
            }
        })
        .await
        .expect("re-armed root delivers later transition");
        assert_eq!(delivered.kind, "snapshot");
        assert_eq!(*broker.lifecycle.borrow(), ActivityBrokerLifecycle::Ready);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn activity_file_notification_publishes_transition_without_http_polling() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tmux = minimal_tmux(tmp.path());
        let context = CliContext {
            state_dir: tmp.path().to_path_buf(),
            host: None,
        };
        let broker = ActivityBroker::start(
            context.clone(),
            MACHINE.to_string(),
            tmux,
            default_session_collector(),
        )
        .await;
        let mut subscription = broker.subscribe(None).expect("ready activity broker");
        assert_eq!(subscription.next_event().await.unwrap().kind, "snapshot");

        let cwd = tmp.path().join("repo");
        std::fs::create_dir_all(&cwd).unwrap();
        seed_fresh_provider_session(
            tmp.path(),
            "streamed-session",
            "codex",
            "hs-streamed-session",
            &cwd,
            None,
        );
        let record = load_session_record(&context, "streamed-session").expect("session record");
        crate::activity::activate_runtime(&context, &record).expect("activity snapshot");

        let event = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let event = subscription.next_event().await.expect("activity event");
                let has_activity = event.sessions.as_ref().is_some_and(|sessions| {
                    sessions.iter().any(|session| {
                        session.id == "streamed-session" && session.turn_state.is_some()
                    })
                });
                if has_activity {
                    break event;
                }
            }
        })
        .await
        .expect("filesystem-driven transition");
        assert_eq!(event.kind, "snapshot");
        let session = event
            .sessions
            .as_ref()
            .unwrap()
            .iter()
            .find(|session| session.id == "streamed-session")
            .unwrap();
        assert_eq!(session.turn_state.as_ref().unwrap().revision, 1);

        std::fs::remove_file(tmp.path().join("sessions/streamed-session/activity.json"))
            .expect("remove activity snapshot");
        let cleared = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let event = subscription.next_event().await.expect("activity event");
                if event.sessions.as_ref().is_some_and(|sessions| {
                    sessions.iter().any(|session| {
                        session.id == "streamed-session" && session.turn_state.is_none()
                    })
                }) {
                    break event;
                }
            }
        })
        .await
        .expect("activity removal transition");
        assert_eq!(cleared.kind, "snapshot");

        std::fs::remove_dir_all(tmp.path().join("sessions/streamed-session"))
            .expect("remove session");
        let deleted = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let event = subscription.next_event().await.expect("activity event");
                if event.sessions.as_ref().is_some_and(|sessions| {
                    sessions
                        .iter()
                        .all(|session| session.id != "streamed-session")
                }) {
                    break event;
                }
            }
        })
        .await
        .expect("session removal transition");
        assert_eq!(deleted.kind, "snapshot");
    }

    #[tokio::test]
    async fn list_preserves_provider_resume_metadata() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cwd = tmp.path().join("repo");
        std::fs::create_dir_all(&cwd).unwrap();
        seed_resumable_session(
            tmp.path(),
            "recover",
            "codex",
            "hs-codex-recover",
            &cwd,
            &[
                "resume",
                "resume-session-id",
                "--cd",
                cwd.to_str().unwrap(),
                "--no-alt-screen",
            ],
        );
        add_provider_resume_extra(tmp.path(), "recover");
        let st = state(tmp.path(), Some(TOKEN), minimal_tmux(tmp.path()));

        let (status, body) = call(router(st.clone()), get("/sessions")).await;
        assert_eq!(status, StatusCode::OK, "body={body}");
        let session = &body["data"]["sessions"][0];
        assert_eq!(session["provider_resume"]["provider"], "codex");
        assert_eq!(
            session["provider_resume"]["session_id"],
            "resume-session-id"
        );
        assert_eq!(
            session["provider_resume"]["resume_args"],
            json!([
                "resume",
                "resume-session-id",
                "--cd",
                cwd.to_str().unwrap(),
                "--no-alt-screen"
            ])
        );
        assert!(session["provider_resume"].get("storage_only").is_none());
    }

    #[tokio::test]
    async fn writes_require_token() {
        let tmp = tempfile::TempDir::new().unwrap();
        let st = state(tmp.path(), Some(TOKEN), PathBuf::from("tmux"));

        // POST create without a token.
        let (status, body) = call(
            router(st.clone()),
            post_json("/sessions", None, json!({"agent": "codex"})),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["error"]["code"], "unauthorized");

        // DELETE without a token.
        let del = Request::builder()
            .method("DELETE")
            .uri("/sessions/abc")
            .body(Body::empty())
            .unwrap();
        let (status, _) = call(router(st.clone()), del).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        // Wrong token is rejected.
        let (status, _) = call(
            router(st.clone()),
            post_json("/sessions", Some("nope"), json!({"agent": "codex"})),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn auto_resume_control_is_authenticated_durable_and_projected() {
        let tmp = tempfile::TempDir::new().unwrap();
        seed_session(tmp.path(), "claude-reset", "claude", "hs-claude-reset");
        let st = state(tmp.path(), Some(TOKEN), minimal_tmux(tmp.path()));

        let (status, body) = call(
            router(st.clone()),
            put_json(
                "/sessions/claude-reset/auto-resume",
                None,
                json!({"enabled": true}),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "body={body}");

        let (status, body) = call(
            router(st.clone()),
            put_json(
                "/sessions/claude-reset/auto-resume",
                Some(TOKEN),
                json!({"enabled": true}),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body={body}");
        assert_eq!(body["data"]["auto_resume"]["enabled"], true);
        assert_eq!(body["data"]["auto_resume"]["state"], "enabled");
        assert_eq!(body["data"]["auto_resume"]["supported"], true);

        let (status, body) = call(router(st.clone()), get("/sessions")).await;
        assert_eq!(status, StatusCode::OK, "body={body}");
        let projected = &body["data"]["sessions"][0]["auto_resume"];
        assert_eq!(projected["enabled"], true);
        assert_eq!(projected["state"], "enabled");
        assert!(projected.get("blocked_turn_id").is_none());

        let request = Request::builder()
            .method("DELETE")
            .uri("/sessions/claude-reset/auto-resume")
            .header("authorization", format!("Bearer {TOKEN}"))
            .body(Body::empty())
            .unwrap();
        let (status, body) = call(router(st.clone()), request).await;
        assert_eq!(status, StatusCode::OK, "body={body}");
        assert_eq!(body["data"]["auto_resume"]["enabled"], false);
        assert_eq!(body["data"]["auto_resume"]["state"], "cancelled");

        let persisted =
            fs::read_to_string(tmp.path().join("sessions/claude-reset/auto-resume.json"))
                .expect("durable auto-resume state");
        assert!(!persisted.contains("prompt"));
        assert!(!persisted.contains("transcript"));
    }

    #[tokio::test]
    async fn auto_resume_put_authenticates_before_returning_stable_body_errors() {
        let tmp = tempfile::TempDir::new().unwrap();
        seed_session(tmp.path(), "claude-reset", "claude", "hs-claude-reset");
        let st = state(tmp.path(), Some(TOKEN), minimal_tmux(tmp.path()));
        let uri = "/sessions/claude-reset/auto-resume";

        let unauthenticated = Request::builder()
            .method("PUT")
            .uri(uri)
            .header(CONTENT_TYPE, "application/json")
            .body(Body::from("{"))
            .unwrap();
        let (status, body) = call(router(st.clone()), unauthenticated).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "body={body}");
        assert_eq!(body["error"]["code"], "unauthorized");

        for (content_type, body_text) in [
            (Some("application/json"), "{"),
            (None, r#"{"enabled":true}"#),
            (Some("application/json"), "{}"),
            (Some("application/json"), r#"{"enabled":"yes"}"#),
        ] {
            let mut request = Request::builder()
                .method("PUT")
                .uri(uri)
                .header(AUTHORIZATION, format!("Bearer {TOKEN}"));
            if let Some(content_type) = content_type {
                request = request.header(CONTENT_TYPE, content_type);
            }
            let (status, body) = call(
                router(st.clone()),
                request.body(Body::from(body_text)).unwrap(),
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "body={body}");
            assert_eq!(body["schema_version"], "cli.agent-session.serve.v1");
            assert_eq!(body["error"]["code"], "invalid-json-body");
        }

        let unconfigured = state(tmp.path(), None, PathBuf::from("tmux"));
        let request = Request::builder()
            .method("PUT")
            .uri(uri)
            .header(CONTENT_TYPE, "application/json")
            .body(Body::from("{"))
            .unwrap();
        let (status, body) = call(router(unconfigured), request).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "body={body}");
        assert_eq!(body["error"]["code"], "token-not-configured");
    }

    #[tokio::test]
    async fn codex_auto_resume_fails_closed_without_authoritative_failure_signal() {
        let tmp = tempfile::TempDir::new().unwrap();
        seed_session(tmp.path(), "codex-reset", "codex", "hs-codex-reset");
        let st = state(tmp.path(), Some(TOKEN), minimal_tmux(tmp.path()));

        let (status, body) = call(
            router(st.clone()),
            put_json(
                "/sessions/codex-reset/auto-resume",
                Some(TOKEN),
                json!({"enabled": true}),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "body={body}");
        assert_eq!(body["error"]["code"], "auto-resume-unsupported");
    }

    #[tokio::test(start_paused = true)]
    async fn codex_scheduler_does_not_serialize_independent_control_timeouts() {
        let tmp = tempfile::TempDir::new().unwrap();
        let st = state(tmp.path(), Some(TOKEN), minimal_tmux(tmp.path()));
        let (slow_handle, _slow_commands) = codex_app_server::control_channel();
        let (fast_handle, mut fast_commands) = codex_app_server::control_channel();
        st.codex_controls.lock().unwrap().extend([
            (
                "slow".to_string(),
                CodexControlEntry {
                    launch_id: "slow-launch".to_string(),
                    handle: slow_handle,
                },
            ),
            (
                "fast".to_string(),
                CodexControlEntry {
                    launch_id: "fast-launch".to_string(),
                    handle: fast_handle,
                },
            ),
        ]);
        let (seen, observed) = tokio::sync::oneshot::channel();
        let responder = tokio::spawn(async move {
            let Some(codex_app_server::ControlCommand::Usage(response)) =
                fast_commands.recv().await
            else {
                panic!("fast control did not receive a usage request");
            };
            let _ = seen.send(());
            let _ = response.send(Ok(UsageSnapshot {
                authoritative: false,
                has_exhausted_windows: false,
                exhausted_reset_epochs: Vec::new(),
            }));
        });
        let scheduler = tokio::spawn(process_codex_auto_resume_ids(
            st,
            vec![
                CodexAutoResumeTarget {
                    id: "slow".to_string(),
                    launch_id: "slow-launch".to_string(),
                    binding: crate::codex_account::BindingSnapshot::Unbound,
                },
                CodexAutoResumeTarget {
                    id: "fast".to_string(),
                    launch_id: "fast-launch".to_string(),
                    binding: crate::codex_account::BindingSnapshot::Unbound,
                },
            ],
        ));

        tokio::time::timeout(Duration::from_millis(1), observed)
            .await
            .expect("fast control waited behind the slow control")
            .unwrap();
        responder.await.unwrap();
        scheduler.abort();
        let _ = scheduler.await;
    }

    #[tokio::test]
    async fn missing_codex_control_cannot_checkpoint_a_same_id_replacement() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        seed_session(&context.state_dir, "replacement", "codex", "hs-replacement");
        let mut record = load_session_record(&context, "replacement").unwrap();
        let socket = tmp.path().join("replacement.sock");
        record.runtime = Some(crate::RuntimeInfo {
            kind: codex_app_server::RUNTIME_KIND.to_string(),
            tmux_session: record.tmux_session.clone(),
            generation: 1,
            started_at: "2030-01-01T00:00:00Z".to_string(),
            launch_id: "launch-a".to_string(),
            extra: std::collections::BTreeMap::from([
                (
                    codex_app_server::PROTOCOL_KEY.to_string(),
                    json!(codex_app_server::PROTOCOL_VERSION),
                ),
                (
                    codex_app_server::SOCKET_KEY.to_string(),
                    json!(crate::display_path(&socket)),
                ),
                (
                    codex_app_server::PROXY_KEY.to_string(),
                    json!(crate::display_path(&socket.with_extension("proxy"))),
                ),
                (
                    codex_app_server::THREAD_HANDOFF_KEY.to_string(),
                    json!(crate::display_path(&socket.with_extension("thread"))),
                ),
                (
                    codex_app_server::THREAD_ATTACHED_KEY.to_string(),
                    json!(crate::display_path(&socket.with_extension("attached"))),
                ),
            ]),
        });
        crate::write_session_record(&context, &record).unwrap();
        crate::activity::activate_runtime(&context, &record).unwrap();
        auto_resume::set_enabled(&context, &record.id, true, "2030-01-01T00:00:00Z").unwrap();
        crate::activity::ingest_codex_app_server_failure(
            &context, &record.id, "launch-a", "thread-a", "turn-a",
        )
        .unwrap();
        assert_eq!(
            auto_resume::tick_for_runtime(
                &context,
                &record.id,
                "launch-a",
                1_893_456_000,
                &UsageSnapshot {
                    authoritative: true,
                    has_exhausted_windows: true,
                    exhausted_reset_epochs: vec![1_893_456_600],
                },
                |_| panic!("blocked usage must not submit"),
            )
            .unwrap(),
            auto_resume::TickOutcome::Scheduled
        );

        let target = CodexAutoResumeTarget {
            id: record.id.clone(),
            launch_id: "launch-a".to_string(),
            binding: crate::codex_account::binding_snapshot(&record),
        };
        record.runtime.as_mut().unwrap().launch_id = "launch-b".to_string();
        crate::write_session_record(&context, &record).unwrap();
        let st = state(&context.state_dir, Some(TOKEN), minimal_tmux(tmp.path()));
        process_codex_auto_resume_id(st, target).await;

        let view = auto_resume::view_for_record(&context, &record);
        assert_eq!(view.state, "scheduled");
        assert!(view.failure_reason.is_none());
    }

    #[test]
    fn codex_control_discovery_uses_one_bulk_tmux_snapshot() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        for (id, tmux_session) in [("first", "hs-first"), ("second", "hs-second")] {
            seed_session(&context.state_dir, id, "codex", tmux_session);
            let mut record = load_session_record(&context, id).unwrap();
            let socket = tmp.path().join(format!("{id}.sock"));
            record.runtime = Some(crate::RuntimeInfo {
                kind: codex_app_server::RUNTIME_KIND.to_string(),
                tmux_session: tmux_session.to_string(),
                generation: 1,
                started_at: "2030-01-01T00:00:00Z".to_string(),
                launch_id: format!("launch-{id}"),
                extra: std::collections::BTreeMap::from([
                    (
                        codex_app_server::PROTOCOL_KEY.to_string(),
                        json!(codex_app_server::PROTOCOL_VERSION),
                    ),
                    (
                        codex_app_server::SOCKET_KEY.to_string(),
                        json!(crate::display_path(&socket)),
                    ),
                    (
                        codex_app_server::PROXY_KEY.to_string(),
                        json!(crate::display_path(&socket.with_extension("proxy"))),
                    ),
                    (
                        codex_app_server::THREAD_HANDOFF_KEY.to_string(),
                        json!(crate::display_path(&socket.with_extension("thread"))),
                    ),
                    (
                        codex_app_server::THREAD_ATTACHED_KEY.to_string(),
                        json!(crate::display_path(&socket.with_extension("attached"))),
                    ),
                ]),
            });
            crate::write_session_record(&context, &record).unwrap();
        }
        let calls = tmp.path().join("tmux.calls");
        let tmux = executable(
            &tmp.path().join("tmux-bulk"),
            &format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> {}\nif [ \"$1\" = list-windows ]; then printf 'hs-first\\t100\\nhs-second\\t100\\n'; exit 0; fi\nexit 1\n",
                shell_words::quote(&calls.to_string_lossy())
            ),
        );

        let records = discover_codex_controls(&context, &tmux);
        assert_eq!(records.len(), 2);
        let calls = fs::read_to_string(calls).unwrap();
        assert_eq!(calls.lines().count(), 1, "calls={calls}");
        assert!(calls.contains("list-windows -a -F"));
        assert!(!calls.contains("has-session"));
    }

    #[tokio::test]
    async fn serve_created_capable_codex_session_projects_supported_app_server_runtime() {
        let lock = GlobalStateLock::new();
        let tmp = tempfile::TempDir::new().unwrap();
        let cwd = tmp.path().join("repo");
        let runtime_dir = tempfile::Builder::new()
            .prefix("cx-")
            .tempdir_in("/tmp")
            .unwrap();
        fs::set_permissions(runtime_dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let log = tmp.path().join("tmux.log");
        fs::create_dir(&cwd).unwrap();
        let codex = executable(
            &tmp.path().join("codex-app-server"),
            "#!/usr/bin/env sh\nif [ \"$1\" = --version ]; then printf '%s\\n' 'codex-cli 0.144.1'; exit 0; fi\nif [ \"$1\" = app-server ] && [ \"$2\" = --help ]; then printf '%s\\n' '  --listen <URL>  unix://'; exit 0; fi\nexit 1\n",
        );
        let _codex_bin = EnvGuard::set(&lock, "AGENT_SESSION_CODEX_BIN", codex.to_str().unwrap());
        let _runtime_dir = EnvGuard::set(
            &lock,
            "XDG_RUNTIME_DIR",
            runtime_dir.path().to_str().unwrap(),
        );
        let _runtime_mode = EnvGuard::set(&lock, "AGENT_SESSION_CODEX_RUNTIME", "app-server");
        let _capture_timeout = EnvGuard::set(&lock, "AGENT_SESSION_CODEX_CAPTURE_TIMEOUT_MS", "10");
        let st = state(tmp.path(), Some(TOKEN), logging_tmux(tmp.path(), &log));

        let (status, body) = call(
            router(st),
            post_json(
                "/sessions",
                Some(TOKEN),
                json!({
                    "agent": "codex",
                    "id": "managed-codex",
                    "cwd": cwd.to_string_lossy()
                }),
            ),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "body={body}");
        let session = &body["data"]["session"];
        assert_eq!(session["auto_resume"]["supported"], true);
        let record: Value = serde_json::from_slice(
            &fs::read(tmp.path().join("sessions/managed-codex/session.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(record["runtime"]["kind"], codex_app_server::RUNTIME_KIND);
        assert_eq!(
            record["runtime"]["codex_app_server_protocol"],
            codex_app_server::PROTOCOL_VERSION
        );
        let handoff = record["runtime"][codex_app_server::THREAD_HANDOFF_KEY]
            .as_str()
            .unwrap();
        assert!(!Path::new(handoff).exists());
        let calls = fs::read_to_string(log).unwrap();
        assert!(calls.contains("agent-session-codex-app-server"));
        assert!(calls.contains("app-server --listen"));
    }

    // The WebSocket attach handler guards with the same `deny_unauthorized` as
    // the write handlers; the `WebSocketUpgrade` extractor cannot be driven
    // through `oneshot` (it needs a real upgradable connection), so the guard is
    // tested directly here and the streaming path is covered by a manual smoke.
    #[test]
    fn deny_unauthorized_enforces_bearer_token() {
        let tmp = tempfile::TempDir::new().unwrap();

        // No token configured -> fail closed (503) even with a bearer presented.
        let st = state(tmp.path(), None, PathBuf::from("tmux"));
        assert!(deny_unauthorized(&st, &auth_headers(Some(TOKEN))).is_some());

        // Token configured: missing / wrong -> denied; correct -> allowed.
        let st = state(tmp.path(), Some(TOKEN), PathBuf::from("tmux"));
        assert!(deny_unauthorized(&st, &auth_headers(None)).is_some());
        assert!(deny_unauthorized(&st, &auth_headers(Some("nope"))).is_some());
        // Equal-length but wrong token exercises the constant-time byte loop's
        // deny path (a shorter wrong token short-circuits on the length check).
        let same_len_wrong = "X".repeat(TOKEN.len());
        assert_eq!(same_len_wrong.len(), TOKEN.len());
        assert!(deny_unauthorized(&st, &auth_headers(Some(&same_len_wrong))).is_some());
        assert!(deny_unauthorized(&st, &auth_headers(Some(TOKEN))).is_none());
    }

    #[test]
    fn attach_status_errors_distinguish_unknown_from_non_running() {
        let record = test_record("attach-me", "hs-codex-attach-me");
        let stopped = attach_unavailable_error(&record, "stopped").into_inner();
        assert_eq!(stopped.code, "session-not-running");
        assert_eq!(stopped.exit_code, exit::DATA);
        assert_eq!(
            stopped.details.unwrap()["status"],
            Value::String("stopped".to_string())
        );

        let unknown = attach_unavailable_error(&record, "unknown").into_inner();
        assert_eq!(unknown.code, "session-status-unknown");
        assert_eq!(unknown.exit_code, exit::RUNTIME);
    }

    #[test]
    fn sanitize_token_blanks_fail_closed() {
        // An empty/whitespace --token collapses to None so serve fails closed
        // instead of authorizing an empty bearer.
        assert_eq!(sanitize_token(Some(String::new())), None);
        assert_eq!(sanitize_token(Some("   ".to_string())), None);
        assert_eq!(sanitize_token(None), None);
        assert_eq!(
            sanitize_token(Some("tok".to_string())),
            Some("tok".to_string())
        );
    }

    #[tokio::test]
    async fn missing_token_config_disables_writes_fail_closed() {
        let tmp = tempfile::TempDir::new().unwrap();
        let st = state(tmp.path(), None, PathBuf::from("tmux"));
        let (status, body) = call(
            router(st.clone()),
            post_json("/sessions", Some(TOKEN), json!({"agent": "codex"})),
        )
        .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["error"]["code"], "token-not-configured");
    }

    #[tokio::test]
    async fn create_imports_codex_session_from_provider_resume_id() {
        let lock = GlobalStateLock::new();
        let tmp = tempfile::TempDir::new().unwrap();
        let cwd = tmp.path().join("repo");
        let codex_home = tmp.path().join("codex-home");
        std::fs::create_dir_all(&cwd).unwrap();
        write_codex_session_meta(&codex_home, "external-codex-id", &cwd);
        let log = tmp.path().join("tmux.log");
        let tmux = logging_tmux(tmp.path(), &log);
        let codex = fake_agent(tmp.path(), "codex");
        let _codex_home = EnvGuard::set(&lock, "CODEX_HOME", codex_home.to_str().unwrap());
        let _codex_bin = EnvGuard::set(&lock, "AGENT_SESSION_CODEX_BIN", codex.to_str().unwrap());
        let st = state(tmp.path(), Some(TOKEN), tmux);

        let (status, body) = call(
            router(st.clone()),
            post_json(
                "/sessions",
                Some(TOKEN),
                json!({
                    "agent": "codex",
                    "id": "imported-codex",
                    "provider_resume_id": "external-codex-id",
                    "title": "Imported Codex"
                }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body={body}");
        let session = &body["data"]["session"];
        assert_eq!(session["id"], "imported-codex");
        assert_eq!(session["agent"], "codex");
        assert_eq!(session["cwd"], cwd.to_string_lossy().as_ref());
        assert_eq!(session["status"], "running");
        assert_eq!(session["resumable"], true);
        assert_eq!(
            session["provider_resume"]["session_id"],
            "external-codex-id"
        );
        assert_eq!(
            session["provider_resume"]["resume_args"],
            json!([
                "resume",
                "external-codex-id",
                "--cd",
                cwd.to_string_lossy(),
                "--no-alt-screen"
            ])
        );

        let calls = std::fs::read_to_string(&log).unwrap();
        assert!(
            calls.contains("new-session -d -s hs-codex-imported-codex"),
            "import must create a tmux runtime: {calls:?}"
        );
        assert!(
            calls.contains("resume external-codex-id"),
            "import must use the external provider id: {calls:?}"
        );
        assert!(
            calls.contains("--cd") && calls.contains(cwd.to_string_lossy().as_ref()),
            "import must resume in the provider cwd: {calls:?}"
        );
        assert!(
            tmp.path()
                .join("sessions/imported-codex/resume.json")
                .is_file(),
            "import must persist durable resume metadata"
        );
    }

    #[tokio::test]
    async fn create_imports_claude_session_from_resume_id_alias() {
        let lock = GlobalStateLock::new();
        let tmp = tempfile::TempDir::new().unwrap();
        let cwd = tmp.path().join("repo");
        let claude_config = tmp.path().join("claude-config");
        std::fs::create_dir_all(&cwd).unwrap();
        write_claude_session_transcript(
            &claude_config,
            "-fixture-project",
            "external-claude-id",
            &cwd,
        );
        let log = tmp.path().join("tmux.log");
        let tmux = logging_tmux(tmp.path(), &log);
        let claude = fake_agent(tmp.path(), "claude");
        let _claude_config =
            EnvGuard::set(&lock, "CLAUDE_CONFIG_DIR", claude_config.to_str().unwrap());
        let _claude_bin =
            EnvGuard::set(&lock, "AGENT_SESSION_CLAUDE_BIN", claude.to_str().unwrap());
        let st = state(tmp.path(), Some(TOKEN), tmux);

        let (status, body) = call(
            router(st.clone()),
            post_json(
                "/sessions",
                Some(TOKEN),
                json!({
                    "agent": "claude",
                    "id": "imported-claude",
                    "resume_id": "external-claude-id",
                    "title": "Imported Claude"
                }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body={body}");
        let session = &body["data"]["session"];
        assert_eq!(session["id"], "imported-claude");
        assert_eq!(session["agent"], "claude");
        assert_eq!(session["cwd"], cwd.to_string_lossy().as_ref());
        assert_eq!(session["status"], "running");
        assert_eq!(session["resumable"], true);
        assert_eq!(
            session["provider_resume"]["session_id"],
            "external-claude-id"
        );
        assert_eq!(
            session["provider_resume"]["resume_args"],
            json!(["--resume", "external-claude-id"])
        );

        let calls = std::fs::read_to_string(&log).unwrap();
        assert!(
            calls.contains("new-session -d -s hs-claude-imported-claude"),
            "import must create a tmux runtime: {calls:?}"
        );
        assert!(
            calls.contains("--resume external-claude-id"),
            "import must use the external provider id: {calls:?}"
        );
        assert!(
            !calls.contains("--session-id"),
            "imported Claude resume must not create a new provider session: {calls:?}"
        );
        assert!(
            tmp.path()
                .join("sessions/imported-claude/resume.json")
                .is_file(),
            "import must persist durable resume metadata"
        );
    }

    #[tokio::test]
    async fn create_provider_resume_id_rejects_cwd_and_prompt_overrides() {
        let tmp = tempfile::TempDir::new().unwrap();
        let st = state(tmp.path(), Some(TOKEN), minimal_tmux(tmp.path()));

        let (status, body) = call(
            router(st.clone()),
            post_json(
                "/sessions",
                Some(TOKEN),
                json!({
                    "agent": "codex",
                    "provider_resume_id": "external-id",
                    "cwd": "/should-not-override"
                }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], "provider-resume-cwd-conflict");

        let (status, body) = call(
            router(st.clone()),
            post_json(
                "/sessions",
                Some(TOKEN),
                json!({
                    "agent": "codex",
                    "provider_resume_id": "external-id",
                    "prompt": "continue"
                }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], "provider-resume-prompt-conflict");

        let (status, body) = call(
            router(st.clone()),
            post_json(
                "/sessions",
                Some(TOKEN),
                json!({
                    "agent": "codex",
                    "provider_resume_id": "external-id",
                    "agent_args": ["--cd", "/should-not-override"]
                }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], "provider-resume-agent-args-conflict");
    }

    #[tokio::test]
    async fn create_provider_resume_id_reports_not_found() {
        let lock = GlobalStateLock::new();
        let tmp = tempfile::TempDir::new().unwrap();
        let codex_home = tmp.path().join("codex-home");
        std::fs::create_dir_all(codex_home.join("sessions")).unwrap();
        let _codex_home = EnvGuard::set(&lock, "CODEX_HOME", codex_home.to_str().unwrap());
        let st = state(tmp.path(), Some(TOKEN), minimal_tmux(tmp.path()));

        let (status, body) = call(
            router(st.clone()),
            post_json(
                "/sessions",
                Some(TOKEN),
                json!({
                    "agent": "codex",
                    "provider_resume_id": "missing-codex-id"
                }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body["error"]["code"], "provider-resume-not-found");
        assert_eq!(
            body["error"]["details"]["provider_resume_id"],
            "missing-codex-id"
        );
    }

    #[tokio::test]
    async fn create_provider_resume_id_rejects_invalid_values() {
        let tmp = tempfile::TempDir::new().unwrap();
        let st = state(tmp.path(), Some(TOKEN), minimal_tmux(tmp.path()));

        for provider_resume_id in ["", "bad\nid"] {
            let (status, body) = call(
                router(st.clone()),
                post_json(
                    "/sessions",
                    Some(TOKEN),
                    json!({
                        "agent": "codex",
                        "provider_resume_id": provider_resume_id
                    }),
                ),
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert_eq!(body["error"]["code"], "invalid-provider-resume-id");
        }
    }

    #[tokio::test]
    async fn create_provider_resume_id_reports_ambiguous_cwd_matches() {
        let lock = GlobalStateLock::new();
        let tmp = tempfile::TempDir::new().unwrap();
        let codex_home = tmp.path().join("codex-home");
        let cwd_a = tmp.path().join("repo-a");
        let cwd_b = tmp.path().join("repo-b");
        std::fs::create_dir_all(&cwd_a).unwrap();
        std::fs::create_dir_all(&cwd_b).unwrap();
        write_codex_session_meta_file(&codex_home, "first.jsonl", "shared-codex-id", &cwd_a);
        write_codex_session_meta_file(&codex_home, "second.jsonl", "shared-codex-id", &cwd_b);
        let _codex_home = EnvGuard::set(&lock, "CODEX_HOME", codex_home.to_str().unwrap());
        let st = state(tmp.path(), Some(TOKEN), minimal_tmux(tmp.path()));

        let (status, body) = call(
            router(st.clone()),
            post_json(
                "/sessions",
                Some(TOKEN),
                json!({
                    "agent": "codex",
                    "provider_resume_id": "shared-codex-id"
                }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body["error"]["code"], "provider-resume-ambiguous");
        assert_eq!(body["error"]["details"]["cwd_count"], 2);
    }

    #[tokio::test]
    async fn create_provider_resume_id_reports_scan_truncation() {
        let lock = GlobalStateLock::new();
        let tmp = tempfile::TempDir::new().unwrap();
        let codex_home = tmp.path().join("codex-home");
        let cwd = tmp.path().join("repo");
        std::fs::create_dir_all(&cwd).unwrap();
        write_codex_session_meta(&codex_home, "external-codex-id", &cwd);
        let _codex_home = EnvGuard::set(&lock, "CODEX_HOME", codex_home.to_str().unwrap());
        let _max_entries = EnvGuard::set(&lock, "AGENT_SESSION_CODEX_RESUME_SCAN_MAX_ENTRIES", "1");
        let st = state(tmp.path(), Some(TOKEN), minimal_tmux(tmp.path()));

        let (status, body) = call(
            router(st.clone()),
            post_json(
                "/sessions",
                Some(TOKEN),
                json!({
                    "agent": "codex",
                    "provider_resume_id": "external-codex-id"
                }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body["error"]["code"], "provider-resume-scan-truncated");
    }

    #[tokio::test]
    async fn create_imports_claude_session_after_oversized_transcript_line() {
        let lock = GlobalStateLock::new();
        let tmp = tempfile::TempDir::new().unwrap();
        let cwd = tmp.path().join("repo");
        let claude_config = tmp.path().join("claude-config");
        let bad_transcript = claude_config
            .join("projects")
            .join("-fixture-project")
            .join("oversized.jsonl");
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::create_dir_all(bad_transcript.parent().expect("parent")).unwrap();
        std::fs::write(
            &bad_transcript,
            "x".repeat(nils_provider_resume::CLAUDE_SESSION_META_MAX_LINE_BYTES + 1),
        )
        .unwrap();
        write_claude_session_transcript(
            &claude_config,
            "-fixture-project",
            "external-claude-id",
            &cwd,
        );
        let log = tmp.path().join("tmux.log");
        let tmux = logging_tmux(tmp.path(), &log);
        let claude = fake_agent(tmp.path(), "claude");
        let _claude_config =
            EnvGuard::set(&lock, "CLAUDE_CONFIG_DIR", claude_config.to_str().unwrap());
        let _claude_bin =
            EnvGuard::set(&lock, "AGENT_SESSION_CLAUDE_BIN", claude.to_str().unwrap());
        let st = state(tmp.path(), Some(TOKEN), tmux);

        let (status, body) = call(
            router(st.clone()),
            post_json(
                "/sessions",
                Some(TOKEN),
                json!({
                    "agent": "claude",
                    "id": "imported-claude-oversized",
                    "provider_resume_id": "external-claude-id"
                }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body={body}");
        assert_eq!(
            body["data"]["session"]["cwd"],
            cwd.to_string_lossy().as_ref()
        );
    }

    #[tokio::test]
    async fn create_provider_resume_id_rejects_unsupported_agent() {
        let tmp = tempfile::TempDir::new().unwrap();
        let st = state(tmp.path(), Some(TOKEN), minimal_tmux(tmp.path()));

        let (status, body) = call(
            router(st.clone()),
            post_json(
                "/sessions",
                Some(TOKEN),
                json!({
                    "agent": "hermes",
                    "provider_resume_id": "external-id"
                }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], "unsupported-provider-resume-agent");
    }

    #[tokio::test]
    async fn send_with_token_delivers_without_leaking_text() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tmux = minimal_tmux(tmp.path());
        seed_session(tmp.path(), "steer", "codex", "hs-codex-steer");
        let st = state(tmp.path(), Some(TOKEN), tmux);

        let secret = "serve-secret-approval";
        let (status, body) = call(
            router(st.clone()),
            post_json(
                "/sessions/steer/send",
                Some(TOKEN),
                json!({ "text": secret, "keys": ["enter"] }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body={body}");
        assert_eq!(body["schema_version"], "cli.agent-session.serve.v1");
        assert_eq!(body["data"]["machine"], MACHINE);
        assert_eq!(body["data"]["sent"]["sent_text"], true);
        assert_eq!(body["data"]["sent"]["keys"][0], "enter");
        // The literal text is never echoed back into the response contract.
        assert!(
            !serde_json::to_string(&body).unwrap().contains(secret),
            "secret text leaked into serve response: {body}"
        );
    }

    #[tokio::test]
    async fn send_rejects_unknown_key() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tmux = minimal_tmux(tmp.path());
        seed_session(tmp.path(), "steer", "codex", "hs-codex-steer");
        let st = state(tmp.path(), Some(TOKEN), tmux);
        let (status, body) = call(
            router(st.clone()),
            post_json(
                "/sessions/steer/send",
                Some(TOKEN),
                json!({ "keys": ["banana"] }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], "invalid-key");
    }

    #[tokio::test]
    async fn structured_prompt_submits_exact_multiline_without_terminal_paste() {
        let tmp = tempfile::TempDir::new().unwrap();
        let launch_id = seed_codex_app_server_session(tmp.path(), "structured");
        let st = state(
            tmp.path(),
            Some(TOKEN),
            PathBuf::from("/terminal-transport-must-not-run"),
        );
        let record = load_session_record(&st.context, "structured").unwrap();
        crate::activity::activate_runtime(&st.context, &record).unwrap();
        auto_resume::set_enabled(&st.context, &record.id, true, "2030-01-01T00:00:00Z").unwrap();
        let (handle, mut commands) = codex_app_server::control_channel();
        st.codex_controls.lock().unwrap().insert(
            "structured".to_string(),
            CodexControlEntry { launch_id, handle },
        );
        let prompt = "inspect first line\nthen second line";
        let responder_context = st.context.clone();
        let responder_record = record.clone();
        let responder = tokio::spawn(async move {
            let Some(codex_app_server::ControlCommand::Prompt { message, response }) =
                commands.recv().await
            else {
                panic!("structured prompt did not reach the Codex control plane");
            };
            assert_eq!(message, prompt);
            let view = auto_resume::view_for_record(&responder_context, &responder_record);
            assert!(view.enabled);
            assert_eq!(view.state, "enabled");
            response.send(Ok("turn-structured".to_string())).unwrap();
        });

        let (status, body) = call(
            router(st.clone()),
            post_json(
                "/sessions/structured/prompt",
                None,
                json!({ "text": prompt }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "body={body}");

        let (status, body) = call(
            router(st),
            post_json(
                "/sessions/structured/prompt",
                Some(TOKEN),
                json!({ "text": prompt }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body={body}");
        assert_eq!(body["data"]["submitted"], true);
        assert!(body.pointer("/data/turn_id").is_none());
        responder.await.unwrap();
    }

    #[tokio::test]
    async fn codex_account_switch_routes_the_nickname_to_the_exact_runtime_control() {
        let lock = GlobalStateLock::new();
        let tmp = tempfile::TempDir::new().unwrap();
        let _broker = EnvGuard::set(
            &lock,
            "AGENT_SESSION_CODEX_ACCOUNT_BROKER",
            r#"["/bin/false"]"#,
        );
        let launch_id = seed_codex_app_server_session(tmp.path(), "account-switch");
        let st = state(tmp.path(), Some(TOKEN), minimal_tmux(tmp.path()));
        let record = load_session_record(&st.context, "account-switch").unwrap();
        crate::activity::activate_runtime(&st.context, &record).unwrap();
        for (event_id, kind) in [
            ("turn-start", "turn_started"),
            ("turn-done", "turn_completed"),
        ] {
            let event = serde_json::from_value(json!({
                "schema_version": crate::activity::TURN_EVENT_VERSION,
                "event_id": event_id,
                "runtime_id": launch_id,
                "provider": "codex",
                "provider_turn_id": "turn-account-switch",
                "kind": kind,
                "confidence": "authoritative"
            }))
            .unwrap();
            crate::activity::ingest_event(&st.context, &record.id, event).unwrap();
        }
        let (handle, mut commands) = codex_app_server::control_channel();
        st.codex_controls.lock().unwrap().insert(
            record.id.clone(),
            CodexControlEntry {
                launch_id: launch_id.clone(),
                handle,
            },
        );
        let responder_launch_id = launch_id.clone();
        let responder = tokio::spawn(async move {
            let Some(codex_app_server::ControlCommand::BindAccount {
                account,
                revision,
                response,
            }) = commands.recv().await
            else {
                panic!("account switch did not reach the Codex control plane");
            };
            assert_eq!(account, "gamania");
            assert_eq!(revision, 1);
            response
                .send(Ok(crate::codex_account::CodexAccountView {
                    schema_version: crate::codex_account::VIEW_SCHEMA_VERSION,
                    supported: true,
                    state: "bound",
                    selected_account: Some(account),
                    revision: 1,
                    applied_runtime_id: Some(responder_launch_id),
                    failure_reason: None,
                }))
                .unwrap();
        });

        let (status, body) = call(
            router(st),
            put_json(
                "/sessions/account-switch/account",
                Some(TOKEN),
                json!({ "account": "gamania", "expected_session_incarnation": launch_id }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body={body}");
        assert_eq!(body["data"]["codex_account"]["state"], "bound");
        assert_eq!(body["data"]["codex_account"]["selected_account"], "gamania");
        assert_eq!(body["data"]["codex_account"]["revision"], 1);
        responder.await.unwrap();
    }

    #[tokio::test]
    async fn codex_account_switch_serializes_the_complete_same_session_transaction() {
        let lock = GlobalStateLock::new();
        let tmp = tempfile::TempDir::new().unwrap();
        let _broker = EnvGuard::set(
            &lock,
            "AGENT_SESSION_CODEX_ACCOUNT_BROKER",
            r#"["/bin/false"]"#,
        );
        let launch_id = seed_codex_app_server_session(tmp.path(), "account-switch-serialized");
        let st = state(tmp.path(), Some(TOKEN), minimal_tmux(tmp.path()));
        let record = load_session_record(&st.context, "account-switch-serialized").unwrap();
        crate::activity::activate_runtime(&st.context, &record).unwrap();
        for (event_id, kind) in [
            ("turn-start", "turn_started"),
            ("turn-done", "turn_completed"),
        ] {
            let event = serde_json::from_value(json!({
                "schema_version": crate::activity::TURN_EVENT_VERSION,
                "event_id": event_id,
                "runtime_id": launch_id,
                "provider": "codex",
                "provider_turn_id": "turn-account-switch-serialized",
                "kind": kind,
                "confidence": "authoritative"
            }))
            .unwrap();
            crate::activity::ingest_event(&st.context, &record.id, event).unwrap();
        }
        let (handle, mut commands) = codex_app_server::control_channel();
        st.codex_controls.lock().unwrap().insert(
            record.id.clone(),
            CodexControlEntry {
                launch_id: launch_id.clone(),
                handle,
            },
        );

        let first_launch_id = launch_id.clone();
        let first = tokio::spawn(call(
            router(st.clone()),
            put_json(
                "/sessions/account-switch-serialized/account",
                Some(TOKEN),
                json!({
                    "account": "gamania",
                    "expected_session_incarnation": first_launch_id
                }),
            ),
        ));
        let Some(codex_app_server::ControlCommand::BindAccount {
            account: first_account,
            revision: first_revision,
            response: first_response,
        }) = tokio::time::timeout(Duration::from_secs(1), commands.recv())
            .await
            .expect("first switch must reach the control plane")
        else {
            panic!("first switch sent an unexpected control command");
        };
        assert_eq!(first_account, "gamania");
        assert_eq!(first_revision, 1);

        let second_launch_id = launch_id.clone();
        let second = tokio::spawn(call(
            router(st.clone()),
            put_json(
                "/sessions/account-switch-serialized/account",
                Some(TOKEN),
                json!({
                    "account": "sym",
                    "expected_session_incarnation": second_launch_id
                }),
            ),
        ));
        assert!(
            tokio::time::timeout(Duration::from_millis(100), commands.recv())
                .await
                .is_err(),
            "the second same-session switch must not enter the control plane before the first finishes"
        );

        first_response
            .send(Ok(crate::codex_account::CodexAccountView {
                schema_version: crate::codex_account::VIEW_SCHEMA_VERSION,
                supported: true,
                state: "bound",
                selected_account: Some(first_account),
                revision: first_revision,
                applied_runtime_id: Some(launch_id.clone()),
                failure_reason: None,
            }))
            .unwrap();
        assert_eq!(first.await.unwrap().0, StatusCode::OK);

        let Some(codex_app_server::ControlCommand::BindAccount {
            account: second_account,
            revision: second_revision,
            response: second_response,
        }) = tokio::time::timeout(Duration::from_secs(1), commands.recv())
            .await
            .expect("second switch must reach the control plane after the first finishes")
        else {
            panic!("second switch sent an unexpected control command");
        };
        assert_eq!(second_account, "sym");
        assert_eq!(second_revision, 2);
        second_response
            .send(Ok(crate::codex_account::CodexAccountView {
                schema_version: crate::codex_account::VIEW_SCHEMA_VERSION,
                supported: true,
                state: "bound",
                selected_account: Some(second_account),
                revision: second_revision,
                applied_runtime_id: Some(launch_id),
                failure_reason: None,
            }))
            .unwrap();
        assert_eq!(second.await.unwrap().0, StatusCode::OK);
    }

    #[tokio::test]
    async fn create_binding_wait_rejects_a_superseding_account_revision() {
        let lock = GlobalStateLock::new();
        let tmp = tempfile::TempDir::new().unwrap();
        let _broker = EnvGuard::set(
            &lock,
            "AGENT_SESSION_CODEX_ACCOUNT_BROKER",
            r#"["/bin/false"]"#,
        );
        let launch_id = seed_codex_app_server_session(tmp.path(), "create-binding-race");
        let st = state(tmp.path(), Some(TOKEN), minimal_tmux(tmp.path()));
        let mut record = load_session_record(&st.context, "create-binding-race").unwrap();
        crate::codex_account::set_initial_binding(&mut record, Some("sym")).unwrap();
        crate::write_session_record(&st.context, &record).unwrap();
        crate::codex_account::finish_binding(&st.context, &record.id, &launch_id, "sym", 1, Ok(()))
            .unwrap();

        let response = wait_for_account_binding(&st, &record.id, &launch_id, "gamania", 1)
            .await
            .unwrap_err();
        assert_eq!(response.status(), StatusCode::CONFLICT);

        let (handle, mut commands) = codex_app_server::control_channel();
        let response = submit_structured_prompt_locked(
            &st,
            &record.id,
            &launch_id,
            Some(("gamania", 1)),
            &handle,
            "must not reach superseding account",
        )
        .await
        .unwrap_err();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert!(matches!(
            commands.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ));
    }

    #[tokio::test]
    async fn structured_prompt_cancels_armed_auto_resume_before_provider_control() {
        let tmp = tempfile::TempDir::new().unwrap();
        let launch_id = seed_codex_app_server_session(tmp.path(), "structured-armed");
        let st = state(tmp.path(), Some(TOKEN), minimal_tmux(tmp.path()));
        let record = load_session_record(&st.context, "structured-armed").unwrap();
        crate::activity::activate_runtime(&st.context, &record).unwrap();
        auto_resume::set_enabled(&st.context, &record.id, true, "2030-01-01T00:00:00Z").unwrap();
        crate::activity::ingest_codex_app_server_failure(
            &st.context,
            &record.id,
            &launch_id,
            "thread-armed",
            "turn-armed",
        )
        .unwrap();
        assert_eq!(
            auto_resume::view_for_record(&st.context, &record).state,
            "armed"
        );

        let (handle, mut commands) = codex_app_server::control_channel();
        st.codex_controls
            .lock()
            .unwrap()
            .insert(record.id.clone(), CodexControlEntry { launch_id, handle });
        let responder_context = st.context.clone();
        let responder_record = record.clone();
        let responder = tokio::spawn(async move {
            let Some(codex_app_server::ControlCommand::Prompt { response, .. }) =
                commands.recv().await
            else {
                panic!("structured prompt did not reach the Codex control plane");
            };
            let view = auto_resume::view_for_record(&responder_context, &responder_record);
            assert!(!view.enabled);
            assert_eq!(view.state, "cancelled");
            assert_eq!(view.failure_reason.as_deref(), Some("manual_input"));
            response
                .send(Ok("turn-armed-cancelled".to_string()))
                .unwrap();
        });

        let (status, body) = call(
            router(st),
            post_json(
                "/sessions/structured-armed/prompt",
                Some(TOKEN),
                json!({ "text": "new\nmanual task" }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body={body}");
        responder.await.unwrap();
    }

    #[test]
    fn account_switch_cancels_and_versions_codex_auto_resume_evidence() {
        let lock = GlobalStateLock::new();
        let tmp = tempfile::TempDir::new().unwrap();
        let _broker = EnvGuard::set(
            &lock,
            "AGENT_SESSION_CODEX_ACCOUNT_BROKER",
            r#"["/bin/false"]"#,
        );
        let launch_id = seed_codex_app_server_session(tmp.path(), "account-auto-resume");
        let context = CliContext {
            state_dir: tmp.path().to_path_buf(),
            host: None,
        };
        let mut record = load_session_record(&context, "account-auto-resume").unwrap();
        crate::codex_account::set_initial_binding(&mut record, Some("gamania")).unwrap();
        crate::write_session_record(&context, &record).unwrap();
        crate::codex_account::finish_binding(
            &context,
            &record.id,
            &launch_id,
            "gamania",
            1,
            Ok(()),
        )
        .unwrap();
        record = load_session_record(&context, &record.id).unwrap();
        crate::activity::activate_runtime(&context, &record).unwrap();
        auto_resume::set_enabled(&context, &record.id, true, "2030-01-01T00:00:00Z").unwrap();
        crate::activity::ingest_codex_app_server_failure(
            &context, &record.id, &launch_id, "thread-a", "turn-a",
        )
        .unwrap();
        let account_a = crate::codex_account::binding_snapshot(&record);

        crate::codex_account::begin_switch_binding(&context, &record.id, &launch_id, "sym")
            .unwrap();
        let mut submissions = 0;
        let outcome = auto_resume::tick_for_runtime_and_binding(
            &context,
            &record.id,
            &launch_id,
            &account_a,
            1_893_456_000,
            &UsageSnapshot {
                authoritative: true,
                has_exhausted_windows: false,
                exhausted_reset_epochs: Vec::new(),
            },
            |_| {
                submissions += 1;
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(outcome, auto_resume::TickOutcome::Unchanged);
        assert_eq!(submissions, 0);
        let view = auto_resume::view_for_record(&context, &record);
        assert_eq!(view.state, "cancelled");
        assert_eq!(view.failure_reason.as_deref(), Some("account_switch"));
    }

    #[tokio::test]
    async fn structured_prompt_rejects_unsupported_sessions_before_terminal_input() {
        let tmp = tempfile::TempDir::new().unwrap();
        seed_session(tmp.path(), "claude", "claude", "hs-claude-structured");
        let st = state(
            tmp.path(),
            Some(TOKEN),
            PathBuf::from("/terminal-transport-must-not-run"),
        );

        let (status, body) = call(
            router(st),
            post_json(
                "/sessions/claude/prompt",
                Some(TOKEN),
                json!({ "text": "first\nsecond" }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "body={body}");
        assert_eq!(body["error"]["code"], "structured-prompt-unsupported");
    }

    #[tokio::test]
    async fn structured_prompt_rejects_terminal_controls() {
        let tmp = tempfile::TempDir::new().unwrap();
        let launch_id = seed_codex_app_server_session(tmp.path(), "controls");
        let st = state(tmp.path(), Some(TOKEN), minimal_tmux(tmp.path()));
        let (handle, _commands) = codex_app_server::control_channel();
        st.codex_controls.lock().unwrap().insert(
            "controls".to_string(),
            CodexControlEntry { launch_id, handle },
        );

        for text in ["carriage\rreturn", "tab\tkey", "escape\u{1b}sequence"] {
            let (status, body) = call(
                router(st.clone()),
                post_json(
                    "/sessions/controls/prompt",
                    Some(TOKEN),
                    json!({ "text": text }),
                ),
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "body={body}");
            assert_eq!(body["error"]["code"], "unsafe-prompt-control");
        }
    }

    #[tokio::test]
    async fn resume_recreates_missing_runtime_with_token() {
        let lock = GlobalStateLock::new();
        let tmp = tempfile::TempDir::new().unwrap();
        let cwd = tmp.path().join("repo");
        std::fs::create_dir_all(&cwd).unwrap();
        let log = tmp.path().join("tmux.log");
        let tmux = resume_tmux(tmp.path(), &log);
        let codex = fake_agent(tmp.path(), "codex");
        let _codex_bin = EnvGuard::set(&lock, "AGENT_SESSION_CODEX_BIN", codex.to_str().unwrap());
        seed_resumable_session(
            tmp.path(),
            "recover",
            "codex",
            "hs-codex-recover",
            &cwd,
            &[
                "resume",
                "resume-session-id",
                "--cd",
                cwd.to_str().unwrap(),
                "--no-alt-screen",
            ],
        );
        let st = state(tmp.path(), Some(TOKEN), tmux);

        let (status, body) = call(
            router(st.clone()),
            post_json("/sessions/recover/resume", None, json!({})),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["error"]["code"], "unauthorized");

        let (status, body) = call(
            router(st.clone()),
            post_json("/sessions/recover/resume", Some(TOKEN), json!({})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body={body}");
        let session = &body["data"]["session"];
        assert_eq!(session["id"], "recover");
        assert_eq!(session["status"], "running");
        assert_eq!(session["resumable"], true);

        let calls = std::fs::read_to_string(&log).unwrap();
        assert!(
            calls.contains("new-session -d -s hs-codex-recover"),
            "resume must create a tmux runtime: {calls:?}"
        );
        assert!(
            calls.contains("resume resume-session-id"),
            "resume must use the exact provider id: {calls:?}"
        );
    }

    #[tokio::test]
    async fn resume_refuses_non_resumable_session_with_token() {
        let tmp = tempfile::TempDir::new().unwrap();
        let log = tmp.path().join("tmux.log");
        let tmux = resume_tmux(tmp.path(), &log);
        seed_session(tmp.path(), "plain", "codex", "hs-codex-plain");
        let st = state(tmp.path(), Some(TOKEN), tmux);

        let (status, body) = call(
            router(st.clone()),
            post_json("/sessions/plain/resume", Some(TOKEN), json!({})),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body["schema_version"], "cli.agent-session.serve.v1");
        assert_eq!(body["ok"], false);
        assert_eq!(body["error"]["code"], "session-not-resumable");
        let calls = std::fs::read_to_string(&log).unwrap_or_default();
        assert!(
            !calls.contains("new-session"),
            "non-resumable sessions must not create tmux runtimes: {calls:?}"
        );
    }

    #[tokio::test]
    async fn update_session_title_persists_and_clears_custom_title() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tmux = minimal_tmux(tmp.path());
        seed_session(tmp.path(), "steer", "codex", "hs-codex-steer");
        let st = state(tmp.path(), Some(TOKEN), tmux);

        let (status, body) = call(
            router(st.clone()),
            patch_json(
                "/sessions/steer",
                Some(TOKEN),
                json!({ "title": "Reviewed title" }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body={body}");
        assert_eq!(body["schema_version"], "cli.agent-session.serve.v1");
        assert_eq!(body["data"]["machine"], MACHINE);
        assert_eq!(body["data"]["session"]["title"], "Reviewed title");

        let record_path = tmp.path().join("sessions/steer/session.json");
        let record: Value =
            serde_json::from_str(&std::fs::read_to_string(&record_path).unwrap()).unwrap();
        assert_eq!(record["title"], "Reviewed title");
        assert_ne!(record["updated_at"], "2000-01-01T00:00:00Z");

        let (status, body) = call(
            router(st.clone()),
            patch_json("/sessions/steer", Some(TOKEN), json!({ "title": null })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body={body}");
        assert_eq!(body["data"]["session"]["title"], Value::Null);

        let (status, body) = call(
            router(st.clone()),
            patch_json("/sessions/steer", Some(TOKEN), json!({ "title": "" })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body={body}");
        assert_eq!(body["data"]["session"]["title"], Value::Null);

        let (status, body) = call(
            router(st.clone()),
            patch_json("/sessions/steer", Some(TOKEN), json!({})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], "missing-title");

        let too_long = "x".repeat(121);
        let (status, body) = call(
            router(st.clone()),
            patch_json("/sessions/steer", Some(TOKEN), json!({ "title": too_long })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], "title-too-long");

        let (status, body) = call(
            router(st.clone()),
            patch_json("/sessions/ghost", Some(TOKEN), json!({ "title": "Ghost" })),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"]["code"], "session-not-found");
    }

    #[tokio::test]
    async fn expected_session_title_uses_the_unicode_title_limit() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tmux = minimal_tmux(tmp.path());
        seed_session_with_runtime(
            tmp.path(),
            "unicode-title",
            "codex",
            "hs-codex-unicode-title",
        );
        let st = state(tmp.path(), Some(TOKEN), tmux);
        let observed_title = "標".repeat(120);

        let (status, body) = call(
            router(st.clone()),
            patch_json(
                "/sessions/unicode-title",
                Some(TOKEN),
                json!({ "title": observed_title }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body={body}");

        let (status, body) = call(
            router(st),
            patch_json(
                "/sessions/unicode-title",
                Some(TOKEN),
                json!({
                    "title": "精簡中文標題",
                    "expected_title_revision": 1,
                    "expected_session_created_at": "2000-01-01T00:00:00Z",
                    "expected_session_incarnation": "launch-unicode-title",
                    "expected_session_title": observed_title
                }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body={body}");
        assert_eq!(body["data"]["session"]["title"], "精簡中文標題");
        assert_eq!(body["data"]["session"]["title_revision"], 2);
    }

    #[tokio::test]
    async fn expected_session_title_repairs_a_long_existing_title() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tmux = minimal_tmux(tmp.path());
        seed_session_with_runtime(tmp.path(), "long-title", "codex", "hs-codex-long-title");
        let record_path = tmp.path().join("sessions/long-title/session.json");
        let long_existing_title = "previous raw prompt ".repeat(20);
        let mut record: Value =
            serde_json::from_str(&std::fs::read_to_string(&record_path).unwrap()).unwrap();
        record["title"] = json!(long_existing_title);
        std::fs::write(&record_path, serde_json::to_vec_pretty(&record).unwrap()).unwrap();
        let st = state(tmp.path(), Some(TOKEN), tmux);

        let (status, body) = call(
            router(st),
            patch_json(
                "/sessions/long-title",
                Some(TOKEN),
                json!({
                    "title": "Concise repaired title",
                    "expected_title_revision": 0,
                    "expected_session_created_at": "2000-01-01T00:00:00Z",
                    "expected_session_incarnation": "launch-long-title",
                    "expected_session_title": long_existing_title
                }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body={body}");
        assert_eq!(body["data"]["session"]["title"], "Concise repaired title");
        assert_eq!(body["data"]["session"]["title_revision"], 1);
    }

    #[tokio::test]
    async fn update_session_title_rejects_stale_revision() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tmux = minimal_tmux(tmp.path());
        seed_session_with_runtime(tmp.path(), "revision", "codex", "hs-codex-revision");
        let st = state(tmp.path(), Some(TOKEN), tmux);

        let (status, body) = call(
            router(st.clone()),
            patch_json(
                "/sessions/revision",
                Some(TOKEN),
                json!({
                    "title": "First title",
                    "expected_title_revision": 0,
                    "expected_session_created_at": "2000-01-01T00:00:00Z",
                    "expected_session_incarnation": "launch-revision",
                    "expected_session_title": null
                }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body={body}");
        assert_eq!(body["data"]["session"]["title"], "First title");
        assert_eq!(body["data"]["session"]["title_revision"], 1);

        let (status, body) = call(
            router(st.clone()),
            patch_json(
                "/sessions/revision",
                Some(TOKEN),
                json!({
                    "title": "Stale title",
                    "expected_title_revision": 0,
                    "expected_session_created_at": "2000-01-01T00:00:00Z"
                }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "body={body}");
        assert_eq!(body["error"]["code"], "title-revision-conflict");
        assert_eq!(body["error"]["details"]["expected_title_revision"], 0);
        assert_eq!(body["error"]["details"]["actual_title_revision"], 1);

        let (status, body) = call(
            router(st.clone()),
            patch_json(
                "/sessions/revision",
                Some(TOKEN),
                json!({
                    "title": "Second title",
                    "expected_title_revision": 1,
                    "expected_session_created_at": "2000-01-01T00:00:00Z"
                }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body={body}");
        assert_eq!(body["data"]["session"]["title"], "Second title");
        assert_eq!(body["data"]["session"]["title_revision"], 2);

        let (status, body) = call(
            router(st),
            patch_json(
                "/sessions/revision",
                Some(TOKEN),
                json!({
                    "title": "Second title",
                    "expected_title_revision": 1,
                    "expected_session_created_at": "2000-01-01T00:00:00Z"
                }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "body={body}");
        assert_eq!(body["error"]["code"], "title-revision-conflict");
        assert_eq!(body["error"]["details"]["actual_title_revision"], 2);

        let record_path = tmp.path().join("sessions/revision/session.json");
        let record: Value =
            serde_json::from_str(&std::fs::read_to_string(&record_path).unwrap()).unwrap();
        assert_eq!(record["title"], "Second title");
        assert_eq!(record["title_revision"], 2);
    }

    #[tokio::test]
    async fn update_session_title_allows_one_concurrent_writer_per_revision() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tmux = minimal_tmux(tmp.path());
        seed_session(
            tmp.path(),
            "concurrent-revision",
            "codex",
            "hs-codex-concurrent-revision",
        );
        let st = state(tmp.path(), Some(TOKEN), tmux);
        let barrier = Arc::new(tokio::sync::Barrier::new(3));

        let spawn_writer = |title: &'static str| {
            let st = st.clone();
            let barrier = barrier.clone();
            tokio::spawn(async move {
                barrier.wait().await;
                call(
                    router(st),
                    patch_json(
                        "/sessions/concurrent-revision",
                        Some(TOKEN),
                        json!({ "title": title, "expected_title_revision": 0 }),
                    ),
                )
                .await
            })
        };
        let first = spawn_writer("First contender");
        let second = spawn_writer("Second contender");
        barrier.wait().await;
        let (first, second) = tokio::join!(first, second);
        let responses = [first.expect("first writer"), second.expect("second writer")];
        let successes = responses
            .iter()
            .filter(|(status, _)| *status == StatusCode::OK)
            .count();
        let conflicts = responses
            .iter()
            .filter(|(status, body)| {
                *status == StatusCode::CONFLICT
                    && body["error"]["code"] == "title-revision-conflict"
            })
            .count();
        assert_eq!(successes, 1, "responses={responses:?}");
        assert_eq!(conflicts, 1, "responses={responses:?}");

        let record_path = tmp.path().join("sessions/concurrent-revision/session.json");
        let record: Value =
            serde_json::from_str(&std::fs::read_to_string(&record_path).unwrap()).unwrap();
        assert!(record["title"] == "First contender" || record["title"] == "Second contender");
        assert_eq!(record["title_revision"], 1);
    }

    #[tokio::test]
    async fn update_session_title_rejects_a_replaced_session_incarnation() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tmux = minimal_tmux(tmp.path());
        seed_session_with_runtime(tmp.path(), "recreated", "codex", "hs-codex-recreated");
        std::fs::remove_dir_all(tmp.path().join("sessions/recreated")).unwrap();
        seed_session_with_runtime(tmp.path(), "recreated", "codex", "hs-codex-recreated");
        let record_path = tmp.path().join("sessions/recreated/session.json");
        let mut replacement: Value =
            serde_json::from_str(&std::fs::read_to_string(&record_path).unwrap()).unwrap();
        replacement["runtime"]["launch_id"] = json!("launch-recreated-v2");
        std::fs::write(
            &record_path,
            serde_json::to_vec_pretty(&replacement).unwrap(),
        )
        .unwrap();
        let st = state(tmp.path(), Some(TOKEN), tmux);

        let (status, body) = call(
            router(st),
            patch_json(
                "/sessions/recreated",
                Some(TOKEN),
                json!({
                    "title": "Stale incarnation title",
                    "expected_title_revision": 0,
                    "expected_session_created_at": "2000-01-01T00:00:00Z",
                    "expected_session_incarnation": "launch-recreated",
                    "expected_session_title": null
                }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "body={body}");
        assert_eq!(body["error"]["code"], "session-incarnation-conflict");
        assert_eq!(
            body["error"]["details"]["actual_session_incarnation"],
            "launch-recreated-v2"
        );
        let persisted: Value =
            serde_json::from_str(&std::fs::read_to_string(&record_path).unwrap()).unwrap();
        assert_eq!(persisted["title"], Value::Null);
        assert_eq!(persisted["title_revision"], Value::Null);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn update_session_title_maps_a_runtime_replaced_while_waiting_to_conflict() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tmux = minimal_tmux(tmp.path());
        seed_session_with_runtime(
            tmp.path(),
            "waiting-replace",
            "codex",
            "hs-codex-waiting-replace",
        );
        let context = crate::CliContext {
            state_dir: tmp.path().to_path_buf(),
            host: None,
        };
        let lock = crate::acquire_session_record_lock(&context, "waiting-replace").unwrap();
        let st = state(tmp.path(), Some(TOKEN), tmux);
        let request = tokio::spawn(async move {
            call(
                router(st),
                patch_json(
                    "/sessions/waiting-replace",
                    Some(TOKEN),
                    json!({
                        "title": "Must not cross runtime replacement",
                        "expected_title_revision": 0,
                        "expected_session_incarnation": "launch-waiting-replace",
                        "expected_session_title": null
                    }),
                ),
            )
            .await
        });

        std::thread::sleep(Duration::from_millis(50));
        let record_path = tmp.path().join("sessions/waiting-replace/session.json");
        let mut replacement: Value =
            serde_json::from_str(&std::fs::read_to_string(&record_path).unwrap()).unwrap();
        replacement["runtime"]["launch_id"] = json!("launch-waiting-replace-v2");
        std::fs::write(
            &record_path,
            serde_json::to_vec_pretty(&replacement).unwrap(),
        )
        .unwrap();
        drop(lock);

        let (status, body) = request.await.unwrap();
        assert_eq!(status, StatusCode::CONFLICT, "body={body}");
        assert_eq!(body["error"]["code"], "session-incarnation-conflict");
        assert_eq!(
            body["error"]["details"]["actual_session_incarnation"],
            "launch-waiting-replace-v2"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn update_session_title_maps_a_record_recreated_while_waiting_to_conflict() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tmux = minimal_tmux(tmp.path());
        seed_session_with_runtime(
            tmp.path(),
            "waiting-recreate",
            "codex",
            "hs-codex-waiting-recreate",
        );
        let context = crate::CliContext {
            state_dir: tmp.path().to_path_buf(),
            host: None,
        };
        let lock = crate::acquire_session_record_lock(&context, "waiting-recreate").unwrap();
        let st = state(tmp.path(), Some(TOKEN), tmux);
        let request = tokio::spawn(async move {
            call(
                router(st),
                patch_json(
                    "/sessions/waiting-recreate",
                    Some(TOKEN),
                    json!({
                        "title": "Must not cross record recreation",
                        "expected_title_revision": 0,
                        "expected_session_created_at": "2000-01-01T00:00:00Z",
                        "expected_session_title": null
                    }),
                ),
            )
            .await
        });

        std::thread::sleep(Duration::from_millis(50));
        let record_path = tmp.path().join("sessions/waiting-recreate/session.json");
        let mut replacement: Value =
            serde_json::from_str(&std::fs::read_to_string(&record_path).unwrap()).unwrap();
        replacement["created_at"] = json!("2001-01-01T00:00:00Z");
        replacement["runtime"]["launch_id"] = json!("launch-waiting-recreate-v2");
        std::fs::write(
            &record_path,
            serde_json::to_vec_pretty(&replacement).unwrap(),
        )
        .unwrap();
        drop(lock);

        let (status, body) = request.await.unwrap();
        assert_eq!(status, StatusCode::CONFLICT, "body={body}");
        assert_eq!(body["error"]["code"], "session-incarnation-conflict");
        assert_eq!(
            body["error"]["details"]["actual_session_created_at"],
            "2001-01-01T00:00:00Z"
        );
    }

    #[tokio::test]
    async fn expected_title_detects_repeated_legacy_writer_changes() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tmux = minimal_tmux(tmp.path());
        seed_session_with_runtime(tmp.path(), "rollback", "codex", "hs-codex-rollback");
        let st = state(tmp.path(), Some(TOKEN), tmux);
        let record_path = tmp.path().join("sessions/rollback/session.json");

        let (status, body) = call(router(st.clone()), get("/sessions")).await;
        assert_eq!(status, StatusCode::OK, "body={body}");
        assert_eq!(body["data"]["sessions"][0]["title"], Value::Null);
        assert_eq!(body["data"]["sessions"][0]["title_revision"], 0);
        assert_eq!(
            body["data"]["sessions"][0]["session_incarnation"],
            "launch-rollback"
        );

        // Simulate an origin/main binary updating an untouched pre-revision record:
        // it mutates only title and preserves unknown revision fields.
        let mut rolled_back: Value =
            serde_json::from_str(&std::fs::read_to_string(&record_path).unwrap()).unwrap();
        rolled_back["title"] = json!("First old daemon title");
        std::fs::write(
            &record_path,
            serde_json::to_vec_pretty(&rolled_back).unwrap(),
        )
        .unwrap();

        let (status, body) = call(
            router(st.clone()),
            patch_json(
                "/sessions/rollback",
                Some(TOKEN),
                json!({
                    "title": "Stale upgraded title",
                    "expected_title_revision": 0,
                    "expected_session_created_at": "2000-01-01T00:00:00Z",
                    "expected_session_title": null
                }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "body={body}");
        assert_eq!(body["error"]["code"], "title-state-conflict");
        assert_eq!(
            body["error"]["details"]["actual_session_title"],
            "First old daemon title"
        );

        // A second old-daemon mutation must also invalidate a client that read
        // the first old-daemon title while the numeric revision stayed at zero.
        let mut rolled_back: Value =
            serde_json::from_str(&std::fs::read_to_string(&record_path).unwrap()).unwrap();
        rolled_back["title"] = json!("Second old daemon title");
        std::fs::write(
            &record_path,
            serde_json::to_vec_pretty(&rolled_back).unwrap(),
        )
        .unwrap();

        let (status, body) = call(
            router(st.clone()),
            patch_json(
                "/sessions/rollback",
                Some(TOKEN),
                json!({
                    "title": "Still stale upgraded title",
                    "expected_title_revision": 0,
                    "expected_session_created_at": "2000-01-01T00:00:00Z",
                    "expected_session_title": "First old daemon title"
                }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "body={body}");
        assert_eq!(body["error"]["code"], "title-state-conflict");

        let (status, body) = call(
            router(st),
            patch_json(
                "/sessions/rollback",
                Some(TOKEN),
                json!({
                    "title": "Recovered upgraded title",
                    "expected_title_revision": 0,
                    "expected_session_created_at": "2000-01-01T00:00:00Z",
                    "expected_session_title": "Second old daemon title"
                }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body={body}");
        assert_eq!(body["data"]["session"]["title"], "Recovered upgraded title");
        assert_eq!(body["data"]["session"]["title_revision"], 1);
    }

    #[tokio::test]
    async fn unique_session_prefix_supports_title_update_and_resume() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state_dir = tmp.path().join("state");
        let cwd = tmp.path().join("repo");
        std::fs::create_dir_all(&cwd).unwrap();
        seed_resumable_session(
            &state_dir,
            "unique-prefix-session-long",
            "codex",
            "hs-unique-prefix-session-long",
            &cwd,
            &["resume", "resume-session-id"],
        );
        let st = state(&state_dir, Some(TOKEN), minimal_tmux(tmp.path()));

        let (status, body) = call(
            router(st.clone()),
            patch_json(
                "/sessions/unique-prefix",
                Some(TOKEN),
                json!({ "title": "Prefix title" }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body={body}");
        assert_eq!(body["data"]["session"]["id"], "unique-prefix-session-long");
        assert_eq!(body["data"]["session"]["title"], "Prefix title");

        let (status, body) = call(
            router(st),
            post_json("/sessions/unique-prefix/resume", Some(TOKEN), json!({})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body={body}");
        assert_eq!(body["data"]["session"]["id"], "unique-prefix-session-long");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn resume_transition_serializes_concurrent_title_mutation() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let state_dir = tmp.path().join("state");
        let cwd = tmp.path().join("repo");
        std::fs::create_dir_all(&cwd).expect("cwd");
        let cwd_arg = cwd.to_string_lossy().to_string();
        seed_resumable_session(
            &state_dir,
            "resume-title-race",
            "codex",
            "hs-resume-title-race",
            &cwd,
            &[
                "resume",
                "resume-session-id",
                "--cd",
                &cwd_arg,
                "--no-alt-screen",
            ],
        );
        let started = tmp.path().join("resume-started");
        let release = tmp.path().join("resume-release");
        let running = tmp.path().join("resume-running");
        let tmux = executable(
            &tmp.path().join("tmux-resume-lock"),
            &format!(
                "#!/usr/bin/env sh\ncase \"$1\" in\n  has-session) [ -f {running} ] ;;\n  new-session) : > {started}; while [ ! -f {release} ]; do sleep 0.01; done; : > {running}; exit 0 ;;\n  *) exit 0 ;;\nesac\n",
                started = shell_words::quote(&started.to_string_lossy()),
                release = shell_words::quote(&release.to_string_lossy()),
                running = shell_words::quote(&running.to_string_lossy()),
            ),
        );
        let context = CliContext {
            state_dir: state_dir.clone(),
            host: None,
        };
        let resume_task = {
            let context = context.clone();
            let tmux = tmux.clone();
            tokio::task::spawn_blocking(move || {
                resume_session_by_id(&context, "resume-title-race", &tmux)
            })
        };
        for _ in 0..200 {
            if started.is_file() {
                break;
            }
            if resume_task.is_finished() {
                let result = resume_task.await;
                panic!("resume exited before entering the tmux launch: {result:?}");
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(started.is_file(), "resume did not enter the tmux launch");
        let mut title_task = {
            let context = context.clone();
            let tmux = tmux.clone();
            tokio::task::spawn_blocking(move || {
                crate::update_session_title(
                    &context,
                    "resume-title-race",
                    Some("Title after resume".to_string()),
                    &tmux,
                )
            })
        };
        assert!(
            tokio::time::timeout(Duration::from_millis(100), &mut title_task)
                .await
                .is_err(),
            "title mutation must wait for the complete resume transaction"
        );
        let mut second_resume_task = {
            let context = context.clone();
            let tmux = tmux.clone();
            tokio::task::spawn_blocking(move || {
                resume_session_by_id(&context, "resume-title-race", &tmux)
            })
        };
        assert!(
            tokio::time::timeout(Duration::from_millis(100), &mut second_resume_task)
                .await
                .is_err(),
            "a second resume must wait for the first resume transaction"
        );
        std::fs::write(&release, b"").expect("release resume");
        resume_task
            .await
            .expect("resume task")
            .expect("resume transition");
        title_task
            .await
            .expect("title task")
            .expect("title mutation");
        second_resume_task
            .await
            .expect("second resume task")
            .expect("second resume result");

        let record = load_session_record(&context, "resume-title-race").expect("session record");
        assert_eq!(record.title.as_deref(), Some("Title after resume"));
        assert_eq!(
            record.runtime.as_ref().expect("runtime").generation,
            2,
            "title update must preserve the installed runtime generation"
        );
        assert_eq!(
            record
                .provider_resume
                .as_ref()
                .expect("provider resume")
                .session_id,
            "resume-session-id"
        );
        assert!(
            state_dir
                .join("sessions/resume-title-race/resume.json")
                .is_file()
        );
    }

    #[tokio::test]
    async fn update_session_title_renames_live_claude_session() {
        let tmp = tempfile::TempDir::new().unwrap();
        let calls_log = tmp.path().join("tmux-calls.log");
        let pasted_log = tmp.path().join("tmux-pasted.log");
        let tmux = rename_probe_tmux(tmp.path(), &calls_log, &pasted_log, true);
        seed_session(tmp.path(), "steer", "claude", "hs-claude-steer");
        let st = state(tmp.path(), Some(TOKEN), tmux);

        let (status, body) = call(
            router(st.clone()),
            patch_json(
                "/sessions/steer",
                Some(TOKEN),
                json!({ "title": "Cleaned up title" }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body={body}");
        assert_eq!(body["data"]["session"]["title"], "Cleaned up title");

        let calls = std::fs::read_to_string(&calls_log).unwrap();
        assert!(
            calls.contains("paste-buffer -b steer-send"),
            "a live Claude rename must paste into the pane: {calls:?}"
        );
        assert!(
            calls.contains("send-keys -t hs-claude-steer:0.0 Enter"),
            "a live Claude rename must be submitted with Enter: {calls:?}"
        );
        let pasted = std::fs::read_to_string(&pasted_log).unwrap();
        assert_eq!(
            pasted.trim_end(),
            "/rename Cleaned up title",
            "the pasted rename command must carry the new title: {pasted:?}"
        );
    }

    #[tokio::test]
    async fn live_claude_rename_skips_a_stale_title_revision() {
        let tmp = tempfile::TempDir::new().unwrap();
        let calls_log = tmp.path().join("tmux-calls.log");
        let pasted_log = tmp.path().join("tmux-pasted.log");
        let tmux = rename_probe_tmux(tmp.path(), &calls_log, &pasted_log, true);
        seed_session(
            tmp.path(),
            "stale-rename",
            "claude",
            "hs-claude-stale-rename",
        );
        let st = state(tmp.path(), Some(TOKEN), tmux.clone());
        let context = st.context.clone();
        let mut stale = crate::load_session_record(&context, "stale-rename").unwrap();
        stale.title = Some("Older title".to_string());
        stale.title_revision = 1;
        let current = crate::mutate_session_record(&context, "stale-rename", |record| {
            record.title = Some("Newer title".to_string());
            record.title_revision = 2;
            Ok(record.clone())
        })
        .unwrap();

        crate::rename_live_claude_session(&context, &stale, "Older title", &tmux).unwrap();
        assert!(
            !pasted_log.exists(),
            "a stale title revision must not reach the live Claude pane"
        );

        crate::rename_live_claude_session(&context, &current, "Newer title", &tmux).unwrap();
        let pasted = std::fs::read_to_string(&pasted_log).unwrap();
        assert_eq!(pasted.trim_end(), "/rename Newer title");
    }

    #[tokio::test]
    async fn update_session_title_does_not_rename_non_claude_session() {
        let tmp = tempfile::TempDir::new().unwrap();
        let calls_log = tmp.path().join("tmux-calls.log");
        let pasted_log = tmp.path().join("tmux-pasted.log");
        let tmux = rename_probe_tmux(tmp.path(), &calls_log, &pasted_log, true);
        seed_session(tmp.path(), "steer", "codex", "hs-codex-steer");
        let st = state(tmp.path(), Some(TOKEN), tmux);

        let (status, body) = call(
            router(st.clone()),
            patch_json(
                "/sessions/steer",
                Some(TOKEN),
                json!({ "title": "Cleaned up title" }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body={body}");
        assert_eq!(body["data"]["session"]["title"], "Cleaned up title");

        let calls = std::fs::read_to_string(&calls_log).unwrap_or_default();
        assert!(
            !calls.contains("paste-buffer"),
            "Codex has no prompt-bar display name; it must not receive /rename: {calls:?}"
        );
        assert!(
            !pasted_log.exists(),
            "no rename text should be pasted for a non-Claude session"
        );
    }

    #[tokio::test]
    async fn update_session_title_skips_rename_for_stopped_claude_session() {
        let tmp = tempfile::TempDir::new().unwrap();
        let calls_log = tmp.path().join("tmux-calls.log");
        let pasted_log = tmp.path().join("tmux-pasted.log");
        let tmux = rename_probe_tmux(tmp.path(), &calls_log, &pasted_log, false);
        seed_session(tmp.path(), "steer", "claude", "hs-claude-steer");
        let st = state(tmp.path(), Some(TOKEN), tmux);

        let (status, body) = call(
            router(st.clone()),
            patch_json(
                "/sessions/steer",
                Some(TOKEN),
                json!({ "title": "New title" }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body={body}");
        assert_eq!(body["data"]["session"]["title"], "New title");
        assert_eq!(body["data"]["session"]["status"], "stopped");

        let calls = std::fs::read_to_string(&calls_log).unwrap_or_default();
        assert!(
            !calls.contains("paste-buffer"),
            "a stopped session has no live pane to rename: {calls:?}"
        );
        assert!(
            !pasted_log.exists(),
            "no rename text should be pasted for a stopped session"
        );
    }

    #[tokio::test]
    async fn update_session_title_clearing_to_null_does_not_rename() {
        let tmp = tempfile::TempDir::new().unwrap();
        let calls_log = tmp.path().join("tmux-calls.log");
        let pasted_log = tmp.path().join("tmux-pasted.log");
        let tmux = rename_probe_tmux(tmp.path(), &calls_log, &pasted_log, true);
        seed_session(tmp.path(), "steer", "claude", "hs-claude-steer");
        let st = state(tmp.path(), Some(TOKEN), tmux);

        let (status, _body) = call(
            router(st.clone()),
            patch_json("/sessions/steer", Some(TOKEN), json!({ "title": "First" })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        // Clearing the title must not fire `/rename` with no argument (which would
        // make Claude auto-generate an unrelated name).
        let (status, body) = call(
            router(st.clone()),
            patch_json("/sessions/steer", Some(TOKEN), json!({ "title": null })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body={body}");
        assert_eq!(body["data"]["session"]["title"], Value::Null);

        let pasted = std::fs::read_to_string(&pasted_log).unwrap();
        assert_eq!(
            pasted.matches("/rename").count(),
            1,
            "only the non-empty title should have been pushed as a rename: {pasted:?}"
        );
        assert!(
            pasted.contains("/rename First"),
            "the one rename must be the non-empty title: {pasted:?}"
        );
    }

    #[tokio::test]
    async fn new_write_routes_require_token() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tmux = minimal_tmux(tmp.path());
        seed_session(tmp.path(), "steer", "codex", "hs-codex-steer");
        let st = state(tmp.path(), Some(TOKEN), tmux);

        let (status, body) = call(
            router(st.clone()),
            patch_json("/sessions/steer", None, json!({ "title": "Nope" })),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["error"]["code"], "unauthorized");

        let (status, body) = call(
            router(st.clone()),
            put_json(
                "/sessions/steer/account",
                None,
                json!({ "account": "gamania" }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["error"]["code"], "unauthorized");

        let (status, body) = call(
            router(st.clone()),
            post_bytes(
                "/sessions/steer/attachments?filename=secret.png",
                None,
                b"bytes",
            ),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["error"]["code"], "unauthorized");
    }

    #[tokio::test]
    async fn codex_accounts_route_is_authenticated_and_projects_no_credentials() {
        let lock = GlobalStateLock::new();
        let tmp = tempfile::TempDir::new().unwrap();
        let broker = tmp.path().join("broker");
        fs::write(
            &broker,
            r#"#!/bin/sh
printf '%s\n' '{"schema_version":"agent-session.codex-auth-broker.v1","accounts":[{"account":"gamania","label":"Gamania","plan":"team"}]}'
"#,
        )
        .unwrap();
        fs::set_permissions(&broker, fs::Permissions::from_mode(0o700)).unwrap();
        let argv = serde_json::to_string(&vec![broker.to_string_lossy().into_owned()]).unwrap();
        let _broker = EnvGuard::set(&lock, "AGENT_SESSION_CODEX_ACCOUNT_BROKER", &argv);
        let st = state(tmp.path(), Some(TOKEN), minimal_tmux(tmp.path()));

        let (status, body) = call(router(st.clone()), get_auth("/codex/accounts", None)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["error"]["code"], "unauthorized");

        let (status, body) = call(router(st), get_auth("/codex/accounts", Some(TOKEN))).await;
        assert_eq!(status, StatusCode::OK, "body={body}");
        assert_eq!(body["data"]["machine"], MACHINE);
        assert_eq!(body["data"]["accounts"][0]["account"], "gamania");
        assert_eq!(body["data"]["accounts"][0]["label"], "Gamania");
        let encoded = body.to_string();
        assert!(!encoded.contains("access_token"));
        assert!(!encoded.contains("chatgpt_account_id"));
    }

    #[tokio::test]
    async fn upload_attachment_writes_private_session_file_with_safe_name() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tmux = minimal_tmux(tmp.path());
        seed_session(tmp.path(), "steer", "codex", "hs-codex-steer");
        let st = state(tmp.path(), Some(TOKEN), tmux);
        let payload = b"not actually a png";

        let (status, body) = call(
            router(st.clone()),
            post_bytes(
                "/sessions/steer/attachments?filename=..%2FScreen%20Shot.png",
                Some(TOKEN),
                payload,
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body={body}");
        assert_eq!(body["schema_version"], "cli.agent-session.serve.v1");
        let attachment = &body["data"]["attachment"];
        assert_eq!(attachment["bytes"], payload.len());
        assert_eq!(attachment["filename"], "Screen_Shot.png");
        let path = PathBuf::from(attachment["path"].as_str().expect("attachment path"));
        assert_eq!(std::fs::read(&path).unwrap(), payload);
        assert!(path.starts_with(tmp.path().join("sessions/steer/attachments")));
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "attachment file must be private");

        let (status, second) = call(
            router(st.clone()),
            post_bytes(
                "/sessions/steer/attachments?filename=Screen%20Shot.png",
                Some(TOKEN),
                b"second payload",
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body={second}");
        let second_path = PathBuf::from(second["data"]["attachment"]["path"].as_str().unwrap());
        assert_ne!(second_path, path);
        assert_eq!(std::fs::read(&path).unwrap(), payload);
        assert_eq!(std::fs::read(&second_path).unwrap(), b"second payload");

        let (status, body) = call(
            router(st.clone()),
            post_bytes(
                "/sessions/ghost/attachments?filename=ghost.png",
                Some(TOKEN),
                b"ghost",
            ),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"]["code"], "session-not-found");

        let before_oversize = std::fs::read_dir(tmp.path().join("sessions/steer/attachments"))
            .unwrap()
            .count();
        let oversize = vec![0u8; MAX_ATTACHMENT_BYTES + 1];
        let (status, body) = call(
            router(st.clone()),
            post_bytes(
                "/sessions/steer/attachments?filename=oversize.bin",
                Some(TOKEN),
                &oversize,
            ),
        )
        .await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(body["error"]["code"], "attachment-too-large");
        let after_oversize = std::fs::read_dir(tmp.path().join("sessions/steer/attachments"))
            .unwrap()
            .count();
        assert_eq!(after_oversize, before_oversize);
    }

    #[tokio::test]
    async fn workdir_search_filters_default_project_and_config_roots() {
        let lock = GlobalStateLock::new();
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path().join("home");
        let project_match = home.join("Project/sympoies/agent-console");
        let second_match = home.join("Project/agent-tools");
        let too_deep_match = home.join("Project/a/b/c/d/e/agent-deep");
        let config_match = home.join(".config/zsh");
        let outside = home.join("Downloads/agent-console-copy");
        std::fs::create_dir_all(project_match.join(".git")).unwrap();
        std::fs::create_dir_all(&second_match).unwrap();
        std::fs::create_dir_all(&too_deep_match).unwrap();
        std::fs::create_dir_all(&config_match).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let _home = EnvGuard::set(&lock, "HOME", home.to_str().unwrap());

        let st = state(tmp.path(), Some(TOKEN), PathBuf::from("tmux"));
        let (status, body) = call(router(st.clone()), get("/workdirs?q=agent&limit=20")).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["error"]["code"], "unauthorized");

        let (status, body) = call(
            router(st.clone()),
            get_auth("/workdirs?q=agent&limit=20", Some(TOKEN)),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body={body}");
        assert_eq!(body["schema_version"], "cli.agent-session.serve.v1");
        let workdirs = body["data"]["workdirs"].as_array().expect("workdirs array");
        assert!(
            workdirs
                .iter()
                .any(|item| item["path"] == project_match.to_string_lossy().as_ref()),
            "Project match missing: {workdirs:?}"
        );
        assert!(
            !workdirs
                .iter()
                .any(|item| item["path"] == outside.to_string_lossy().as_ref()),
            "outside root must not be returned: {workdirs:?}"
        );
        assert!(
            !workdirs
                .iter()
                .any(|item| item["path"] == too_deep_match.to_string_lossy().as_ref()),
            "matches beyond max depth must not be returned: {workdirs:?}"
        );

        let (status, limited) = call(
            router(st.clone()),
            get_auth("/workdirs?q=agent&limit=1", Some(TOKEN)),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body={limited}");
        assert_eq!(
            limited["data"]["workdirs"]
                .as_array()
                .expect("workdirs")
                .len(),
            1
        );

        let (status, body) = call(
            router(st.clone()),
            get_auth("/workdirs?q=zsh&limit=20", Some(TOKEN)),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body={body}");
        let workdirs = body["data"]["workdirs"].as_array().expect("workdirs array");
        assert!(
            workdirs
                .iter()
                .any(|item| item["path"] == config_match.to_string_lossy().as_ref()),
            ".config match missing: {workdirs:?}"
        );
    }

    #[tokio::test]
    async fn repo_remote_url_resolves_github_and_gitlab_remotes() {
        let tmp = tempfile::TempDir::new().unwrap();
        let github_repo = tmp.path().join("home/Project/sympoies/agent-console");
        let gitlab_repo = tmp.path().join("home/Project/acme/backend");
        init_git_remote(&github_repo, "git@github.com:sympoies/agent-console.git");
        init_git_remote(
            &gitlab_repo,
            "ssh://git@gitlab.example.com:2222/group/sub/project.git",
        );

        let st = state(tmp.path(), Some(TOKEN), PathBuf::from("tmux"));
        let unauthorized = format!("/repos/remote-url?cwd={}", github_repo.to_string_lossy());
        let (status, body) = call(router(st.clone()), get(&unauthorized)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["error"]["code"], "unauthorized");

        let github_path = format!("/repos/remote-url?cwd={}", github_repo.to_string_lossy());
        let (status, body) = call(router(st.clone()), get_auth(&github_path, Some(TOKEN))).await;
        assert_eq!(status, StatusCode::OK, "body={body}");
        assert_eq!(body["data"]["machine"], "test-machine");
        assert_eq!(
            body["data"]["url"],
            "https://github.com/sympoies/agent-console"
        );

        let gitlab_path = format!("/repos/remote-url?cwd={}", gitlab_repo.to_string_lossy());
        let (status, body) = call(router(st.clone()), get_auth(&gitlab_path, Some(TOKEN))).await;
        assert_eq!(status, StatusCode::OK, "body={body}");
        assert_eq!(
            body["data"]["url"],
            "https://gitlab.example.com/group/sub/project"
        );

        let missing = tmp.path().join("home/Project/no-remote");
        std::fs::create_dir_all(&missing).unwrap();
        let missing_path = format!("/repos/remote-url?cwd={}", missing.to_string_lossy());
        let (status, body) = call(router(st), get_auth(&missing_path, Some(TOKEN))).await;
        assert_eq!(status, StatusCode::OK, "body={body}");
        assert!(body["data"]["url"].is_null());
    }

    #[tokio::test]
    async fn workdir_search_can_return_git_repos_without_linked_worktrees() {
        let lock = GlobalStateLock::new();
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path().join("home");
        let project_repo = home.join("Project/sympoies/agent-console");
        let linked_worktree = home.join("Project/sympoies/wt-agent-console-21");
        let non_git_dir = home.join("Project/sympoies/notes");
        let config_repo = home.join(".config/zsh");
        std::fs::create_dir_all(project_repo.join(".git")).unwrap();
        std::fs::create_dir_all(&linked_worktree).unwrap();
        std::fs::write(
            linked_worktree.join(".git"),
            "gitdir: ../agent-console/.git/worktrees/wt-agent-console-21\n",
        )
        .unwrap();
        std::fs::create_dir_all(&non_git_dir).unwrap();
        std::fs::create_dir_all(config_repo.join(".git")).unwrap();
        let _home = EnvGuard::set(&lock, "HOME", home.to_str().unwrap());

        let st = state(tmp.path(), Some(TOKEN), PathBuf::from("tmux"));
        let (status, body) = call(
            router(st.clone()),
            get_auth(
                "/workdirs?q=&limit=20&git_only=true&exclude_worktrees=true",
                Some(TOKEN),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body={body}");
        let workdirs = body["data"]["workdirs"].as_array().expect("workdirs array");
        let paths: Vec<&str> = workdirs
            .iter()
            .filter_map(|item| item["path"].as_str())
            .collect();
        assert!(
            paths.contains(&project_repo.to_string_lossy().as_ref()),
            "primary project repo missing: {workdirs:?}"
        );
        assert!(
            paths.contains(&config_repo.to_string_lossy().as_ref()),
            "config repo missing: {workdirs:?}"
        );
        assert!(
            !paths.contains(&non_git_dir.to_string_lossy().as_ref()),
            "non-git dir must not be returned: {workdirs:?}"
        );
        assert!(
            !paths.contains(&linked_worktree.to_string_lossy().as_ref()),
            "linked worktree must not be returned: {workdirs:?}"
        );
        assert!(
            workdirs.iter().all(|item| item["is_git_repo"] == true),
            "all returned rows must be git repos: {workdirs:?}"
        );
        assert!(
            workdirs.iter().all(|item| item.get("last_used").is_some()),
            "last_used field must be present for every row: {workdirs:?}"
        );
    }

    #[tokio::test]
    async fn create_records_workdir_usage_for_recent_first_search() {
        let lock = GlobalStateLock::new();
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path().join("home");
        let alpha_repo = home.join("Project/sympoies/alpha");
        let beta_repo = home.join("Project/sympoies/beta");
        std::fs::create_dir_all(alpha_repo.join(".git")).unwrap();
        std::fs::create_dir_all(beta_repo.join(".git")).unwrap();
        let _home = EnvGuard::set(&lock, "HOME", home.to_str().unwrap());
        let tmux = minimal_tmux(tmp.path());
        let st = state(tmp.path(), Some(TOKEN), tmux);

        let (status, body) = call(
            router(st.clone()),
            post_json(
                "/sessions",
                Some(TOKEN),
                json!({
                    "agent": "codex",
                    "id": "uses-beta",
                    "cwd": beta_repo.to_string_lossy(),
                }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body={body}");

        let (status, body) = call(
            router(st.clone()),
            get_auth(
                "/workdirs?q=&limit=20&git_only=true&exclude_worktrees=true",
                Some(TOKEN),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body={body}");
        let workdirs = body["data"]["workdirs"].as_array().expect("workdirs array");
        assert_eq!(
            workdirs
                .first()
                .and_then(|item| item["path"].as_str())
                .unwrap_or_default(),
            beta_repo.to_string_lossy().as_ref(),
            "created cwd must rank first: {workdirs:?}"
        );
        assert!(
            workdirs
                .first()
                .and_then(|item| item["last_used"].as_str())
                .is_some_and(|value| !value.is_empty()),
            "created cwd must expose last_used: {workdirs:?}"
        );
        let alpha = workdirs
            .iter()
            .find(|item| item["path"] == alpha_repo.to_string_lossy().as_ref())
            .expect("unused repo row");
        assert_eq!(alpha["last_used"], Value::Null);
    }

    #[tokio::test]
    async fn workdir_search_rejects_symlink_roots() {
        let lock = GlobalStateLock::new();
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path().join("home");
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(outside.join("agent-secret")).unwrap();
        symlink(&outside, home.join("Project")).unwrap();
        let _home = EnvGuard::set(&lock, "HOME", home.to_str().unwrap());

        let st = state(tmp.path(), Some(TOKEN), PathBuf::from("tmux"));
        let (status, body) = call(
            router(st.clone()),
            get_auth("/workdirs?q=agent&limit=20", Some(TOKEN)),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body={body}");
        let workdirs = body["data"]["workdirs"].as_array().expect("workdirs array");
        assert!(
            workdirs.is_empty(),
            "symlink roots must not expose outside directories: {workdirs:?}"
        );
    }

    #[test]
    fn serve_default_bind_is_loopback() {
        use crate::cli::{Cli, Command};
        use clap::Parser;
        let cli = Cli::try_parse_from(["agent-session", "serve"]).unwrap();
        let Command::Serve(args) = cli.command else {
            panic!("expected serve command");
        };
        let addr: SocketAddr = args.bind.parse().unwrap();
        assert!(addr.ip().is_loopback(), "default bind must be loopback");
    }

    #[test]
    fn serve_token_stdin_conflicts_with_token_without_leaking_token() {
        use crate::cli::Cli;
        use clap::Parser;

        let err = Cli::try_parse_from([
            "agent-session",
            "serve",
            "--token",
            "secret-from-argv",
            "--token-stdin",
        ])
        .unwrap_err();
        let message = err.to_string();

        assert!(message.contains("--token"));
        assert!(message.contains("--token-stdin"));
        assert!(
            !message.contains("secret-from-argv"),
            "parse error leaked token material: {message}"
        );
    }

    #[test]
    fn token_stdin_reads_single_trimmed_token() {
        assert_eq!(
            read_token_from_stdin("  stdin-token\n".as_bytes()).unwrap(),
            "stdin-token"
        );
    }

    #[test]
    fn token_stdin_rejects_empty_input_without_leaking_material() {
        let err = read_token_from_stdin("   \n".as_bytes()).unwrap_err();
        let message = err.to_string();

        assert!(message.contains("empty"));
        assert!(!message.contains("\\n"));
    }

    #[test]
    fn token_stdin_rejects_multiple_lines_without_leaking_material() {
        let err = read_token_from_stdin("first-token\nsecond-token\n".as_bytes()).unwrap_err();
        let message = err.to_string();

        assert!(message.contains("exactly one token"));
        assert!(!message.contains("first-token"));
        assert!(!message.contains("second-token"));
    }

    #[test]
    fn token_stdin_rejects_oversized_input() {
        let oversized = "a".repeat((MAX_STDIN_TOKEN_BYTES + 1) as usize);
        let err = read_token_from_stdin(oversized.as_bytes()).unwrap_err();
        assert!(err.to_string().contains("8192 bytes"));
    }

    #[tokio::test]
    async fn create_rejects_unknown_agent() {
        let tmp = tempfile::TempDir::new().unwrap();
        let st = state(tmp.path(), Some(TOKEN), PathBuf::from("tmux"));
        let (status, body) = call(
            router(st.clone()),
            post_json("/sessions", Some(TOKEN), json!({"agent": "banana"})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], "invalid-agent");
    }

    #[tokio::test]
    async fn glance_reads_open_with_machine() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tmux = minimal_tmux(tmp.path());
        seed_session(tmp.path(), "look", "codex", "hs-codex-look");
        let st = state(tmp.path(), Some(TOKEN), tmux);
        // No auth header: glance is a read, open on loopback.
        let (status, body) = call(router(st.clone()), get("/sessions/look/glance")).await;
        assert_eq!(status, StatusCode::OK, "body={body}");
        assert_eq!(body["schema_version"], "cli.agent-session.serve.v1");
        assert_eq!(body["data"]["machine"], MACHINE);
        assert!(body["data"]["glance"].is_object());
    }

    #[tokio::test]
    async fn buffer_returns_tmux_show_buffer_text_open_on_reads() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tmux = minimal_tmux(tmp.path());
        seed_session(tmp.path(), "look", "claude", "hs-claude-look");
        let st = state(tmp.path(), Some(TOKEN), tmux);
        // No auth header: buffer is a read, open on loopback like glance.
        let (status, body) = call(router(st.clone()), get("/sessions/look/buffer")).await;
        assert_eq!(status, StatusCode::OK, "body={body}");
        assert_eq!(body["schema_version"], "cli.agent-session.serve.v1");
        assert_eq!(body["data"]["machine"], MACHINE);
        assert_eq!(body["data"]["text"], "buffered selection\n");
    }

    #[tokio::test]
    async fn buffer_unknown_session_is_not_found() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tmux = minimal_tmux(tmp.path());
        let st = state(tmp.path(), Some(TOKEN), tmux);
        let (status, body) = call(router(st.clone()), get("/sessions/ghost/buffer")).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "body={body}");
        assert_eq!(body["error"]["code"], "session-not-found");
    }

    #[tokio::test]
    async fn buffer_reports_empty_when_no_buffer_set() {
        // A fresh tmux server has no paste buffer yet: `show-buffer` exits non-zero
        // ("no buffers"), which must surface as an empty selection, not an error.
        let tmp = tempfile::TempDir::new().unwrap();
        let bin = tmp.path().join("tmux");
        std::fs::write(
            &bin,
            "#!/usr/bin/env sh\ncase \"$1\" in\n  show-buffer) echo 'no buffers' >&2; exit 1 ;;\n  *) exit 0 ;;\nesac\n",
        )
        .unwrap();
        let mut perms = std::fs::metadata(&bin).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&bin, perms).unwrap();
        seed_session(tmp.path(), "look", "claude", "hs-claude-look");
        let st = state(tmp.path(), Some(TOKEN), bin);
        let (status, body) = call(router(st.clone()), get("/sessions/look/buffer")).await;
        assert_eq!(status, StatusCode::OK, "body={body}");
        assert_eq!(body["data"]["text"], "");
    }

    #[tokio::test]
    async fn glance_preserves_provider_resume_metadata() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cwd = tmp.path().join("repo");
        std::fs::create_dir_all(&cwd).unwrap();
        seed_resumable_session(
            tmp.path(),
            "recover",
            "claude",
            "hs-claude-recover",
            &cwd,
            &["--resume", "resume-session-id"],
        );
        add_provider_resume_extra(tmp.path(), "recover");
        let st = state(tmp.path(), Some(TOKEN), minimal_tmux(tmp.path()));

        let (status, body) = call(router(st.clone()), get("/sessions/recover/glance")).await;
        assert_eq!(status, StatusCode::OK, "body={body}");
        let glance = &body["data"]["glance"];
        assert_eq!(glance["provider_resume"]["provider"], "claude");
        assert_eq!(
            glance["provider_resume"]["resume_args"],
            json!(["--resume", "resume-session-id"])
        );
        assert!(glance["provider_resume"].get("storage_only").is_none());
    }

    #[tokio::test]
    async fn delete_missing_session_maps_to_404() {
        let tmp = tempfile::TempDir::new().unwrap();
        let st = state(tmp.path(), Some(TOKEN), PathBuf::from("tmux"));
        let del = Request::builder()
            .method("DELETE")
            .uri("/sessions/ghost")
            .header("authorization", format!("Bearer {TOKEN}"))
            .body(Body::empty())
            .unwrap();
        let (status, body) = call(router(st.clone()), del).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"]["code"], "session-not-found");
    }

    #[tokio::test]
    async fn handle_input_resizes_and_ignores_malformed_frames() {
        let tmp = tempfile::TempDir::new().unwrap();
        let log = tmp.path().join("calls.log");
        let tmux = logging_tmux(tmp.path(), &log);
        let ctx = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let record = test_record("look", "hs-codex-look");
        let target = "hs-codex-look:0.0";
        let resize_lock = tokio::sync::Mutex::new(());

        // Malformed JSON and an empty object must not touch tmux.
        let mut pending = false;
        handle_input(
            &ctx,
            &tmux,
            &record,
            target,
            "{ not json",
            &mut pending,
            &resize_lock,
        )
        .await
        .unwrap();
        handle_input(
            &ctx,
            &tmux,
            &record,
            target,
            "{}",
            &mut pending,
            &resize_lock,
        )
        .await
        .unwrap();
        let after_noops = std::fs::read_to_string(&log).unwrap_or_default();
        assert!(
            after_noops.trim().is_empty(),
            "malformed/empty frames must not call tmux: {after_noops:?}"
        );

        // A non-initial resize frame maps to a single `tmux resize-window
        // -x <cols> -y <rows>` (no repaint nudge once the flag is spent).
        handle_input(
            &ctx,
            &tmux,
            &record,
            target,
            r#"{"resize":{"cols":123,"rows":45}}"#,
            &mut pending,
            &resize_lock,
        )
        .await
        .unwrap();
        let calls = std::fs::read_to_string(&log).unwrap_or_default();
        let resizes: Vec<&str> = calls
            .lines()
            .filter(|line| line.contains("resize-window"))
            .collect();
        assert_eq!(
            resizes.len(),
            1,
            "a non-initial resize must issue exactly one resize-window: {calls:?}"
        );
        assert!(
            resizes[0].contains("123") && resizes[0].contains("45"),
            "resize frame must invoke resize-window with the requested size: {calls:?}"
        );
    }

    #[tokio::test]
    async fn incompatible_proxy_input_is_reported_before_auto_resume_mutation() {
        let tmp = tempfile::TempDir::new().unwrap();
        let log = tmp.path().join("calls.log");
        let tmux = logging_tmux(tmp.path(), &log);
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let mut record = test_record("old-proxy", "hs-codex-old-proxy");
        let socket = tmp.path().join("old-proxy.sock");
        record.runtime = Some(crate::RuntimeInfo {
            kind: codex_app_server::RUNTIME_KIND.to_string(),
            tmux_session: record.tmux_session.clone(),
            generation: 1,
            started_at: "2030-01-01T00:00:00Z".to_string(),
            launch_id: "old-proxy-launch".to_string(),
            extra: std::collections::BTreeMap::from([
                (
                    codex_app_server::PROTOCOL_KEY.to_string(),
                    json!(codex_app_server::PROTOCOL_VERSION),
                ),
                (
                    codex_app_server::SOCKET_KEY.to_string(),
                    json!(crate::display_path(&socket)),
                ),
                (
                    codex_app_server::PROXY_KEY.to_string(),
                    json!(crate::display_path(&socket.with_extension("proxy"))),
                ),
                (
                    codex_app_server::THREAD_HANDOFF_KEY.to_string(),
                    json!(crate::display_path(&socket.with_extension("thread"))),
                ),
                (
                    codex_app_server::THREAD_ATTACHED_KEY.to_string(),
                    json!(crate::display_path(&socket.with_extension("attached"))),
                ),
            ]),
        });
        crate::write_session_record(&context, &record).unwrap();
        crate::activity::activate_runtime(&context, &record).unwrap();
        auto_resume::set_enabled(&context, &record.id, true, "2030-01-01T00:00:00Z").unwrap();

        let mut repaint = false;
        let error = handle_input(
            &context,
            &tmux,
            &record,
            "hs-codex-old-proxy:0.0",
            r#"{"text":"\r"}"#,
            &mut repaint,
            &tokio::sync::Mutex::new(()),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code(), "codex-input-section-unavailable");
        assert!(auto_resume::view_for_record(&context, &record).enabled);
    }

    #[tokio::test]
    async fn raw_terminal_carriage_return_is_delivered_as_enter_not_paste() {
        let tmp = tempfile::TempDir::new().unwrap();
        let log = tmp.path().join("calls.log");
        let tmux = logging_tmux(tmp.path(), &log);
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let record = test_record("raw-enter", "hs-codex-raw-enter");
        crate::write_session_record(&context, &record).unwrap();
        let mut repaint = false;

        handle_input(
            &context,
            &tmux,
            &record,
            "hs-codex-raw-enter:0.0",
            r#"{"text":"\r"}"#,
            &mut repaint,
            &tokio::sync::Mutex::new(()),
        )
        .await
        .unwrap();
        let calls = std::fs::read_to_string(&log).unwrap();
        assert!(calls.contains("send-keys -t hs-codex-raw-enter:0.0 Enter"));
        assert!(!calls.contains("load-buffer"));
        assert!(!calls.contains("paste-buffer"));
    }

    #[tokio::test]
    async fn attach_backspace_is_delivered_as_named_tmux_key() {
        let tmp = tempfile::TempDir::new().unwrap();
        let log = tmp.path().join("calls.log");
        let tmux = logging_tmux(tmp.path(), &log);
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let record = test_record("backspace", "hs-codex-backspace");
        crate::write_session_record(&context, &record).unwrap();
        let mut repaint = false;

        handle_input(
            &context,
            &tmux,
            &record,
            "hs-codex-backspace:0.0",
            r#"{"keys":["backspace"]}"#,
            &mut repaint,
            &tokio::sync::Mutex::new(()),
        )
        .await
        .unwrap();

        let calls = std::fs::read_to_string(&log).unwrap_or_default();
        assert!(
            calls.contains("send-keys -t hs-codex-backspace:0.0 BSpace"),
            "Backspace must use a named tmux key: {calls:?}"
        );
        assert!(!calls.contains("load-buffer"));
        assert!(!calls.contains("paste-buffer"));
    }

    #[tokio::test]
    async fn first_resize_forces_a_repaint_nudge_then_plain_resizes() {
        let tmp = tempfile::TempDir::new().unwrap();
        let log = tmp.path().join("calls.log");
        let tmux = logging_tmux(tmp.path(), &log);
        let ctx = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let record = test_record("look", "hs-codex-look");
        let target = "hs-codex-look:0.0";
        let resize_lock = tokio::sync::Mutex::new(());

        // First resize after attach: nudge to rows-1, then the real rows, so the
        // agent is guaranteed a SIGWINCH and repaints its whole frame into the
        // freshly rebuilt client grid (fixes stale layout on re-entry).
        let mut pending = true;
        handle_input(
            &ctx,
            &tmux,
            &record,
            target,
            r#"{"resize":{"cols":80,"rows":24}}"#,
            &mut pending,
            &resize_lock,
        )
        .await
        .unwrap();
        let calls = std::fs::read_to_string(&log).unwrap_or_default();
        let resizes: Vec<&str> = calls
            .lines()
            .filter(|line| line.contains("resize-window"))
            .collect();
        assert_eq!(
            resizes.len(),
            2,
            "the first resize must nudge then set the real size: {calls:?}"
        );
        assert!(
            resizes[0].contains("-y 23"),
            "nudge step must use rows-1: {:?}",
            resizes[0]
        );
        assert!(
            resizes[1].contains("-y 24"),
            "final step must use the requested rows: {:?}",
            resizes[1]
        );
        assert!(
            !pending,
            "the initial-repaint flag must be consumed after the first resize"
        );

        // Subsequent resizes are a single plain resize-window (no flicker).
        std::fs::write(&log, "").unwrap();
        handle_input(
            &ctx,
            &tmux,
            &record,
            target,
            r#"{"resize":{"cols":100,"rows":40}}"#,
            &mut pending,
            &resize_lock,
        )
        .await
        .unwrap();
        let calls = std::fs::read_to_string(&log).unwrap_or_default();
        let resizes: Vec<&str> = calls
            .lines()
            .filter(|line| line.contains("resize-window"))
            .collect();
        assert_eq!(resizes.len(), 1, "later resizes must not nudge: {calls:?}");
        assert!(
            resizes[0].contains("-y 40"),
            "later resize must use the requested rows: {:?}",
            resizes[0]
        );
    }

    #[tokio::test]
    async fn concurrent_first_resize_sequences_do_not_interleave() {
        let tmp = tempfile::TempDir::new().unwrap();
        let log = tmp.path().join("calls.log");
        let tmux = logging_tmux(tmp.path(), &log);
        let ctx = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let record = test_record("look", "hs-codex-look");
        let target = "hs-codex-look:0.0";
        let resize_lock = tokio::sync::Mutex::new(());
        let mut first_pending = true;
        let mut second_pending = true;

        let (first, second) = tokio::join!(
            handle_input(
                &ctx,
                &tmux,
                &record,
                target,
                r#"{"resize":{"cols":80,"rows":24}}"#,
                &mut first_pending,
                &resize_lock,
            ),
            handle_input(
                &ctx,
                &tmux,
                &record,
                target,
                r#"{"resize":{"cols":100,"rows":40}}"#,
                &mut second_pending,
                &resize_lock,
            ),
        );
        first.unwrap();
        second.unwrap();

        let calls = std::fs::read_to_string(&log).unwrap();
        let rows: Vec<&str> = calls
            .lines()
            .filter(|line| line.contains("resize-window"))
            .map(|line| line.rsplit_once("-y ").unwrap().1)
            .collect();
        assert!(
            rows == ["23", "24", "39", "40"] || rows == ["39", "40", "23", "24"],
            "each client's nudge/final resize pair must stay contiguous: {calls:?}"
        );
    }

    #[tokio::test]
    async fn timed_out_resize_releases_the_shared_lock() {
        let tmp = tempfile::TempDir::new().unwrap();
        let log = tmp.path().join("calls.log");
        let first_started = tmp.path().join("first-started");
        let tmux = executable(
            &tmp.path().join("tmux"),
            &format!(
                r#"#!/usr/bin/env sh
printf '%s\n' "$*" >> {}
if [ "$1" = "resize-window" ] && [ ! -e {} ]; then
  : > {}
  sleep 1
fi
exit 0
"#,
                shell_words::quote(&log.to_string_lossy()),
                shell_words::quote(&first_started.to_string_lossy()),
                shell_words::quote(&first_started.to_string_lossy()),
            ),
        );
        let resize_lock = tokio::sync::Mutex::new(());

        let first = async {
            let _guard = resize_lock.lock().await;
            resize_pane_with_timeout(
                &tmux,
                "hs-codex-look:0.0",
                80,
                24,
                false,
                Duration::from_millis(20),
            )
            .await;
        };
        let second = async {
            while !first_started.exists() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            tokio::time::timeout(Duration::from_millis(250), async {
                let _guard = resize_lock.lock().await;
                resize_pane_with_timeout(
                    &tmux,
                    "hs-codex-look:0.0",
                    100,
                    40,
                    false,
                    Duration::from_millis(20),
                )
                .await;
            })
            .await
            .expect("a timed-out resize kept the shared lock");
        };
        tokio::join!(first, second);

        let calls = std::fs::read_to_string(&log).unwrap();
        assert_eq!(
            calls
                .lines()
                .filter(|line| line.contains("resize-window"))
                .count(),
            2,
            "the second resize must run after the first command times out: {calls:?}"
        );
    }

    #[test]
    fn from_name_round_trips_agent_and_key() {
        use clap::ValueEnum;
        for agent in AgentKind::value_variants() {
            assert_eq!(AgentKind::from_name(agent.as_str()), Some(*agent));
        }
        assert_eq!(AgentKind::from_name("banana"), None);
        for key in SpecialKey::value_variants() {
            assert_eq!(SpecialKey::from_name(key.as_str()), Some(*key));
        }
        assert_eq!(SpecialKey::from_name("f13"), None);
    }

    #[test]
    fn backspace_key_round_trips_cli_and_tmux() {
        use clap::Parser;

        let cli = crate::cli::Cli::try_parse_from([
            "agent-session",
            "send",
            "session-id",
            "--key",
            "backspace",
        ])
        .expect("Backspace must be accepted by --key");
        let crate::cli::Command::Send(args) = cli.command else {
            panic!("expected send command");
        };
        assert_eq!(args.keys.len(), 1);
        assert_eq!(args.keys[0].as_str(), "backspace");
        assert_eq!(args.keys[0].tmux_key(), "BSpace");
    }

    #[test]
    fn lagged_live_output_disconnects_the_slow_client() {
        assert!(
            attach_event_bytes(Err(tokio::sync::broadcast::error::RecvError::Lagged(1))).is_none(),
            "a lagged subscriber must disconnect instead of blocking the broker"
        );
        assert!(
            attach_event_bytes(Ok(AttachEvent::Closed)).is_none(),
            "a closed broker must disconnect its subscribers"
        );
    }

    #[tokio::test]
    async fn stalled_websocket_send_is_bounded() {
        struct PendingSink;

        impl futures_util::Sink<Message> for PendingSink {
            type Error = io::Error;

            fn poll_ready(
                self: std::pin::Pin<&mut Self>,
                _cx: &mut std::task::Context<'_>,
            ) -> std::task::Poll<Result<(), Self::Error>> {
                std::task::Poll::Pending
            }

            fn start_send(
                self: std::pin::Pin<&mut Self>,
                _item: Message,
            ) -> Result<(), Self::Error> {
                Ok(())
            }

            fn poll_flush(
                self: std::pin::Pin<&mut Self>,
                _cx: &mut std::task::Context<'_>,
            ) -> std::task::Poll<Result<(), Self::Error>> {
                std::task::Poll::Pending
            }

            fn poll_close(
                self: std::pin::Pin<&mut Self>,
                _cx: &mut std::task::Context<'_>,
            ) -> std::task::Poll<Result<(), Self::Error>> {
                std::task::Poll::Ready(Ok(()))
            }
        }

        assert!(
            !send_attach_message(
                &mut PendingSink,
                Message::Binary(Bytes::from_static(b"blocked")),
                Duration::from_millis(20),
            )
            .await,
            "a stalled WebSocket sink must time out"
        );
    }

    #[tokio::test]
    async fn outbound_writer_reports_a_stalled_sink() {
        struct PendingSink;

        impl futures_util::Sink<Message> for PendingSink {
            type Error = io::Error;

            fn poll_ready(
                self: std::pin::Pin<&mut Self>,
                _cx: &mut std::task::Context<'_>,
            ) -> std::task::Poll<Result<(), Self::Error>> {
                std::task::Poll::Pending
            }

            fn start_send(
                self: std::pin::Pin<&mut Self>,
                _item: Message,
            ) -> Result<(), Self::Error> {
                Ok(())
            }

            fn poll_flush(
                self: std::pin::Pin<&mut Self>,
                _cx: &mut std::task::Context<'_>,
            ) -> std::task::Poll<Result<(), Self::Error>> {
                std::task::Poll::Pending
            }

            fn poll_close(
                self: std::pin::Pin<&mut Self>,
                _cx: &mut std::task::Context<'_>,
            ) -> std::task::Poll<Result<(), Self::Error>> {
                std::task::Poll::Ready(Ok(()))
            }
        }

        let (terminal_tx, terminal_rx) = mpsc::channel(1);
        let (control_tx, control_rx) = mpsc::channel(1);
        terminal_tx
            .send(Message::Binary(Bytes::from_static(b"blocked")))
            .await
            .unwrap();
        drop(terminal_tx);
        drop(control_tx);

        assert_eq!(
            outbound_writer(
                PendingSink,
                terminal_rx,
                control_rx,
                Duration::from_millis(20),
            )
            .await,
            AttachWriterExit::SendFailed
        );
    }

    #[tokio::test]
    async fn normal_broker_close_drains_frames_already_accepted_by_the_writer() {
        #[derive(Clone)]
        struct RecordingSink(Arc<std::sync::Mutex<Vec<Message>>>);

        impl futures_util::Sink<Message> for RecordingSink {
            type Error = io::Error;

            fn poll_ready(
                self: std::pin::Pin<&mut Self>,
                _cx: &mut std::task::Context<'_>,
            ) -> std::task::Poll<Result<(), Self::Error>> {
                std::task::Poll::Ready(Ok(()))
            }

            fn start_send(
                self: std::pin::Pin<&mut Self>,
                item: Message,
            ) -> Result<(), Self::Error> {
                self.0.lock().unwrap().push(item);
                Ok(())
            }

            fn poll_flush(
                self: std::pin::Pin<&mut Self>,
                _cx: &mut std::task::Context<'_>,
            ) -> std::task::Poll<Result<(), Self::Error>> {
                std::task::Poll::Ready(Ok(()))
            }

            fn poll_close(
                self: std::pin::Pin<&mut Self>,
                _cx: &mut std::task::Context<'_>,
            ) -> std::task::Poll<Result<(), Self::Error>> {
                std::task::Poll::Ready(Ok(()))
            }
        }

        let written = Arc::new(std::sync::Mutex::new(Vec::new()));
        let (terminal_tx, terminal_rx) = mpsc::channel(2);
        let (control_tx, control_rx) = mpsc::channel(1);
        terminal_tx
            .send(Message::Binary(Bytes::from_static(b"first")))
            .await
            .unwrap();
        terminal_tx
            .send(Message::Binary(Bytes::from_static(b"second")))
            .await
            .unwrap();
        drop(terminal_tx);
        drop(control_tx);

        let task = AbortOnDropTask::new(tokio::spawn(outbound_writer(
            RecordingSink(written.clone()),
            terminal_rx,
            control_rx,
            Duration::from_secs(1),
        )));
        finish_outbound_writer(task, true).await;

        let written = written.lock().unwrap();
        assert_eq!(written.len(), 2);
        assert_eq!(written[0], Message::Binary(Bytes::from_static(b"first")));
        assert_eq!(written[1], Message::Binary(Bytes::from_static(b"second")));
    }

    #[tokio::test]
    async fn attach_pump_drains_beyond_the_broadcast_ring_capacity() {
        let (broker_tx, broker_rx) = tokio::sync::broadcast::channel(ATTACH_BROADCAST_CAPACITY);
        let (terminal_tx, mut terminal_rx) = mpsc::channel(ATTACH_BROADCAST_CAPACITY * 2);
        let task = tokio::spawn(pump_attach_events(broker_rx, terminal_tx));

        for index in 0..=ATTACH_BROADCAST_CAPACITY {
            broker_tx
                .send(AttachEvent::Output(Bytes::from(vec![index as u8])))
                .unwrap();
            tokio::task::yield_now().await;
        }
        broker_tx.send(AttachEvent::Closed).unwrap();

        assert_eq!(task.await.unwrap(), AttachPumpExit::BrokerClosed);
        let mut received = 0;
        while terminal_rx.recv().await.is_some() {
            received += 1;
        }
        assert_eq!(received, ATTACH_BROADCAST_CAPACITY + 1);
    }

    #[tokio::test]
    async fn dropping_an_attach_child_task_aborts_it() {
        struct DropSignal(Option<tokio::sync::oneshot::Sender<()>>);

        impl Drop for DropSignal {
            fn drop(&mut self) {
                if let Some(sender) = self.0.take() {
                    let _ = sender.send(());
                }
            }
        }

        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (dropped_tx, dropped_rx) = tokio::sync::oneshot::channel();
        let task = AbortOnDropTask::new(tokio::spawn(async move {
            let _signal = DropSignal(Some(dropped_tx));
            let _ = started_tx.send(());
            std::future::pending::<()>().await;
        }));
        started_rx.await.unwrap();
        drop(task);

        tokio::time::timeout(Duration::from_millis(250), dropped_rx)
            .await
            .expect("aborted task must drop its owned state")
            .expect("drop signal");
    }

    #[tokio::test]
    async fn snapshot_handoff_recaptures_after_internal_buffer_overflow() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state_dir = tmp.path().join("state");
        let calls = tmp.path().join("capture-calls");
        let first_started = tmp.path().join("first-started");
        let first_release = tmp.path().join("first-release");
        let second_started = tmp.path().join("second-started");
        let second_release = tmp.path().join("second-release");
        seed_session(&state_dir, "recapture", "codex", "hs-codex-recapture");
        let tmux = executable(
            &tmp.path().join("tmux"),
            &format!(
                r#"#!/usr/bin/env sh
if [ "$1" = "capture-pane" ]; then
  printf 'capture\n' >> {calls}
  count=$(wc -l < {calls})
  if [ "$count" -eq 1 ]; then
    : > {first_started}
    while [ ! -f {first_release} ]; do sleep 0.01; done
    printf 'stale\n'
  else
    : > {second_started}
    while [ ! -f {second_release} ]; do sleep 0.01; done
    printf 'fresh\n'
  fi
fi
exit 0
"#,
                calls = shell_words::quote(&calls.to_string_lossy()),
                first_started = shell_words::quote(&first_started.to_string_lossy()),
                first_release = shell_words::quote(&first_release.to_string_lossy()),
                second_started = shell_words::quote(&second_started.to_string_lossy()),
                second_release = shell_words::quote(&second_release.to_string_lossy()),
            ),
        );
        let context = CliContext {
            state_dir,
            host: None,
        };
        let record = load_session_record(&context, "recapture").unwrap();
        let (broker_tx, mut broker_rx) = tokio::sync::broadcast::channel(ATTACH_BROADCAST_CAPACITY);
        let task =
            tokio::spawn(
                async move { capture_attach_handoff(&tmux, &record, &mut broker_rx).await },
            );

        wait_for_file(&first_started).await;
        for index in 0..=ATTACH_HANDOFF_BUFFER_CAPACITY {
            broker_tx
                .send(AttachEvent::Output(Bytes::from(vec![index as u8])))
                .unwrap();
            tokio::task::yield_now().await;
        }
        std::fs::write(&first_release, b"").unwrap();
        wait_for_file(&second_started).await;
        broker_tx
            .send(AttachEvent::Output(Bytes::from_static(b"after-recapture")))
            .unwrap();
        tokio::task::yield_now().await;
        std::fs::write(&second_release, b"").unwrap();

        let handoff = task.await.unwrap().unwrap();
        assert_eq!(handoff.snapshot.as_deref(), Some("fresh\n"));
        assert_eq!(handoff.live.len(), 1);
        assert_eq!(handoff.live[0], Bytes::from_static(b"after-recapture"));
        assert_eq!(std::fs::read_to_string(calls).unwrap().lines().count(), 2);
    }

    #[tokio::test]
    async fn attach_broker_fans_out_and_only_last_subscriber_tears_down_pipe() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state_dir = tmp.path().join("state");
        let calls = tmp.path().join("calls.log");
        let source = tmp.path().join("pane-output.fifo");
        let writer_pid = tmp.path().join("writer.pid");
        create_private_fifo(&source).unwrap();
        seed_session(&state_dir, "fanout", "codex", "hs-codex-fanout");

        let tmux = fanout_tmux(tmp.path(), &calls, &source, &writer_pid);
        let context = CliContext {
            state_dir: state_dir.clone(),
            host: None,
        };
        let record = load_session_record(&context, "fanout").unwrap();
        let registry = AttachBrokerRegistry::default();
        let live_fifo = session_dir(&context, "fanout").join(ATTACH_LIVE_FIFO_NAME);
        crate::write_private_file(&live_fifo, b"stale daemon bytes").unwrap();

        let mut first = registry.subscribe(&context, &tmux, &record).await.unwrap();
        let mut second = registry.subscribe(&context, &tmux, &record).await.unwrap();
        assert!(
            std::fs::symlink_metadata(&live_fifo)
                .unwrap()
                .file_type()
                .is_fifo(),
            "the live broker transport must be an ephemeral FIFO"
        );

        let mut source_writer = std::fs::OpenOptions::new()
            .write(true)
            .open(&source)
            .unwrap();
        source_writer.write_all(b"hello").unwrap();
        source_writer.flush().unwrap();

        assert_eq!(recv_attach_bytes(&mut first).await, b"hello");
        assert_eq!(recv_attach_bytes(&mut second).await, b"hello");

        let closes_before_detach = pipe_close_count(&calls);
        first.release().await;
        assert_eq!(
            pipe_close_count(&calls),
            closes_before_detach,
            "disconnecting one subscriber must not close the shared tmux pipe"
        );
        assert!(live_fifo.exists());

        source_writer.write_all(b"again").unwrap();
        source_writer.flush().unwrap();
        assert_eq!(recv_attach_bytes(&mut second).await, b"again");

        second.release().await;
        assert_eq!(
            pipe_close_count(&calls),
            closes_before_detach + 1,
            "the final subscriber must close the shared tmux pipe"
        );
        assert!(
            !live_fifo.exists(),
            "broker teardown must remove the ephemeral FIFO"
        );

        let calls = std::fs::read_to_string(&calls).unwrap();
        let enables: Vec<_> = calls
            .lines()
            .filter(|line| line.contains("cat >"))
            .collect();
        assert_eq!(
            enables.len(),
            1,
            "simultaneous subscribers must share one tmux pipe: {calls:?}"
        );
        assert!(
            !enables[0].split_whitespace().any(|arg| arg == "-o"),
            "a fresh broker must replace a stale tmux pipe instead of preserving it: {calls:?}"
        );
    }

    #[tokio::test]
    async fn websocket_attach_fans_out_live_output_after_snapshot() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state_dir = tmp.path().join("state");
        let calls = tmp.path().join("calls.log");
        let source = tmp.path().join("pane-output.fifo");
        let writer_pid = tmp.path().join("writer.pid");
        create_private_fifo(&source).unwrap();
        seed_session(&state_dir, "socket", "codex", "hs-codex-socket");

        let tmux = fanout_tmux(tmp.path(), &calls, &source, &writer_pid);
        let state = state(&state_dir, Some(TOKEN), tmux);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = router(state.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let connect = || async {
            let mut request = format!("ws://{address}/sessions/socket/attach")
                .into_client_request()
                .unwrap();
            request
                .headers_mut()
                .insert("authorization", format!("Bearer {TOKEN}").parse().unwrap());
            tokio_tungstenite::connect_async(request).await.unwrap().0
        };
        let mut first = connect().await;
        let mut second = connect().await;
        assert_eq!(recv_websocket_binary(&mut first).await, b"pane\n");
        assert_eq!(recv_websocket_binary(&mut second).await, b"pane\n");
        wait_for_subscriber_count(&state.attach_brokers, "socket", 2).await;

        let mut source_writer = std::fs::OpenOptions::new()
            .write(true)
            .open(&source)
            .unwrap();
        source_writer.write_all(b"both-clients").unwrap();
        source_writer.flush().unwrap();
        assert_eq!(recv_websocket_binary(&mut first).await, b"both-clients");
        assert_eq!(recv_websocket_binary(&mut second).await, b"both-clients");

        first.close(None).await.unwrap();
        wait_for_subscriber_count(&state.attach_brokers, "socket", 1).await;
        source_writer.write_all(b"remaining-client").unwrap();
        source_writer.flush().unwrap();
        assert_eq!(
            recv_websocket_binary(&mut second).await,
            b"remaining-client"
        );

        second.close(None).await.unwrap();
        wait_for_subscriber_count(&state.attach_brokers, "socket", 0).await;
        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn websocket_attach_buffers_live_output_during_snapshot_capture() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state_dir = tmp.path().join("state");
        let calls = tmp.path().join("calls.log");
        let fifo_destination = tmp.path().join("fifo-destination");
        let writer_pid = tmp.path().join("writer.pid");
        let capture_started = tmp.path().join("capture-started");
        let capture_release = tmp.path().join("capture-release");
        seed_session(&state_dir, "handoff", "codex", "hs-codex-handoff");

        let tmux = snapshot_handoff_tmux(
            tmp.path(),
            &calls,
            &fifo_destination,
            &writer_pid,
            &capture_started,
            &capture_release,
        );
        let state = state(&state_dir, Some(TOKEN), tmux);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = router(state.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let mut request = format!("ws://{address}/sessions/handoff/attach")
            .into_client_request()
            .unwrap();
        request
            .headers_mut()
            .insert("authorization", format!("Bearer {TOKEN}").parse().unwrap());
        let mut socket = tokio_tungstenite::connect_async(request).await.unwrap().0;
        tokio::time::timeout(Duration::from_secs(1), async {
            while !capture_started.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("capture-pane never reached its controlled wait");

        let live_fifo = PathBuf::from(std::fs::read_to_string(&fifo_destination).unwrap());
        let mut live_writer = std::fs::OpenOptions::new()
            .write(true)
            .open(live_fifo)
            .unwrap();
        live_writer.write_all(b"during-capture").unwrap();
        live_writer.flush().unwrap();
        std::fs::write(&capture_release, b"").unwrap();

        assert_eq!(recv_websocket_binary(&mut socket).await, b"pane\n");
        assert_eq!(recv_websocket_binary(&mut socket).await, b"during-capture");
        socket.close(None).await.unwrap();
        wait_for_subscriber_count(&state.attach_brokers, "handoff", 0).await;
        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn snapshot_timeout_releases_the_final_attach_broker() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state_dir = tmp.path().join("state");
        let calls = tmp.path().join("calls.log");
        let fifo_destination = tmp.path().join("fifo-destination");
        let writer_pid = tmp.path().join("writer.pid");
        let capture_started = tmp.path().join("capture-started");
        let capture_release = tmp.path().join("never-release");
        seed_session(&state_dir, "timeout", "codex", "hs-codex-timeout");

        let tmux = snapshot_handoff_tmux(
            tmp.path(),
            &calls,
            &fifo_destination,
            &writer_pid,
            &capture_started,
            &capture_release,
        );
        let state = state(&state_dir, Some(TOKEN), tmux);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = router(state.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let mut request = format!("ws://{address}/sessions/timeout/attach")
            .into_client_request()
            .unwrap();
        request
            .headers_mut()
            .insert("authorization", format!("Bearer {TOKEN}").parse().unwrap());
        let mut socket = tokio_tungstenite::connect_async(request).await.unwrap().0;
        tokio::time::timeout(Duration::from_secs(1), async {
            while !capture_started.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("capture-pane never started");
        assert_eq!(state.attach_brokers.subscriber_count("timeout").await, 1);
        tokio::time::timeout(Duration::from_secs(4), async {
            loop {
                if state.attach_brokers.subscriber_count("timeout").await == 0 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("snapshot timeout did not release the final broker lease");
        assert!(
            !session_dir(&state.context, "timeout")
                .join(ATTACH_LIVE_FIFO_NAME)
                .exists(),
            "snapshot timeout must remove the broker FIFO"
        );
        let _ = socket.close(None).await;
        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn dropping_last_attach_subscription_releases_the_broker() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state_dir = tmp.path().join("state");
        let calls = tmp.path().join("calls.log");
        let source = tmp.path().join("pane-output.fifo");
        let writer_pid = tmp.path().join("writer.pid");
        create_private_fifo(&source).unwrap();
        seed_session(&state_dir, "cancel", "codex", "hs-codex-cancel");

        let tmux = fanout_tmux(tmp.path(), &calls, &source, &writer_pid);
        let context = CliContext {
            state_dir: state_dir.clone(),
            host: None,
        };
        let record = load_session_record(&context, "cancel").unwrap();
        let registry = AttachBrokerRegistry::default();
        let live_fifo = session_dir(&context, "cancel").join(ATTACH_LIVE_FIFO_NAME);

        let subscription = registry.subscribe(&context, &tmux, &record).await.unwrap();
        assert_eq!(registry.subscriber_count("cancel").await, 1);
        drop(subscription);

        wait_for_subscriber_count(&registry, "cancel", 0).await;
        assert!(
            !live_fifo.exists(),
            "cancelling an attach task must remove its final broker FIFO"
        );
        assert_eq!(pipe_close_count(&calls), 1);
    }

    #[tokio::test]
    async fn aborting_release_during_teardown_does_not_strand_the_broker() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state_dir = tmp.path().join("state");
        let calls = tmp.path().join("calls.log");
        let close_started = tmp.path().join("close-started");
        seed_session(&state_dir, "abort", "codex", "hs-codex-abort");
        let tmux = executable(
            &tmp.path().join("tmux"),
            &format!(
                r#"#!/usr/bin/env sh
printf '%s\n' "$*" >> {}
if [ "$1" = "pipe-pane" ]; then
  last=""
  for arg in "$@"; do last="$arg"; done
  case "$last" in
    "cat > "*) ;;
    *)
      if [ ! -e {} ]; then
        : > {}
        sleep 1
      fi
      ;;
  esac
fi
exit 0
"#,
                shell_words::quote(&calls.to_string_lossy()),
                shell_words::quote(&close_started.to_string_lossy()),
                shell_words::quote(&close_started.to_string_lossy()),
            ),
        );
        let context = CliContext {
            state_dir,
            host: None,
        };
        let record = load_session_record(&context, "abort").unwrap();
        let registry = AttachBrokerRegistry::default();
        let live_fifo = session_dir(&context, "abort").join(ATTACH_LIVE_FIFO_NAME);
        let subscription = registry.subscribe(&context, &tmux, &record).await.unwrap();

        let release_task = tokio::spawn(subscription.release());
        tokio::time::timeout(Duration::from_secs(1), async {
            while !close_started.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("broker teardown never reached the blocking close command");
        release_task.abort();
        let _ = release_task.await;

        tokio::time::timeout(Duration::from_secs(2), async {
            while live_fifo.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("supervised teardown did not remove the broker FIFO");
        assert_eq!(pipe_close_count(&calls), 1);

        let replacement = registry.subscribe(&context, &tmux, &record).await.unwrap();
        replacement.release().await;
        assert_eq!(pipe_close_count(&calls), 2);
    }

    #[tokio::test]
    async fn slow_broker_start_does_not_block_an_unrelated_session() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state_dir = tmp.path().join("state");
        let slow_started = tmp.path().join("slow-started");
        seed_session(&state_dir, "slow", "codex", "hs-codex-slow");
        seed_session(&state_dir, "fast", "codex", "hs-codex-fast");
        let tmux = executable(
            &tmp.path().join("tmux"),
            &format!(
                "#!/usr/bin/env sh\ncase \"$*\" in\n  *hs-codex-slow*\\ cat\\ \\>* ) : > {}; sleep 1 ;;\nesac\nexit 0\n",
                shell_words::quote(&slow_started.to_string_lossy())
            ),
        );
        let context = CliContext {
            state_dir,
            host: None,
        };
        let slow_record = load_session_record(&context, "slow").unwrap();
        let fast_record = load_session_record(&context, "fast").unwrap();
        let registry = Arc::new(AttachBrokerRegistry::default());

        let slow_task = tokio::spawn({
            let registry = registry.clone();
            let context = context.clone();
            let tmux = tmux.clone();
            async move {
                registry
                    .subscribe(&context, &tmux, &slow_record)
                    .await
                    .unwrap()
            }
        });
        tokio::time::timeout(Duration::from_secs(1), async {
            while !slow_started.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("slow session never began broker startup");

        let fast = tokio::time::timeout(
            Duration::from_millis(250),
            registry.subscribe(&context, &tmux, &fast_record),
        )
        .await
        .expect("an unrelated session was blocked by slow broker startup")
        .unwrap();
        fast.release().await;

        let slow = slow_task.await.unwrap();
        slow.release().await;
    }

    #[tokio::test]
    async fn late_subscriber_restarts_a_closed_broker_generation() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state_dir = tmp.path().join("state");
        let calls = tmp.path().join("calls.log");
        let source = tmp.path().join("pane-output.fifo");
        let writer_pid = tmp.path().join("writer.pid");
        create_private_fifo(&source).unwrap();
        seed_session(&state_dir, "restart", "codex", "hs-codex-restart");

        let tmux = fanout_tmux(tmp.path(), &calls, &source, &writer_pid);
        let context = CliContext {
            state_dir,
            host: None,
        };
        let record = load_session_record(&context, "restart").unwrap();
        let registry = AttachBrokerRegistry::default();
        let mut first = registry.subscribe(&context, &tmux, &record).await.unwrap();

        let mut source_writer = std::fs::OpenOptions::new()
            .write(true)
            .open(&source)
            .unwrap();
        source_writer.write_all(b"before-close").unwrap();
        source_writer.flush().unwrap();
        assert_eq!(recv_attach_bytes(&mut first).await, b"before-close");
        drop(source_writer);
        terminate_process_from_file(&writer_pid);

        let closed = tokio::time::timeout(Duration::from_secs(5), first.receiver_mut().recv())
            .await
            .expect("timed out waiting for the old broker to close");
        assert!(attach_event_bytes(closed).is_none());

        let mut second = registry.subscribe(&context, &tmux, &record).await.unwrap();
        let mut source_writer = std::fs::OpenOptions::new()
            .write(true)
            .open(&source)
            .unwrap();
        source_writer.write_all(b"after-restart").unwrap();
        source_writer.flush().unwrap();
        assert_eq!(recv_attach_bytes(&mut second).await, b"after-restart");

        first.release().await;
        second.release().await;
        let enables = std::fs::read_to_string(&calls)
            .unwrap()
            .lines()
            .filter(|line| line.contains("cat >"))
            .count();
        assert_eq!(enables, 2, "a closed generation must be replaced once");
    }

    async fn recv_attach_bytes(subscription: &mut AttachSubscription) -> Vec<u8> {
        let event =
            tokio::time::timeout(Duration::from_secs(2), subscription.receiver_mut().recv())
                .await
                .expect("timed out waiting for broker output");
        attach_event_bytes(event)
            .expect("broker closed before delivering output")
            .to_vec()
    }

    async fn recv_websocket_binary<S>(socket: &mut S) -> Vec<u8>
    where
        S: futures_util::Stream<
                Item = Result<
                    tokio_tungstenite::tungstenite::Message,
                    tokio_tungstenite::tungstenite::Error,
                >,
            > + Unpin,
    {
        let message = tokio::time::timeout(Duration::from_secs(2), socket.next())
            .await
            .expect("timed out waiting for WebSocket output")
            .expect("WebSocket closed before output")
            .expect("WebSocket output failed");
        match message {
            tokio_tungstenite::tungstenite::Message::Binary(bytes) => bytes.to_vec(),
            other => panic!("expected binary WebSocket output, got {other:?}"),
        }
    }

    async fn wait_for_subscriber_count(
        registry: &AttachBrokerRegistry,
        session_id: &str,
        expected: usize,
    ) {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if registry.subscriber_count(session_id).await == expected {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("timed out waiting for subscriber cleanup");
    }

    async fn wait_for_file(path: &Path) {
        tokio::time::timeout(Duration::from_secs(2), async {
            while !path.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("timed out waiting for test marker");
    }

    fn pipe_close_count(calls: &Path) -> usize {
        std::fs::read_to_string(calls)
            .unwrap_or_default()
            .lines()
            .filter(|line| line.starts_with("pipe-pane") && !line.contains("cat >"))
            .count()
    }

    fn terminate_process_from_file(pid_file: &Path) {
        let pid = std::fs::read_to_string(pid_file)
            .unwrap()
            .trim()
            .parse::<libc::pid_t>()
            .unwrap();
        // SAFETY: the pid comes from the test-owned tmux stub. ESRCH is fine:
        // closing the source FIFO may have already ended the writer naturally.
        if unsafe { libc::kill(pid, libc::SIGTERM) } != 0 {
            let err = io::Error::last_os_error();
            assert_eq!(err.raw_os_error(), Some(libc::ESRCH));
        }
    }

    fn fanout_tmux(dir: &Path, calls: &Path, source: &Path, writer_pid: &Path) -> PathBuf {
        executable(
            &dir.join("tmux"),
            &format!(
                r#"#!/usr/bin/env sh
printf '%s\n' "$*" >> {}
if [ "$1" = "capture-pane" ]; then
  printf 'pane\n'
  exit 0
fi
if [ "$1" = "pipe-pane" ]; then
  last=""
  for arg in "$@"; do last="$arg"; done
  case "$last" in
    "cat > "*)
      dest=${{last#cat > }}
      (exec 3>"$dest"; exec cat {} >&3) &
      printf '%s\n' "$!" > {}
      ;;
    *)
      if [ -s {} ]; then
        kill "$(cat {})" 2>/dev/null || true
        rm -f {}
      fi
      ;;
  esac
fi
exit 0
"#,
                shell_words::quote(&calls.to_string_lossy()),
                shell_words::quote(&source.to_string_lossy()),
                shell_words::quote(&writer_pid.to_string_lossy()),
                shell_words::quote(&writer_pid.to_string_lossy()),
                shell_words::quote(&writer_pid.to_string_lossy()),
                shell_words::quote(&writer_pid.to_string_lossy()),
            ),
        )
    }

    fn snapshot_handoff_tmux(
        dir: &Path,
        calls: &Path,
        fifo_destination: &Path,
        writer_pid: &Path,
        capture_started: &Path,
        capture_release: &Path,
    ) -> PathBuf {
        executable(
            &dir.join("tmux"),
            &format!(
                r#"#!/usr/bin/env sh
printf '%s\n' "$*" >> {}
if [ "$1" = "capture-pane" ]; then
  : > {}
  while [ ! -e {} ]; do sleep 0.01; done
  printf 'pane\n'
  exit 0
fi
if [ "$1" = "pipe-pane" ]; then
  last=""
  for arg in "$@"; do last="$arg"; done
  case "$last" in
    "cat > "*)
      dest=${{last#cat > }}
      printf '%s' "$dest" > {}
      (exec 3>"$dest"; exec sleep 30) &
      printf '%s\n' "$!" > {}
      ;;
    *)
      if [ -s {} ]; then
        kill "$(cat {})" 2>/dev/null || true
        rm -f {}
      fi
      ;;
  esac
fi
exit 0
"#,
                shell_words::quote(&calls.to_string_lossy()),
                shell_words::quote(&capture_started.to_string_lossy()),
                shell_words::quote(&capture_release.to_string_lossy()),
                shell_words::quote(&fifo_destination.to_string_lossy()),
                shell_words::quote(&writer_pid.to_string_lossy()),
                shell_words::quote(&writer_pid.to_string_lossy()),
                shell_words::quote(&writer_pid.to_string_lossy()),
                shell_words::quote(&writer_pid.to_string_lossy()),
            ),
        )
    }

    #[test]
    fn private_files_and_attach_fifos_are_0600() {
        let tmp = tempfile::TempDir::new().unwrap();
        let file_path = tmp.path().join("private.file");
        crate::write_private_file(&file_path, b"").unwrap();
        let mode = std::fs::metadata(&file_path).unwrap().permissions().mode();
        assert_eq!(
            mode & 0o077,
            0,
            "private files must not be group/world readable"
        );

        let fifo_path = tmp.path().join("attach.fifo");
        create_private_fifo(&fifo_path).unwrap();
        let metadata = std::fs::metadata(&fifo_path).unwrap();
        assert!(metadata.file_type().is_fifo());
        assert_eq!(
            metadata.permissions().mode() & 0o077,
            0,
            "live terminal FIFO must not be group/world readable"
        );
    }

    #[tokio::test]
    async fn broker_start_removes_fifo_after_post_create_failure() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state_dir = tmp.path().join("state");
        seed_session(&state_dir, "rollback", "codex", "hs-codex-rollback");
        let context = CliContext {
            state_dir,
            host: None,
        };
        let record = load_session_record(&context, "rollback").unwrap();
        let fifo_path = session_dir(&context, "rollback").join(ATTACH_LIVE_FIFO_NAME);

        let result = AttachBroker::start_with_fifo_opener(
            &context,
            Path::new("unused-tmux"),
            &record,
            |_| Err(io::Error::other("injected FIFO open failure")),
        )
        .await;
        assert!(result.is_err());
        assert!(
            !fifo_path.exists(),
            "broker startup failure must roll back its ephemeral FIFO"
        );
    }

    fn logging_tmux(dir: &Path, log: &Path) -> PathBuf {
        let bin = dir.join("tmux");
        std::fs::write(
            &bin,
            format!(
                "#!/usr/bin/env sh\nprintf '%s\\n' \"$*\" >> {}\ncase \"$1\" in\n  has-session) exit 0 ;;\n  capture-pane) printf 'pane\\n'; exit 0 ;;\n  *) exit 0 ;;\nesac\n",
                shell_words::quote(&log.to_string_lossy())
            ),
        )
        .unwrap();
        let mut perms = std::fs::metadata(&bin).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&bin, perms).unwrap();
        bin
    }

    /// tmux stub that logs every invocation to `calls` and, on `load-buffer`,
    /// appends the pasted buffer file's content to `pasted`. `running` toggles
    /// the `has-session` result so title-rename gating can be exercised for both
    /// live and stopped sessions.
    fn rename_probe_tmux(dir: &Path, calls: &Path, pasted: &Path, running: bool) -> PathBuf {
        let bin = dir.join("tmux");
        let has_session_exit = if running { 0 } else { 1 };
        std::fs::write(
            &bin,
            format!(
                "#!/usr/bin/env sh\nprintf '%s\\n' \"$*\" >> {}\ncase \"$1\" in\n  has-session) exit {} ;;\n  load-buffer) cat \"$4\" >> {}; exit 0 ;;\n  capture-pane) printf 'pane\\n'; exit 0 ;;\n  *) exit 0 ;;\nesac\n",
                shell_words::quote(&calls.to_string_lossy()),
                has_session_exit,
                shell_words::quote(&pasted.to_string_lossy()),
            ),
        )
        .unwrap();
        let mut perms = std::fs::metadata(&bin).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&bin, perms).unwrap();
        bin
    }

    fn test_record(id: &str, tmux_session: &str) -> crate::SessionRecord {
        crate::SessionRecord {
            schema_version: crate::SESSION_DOCUMENT_VERSION.to_string(),
            id: id.to_string(),
            agent: "codex".to_string(),
            mode: "interactive".to_string(),
            title: None,
            title_revision: 0,
            cwd: "/tmp".to_string(),
            tmux_session: tmux_session.to_string(),
            prompt_file: None,
            log_file: None,
            created_at: "2000-01-01T00:00:00Z".to_string(),
            updated_at: "2000-01-01T00:00:00Z".to_string(),
            provider_resume: None,
            runtime: None,
            agent_args: Vec::new(),
            agent_bin: None,
            extra: std::collections::BTreeMap::new(),
            resume_sidecar_extra: std::collections::BTreeMap::new(),
        }
    }

    fn provider_discovery_record(
        id: &str,
        tmux_session: &str,
        launch_id: &str,
        generation: u64,
    ) -> crate::SessionRecord {
        let mut record = test_record(id, tmux_session);
        record.agent = "claude".to_string();
        record.provider_resume = Some(crate::ProviderResume {
            provider: "claude".to_string(),
            session_id: format!("{id}-provider"),
            captured_at: "2026-07-11T00:00:00Z".to_string(),
            capture_method: "claude-explicit-session-id".to_string(),
            resume_args: vec!["--resume".to_string(), format!("{id}-provider")],
            extra: std::collections::BTreeMap::new(),
        });
        record.runtime = Some(crate::RuntimeInfo {
            kind: "tmux".to_string(),
            tmux_session: tmux_session.to_string(),
            generation,
            started_at: "2026-07-11T00:00:00Z".to_string(),
            launch_id: launch_id.to_string(),
            extra: std::collections::BTreeMap::new(),
        });
        record
    }
}
