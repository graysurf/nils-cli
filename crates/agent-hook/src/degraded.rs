//! Private redacted evidence for timeout-degraded hook decisions.

use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::error::HookError;
use crate::model::{Capability, LoadedPolicy, NormalizedRequest, OperationEffectClass, PolicyRule};

const SCHEMA_VERSION: &str = "agent-hook.degraded-incident.v1";

const MAX_INCIDENT_BYTES: u64 = 64 * 1024;
const MAX_INCIDENT_FILES: usize = 256;
const RETENTION: Duration = Duration::from_secs(30 * 24 * 60 * 60);

#[derive(Debug)]
struct SummaryLock(File);

impl Drop for SummaryLock {
    fn drop(&mut self) {
        // SAFETY: `flock` observes the valid descriptor owned by this guard.
        let _ = unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}

#[derive(Serialize)]
struct DegradedIncident<'a> {
    schema_version: &'static str,
    incident_id: &'a str,
    recorded_at: String,
    product: &'static str,
    event: &'a str,
    request_id: String,
    correlation_digest: String,
    rule_id: &'a str,
    capability: &'static str,
    handler_id: Option<&'a str>,
    error_code: &'static str,
    deadline_ms: u64,
    elapsed_ms: u64,
    effect_class: &'static str,
    disposition: &'a str,
    policy_digest: &'a str,
    config_digest: &'a str,
    completion: &'static str,
}

pub(crate) fn record_timeout(
    loaded: &LoadedPolicy,
    request: &NormalizedRequest,
    rule: &PolicyRule,
    effect: OperationEffectClass,
    disposition: &str,
    deadline_ms: u64,
    raw: &[u8],
) -> Result<String, HookError> {
    let directory = incident_directory()?;
    ensure_private_directory(&directory)?;
    let correlation_digest = correlation_digest(raw, request).unwrap_or_else(|| {
        digest(&[
            b"agent-hook.degraded.uncorrelated.v1",
            request.request_id.as_bytes(),
        ])
    });
    let nonce = uuid::Uuid::new_v4().to_string();
    let incident_digest = digest(&[
        b"agent-hook.degraded.incident.v1",
        loaded.policy_digest.as_bytes(),
        rule.id.as_bytes(),
        correlation_digest.as_bytes(),
        nonce.as_bytes(),
    ]);
    let incident_id = format!("incident:{incident_digest}");
    let path = directory.join(format!(
        "{}.json",
        incident_digest
            .strip_prefix("sha256:")
            .expect("digest prefix")
    ));
    let (capability, handler_id) = capability_identity(&rule.capability);
    let incident = DegradedIncident {
        schema_version: SCHEMA_VERSION,
        incident_id: &incident_id,
        recorded_at: jiff::Timestamp::now().to_string(),
        product: request.product.as_str(),
        event: &request.event,
        request_id: format!(
            "request:{}",
            digest(&[
                b"agent-hook.degraded.request.v1",
                request.request_id.as_bytes()
            ])
            .trim_start_matches("sha256:")
        ),
        correlation_digest,
        rule_id: &rule.id,
        capability,
        handler_id,
        error_code: "capability-timeout",
        deadline_ms,
        elapsed_ms: deadline_ms,
        effect_class: effect.as_str(),
        disposition,
        policy_digest: &loaded.policy_digest,
        config_digest: &loaded.config_digest,
        completion: "pending",
    };
    let bytes = serde_json::to_vec_pretty(&incident).map_err(|error| {
        HookError::runtime(
            "degraded-incident-serialize-failed",
            format!("failed to serialize degraded incident: {error}"),
        )
    })?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&path)
        .map_err(|error| {
            HookError::runtime(
                "degraded-incident-write-failed",
                format!("failed to create degraded incident: {error}"),
            )
        })?;
    file.write_all(&bytes)
        .and_then(|_| file.write_all(b"\n"))
        .and_then(|_| file.sync_all())
        .map_err(|error| {
            HookError::runtime(
                "degraded-incident-write-failed",
                format!("failed to persist degraded incident: {error}"),
            )
        })?;
    fs::File::open(&directory)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| {
            HookError::runtime(
                "degraded-incident-write-failed",
                format!("failed to sync degraded incident directory: {error}"),
            )
        })?;
    update_summary(
        &directory,
        loaded,
        request,
        rule,
        effect,
        &incident_id,
        "pending",
    )?;
    prune(&directory);
    Ok(incident_id)
}

pub(crate) fn complete_terminal(raw: &[u8], request: &NormalizedRequest) -> Result<(), HookError> {
    let completion = match request.event.as_str() {
        "PostToolUse" => "succeeded",
        "PostToolUseFailure" => "failed",
        _ => return Ok(()),
    };
    let Some(correlation) = correlation_digest(raw, request) else {
        return Ok(());
    };
    let directory = incident_directory()?;
    if !directory.is_dir() {
        return Ok(());
    }
    ensure_private_directory(&directory)?;
    for entry in fs::read_dir(&directory).map_err(|error| {
        HookError::runtime(
            "degraded-incident-read-failed",
            format!("failed to inspect degraded incident directory: {error}"),
        )
    })? {
        let path = entry
            .map_err(|error| {
                HookError::runtime("degraded-incident-read-failed", error.to_string())
            })?
            .path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let Some(mut value) = read_incident(&path)? else {
            continue;
        };
        if value.get("correlation_digest").and_then(Value::as_str) != Some(&correlation)
            || value.get("completion").and_then(Value::as_str) != Some("pending")
        {
            continue;
        }
        let Some(object) = value.as_object_mut() else {
            continue;
        };
        let incident_id = object
            .get("incident_id")
            .and_then(Value::as_str)
            .map(str::to_string);
        object.insert(
            "completion".to_string(),
            Value::String(completion.to_string()),
        );
        replace_incident(&directory, &path, &value)?;
        if let Some(incident_id) = incident_id {
            update_summary_completion(&directory, &incident_id, completion)?;
        }
    }
    Ok(())
}

fn update_summary(
    directory: &Path,
    loaded: &LoadedPolicy,
    request: &NormalizedRequest,
    rule: &PolicyRule,
    effect: OperationEffectClass,
    incident_id: &str,
    completion: &str,
) -> Result<(), HookError> {
    let _lock = summary_lock(directory)?;
    let platform = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);
    let fingerprint = digest(&[
        b"agent-hook.degraded.summary.v1",
        loaded.policy_digest.as_bytes(),
        rule.id.as_bytes(),
        b"capability-timeout",
        effect.as_str().as_bytes(),
        request.product.as_str().as_bytes(),
        platform.as_bytes(),
    ]);
    let path = directory.join(format!(
        "summary-{}.json",
        fingerprint.strip_prefix("sha256:").expect("digest prefix")
    ));
    let now = jiff::Timestamp::now().to_string();
    let value = if path.exists() {
        let Some(mut value) = read_incident(&path)? else {
            return Err(HookError::data(
                "degraded-summary-untrusted",
                "degraded summary is not a private regular record",
            ));
        };
        if value.get("schema_version").and_then(Value::as_str)
            != Some("agent-hook.degraded-summary.v1")
        {
            return Err(HookError::data(
                "degraded-summary-invalid",
                "degraded summary schema is invalid",
            ));
        }
        let object = value.as_object_mut().ok_or_else(|| {
            HookError::data(
                "degraded-summary-invalid",
                "degraded summary must be an object",
            )
        })?;
        let count = object
            .get("count")
            .and_then(Value::as_u64)
            .and_then(|count| count.checked_add(1))
            .ok_or_else(|| {
                HookError::data(
                    "degraded-summary-invalid",
                    "degraded summary count is invalid",
                )
            })?;
        object.insert("last_seen".to_string(), Value::String(now));
        object.insert("count".to_string(), Value::from(count));
        object.insert(
            "most_recent_incident_id".to_string(),
            Value::String(incident_id.to_string()),
        );
        object.insert(
            "latest_completion".to_string(),
            Value::String(completion.to_string()),
        );
        value
    } else {
        json!({
            "schema_version": "agent-hook.degraded-summary.v1",
            "summary_id": format!("summary:{fingerprint}"),
            "first_seen": now,
            "last_seen": now,
            "count": 1,
            "most_recent_incident_id": incident_id,
            "latest_completion": completion,
            "rule_id": rule.id,
            "error_code": "capability-timeout",
            "effect_class": effect.as_str(),
            "product": request.product.as_str(),
            "platform": platform,
            "policy_digest": loaded.policy_digest
        })
    };
    if path.exists() {
        replace_incident(directory, &path, &value)
    } else {
        create_private_record(directory, &path, &value)
    }
}

fn update_summary_completion(
    directory: &Path,
    incident_id: &str,
    completion: &str,
) -> Result<(), HookError> {
    let _lock = summary_lock(directory)?;
    for entry in fs::read_dir(directory)
        .map_err(|error| HookError::runtime("degraded-summary-read-failed", error.to_string()))?
    {
        let path = entry
            .map_err(|error| HookError::runtime("degraded-summary-read-failed", error.to_string()))?
            .path();
        if !path
            .file_name()
            .and_then(|value| value.to_str())
            .is_some_and(|name| name.starts_with("summary-") && name.ends_with(".json"))
        {
            continue;
        }
        let Some(mut value) = read_incident(&path)? else {
            continue;
        };
        if value.get("most_recent_incident_id").and_then(Value::as_str) != Some(incident_id) {
            continue;
        }
        let Some(object) = value.as_object_mut() else {
            continue;
        };
        object.insert(
            "latest_completion".to_string(),
            Value::String(completion.to_string()),
        );
        replace_incident(directory, &path, &value)?;
    }
    Ok(())
}

fn summary_lock(directory: &Path) -> Result<SummaryLock, HookError> {
    let path = directory.join(".summary.lock");
    match fs::symlink_metadata(&path) {
        Ok(metadata) => validate_summary_lock_metadata(&metadata)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(HookError::runtime(
                "degraded-summary-lock-unavailable",
                format!("failed to inspect degraded summary lock: {error}"),
            ));
        }
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&path)
        .map_err(|error| {
            HookError::runtime(
                "degraded-summary-lock-unavailable",
                format!("failed to open degraded summary lock: {error}"),
            )
        })?;
    validate_summary_lock_metadata(&file.metadata().map_err(|error| {
        HookError::runtime(
            "degraded-summary-lock-unavailable",
            format!("failed to inspect degraded summary lock descriptor: {error}"),
        )
    })?)?;
    loop {
        // SAFETY: `flock` observes the valid descriptor retained by the returned guard.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } == 0 {
            return Ok(SummaryLock(file));
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(HookError::runtime(
                "degraded-summary-lock-unavailable",
                format!("failed to acquire degraded summary lock: {error}"),
            ));
        }
    }
}

fn validate_summary_lock_metadata(metadata: &Metadata) -> Result<(), HookError> {
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err(HookError::data(
            "degraded-summary-lock-untrusted",
            "degraded summary lock is not a private owned regular file",
        ));
    }
    Ok(())
}

fn incident_directory() -> Result<PathBuf, HookError> {
    let state = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))
        .filter(|path| path.is_absolute())
        .ok_or_else(|| {
            HookError::runtime(
                "degraded-incident-state-unavailable",
                "an absolute state home is required for degraded incidents",
            )
        })?;
    Ok(state.join("agent-hook/degraded"))
}

fn ensure_private_directory(path: &PathBuf) -> Result<(), HookError> {
    fs::create_dir_all(path).map_err(|error| {
        HookError::runtime(
            "degraded-incident-state-unavailable",
            format!("failed to create degraded incident directory: {error}"),
        )
    })?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|error| {
        HookError::runtime(
            "degraded-incident-state-unavailable",
            format!("failed to protect degraded incident directory: {error}"),
        )
    })?;
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        HookError::runtime(
            "degraded-incident-state-unavailable",
            format!("failed to inspect degraded incident directory: {error}"),
        )
    })?;
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err(HookError::data(
            "degraded-incident-state-untrusted",
            "degraded incident directory is not a private owned directory",
        ));
    }
    Ok(())
}

fn correlation_digest(raw: &[u8], request: &NormalizedRequest) -> Option<String> {
    let value: Value = serde_json::from_slice(raw).ok()?;
    let object = value.as_object()?;
    let session = object
        .get("session_id")
        .or_else(|| object.get("session_key"))
        .and_then(Value::as_str)?;
    let tool = object
        .get("tool_use_id")
        .or_else(|| object.get("tool_call_id"))
        .or_else(|| object.get("call_id"))
        .and_then(Value::as_str)?;
    if session.len() > 256 || tool.len() > 256 {
        return None;
    }
    Some(digest(&[
        b"agent-hook.degraded.correlation.v1",
        request.product.as_str().as_bytes(),
        session.as_bytes(),
        tool.as_bytes(),
    ]))
}

fn read_incident(path: &Path) -> Result<Option<Value>, HookError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| HookError::runtime("degraded-incident-read-failed", error.to_string()))?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() > MAX_INCIDENT_BYTES
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Ok(None);
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|error| HookError::runtime("degraded-incident-read-failed", error.to_string()))?;
    let mut bytes = Vec::new();
    file.take(MAX_INCIDENT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| HookError::runtime("degraded-incident-read-failed", error.to_string()))?;
    if bytes.len() as u64 > MAX_INCIDENT_BYTES {
        return Ok(None);
    }
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|error| HookError::data("degraded-incident-invalid", error.to_string()))
}

fn create_private_record(directory: &Path, path: &Path, value: &Value) -> Result<(), HookError> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|error| {
        HookError::runtime("degraded-summary-serialize-failed", error.to_string())
    })?;
    if bytes.len() as u64 > MAX_INCIDENT_BYTES {
        return Err(HookError::data(
            "degraded-summary-too-large",
            "degraded summary exceeds the private record size limit",
        ));
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|error| HookError::runtime("degraded-summary-write-failed", error.to_string()))?;
    file.write_all(&bytes)
        .and_then(|_| file.write_all(b"\n"))
        .and_then(|_| file.sync_all())
        .and_then(|_| fs::File::open(directory)?.sync_all())
        .map_err(|error| HookError::runtime("degraded-summary-write-failed", error.to_string()))
}

fn replace_incident(directory: &Path, path: &Path, value: &Value) -> Result<(), HookError> {
    let temporary = directory.join(format!(".{}.tmp", uuid::Uuid::new_v4()));
    let bytes = serde_json::to_vec_pretty(value).map_err(|error| {
        HookError::runtime("degraded-incident-serialize-failed", error.to_string())
    })?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&temporary)
        .map_err(|error| HookError::runtime("degraded-incident-write-failed", error.to_string()))?;
    let result = file
        .write_all(&bytes)
        .and_then(|_| file.write_all(b"\n"))
        .and_then(|_| file.sync_all())
        .and_then(|_| fs::rename(&temporary, path));
    if let Err(error) = result {
        let _ = fs::remove_file(&temporary);
        return Err(HookError::runtime(
            "degraded-incident-write-failed",
            error.to_string(),
        ));
    }
    Ok(())
}

fn prune(directory: &Path) {
    let now = SystemTime::now();
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    let mut files = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path).ok()?;
            if metadata.file_type().is_symlink()
                || !metadata.is_file()
                || path.extension().and_then(|value| value.to_str()) != Some("json")
            {
                return None;
            }
            let modified = metadata.modified().ok()?;
            Some((path, modified))
        })
        .collect::<Vec<_>>();
    files.sort_by_key(|(_, modified)| *modified);
    let excess = files.len().saturating_sub(MAX_INCIDENT_FILES);
    for (index, (path, modified)) in files.into_iter().enumerate() {
        if index < excess
            || now
                .duration_since(modified)
                .is_ok_and(|age| age > RETENTION)
        {
            let _ = fs::remove_file(path);
        }
    }
}

fn capability_identity(capability: &Capability) -> (&'static str, Option<&str>) {
    match capability {
        Capability::RuntimeKitHandler { handler_id } => {
            ("runtime-kit.handler.v1", Some(handler_id.as_str()))
        }
        Capability::ExecutionReadOnly { .. } => ("execution.read-only.v1", None),
        Capability::SessionActivity { .. } => ("agent-session.activity.v1", None),
        Capability::SessionCoordination { .. } => ("agent-session.coordination.v1", None),
        _ => ("decision.builtin.v1", None),
    }
}

fn digest(parts: &[&[u8]]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update((part.len() as u64).to_le_bytes());
        hasher.update(part);
    }
    let bytes = hasher.finalize();
    format!(
        "sha256:{}",
        bytes
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    )
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::{Arc, Barrier};

    use super::*;
    use crate::model::{
        Config, FailurePosture, OverrideClass, PolicyBundle, PolicySelection, Product, RuleMode,
        TimeoutPosture,
    };

    #[test]
    fn concurrent_summary_updates_are_lossless() {
        const WRITERS: usize = 64;

        let temporary = tempfile::TempDir::new().expect("temporary directory");
        let directory = fs::canonicalize(temporary.path()).expect("canonical temporary directory");
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))
            .expect("private temporary directory");
        let rule = PolicyRule {
            id: "runtime.concurrent-timeout".to_string(),
            products: vec![Product::Codex],
            events: vec!["PreToolUse".to_string()],
            matcher: Some("Write".to_string()),
            priority: 10,
            mode: RuleMode::Enforce,
            failure_posture: FailurePosture::Closed,
            timeout_posture: TimeoutPosture::EffectGated,
            override_class: OverrideClass::Locked,
            capability: Capability::RuntimeKitHandler {
                handler_id: "pre-edit-intent-gate".to_string(),
            },
        };
        let loaded = LoadedPolicy {
            config: Config {
                schema_version: "agent-hook.config.v1".to_string(),
                policy: PolicySelection {
                    path: directory.join("policy.toml"),
                    digest: "sha256:config-policy".to_string(),
                },
                providers: BTreeMap::new(),
                overrides: BTreeMap::new(),
            },
            bundle: PolicyBundle {
                schema_version: "agent-hook.policy.v1".to_string(),
                bundle_id: "runtime-kit".to_string(),
                version: "2026.07.23.1".to_string(),
                rules: vec![rule.clone()],
            },
            config_digest: "sha256:config".to_string(),
            policy_digest: "sha256:policy".to_string(),
        };
        let request = NormalizedRequest {
            schema_version: "agent-hook.normalized-request.v1".to_string(),
            request_id: "request-concurrent-timeout".to_string(),
            product: Product::Codex,
            event: "PreToolUse".to_string(),
            matcher: Some("Write".to_string()),
            target_digest: "sha256:target".to_string(),
            command_digest: "sha256:command".to_string(),
            snapshot_digest: "sha256:snapshot".to_string(),
            worktree_fingerprint: None,
            semantic_conflict: None,
            stop_reentry: None,
            target_paths: Vec::new(),
            execution_path: None,
            binding_roots: Vec::new(),
        };
        let barrier = Arc::new(Barrier::new(WRITERS));
        let handles = (0..WRITERS)
            .map(|index| {
                let directory = directory.clone();
                let loaded = loaded.clone();
                let request = request.clone();
                let rule = rule.clone();
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    update_summary(
                        &directory,
                        &loaded,
                        &request,
                        &rule,
                        OperationEffectClass::LocalReversible,
                        &format!("incident:{index}"),
                        "pending",
                    )
                })
            })
            .collect::<Vec<_>>();

        for handle in handles {
            handle
                .join()
                .expect("summary writer thread")
                .expect("summary writer result");
        }

        let summary_path = fs::read_dir(&directory)
            .expect("summary directory")
            .map(|entry| entry.expect("summary entry").path())
            .find(|path| {
                path.file_name()
                    .and_then(|value| value.to_str())
                    .is_some_and(|name| name.starts_with("summary-") && name.ends_with(".json"))
            })
            .expect("summary record");
        let summary = read_incident(&summary_path)
            .expect("read summary")
            .expect("trusted summary");
        assert_eq!(summary["count"], WRITERS as u64);
    }

    #[test]
    fn summary_lock_rejects_non_private_existing_file() {
        let temporary = tempfile::TempDir::new().expect("temporary directory");
        let directory = fs::canonicalize(temporary.path()).expect("canonical temporary directory");
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))
            .expect("private temporary directory");
        let lock_path = directory.join(".summary.lock");
        fs::write(&lock_path, b"").expect("summary lock fixture");
        fs::set_permissions(&lock_path, fs::Permissions::from_mode(0o640))
            .expect("non-private summary lock mode");

        let error = summary_lock(&directory).expect_err("non-private lock must be rejected");

        assert_eq!(error.code, "degraded-summary-lock-untrusted");
    }
}
