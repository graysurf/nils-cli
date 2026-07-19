use std::fs;
use std::path::Path;

use jiff::Timestamp;
use nils_common::fs::{SECRET_FILE_MODE, write_atomic};
use serde::Serialize;

use crate::error::HookError;
use crate::model::{NormalizedDecision, NormalizedRequest};

const MAX_TRACE_BYTES: usize = 256 * 1024;
const MAX_TRACE_ENTRIES: usize = 256;

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
    config_digest: &'a str,
    policy_digest: &'a str,
    recovery_applied: bool,
    elapsed_micros: u128,
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
    write_atomic(&path, &bytes, SECRET_FILE_MODE)
        .map_err(|_| HookError::runtime("trace-write-failed", "redacted trace write failed"))
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
