//! Bounded deterministic data classification for DSH's normalized policy seam.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::error::HookError;

const REQUEST_SCHEMA: &str = "agent-hook.data-policy.evaluate.v1";
const DECISION_SCHEMA: &str = "agent-hook.data-policy.decision.v1";
const MAX_ID_BYTES: usize = 256;
const REDACTED_SENSITIVE: &str = "[redacted:sensitive]";
const REDACTED_PATH: &str = "[redacted:machine-local-path]";

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd)]
#[serde(rename_all = "kebab-case")]
enum DataClass {
    Sensitive,
    MachineLocalPath,
    ProtectedRoot,
}

impl DataClass {
    fn as_str(self) -> &'static str {
        match self {
            Self::Sensitive => "sensitive",
            Self::MachineLocalPath => "machine-local-path",
            Self::ProtectedRoot => "protected-root",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Action {
    Allow,
    Deny,
    Redact,
    Quarantine,
}

impl Action {
    fn rank(self) -> u8 {
        match self {
            Self::Allow => 0,
            Self::Redact => 1,
            Self::Quarantine => 2,
            Self::Deny => 3,
        }
    }
}

impl std::fmt::Display for Action {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Allow => "allow",
            Self::Deny => "deny",
            Self::Redact => "redact",
            Self::Quarantine => "quarantine",
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Rule {
    class_id: DataClass,
    action: Action,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Identity {
    session_id: String,
    workspace_digest: String,
    workspace_generation: String,
    call_id: String,
    root_call_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    parent_call_id: Option<String>,
    turn: u64,
    step: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum Phase {
    PreCall,
    FinalResult,
    ProtectedRoot,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    schema_version: String,
    phase: Phase,
    source_id: String,
    sink_id: String,
    identity: Identity,
    rules: Vec<Rule>,
    payload: Value,
}

#[derive(Debug, Serialize)]
struct Audit {
    action: Action,
    code: String,
    classes: Vec<&'static str>,
    source_id: String,
    sink_id: String,
    payload_digest: String,
    binding_digest: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct Decision {
    schema_version: &'static str,
    request_id: String,
    pub(crate) action: Action,
    pub(crate) code: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    replacement: Option<Value>,
    audit: Audit,
}

fn data_error(code: &str, message: &str) -> HookError {
    HookError::data(code, message)
}

fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_ID_BYTES
        && !value.bytes().any(|byte| byte.is_ascii_control())
}

fn valid_digest(value: &str) -> bool {
    value.len() == 71
        && value.starts_with("sha256:")
        && value[7..]
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn digest(domain: &[u8], bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(domain);
    hasher.update([0]);
    hasher.update(bytes);
    let encoded = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("sha256:{encoded}")
}

fn request_id(raw: &[u8]) -> String {
    let encoded = Sha256::digest(raw)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("request:{}", &encoded[..32])
}

fn sensitive_key(key: &str) -> bool {
    let normalized = key.to_ascii_lowercase().replace('-', "_");
    [
        "secret",
        "token",
        "password",
        "passwd",
        "api_key",
        "private_key",
        "credential",
    ]
    .iter()
    .any(|needle| normalized == *needle || normalized.ends_with(&format!("_{needle}")))
}

fn sensitive_value(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    if lower.contains("-----begin private key-----") {
        return true;
    }
    ["ghp_", "github_pat_", "glpat-", "sk-proj-"]
        .iter()
        .any(|prefix| {
            lower.match_indices(prefix).any(|(index, _)| {
                lower[index + prefix.len()..]
                    .chars()
                    .take_while(|character| {
                        character.is_ascii_alphanumeric() || matches!(character, '_' | '-')
                    })
                    .take(8)
                    .count()
                    == 8
            })
        })
}

fn machine_local_path(value: &str) -> bool {
    fn has_component_after(value: &str, marker: &str) -> bool {
        value.match_indices(marker).any(|(index, _)| {
            value[index + marker.len()..]
                .split(['/', '\\', ' ', '\t', '\r', '\n', '"', '\''])
                .next()
                .is_some_and(|component| !component.is_empty())
        })
    }
    let bytes = value.as_bytes();
    has_component_after(value, "/home/")
        || has_component_after(value, "/Users/")
        || has_component_after(value, "/private/var/folders/")
        || bytes.windows(3).any(|window| {
            window[0].is_ascii_alphabetic()
                && window[1] == b':'
                && matches!(window[2], b'\\' | b'/')
        })
}

fn sequence_text(value: &Value) -> Option<&str> {
    match value {
        Value::String(text) => Some(text),
        Value::Object(object) => object.get("text").and_then(Value::as_str),
        _ => None,
    }
}

fn scan(value: &Value, key_sensitive: bool, classes: &mut BTreeSet<DataClass>) {
    if key_sensitive {
        classes.insert(DataClass::Sensitive);
    }
    match value {
        Value::String(text) => {
            if sensitive_value(text) {
                classes.insert(DataClass::Sensitive);
            }
            if machine_local_path(text) {
                classes.insert(DataClass::MachineLocalPath);
            }
        }
        Value::Array(items) => {
            for item in items {
                scan(item, key_sensitive, classes);
            }
            if items.len() > 1 {
                let sequence = items.iter().map(sequence_text).collect::<Option<String>>();
                if let Some(text) = sequence {
                    if sensitive_value(&text) {
                        classes.insert(DataClass::Sensitive);
                    }
                    if machine_local_path(&text) {
                        classes.insert(DataClass::MachineLocalPath);
                    }
                }
            }
        }
        Value::Object(items) => {
            for (key, item) in items {
                scan(item, key_sensitive || sensitive_key(key), classes);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn redact(value: &Value, key_sensitive: bool, classes: &BTreeSet<DataClass>) -> Value {
    if key_sensitive && classes.contains(&DataClass::Sensitive) {
        return Value::String(REDACTED_SENSITIVE.to_string());
    }
    match value {
        Value::String(text) if classes.contains(&DataClass::Sensitive) && sensitive_value(text) => {
            Value::String(REDACTED_SENSITIVE.to_string())
        }
        Value::String(text)
            if classes.contains(&DataClass::MachineLocalPath) && machine_local_path(text) =>
        {
            Value::String(REDACTED_PATH.to_string())
        }
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| redact(item, key_sensitive, classes))
                .collect(),
        ),
        Value::Object(items) => Value::Object(
            items
                .iter()
                .map(|(key, item)| {
                    (
                        key.clone(),
                        redact(item, key_sensitive || sensitive_key(key), classes),
                    )
                })
                .collect::<Map<_, _>>(),
        ),
        _ => value.clone(),
    }
}

fn validate(request: &Request) -> Result<(), HookError> {
    if request.schema_version != REQUEST_SCHEMA {
        return Err(data_error(
            "data-policy-schema-invalid",
            "unsupported data-policy request schema",
        ));
    }
    if !valid_id(&request.source_id) || !valid_id(&request.sink_id) {
        return Err(data_error(
            "data-policy-identity-invalid",
            "source or sink identity is invalid",
        ));
    }
    let identity = &request.identity;
    if !valid_id(&identity.session_id)
        || !valid_digest(&identity.workspace_digest)
        || !valid_id(&identity.workspace_generation)
        || !valid_id(&identity.call_id)
        || !valid_id(&identity.root_call_id)
        || identity
            .parent_call_id
            .as_deref()
            .is_some_and(|value| !valid_id(value))
    {
        return Err(data_error(
            "data-policy-identity-invalid",
            "data-policy execution identity is invalid",
        ));
    }
    if request.rules.is_empty() || request.rules.len() > 16 {
        return Err(data_error(
            "data-policy-rules-invalid",
            "data-policy rules must contain 1 through 16 entries",
        ));
    }
    let mut seen = BTreeSet::new();
    for rule in &request.rules {
        if !seen.insert(rule.class_id) {
            return Err(data_error(
                "data-policy-rules-invalid",
                "data-policy class rules must be unique",
            ));
        }
    }
    Ok(())
}

pub(crate) fn evaluate(raw: &[u8]) -> Result<Decision, HookError> {
    let request: Request = serde_json::from_slice(raw).map_err(|_| {
        data_error(
            "data-policy-request-invalid",
            "data-policy input must be one strict JSON object",
        )
    })?;
    validate(&request)?;
    let payload_bytes = serde_json::to_vec(&request.payload).map_err(|_| {
        data_error(
            "data-policy-request-invalid",
            "data-policy payload is not serializable",
        )
    })?;
    let payload_digest = digest(b"agent-hook.data-policy.payload.v1", &payload_bytes);
    let identity_bytes = serde_json::to_vec(&request.identity).map_err(|_| {
        data_error(
            "data-policy-identity-invalid",
            "data-policy identity is not serializable",
        )
    })?;
    let mut binding_material = identity_bytes;
    binding_material.extend_from_slice(payload_digest.as_bytes());
    let binding_digest = digest(b"agent-hook.data-policy.binding.v1", &binding_material);

    let mut classes = BTreeSet::new();
    scan(&request.payload, false, &mut classes);
    if matches!(request.phase, Phase::ProtectedRoot) {
        classes.insert(DataClass::ProtectedRoot);
    }
    if request.source_id == "provider.opaque-reference" {
        classes.remove(&DataClass::MachineLocalPath);
    }

    let action = request
        .rules
        .iter()
        .filter(|rule| classes.contains(&rule.class_id))
        .map(|rule| rule.action)
        .max_by_key(|action| action.rank())
        .unwrap_or(Action::Allow);
    let class_names = classes
        .iter()
        .copied()
        .map(DataClass::as_str)
        .collect::<Vec<_>>();
    let code = if class_names.is_empty() {
        "data-policy-allow".to_string()
    } else {
        format!("data-policy-{action}-{}", class_names.join("+"))
    };
    let replacement = match action {
        Action::Redact => Some(redact(&request.payload, false, &classes)),
        Action::Quarantine => Some(json!({
            "quarantined": true,
            "locator": payload_digest,
        })),
        Action::Allow | Action::Deny => None,
    };
    Ok(Decision {
        schema_version: DECISION_SCHEMA,
        request_id: request_id(raw),
        action,
        code: code.clone(),
        replacement,
        audit: Audit {
            action,
            code,
            classes: class_names,
            source_id: request.source_id,
            sink_id: request.sink_id,
            payload_digest,
            binding_digest,
        },
    })
}
