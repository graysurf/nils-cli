//! Centralized, privacy-safe control-plane event plane
//! (`agent-session.observation.v1`).
//!
//! Before this module existed, control-plane evidence was split between
//! per-session activity journals, a replaceable current hook diagnostic, an
//! opt-in `agent-hook --trace` that the installed provider ingress never
//! passes, and unstructured daemon stderr. Failures that happened before a
//! provider payload could be normalized, or before a policy could be loaded,
//! left no durable record at all — which is exactly the class of failure that
//! deadlocked live sessions in `sympoies/nils-cli#1409`.
//!
//! ## Failure-domain independence
//!
//! Writers append to bounded local spool segments directly. Nothing here
//! requires a healthy `agent-session serve` daemon, coordination broker, or
//! capability: recovery-critical logging must never sit behind the subsystem it
//! is diagnosing. Aggregation and indexing happen later, on read.
//!
//! ## Privacy budget
//!
//! Events carry classification only. Every string field is validated against a
//! bounded character set before a write is attempted, and a field that does not
//! validate refuses the whole append instead of writing unchecked bytes.
//! Prompts, transcripts, command bodies, filesystem paths, raw provider or
//! session identities, capabilities, and provider error text can therefore not
//! reach the plane by construction. Correlation is a digest, never an identity.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Literal schema version for every persisted observation event.
pub const OBSERVATION_VERSION: &str = "agent-session.observation.v1";

/// Maximum retained bytes for one spool segment.
pub const MAX_SEGMENT_BYTES: u64 = 256 * 1024;
/// Maximum retained spool segments.
pub const MAX_SEGMENTS: usize = 16;
/// Retention horizon for a spool segment, in seconds.
pub const RETENTION_SECONDS: i64 = 14 * 24 * 60 * 60;
/// Maximum accepted length for a bounded classification token.
const MAX_TOKEN_BYTES: usize = 128;
/// Maximum accepted length for a bounded recovery-action hint.
const MAX_ACTION_BYTES: usize = 256;
/// Maximum bytes read back from one segment.
const MAX_READ_BYTES: u64 = MAX_SEGMENT_BYTES.saturating_mul(2);

const SPOOL_RELATIVE: &str = "observation/spool";
const SEGMENT_PREFIX: &str = "segment-";
const SEGMENT_SUFFIX: &str = ".jsonl";

/// Which control-plane component produced an event.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
pub enum Component {
    /// The provider hook ingress binary.
    AgentHook,
    /// The session control plane, including its serve daemon.
    AgentSession,
    /// An immutable launcher or supervisor that records exec failure for a
    /// target binary that never started.
    Launcher,
}

impl Component {
    /// Stable kebab-case name, also used as the spool segment owner tag.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AgentHook => "agent-hook",
            Self::AgentSession => "agent-session",
            Self::Launcher => "launcher",
        }
    }
}

/// Display severity for a recorded event.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
pub enum Severity {
    /// Expected terminal outcome.
    Info,
    /// Degraded but live: the lane continued under a reduced contract.
    Warn,
    /// The stage failed and its caller could not complete normally.
    Error,
    /// The failure domain itself is compromised.
    Critical,
}

impl Severity {
    /// Stable kebab-case name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
            Self::Critical => "critical",
        }
    }
}

/// Why a field was refused. The offending value is never included.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldError {
    /// The field was empty.
    Empty,
    /// The field exceeded its byte budget.
    TooLong,
    /// The field contained a character outside its permitted set.
    Unsupported,
}

/// Why a spool operation failed. Callers treat these as best-effort.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpoolError {
    /// A field did not satisfy the privacy budget, so nothing was written.
    Field(FieldError),
    /// The spool root could not be resolved, created, or trusted.
    Untrusted,
    /// The spool was temporarily unavailable.
    Unavailable,
}

impl From<FieldError> for SpoolError {
    fn from(error: FieldError) -> Self {
        Self::Field(error)
    }
}

/// One bounded, redacted control-plane observation.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Event {
    /// Literal `agent-session.observation.v1`.
    pub schema_version: String,
    /// RFC3339 UTC instant the event was recorded.
    pub recorded_at: String,
    /// Unix second the event was recorded, for cheap windowing.
    pub recorded_at_epoch: i64,
    /// Producing component.
    pub component: Component,
    /// Stable pipeline stage slug, for example `dispatch` or `normalize`.
    pub stage: String,
    /// Stable outcome code slug.
    pub code: String,
    /// Display severity.
    pub severity: Severity,
    /// Producing binary release.
    pub binary_version: String,
    /// Canonical provider name, when the stage had one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub product: Option<String>,
    /// Canonical provider event name, when the stage had one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event: Option<String>,
    /// Terminal disposition slug, for example `allow` or `reconciliation-pending`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disposition: Option<String>,
    /// Wall duration of the stage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// Peer release observed across a protocol boundary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peer_version: Option<String>,
    /// Opaque runtime generation, for example a state revision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_generation: Option<String>,
    /// `sha256:<64 hex>` correlation digest. Never a raw identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation: Option<String>,
    /// One bounded, safe next action for the operator.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery_action: Option<String>,
}

impl Event {
    /// Build one validated event.
    ///
    /// `recorded_at_epoch` is supplied by the caller so this crate stays free of
    /// wall-clock reads on the render path and so tests remain deterministic.
    pub fn new(
        component: Component,
        stage: &str,
        code: &str,
        severity: Severity,
        binary_version: &str,
        recorded_at_epoch: i64,
    ) -> Result<Self, FieldError> {
        Ok(Self {
            schema_version: OBSERVATION_VERSION.to_string(),
            recorded_at: rfc3339(recorded_at_epoch),
            recorded_at_epoch,
            component,
            stage: token(stage)?,
            code: token(code)?,
            severity,
            binary_version: version(binary_version)?,
            product: None,
            event: None,
            disposition: None,
            duration_ms: None,
            peer_version: None,
            runtime_generation: None,
            correlation: None,
            recovery_action: None,
        })
    }

    /// Attach the canonical provider and event names.
    pub fn with_provider(mut self, product: &str, event: &str) -> Result<Self, FieldError> {
        self.product = Some(token(product)?);
        self.event = Some(alphanumeric_token(event)?);
        Ok(self)
    }

    /// Attach the terminal disposition slug.
    pub fn with_disposition(mut self, disposition: &str) -> Result<Self, FieldError> {
        self.disposition = Some(token(disposition)?);
        Ok(self)
    }

    /// Attach the stage duration.
    pub fn with_duration_ms(mut self, duration_ms: u64) -> Self {
        self.duration_ms = Some(duration_ms);
        self
    }

    /// Attach the peer release observed across a protocol boundary.
    pub fn with_peer_version(mut self, peer_version: &str) -> Result<Self, FieldError> {
        self.peer_version = Some(version(peer_version)?);
        Ok(self)
    }

    /// Attach an opaque runtime generation marker.
    pub fn with_runtime_generation(mut self, generation: &str) -> Result<Self, FieldError> {
        self.runtime_generation = Some(token(generation)?);
        Ok(self)
    }

    /// Attach a `sha256:<64 hex>` correlation digest.
    pub fn with_correlation(mut self, correlation: &str) -> Result<Self, FieldError> {
        self.correlation = Some(digest(correlation)?);
        Ok(self)
    }

    /// Attach one bounded safe next action.
    pub fn with_recovery_action(mut self, action: &str) -> Result<Self, FieldError> {
        self.recovery_action = Some(recovery_action(action)?);
        Ok(self)
    }
}

/// Append one event to the component's bounded spool.
///
/// Best-effort by contract: a caller on a decision path records the returned
/// error as diagnostics but must not change its decision because logging failed.
pub fn append(state_root: &Path, event: &Event) -> Result<(), SpoolError> {
    if event.schema_version != OBSERVATION_VERSION {
        return Err(SpoolError::Field(FieldError::Unsupported));
    }
    let mut line = serde_json::to_vec(event).map_err(|_| SpoolError::Unavailable)?;
    if line.len() as u64 > MAX_SEGMENT_BYTES {
        return Err(SpoolError::Field(FieldError::TooLong));
    }
    line.push(b'\n');
    let directory = ensure_spool_root(state_root)?;
    let guard = lock_spool(&directory)?;
    prune(&segment_paths(&directory), event.recorded_at_epoch);
    let target = select_segment(&directory, line.len() as u64);
    let mut file = OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&target)
        .map_err(|error| classify(&error))?;
    let metadata = file.metadata().map_err(|_| SpoolError::Unavailable)?;
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err(SpoolError::Untrusted);
    }
    file.write_all(&line).map_err(|_| SpoolError::Unavailable)?;
    drop(guard);
    Ok(())
}

/// Read the most recent events across every retained segment, oldest first.
pub fn read_recent(state_root: &Path, limit: usize) -> Result<Vec<Event>, SpoolError> {
    let directory = spool_root(state_root);
    if !directory.is_dir() {
        return Ok(Vec::new());
    }
    let mut events = Vec::new();
    for segment in segment_paths(&directory) {
        let Ok(bytes) = read_private(&segment, MAX_READ_BYTES) else {
            continue;
        };
        let Ok(body) = String::from_utf8(bytes) else {
            continue;
        };
        for line in body.lines().filter(|line| !line.trim().is_empty()) {
            // A truncated tail line from a concurrent rotation is skipped
            // rather than failing the whole read.
            if let Ok(event) = serde_json::from_str::<Event>(line) {
                events.push(event);
            }
        }
    }
    events.sort_by_key(|event| event.recorded_at_epoch);
    if limit > 0 && events.len() > limit {
        events.drain(..events.len() - limit);
    }
    Ok(events)
}

/// Aggregated counters for one `(component, code)` pair.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CodeSummary {
    /// Producing component.
    pub component: Component,
    /// Stable outcome code.
    pub code: String,
    /// Highest severity observed for this code.
    pub severity: Severity,
    /// Number of occurrences in the summarized window.
    pub count: u64,
    /// Unix second of the first occurrence.
    pub first_seen_epoch: i64,
    /// Unix second of the most recent occurrence.
    pub last_seen_epoch: i64,
    /// Most recent recovery action recorded for this code, when any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery_action: Option<String>,
}

/// Collapse events into stable per-code counters.
///
/// Repeated display is rate-limited by the caller; the counters and the
/// first/last-seen window are retained so a dominant failure shape stays
/// visible without reprinting every occurrence.
pub fn summarize(events: &[Event]) -> Vec<CodeSummary> {
    let mut index: BTreeMap<(Component, String), CodeSummary> = BTreeMap::new();
    for event in events {
        let key = (event.component, event.code.clone());
        match index.get_mut(&key) {
            Some(summary) => {
                summary.count = summary.count.saturating_add(1);
                summary.severity = summary.severity.max(event.severity);
                summary.first_seen_epoch = summary.first_seen_epoch.min(event.recorded_at_epoch);
                summary.last_seen_epoch = summary.last_seen_epoch.max(event.recorded_at_epoch);
                if event.recovery_action.is_some() {
                    summary.recovery_action.clone_from(&event.recovery_action);
                }
            }
            None => {
                index.insert(
                    key,
                    CodeSummary {
                        component: event.component,
                        code: event.code.clone(),
                        severity: event.severity,
                        count: 1,
                        first_seen_epoch: event.recorded_at_epoch,
                        last_seen_epoch: event.recorded_at_epoch,
                        recovery_action: event.recovery_action.clone(),
                    },
                );
            }
        }
    }
    let mut summaries: Vec<CodeSummary> = index.into_values().collect();
    summaries.sort_by(|left, right| {
        right
            .severity
            .cmp(&left.severity)
            .then_with(|| right.count.cmp(&left.count))
            .then_with(|| left.code.cmp(&right.code))
    });
    summaries
}

/// Absolute spool directory for a control-plane state root.
pub fn spool_root(state_root: &Path) -> PathBuf {
    state_root.join(SPOOL_RELATIVE)
}

struct SpoolLock(File);

impl Drop for SpoolLock {
    fn drop(&mut self) {
        // SAFETY: `flock` observes the valid descriptor owned by this guard.
        let _ = unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}

fn lock_spool(directory: &Path) -> Result<SpoolLock, SpoolError> {
    let path = directory.join(".lock");
    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&path)
        .map_err(|error| classify(&error))?;
    // SAFETY: the descriptor stays owned by `file` for the duration of the call.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(SpoolError::Unavailable);
    }
    Ok(SpoolLock(file))
}

fn ensure_spool_root(state_root: &Path) -> Result<PathBuf, SpoolError> {
    if !state_root.is_absolute() {
        return Err(SpoolError::Untrusted);
    }
    let directory = spool_root(state_root);
    for ancestor in directory
        .ancestors()
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .skip(1)
    {
        match fs::symlink_metadata(ancestor) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(SpoolError::Untrusted);
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                fs::create_dir(ancestor).map_err(|error| classify(&error))?;
                fs::set_permissions(ancestor, fs::Permissions::from_mode(0o700))
                    .map_err(|_| SpoolError::Untrusted)?;
            }
            Err(_) => return Err(SpoolError::Unavailable),
        }
    }
    let metadata = fs::symlink_metadata(&directory).map_err(|_| SpoolError::Unavailable)?;
    if !metadata.is_dir()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err(SpoolError::Untrusted);
    }
    Ok(directory)
}

fn segment_paths(directory: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut segments: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| segment_index(path).is_some())
        .collect();
    segments.sort();
    segments
}

fn segment_index(path: &Path) -> Option<u64> {
    path.file_name()?
        .to_str()?
        .strip_prefix(SEGMENT_PREFIX)?
        .strip_suffix(SEGMENT_SUFFIX)?
        .parse()
        .ok()
}

fn segment_path(directory: &Path, index: u64) -> PathBuf {
    directory.join(format!("{SEGMENT_PREFIX}{index:012}{SEGMENT_SUFFIX}"))
}

/// Select the segment that can still absorb `length` bytes, rotating when the
/// current one is full.
fn select_segment(directory: &Path, length: u64) -> PathBuf {
    let segments = segment_paths(directory);
    let Some(current) = segments.last() else {
        return segment_path(directory, 1);
    };
    let index = segment_index(current).unwrap_or(1);
    let occupied = fs::symlink_metadata(current)
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    if occupied.saturating_add(length) > MAX_SEGMENT_BYTES {
        return segment_path(directory, index.saturating_add(1));
    }
    current.clone()
}

/// Drop segments outside the count and retention budgets.
///
/// The count budget reserves one slot for the append that is about to happen, so
/// the retained set never exceeds [`MAX_SEGMENTS`] once the write lands.
fn prune(segments: &[PathBuf], now_epoch: i64) {
    let excess = segments
        .len()
        .saturating_sub(MAX_SEGMENTS.saturating_sub(1));
    for segment in segments.iter().take(excess) {
        let _ = fs::remove_file(segment);
    }
    for segment in segments.iter().skip(excess) {
        let Ok(metadata) = fs::symlink_metadata(segment) else {
            continue;
        };
        let Some(modified_epoch) = metadata
            .modified()
            .ok()
            .and_then(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|since| since.as_secs() as i64)
        else {
            continue;
        };
        if now_epoch.saturating_sub(modified_epoch) > RETENTION_SECONDS {
            let _ = fs::remove_file(segment);
        }
    }
}

fn read_private(path: &Path, max_bytes: u64) -> io::Result<Vec<u8>> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "observation segment is untrusted",
        ));
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "observation segment is oversized",
        ));
    }
    Ok(bytes)
}

fn classify(error: &io::Error) -> SpoolError {
    match error.kind() {
        io::ErrorKind::PermissionDenied => SpoolError::Untrusted,
        _ => SpoolError::Unavailable,
    }
}

fn rfc3339(epoch: i64) -> String {
    jiff::Timestamp::from_second(epoch)
        .map(|timestamp| timestamp.to_string())
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

/// Accept a lowercase kebab-case classification token.
fn token(value: &str) -> Result<String, FieldError> {
    bounded(value, MAX_TOKEN_BYTES, |ch| {
        ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-' || ch == '.'
    })
}

/// Accept a canonical provider event name, which is upper camel case.
fn alphanumeric_token(value: &str) -> Result<String, FieldError> {
    bounded(value, MAX_TOKEN_BYTES, |ch| ch.is_ascii_alphanumeric())
}

/// Accept a semantic release string.
fn version(value: &str) -> Result<String, FieldError> {
    bounded(value, 64, |ch| {
        ch.is_ascii_alphanumeric() || matches!(ch, '.' | '+' | '-' | '_' | '(' | ')' | ' ')
    })
}

/// Accept a `sha256:<64 hex>` digest and nothing else.
fn digest(value: &str) -> Result<String, FieldError> {
    let Some(hex) = value.strip_prefix("sha256:") else {
        return Err(FieldError::Unsupported);
    };
    if hex.len() != 64 || !hex.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return Err(FieldError::Unsupported);
    }
    Ok(value.to_string())
}

/// Accept a bounded operator-facing command hint.
///
/// Path separators are rejected so a recovery hint can never smuggle a
/// filesystem location into the plane.
fn recovery_action(value: &str) -> Result<String, FieldError> {
    bounded(value, MAX_ACTION_BYTES, |ch| {
        ch.is_ascii_alphanumeric() || matches!(ch, ' ' | '-' | '_' | '.' | '<' | '>' | '=')
    })
}

fn bounded(
    value: &str,
    max_bytes: usize,
    permitted: impl Fn(char) -> bool,
) -> Result<String, FieldError> {
    if value.is_empty() {
        return Err(FieldError::Empty);
    }
    if value.len() > max_bytes {
        return Err(FieldError::TooLong);
    }
    if !value.chars().all(permitted) {
        return Err(FieldError::Unsupported);
    }
    Ok(value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn root() -> (tempfile::TempDir, PathBuf) {
        let temporary = tempfile::TempDir::new().expect("temporary state");
        let root = fs::canonicalize(temporary.path()).expect("canonical state root");
        (temporary, root)
    }

    fn event(code: &str, epoch: i64) -> Event {
        Event::new(
            Component::AgentHook,
            "dispatch",
            code,
            Severity::Info,
            "1.2.3",
            epoch,
        )
        .expect("valid event")
    }

    #[test]
    fn append_creates_a_private_spool_and_read_recent_returns_events_in_order() {
        let (_temporary, root) = root();

        append(&root, &event("dispatch-completed", 200)).expect("second append");
        append(&root, &event("dispatch-blocked", 100)).expect("first append");

        let directory = spool_root(&root);
        let metadata = fs::symlink_metadata(&directory).expect("spool metadata");
        assert_eq!(metadata.permissions().mode() & 0o777, 0o700);
        let segment = segment_path(&directory, 1);
        assert_eq!(
            fs::symlink_metadata(&segment)
                .expect("segment metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );

        let events = read_recent(&root, 0).expect("read spool");
        assert_eq!(
            events
                .iter()
                .map(|event| event.code.as_str())
                .collect::<Vec<_>>(),
            vec!["dispatch-blocked", "dispatch-completed"],
            "events must be ordered oldest first"
        );
        assert_eq!(events[0].schema_version, OBSERVATION_VERSION);
        assert_eq!(events[0].recorded_at, "1970-01-01T00:01:40Z");
    }

    #[test]
    fn read_recent_honors_its_limit_and_missing_spool_is_not_an_error() {
        let (_temporary, root) = root();
        assert!(read_recent(&root, 8).expect("absent spool").is_empty());

        for epoch in 1..=5 {
            append(&root, &event("dispatch-completed", epoch)).expect("append");
        }

        let events = read_recent(&root, 2).expect("read spool");
        assert_eq!(
            events
                .iter()
                .map(|event| event.recorded_at_epoch)
                .collect::<Vec<_>>(),
            vec![4, 5]
        );
    }

    #[test]
    fn segments_rotate_and_stay_inside_the_retained_count() {
        let (_temporary, root) = root();
        let directory = ensure_spool_root(&root).expect("spool root");
        // Pre-fill more segments than the budget retains, each already full.
        let filler = vec![b'x'; MAX_SEGMENT_BYTES as usize];
        for index in 1..=(MAX_SEGMENTS as u64 + 4) {
            let path = segment_path(&directory, index);
            fs::write(&path, &filler).expect("filler segment");
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("filler mode");
        }

        append(&root, &event("dispatch-completed", 10)).expect("append after rotation");

        let segments = segment_paths(&directory);
        assert!(
            segments.len() <= MAX_SEGMENTS,
            "retained {} segments",
            segments.len()
        );
        for segment in &segments {
            let length = fs::symlink_metadata(segment)
                .expect("segment metadata")
                .len();
            assert!(
                length <= MAX_SEGMENT_BYTES,
                "segment {} exceeded its byte budget: {length}",
                segment.display()
            );
        }
        assert!(
            read_recent(&root, 0)
                .expect("read spool")
                .iter()
                .any(|event| event.code == "dispatch-completed"),
            "the newest append must survive rotation"
        );
    }

    #[test]
    fn field_validation_refuses_content_that_would_leak() {
        assert_eq!(
            Event::new(
                Component::AgentHook,
                "dispatch",
                "code with spaces",
                Severity::Info,
                "1.2.3",
                0
            )
            .expect_err("codes are bounded tokens"),
            FieldError::Unsupported
        );
        assert_eq!(
            event("dispatch-completed", 0)
                .with_correlation("claude-session-secret")
                .expect_err("correlation must be a digest"),
            FieldError::Unsupported
        );
        assert_eq!(
            event("dispatch-completed", 0)
                .with_recovery_action("run /home/user/secret/script.sh")
                .expect_err("recovery hints must not carry paths"),
            FieldError::Unsupported
        );
        assert_eq!(
            event("dispatch-completed", 0)
                .with_recovery_action("agent-session broker status --session <id>")
                .expect("bounded command hint")
                .recovery_action
                .as_deref(),
            Some("agent-session broker status --session <id>")
        );
        assert_eq!(
            event("dispatch-completed", 0)
                .with_correlation(&format!("sha256:{}", "a".repeat(64)))
                .expect("digest correlation")
                .correlation
                .as_deref(),
            Some(format!("sha256:{}", "a".repeat(64))).as_deref()
        );
    }

    #[test]
    fn summarize_ranks_by_severity_then_volume_and_keeps_the_seen_window() {
        let mut events = Vec::new();
        for epoch in [10, 20, 30] {
            events.push(event("dispatch-completed", epoch));
        }
        events.push(
            Event::new(
                Component::AgentHook,
                "coordination",
                "coordination-degraded-read-only",
                Severity::Warn,
                "1.2.3",
                25,
            )
            .expect("valid event")
            .with_recovery_action("agent-session broker status")
            .expect("bounded action"),
        );

        let summaries = summarize(&events);
        assert_eq!(summaries.len(), 2);
        assert_eq!(summaries[0].code, "coordination-degraded-read-only");
        assert_eq!(summaries[0].severity, Severity::Warn);
        assert_eq!(
            summaries[0].recovery_action.as_deref(),
            Some("agent-session broker status")
        );
        assert_eq!(summaries[1].code, "dispatch-completed");
        assert_eq!(summaries[1].count, 3);
        assert_eq!(summaries[1].first_seen_epoch, 10);
        assert_eq!(summaries[1].last_seen_epoch, 30);
    }

    #[test]
    fn a_symlinked_spool_root_is_refused_without_writing() {
        let (_temporary, root) = root();
        let elsewhere = root.join("elsewhere");
        fs::create_dir_all(&elsewhere).expect("decoy directory");
        fs::create_dir(root.join("observation")).expect("observation directory");
        std::os::unix::fs::symlink(&elsewhere, spool_root(&root)).expect("symlink spool");

        assert_eq!(
            append(&root, &event("dispatch-completed", 0)).expect_err("symlinked spool"),
            SpoolError::Untrusted
        );
        assert!(
            fs::read_dir(&elsewhere)
                .expect("decoy entries")
                .next()
                .is_none(),
            "nothing may be written through a symlinked spool root"
        );
    }

    #[test]
    fn a_relative_state_root_is_refused() {
        assert_eq!(
            append(Path::new("relative/state"), &event("dispatch-completed", 0))
                .expect_err("relative root"),
            SpoolError::Untrusted
        );
    }
}
