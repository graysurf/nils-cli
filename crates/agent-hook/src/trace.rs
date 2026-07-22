use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

use jiff::Timestamp;
use nils_common::fs::SECRET_FILE_MODE;
use serde::Serialize;
use uuid::Uuid;

use crate::error::HookError;
use crate::model::{NormalizedDecision, NormalizedRequest};

const MAX_TRACE_BYTES: usize = 256 * 1024;
const MAX_TRACE_ENTRIES: usize = 256;
const MAX_TRACE_TEMP_ATTEMPTS: usize = 10;

#[derive(Serialize)]
struct TraceEntry<'a> {
    schema_version: &'static str,
    recorded_at: String,
    product: &'static str,
    event: &'a str,
    request_id: &'a str,
    action: &'static str,
    rule_ids: Vec<&'a str>,
    shadow_rule_ids: Vec<&'a str>,
    shadow: Vec<ShadowTrace<'a>>,
    config_digest: &'a str,
    policy_digest: &'a str,
    recovery_applied: bool,
    elapsed_micros: u128,
}

#[derive(Serialize)]
struct ShadowTrace<'a> {
    rule_id: &'a str,
    action: &'static str,
    code: &'a str,
}

pub fn append(
    state_root: &Path,
    request: &NormalizedRequest,
    decision: &NormalizedDecision,
    elapsed_micros: u128,
) -> Result<(), HookError> {
    crate::paths::ensure_private_state_dir(state_root, "trace-dir")?;
    let path = state_root.join("trace.jsonl");
    let mut lines = match fs::read(&path) {
        Ok(bytes) if bytes.len() <= MAX_TRACE_BYTES => bytes
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(Vec::from)
            .collect::<Vec<_>>(),
        Ok(_) => Vec::new(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(_) => {
            return Err(HookError::runtime(
                "trace-read-failed",
                "redacted trace could not be read",
            ));
        }
    };
    let entry = TraceEntry {
        schema_version: "agent-hook.trace.v1",
        recorded_at: Timestamp::now().to_string(),
        product: request.product.as_str(),
        event: &request.event,
        request_id: &request.request_id,
        action: action_name(decision.action),
        rule_ids: decision
            .reasons
            .iter()
            .map(|reason| reason.rule_id.as_str())
            .collect(),
        shadow_rule_ids: decision
            .shadow
            .iter()
            .map(|observation| observation.rule_id.as_str())
            .collect(),
        shadow: decision
            .shadow
            .iter()
            .map(|observation| ShadowTrace {
                rule_id: &observation.rule_id,
                action: action_name(observation.action),
                code: &observation.code,
            })
            .collect(),
        config_digest: &decision.config_digest,
        policy_digest: &decision.policy_digest,
        recovery_applied: decision.recovery_applied,
        elapsed_micros,
    };
    lines.push(
        serde_json::to_vec(&entry)
            .map_err(|_| HookError::runtime("trace-render-failed", "trace render failed"))?,
    );
    while lines.len() > MAX_TRACE_ENTRIES || encoded_len(&lines) > MAX_TRACE_BYTES {
        lines.remove(0);
    }
    let mut bytes = Vec::with_capacity(encoded_len(&lines));
    for line in lines {
        bytes.extend_from_slice(&line);
        bytes.push(b'\n');
    }
    write_trace_atomic(&path, &bytes)
        .map_err(|_| HookError::runtime("trace-write-failed", "redacted trace write failed"))
}

fn write_trace_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    for _ in 0..MAX_TRACE_TEMP_ATTEMPTS {
        let temp_path = trace_temp_path(path);
        let mut file = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(SECRET_FILE_MODE)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&temp_path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        };
        if let Err(error) = file
            .set_permissions(fs::Permissions::from_mode(SECRET_FILE_MODE))
            .and_then(|()| file.write_all(bytes))
        {
            drop(file);
            let _ = fs::remove_file(&temp_path);
            return Err(error);
        }
        drop(file);
        if let Err(error) = fs::rename(&temp_path, path) {
            let _ = fs::remove_file(&temp_path);
            return Err(error);
        }
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "failed to allocate unique trace temporary file",
    ))
}

fn trace_temp_path(path: &Path) -> std::path::PathBuf {
    let mut name = OsString::from(".");
    name.push(path.file_name().unwrap_or_default());
    name.push(".tmp-");
    name.push(Uuid::new_v4().simple().to_string());
    path.with_file_name(name)
}

fn encoded_len(lines: &[Vec<u8>]) -> usize {
    lines.iter().map(|line| line.len() + 1).sum()
}

fn action_name(action: crate::model::DecisionAction) -> &'static str {
    match action {
        crate::model::DecisionAction::Allow => "allow",
        crate::model::DecisionAction::Warn => "warn",
        crate::model::DecisionAction::Context => "context",
        crate::model::DecisionAction::Transform => "transform",
        crate::model::DecisionAction::Block => "block",
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    use pretty_assertions::assert_eq;
    use tempfile::TempDir;

    use super::{SECRET_FILE_MODE, write_trace_atomic};

    #[test]
    fn write_trace_atomic_replaces_content_with_private_mode_without_temp_residue() {
        let directory = TempDir::new().expect("temporary directory");
        let path = directory.path().join("trace.jsonl");
        fs::write(&path, b"stale\n").expect("seed trace");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("seed mode");

        write_trace_atomic(&path, b"replacement\n").expect("replace trace");

        assert_eq!(fs::read(&path).expect("read trace"), b"replacement\n");
        assert_eq!(
            fs::metadata(&path)
                .expect("trace metadata")
                .permissions()
                .mode()
                & 0o777,
            SECRET_FILE_MODE
        );
        assert_eq!(directory_entries(directory.path()), ["trace.jsonl"]);
    }

    #[test]
    fn write_trace_atomic_cleans_temp_after_replace_error() {
        let directory = TempDir::new().expect("temporary directory");
        let path = directory.path().join("trace.jsonl");
        fs::create_dir(&path).expect("blocking destination directory");

        write_trace_atomic(&path, b"replacement\n").expect_err("replace must fail");

        assert!(path.is_dir());
        assert_eq!(directory_entries(directory.path()), ["trace.jsonl"]);
    }

    fn directory_entries(path: &std::path::Path) -> Vec<OsString> {
        let mut entries = fs::read_dir(path)
            .expect("read directory")
            .map(|entry| entry.expect("directory entry").file_name())
            .collect::<Vec<_>>();
        entries.sort();
        entries
    }
}
