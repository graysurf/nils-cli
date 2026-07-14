//! Per-session Codex account binding and host credential-broker contract.
//!
//! Durable session state contains only an allowlisted account nickname and
//! binding metadata. Access tokens are resolved on demand, kept in memory, and
//! never serialized into the session document or HTTP projection.

use std::collections::BTreeSet;
use std::env;
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    CliContext, CliError, SessionRecord, acquire_session_record_lock, load_session_record,
    write_session_record,
};

pub(crate) const BROKER_SCHEMA_VERSION: &str = "agent-session.codex-auth-broker.v1";
pub(crate) const BINDING_SCHEMA_VERSION: &str = "agent-session.codex-account-binding.v1";
pub(crate) const VIEW_SCHEMA_VERSION: &str = "agent-session.codex-account.v1";
const BINDING_KEY: &str = "codex_account_binding";
const INPUT_FENCE_KEY: &str = "codex_account_input_fence";
const BROKER_ENV: &str = "AGENT_SESSION_CODEX_ACCOUNT_BROKER";
const BROKER_TIMEOUT: Duration = Duration::from_secs(10);
// Codex 0.144.1 waits ten seconds for external-auth refresh. Leave transport
// margin so a late helper result is never persisted after Codex gives up.
const BROKER_REFRESH_TIMEOUT: Duration = Duration::from_secs(8);
const BROKER_OUTPUT_LIMIT: u64 = 1024 * 1024;
const MAX_BROKER_ARGV: usize = 16;
const MAX_BROKER_ARG_BYTES: usize = 4096;
const MAX_ACCOUNT_BYTES: usize = 64;
const MAX_ACCOUNT_ID_BYTES: usize = 512;
const MAX_PLAN_BYTES: usize = 128;

#[derive(Clone)]
pub(crate) struct CodexAccountCredentials {
    pub(crate) access_token: String,
    pub(crate) chatgpt_account_id: String,
    pub(crate) chatgpt_plan_type: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
struct DurableBinding {
    schema_version: String,
    selected_account: String,
    revision: u64,
    state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    applied_runtime_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    failure_reason: Option<String>,
    updated_at: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
struct DurableInputFence {
    schema_version: String,
    launch_id: String,
    activity_revision: u64,
}

enum DecodedBinding {
    Absent,
    Valid(DurableBinding),
    Invalid,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BindingSnapshot {
    Unbound,
    Bound { account: String, revision: u64 },
    Blocked,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct CodexAccountView {
    pub(crate) schema_version: &'static str,
    pub(crate) supported: bool,
    pub(crate) state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) selected_account: Option<String>,
    pub(crate) revision: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) applied_runtime_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) failure_reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct CodexAccountSummary {
    #[serde(alias = "nickname")]
    pub(crate) account: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) label: Option<String>,
    #[serde(
        default,
        alias = "chatgpt_plan_type",
        skip_serializing_if = "Option::is_none"
    )]
    pub(crate) plan: Option<String>,
}

#[derive(Deserialize)]
struct BrokerListResponse {
    schema_version: String,
    accounts: Vec<CodexAccountSummary>,
}

#[derive(Deserialize)]
struct BrokerResolveResponse {
    schema_version: String,
    #[serde(alias = "nickname")]
    account: String,
    access_token: String,
    chatgpt_account_id: String,
    #[serde(default, alias = "chatgpt_plan_type")]
    plan: Option<String>,
}

pub(crate) fn broker_is_configured() -> bool {
    matches!(broker_argv(), Ok(Some(_)))
}

pub(crate) fn view_for_record(record: &SessionRecord) -> CodexAccountView {
    let decoded = decode_binding(record);
    if record.agent != "codex"
        || !crate::codex_app_server::runtime_is_supported(record)
        || !broker_is_configured()
    {
        return CodexAccountView {
            schema_version: VIEW_SCHEMA_VERSION,
            supported: false,
            state: "unsupported",
            selected_account: match &decoded {
                DecodedBinding::Valid(binding) => Some(binding.selected_account.clone()),
                DecodedBinding::Absent | DecodedBinding::Invalid => None,
            },
            revision: match &decoded {
                DecodedBinding::Valid(binding) => binding.revision,
                DecodedBinding::Absent | DecodedBinding::Invalid => 0,
            },
            applied_runtime_id: None,
            failure_reason: None,
        };
    }
    let binding = match decoded {
        DecodedBinding::Absent => {
            return CodexAccountView {
                schema_version: VIEW_SCHEMA_VERSION,
                supported: true,
                state: "unbound",
                selected_account: None,
                revision: 0,
                applied_runtime_id: None,
                failure_reason: None,
            };
        }
        DecodedBinding::Invalid => {
            return CodexAccountView {
                schema_version: VIEW_SCHEMA_VERSION,
                supported: true,
                state: "failed",
                selected_account: None,
                revision: 0,
                applied_runtime_id: None,
                failure_reason: Some("binding_invalid".to_string()),
            };
        }
        DecodedBinding::Valid(binding) => binding,
    };
    let state = match binding.state.as_str() {
        "pending" => "pending",
        "bound" => "bound",
        "failed" => "failed",
        _ => "failed",
    };
    CodexAccountView {
        schema_version: VIEW_SCHEMA_VERSION,
        supported: true,
        state,
        selected_account: Some(binding.selected_account),
        revision: binding.revision,
        applied_runtime_id: binding.applied_runtime_id,
        failure_reason: binding.failure_reason,
    }
}

pub(crate) fn selected_account(record: &SessionRecord) -> Option<String> {
    match decode_binding(record) {
        DecodedBinding::Valid(binding) => Some(binding.selected_account),
        DecodedBinding::Absent | DecodedBinding::Invalid => None,
    }
}

pub(crate) fn binding_is_present(record: &SessionRecord) -> bool {
    record.extra.contains_key(BINDING_KEY)
}

pub(crate) fn binding_snapshot(record: &SessionRecord) -> BindingSnapshot {
    match decode_binding(record) {
        DecodedBinding::Absent => BindingSnapshot::Unbound,
        DecodedBinding::Valid(binding)
            if binding.state == "bound"
                && binding.applied_runtime_id.as_deref()
                    == record
                        .runtime
                        .as_ref()
                        .map(|runtime| runtime.launch_id.as_str()) =>
        {
            BindingSnapshot::Bound {
                account: binding.selected_account,
                revision: binding.revision,
            }
        }
        DecodedBinding::Valid(_) | DecodedBinding::Invalid => BindingSnapshot::Blocked,
    }
}

pub(crate) fn account_for_control_rebind(
    record: &SessionRecord,
) -> Result<Option<(String, u64)>, CliError> {
    match decode_binding(record) {
        DecodedBinding::Absent => Ok(None),
        // A malformed binding stays fenced, but the control loop must remain
        // available so an explicit account switch can repair it.
        DecodedBinding::Invalid => Ok(None),
        DecodedBinding::Valid(binding) if binding.state == "pending" => {
            Ok(Some((binding.selected_account, binding.revision)))
        }
        DecodedBinding::Valid(_) => Ok(None),
    }
}

pub(crate) fn set_initial_binding(
    record: &mut SessionRecord,
    account: Option<&str>,
) -> Result<(), CliError> {
    let Some(account) = account else {
        return Ok(());
    };
    validate_account(account)?;
    if record.agent != "codex" {
        return Err(CliError::usage(
            "codex-account-agent-conflict",
            "codex_account is supported only for Codex sessions",
            None,
        ));
    }
    if broker_argv()?.is_none() {
        return Err(CliError::data(
            "codex-account-unsupported",
            "Codex account switching is not configured for this daemon",
            None,
        ));
    }
    store_binding(
        record,
        &DurableBinding {
            schema_version: BINDING_SCHEMA_VERSION.to_string(),
            selected_account: account.to_string(),
            revision: 1,
            state: "pending".to_string(),
            applied_runtime_id: None,
            failure_reason: None,
            updated_at: jiff::Timestamp::now().to_string(),
        },
    )
}

pub(crate) fn mark_runtime_pending(record: &mut SessionRecord) -> Result<(), CliError> {
    let mut binding = match decode_binding(record) {
        DecodedBinding::Absent => return Ok(()),
        // Keep malformed state fenced across resume. A repair-capable control
        // is still launched so an explicit switch can replace it.
        DecodedBinding::Invalid => return Ok(()),
        DecodedBinding::Valid(binding) if binding.state == "failed" => return Ok(()),
        DecodedBinding::Valid(binding) => binding,
    };
    binding.state = "pending".to_string();
    binding.applied_runtime_id = None;
    binding.failure_reason = None;
    binding.updated_at = jiff::Timestamp::now().to_string();
    store_binding(record, &binding)
}

pub(crate) fn prepare_control_reconnect(
    context: &CliContext,
    id: &str,
    expected_launch_id: &str,
) -> Result<SessionRecord, CliError> {
    let _lock = acquire_session_record_lock(context, id)?;
    let mut record = load_session_record(context, id)?;
    ensure_runtime(&record, expected_launch_id)?;
    match decode_binding(&record) {
        DecodedBinding::Absent => {}
        DecodedBinding::Valid(DurableBinding { state, .. })
            if state == "pending" || state == "failed" => {}
        DecodedBinding::Valid(mut binding) => {
            binding.state = "pending".to_string();
            binding.applied_runtime_id = None;
            binding.failure_reason = None;
            binding.updated_at = jiff::Timestamp::now().to_string();
            store_binding(&mut record, &binding)?;
            record.updated_at = jiff::Timestamp::now().to_string();
            write_session_record(context, &record)?;
        }
        // Preserve malformed state. Input remains fail-closed and the explicit
        // switch path is the only operation allowed to replace it.
        DecodedBinding::Invalid => {}
    }
    Ok(record)
}

pub(crate) fn begin_binding(
    context: &CliContext,
    id: &str,
    expected_launch_id: &str,
    account: &str,
) -> Result<u64, CliError> {
    validate_account(account)?;
    let _lock = acquire_session_record_lock(context, id)?;
    let mut record = load_session_record(context, id)?;
    ensure_runtime(&record, expected_launch_id)?;
    if !broker_is_configured() {
        return Err(CliError::data(
            "codex-account-unsupported",
            "Codex account switching is not configured for this daemon",
            Some(json!({ "id": id })),
        ));
    }
    let prior = match decode_binding(&record) {
        DecodedBinding::Valid(binding)
            if binding.state == "bound"
                && binding.selected_account == account
                && binding.applied_runtime_id.as_deref() == Some(expected_launch_id) =>
        {
            binding
        }
        DecodedBinding::Valid(_) | DecodedBinding::Absent | DecodedBinding::Invalid => {
            return Err(CliError::runtime(
                "codex-account-refresh-superseded",
                "Codex account binding changed before its credential refresh",
                Some(json!({ "id": id })),
            ));
        }
    };
    let revision = prior.revision.saturating_add(1).max(1);
    store_binding(
        &mut record,
        &DurableBinding {
            schema_version: BINDING_SCHEMA_VERSION.to_string(),
            selected_account: account.to_string(),
            revision,
            state: "pending".to_string(),
            applied_runtime_id: None,
            failure_reason: None,
            updated_at: jiff::Timestamp::now().to_string(),
        },
    )?;
    record.updated_at = jiff::Timestamp::now().to_string();
    write_session_record(context, &record)?;
    Ok(revision)
}

pub(crate) fn finish_binding(
    context: &CliContext,
    id: &str,
    expected_launch_id: &str,
    account: &str,
    revision: u64,
    result: Result<(), &'static str>,
) -> Result<CodexAccountView, CliError> {
    let _lock = acquire_session_record_lock(context, id)?;
    let mut record = load_session_record(context, id)?;
    ensure_runtime(&record, expected_launch_id)?;
    let current = match decode_binding(&record) {
        DecodedBinding::Valid(binding) => binding,
        DecodedBinding::Absent => {
            return Err(CliError::data(
                "codex-account-binding-missing",
                "Codex account binding state is missing",
                Some(json!({ "id": id })),
            ));
        }
        DecodedBinding::Invalid => return Err(invalid_binding_error(&record)),
    };
    if current.selected_account != account || current.revision != revision {
        return Err(CliError::runtime(
            "codex-account-binding-superseded",
            "Codex account binding changed while it was being applied",
            Some(json!({ "id": id })),
        ));
    }
    let (state, applied_runtime_id, failure_reason) = match result {
        Ok(()) => ("bound", Some(expected_launch_id.to_string()), None),
        Err(reason) => ("failed", None, Some(reason.to_string())),
    };
    store_binding(
        &mut record,
        &DurableBinding {
            schema_version: BINDING_SCHEMA_VERSION.to_string(),
            selected_account: account.to_string(),
            revision,
            state: state.to_string(),
            applied_runtime_id,
            failure_reason,
            updated_at: jiff::Timestamp::now().to_string(),
        },
    )?;
    record.updated_at = jiff::Timestamp::now().to_string();
    write_session_record(context, &record)?;
    Ok(view_for_record(&record))
}

pub(crate) fn ensure_input_allowed(record: &SessionRecord) -> Result<(), CliError> {
    let binding = match decode_binding(record) {
        DecodedBinding::Absent => return Ok(()),
        DecodedBinding::Invalid => return Err(not_bound_error(record, None)),
        DecodedBinding::Valid(binding) => binding,
    };
    let launch_id = record
        .runtime
        .as_ref()
        .map(|runtime| runtime.launch_id.as_str())
        .unwrap_or_default();
    if binding.state == "bound"
        && binding.applied_runtime_id.as_deref() == Some(launch_id)
        && broker_is_configured()
        && crate::codex_app_server::runtime_is_supported(record)
    {
        return Ok(());
    }
    Err(not_bound_error(record, Some(&binding)))
}

/// Record provider input authorization while the caller holds the session
/// record lock. The fence closes the small interval before activity advances.
pub(crate) fn authorize_input_locked(
    context: &CliContext,
    record: &mut SessionRecord,
) -> Result<(), CliError> {
    if record.agent != "codex" {
        return Ok(());
    }
    if binding_is_present(record) {
        ensure_input_allowed(record)?;
    }
    if !crate::codex_app_server::runtime_is_supported(record) || !broker_is_configured() {
        return Ok(());
    }
    ensure_input_allowed(record)?;
    let launch_id = record
        .runtime
        .as_ref()
        .map(|runtime| runtime.launch_id.clone())
        .ok_or_else(|| invalid_binding_error(record))?;
    let activity_revision =
        crate::activity::state_for_view(context, record).map_or(0, |state| state.revision);
    record.extra.insert(
        INPUT_FENCE_KEY.to_string(),
        serde_json::to_value(DurableInputFence {
            schema_version: BINDING_SCHEMA_VERSION.to_string(),
            launch_id,
            activity_revision,
        })
        .map_err(|_| invalid_binding_error(record))?,
    );
    record.updated_at = jiff::Timestamp::now().to_string();
    write_session_record(context, record)
}

/// Atomically revalidate incarnation and idleness and publish `pending`.
/// Input authorization uses the same record lock, so only one transition wins.
pub(crate) fn begin_switch_binding(
    context: &CliContext,
    id: &str,
    expected_launch_id: &str,
    account: &str,
) -> Result<u64, CliError> {
    validate_account(account)?;
    let _lock = acquire_session_record_lock(context, id)?;
    let mut record = load_session_record(context, id)?;
    ensure_runtime(&record, expected_launch_id).map_err(|_| {
        CliError::data(
            "codex-account-session-incarnation-conflict",
            "session was replaced before its Codex account switch was applied",
            Some(json!({ "id": id, "expected_session_incarnation": expected_launch_id })),
        )
    })?;
    if !broker_is_configured() {
        return Err(CliError::data(
            "codex-account-unsupported",
            "Codex account switching is not configured for this daemon",
            Some(json!({ "id": id })),
        ));
    }
    let activity = crate::activity::state_for_view(context, &record);
    let Some(activity) =
        activity.filter(|activity| activity.phase == crate::activity::TurnPhase::Waiting)
    else {
        return Err(session_busy_error(&record));
    };
    if input_fence(&record)?.is_some_and(|fence| {
        fence.launch_id == expected_launch_id && activity.revision <= fence.activity_revision
    }) {
        return Err(session_busy_error(&record));
    }
    crate::auto_resume::cancel_for_account_switch_locked(
        context,
        &record.id,
        &jiff::Timestamp::now().to_string(),
    )?;
    let prior = match decode_binding(&record) {
        DecodedBinding::Valid(binding) => Some(binding),
        DecodedBinding::Absent | DecodedBinding::Invalid => None,
    };
    let revision = match prior.as_ref() {
        Some(prior) => prior.revision.saturating_add(1).max(1),
        None => 1,
    };
    store_binding(
        &mut record,
        &DurableBinding {
            schema_version: BINDING_SCHEMA_VERSION.to_string(),
            selected_account: account.to_string(),
            revision,
            state: "pending".to_string(),
            applied_runtime_id: None,
            failure_reason: None,
            updated_at: jiff::Timestamp::now().to_string(),
        },
    )?;
    record.updated_at = jiff::Timestamp::now().to_string();
    write_session_record(context, &record)?;
    Ok(revision)
}

fn input_fence(record: &SessionRecord) -> Result<Option<DurableInputFence>, CliError> {
    let Some(value) = record.extra.get(INPUT_FENCE_KEY).cloned() else {
        return Ok(None);
    };
    let fence: DurableInputFence =
        serde_json::from_value(value).map_err(|_| invalid_binding_error(record))?;
    if fence.schema_version != BINDING_SCHEMA_VERSION || fence.launch_id.is_empty() {
        return Err(invalid_binding_error(record));
    }
    Ok(Some(fence))
}

fn session_busy_error(record: &SessionRecord) -> CliError {
    CliError::data(
        "codex-account-session-busy",
        "wait for the current Codex turn to finish before switching accounts",
        Some(json!({ "id": record.id })),
    )
}

fn not_bound_error(record: &SessionRecord, binding: Option<&DurableBinding>) -> CliError {
    CliError::runtime(
        "codex-account-not-bound",
        "the selected Codex account is not ready; retry the account switch before submitting input",
        Some(json!({
            "id": record.id,
            "account": binding.map(|binding| binding.selected_account.as_str()),
            "revision": binding.map(|binding| binding.revision).unwrap_or(0),
            "state": binding.map(|binding| binding.state.as_str()).unwrap_or("invalid")
        })),
    )
}

pub(crate) fn list_accounts() -> Result<Vec<CodexAccountSummary>, CliError> {
    let value = run_broker(&["list", "--format", "json"], BROKER_TIMEOUT)?;
    let response: BrokerListResponse = serde_json::from_value(value).map_err(|_| {
        broker_error(
            "codex-account-broker-invalid-response",
            "Codex account broker returned an invalid account list",
        )
    })?;
    ensure_schema(&response.schema_version)?;
    let mut seen = BTreeSet::new();
    let mut accounts = Vec::with_capacity(response.accounts.len());
    for mut account in response.accounts {
        validate_account(&account.account)?;
        validate_optional_public_string(&account.label, MAX_ACCOUNT_BYTES)?;
        validate_optional_public_string(&account.plan, MAX_PLAN_BYTES)?;
        if !seen.insert(account.account.clone()) {
            return Err(broker_error(
                "codex-account-broker-invalid-response",
                "Codex account broker returned duplicate account nicknames",
            ));
        }
        account.label = account.label.filter(|value| !value.trim().is_empty());
        account.plan = account.plan.filter(|value| !value.trim().is_empty());
        accounts.push(account);
    }
    Ok(accounts)
}

pub(crate) fn resolve_account(
    account: &str,
    force_refresh: bool,
) -> Result<CodexAccountCredentials, CliError> {
    validate_account(account)?;
    let mut args = vec!["resolve", "--account", account];
    if force_refresh {
        args.push("--force-refresh");
    }
    args.extend(["--format", "json"]);
    let timeout = if force_refresh {
        BROKER_REFRESH_TIMEOUT
    } else {
        BROKER_TIMEOUT
    };
    let value = run_broker(&args, timeout)?;
    let response: BrokerResolveResponse = serde_json::from_value(value).map_err(|_| {
        broker_error(
            "codex-account-broker-invalid-response",
            "Codex account broker returned invalid credentials",
        )
    })?;
    ensure_schema(&response.schema_version)?;
    validate_account(&response.account)?;
    if response.account != account
        || response.access_token.trim().is_empty()
        || response.access_token.len() as u64 > BROKER_OUTPUT_LIMIT
        || response.chatgpt_account_id.trim().is_empty()
        || response.chatgpt_account_id.len() > MAX_ACCOUNT_ID_BYTES
    {
        return Err(broker_error(
            "codex-account-broker-invalid-response",
            "Codex account broker returned mismatched or invalid credentials",
        ));
    }
    validate_optional_public_string(&response.plan, MAX_PLAN_BYTES)?;
    Ok(CodexAccountCredentials {
        access_token: response.access_token,
        chatgpt_account_id: response.chatgpt_account_id,
        chatgpt_plan_type: response.plan.filter(|value| !value.trim().is_empty()),
    })
}

fn decode_binding(record: &SessionRecord) -> DecodedBinding {
    let Some(value) = record.extra.get(BINDING_KEY).cloned() else {
        return DecodedBinding::Absent;
    };
    let decoded: Result<DurableBinding, _> = serde_json::from_value(value);
    match decoded {
        Ok(binding)
            if binding.schema_version == BINDING_SCHEMA_VERSION
                && validate_account(&binding.selected_account).is_ok()
                && binding.revision > 0
                && matches!(binding.state.as_str(), "pending" | "bound" | "failed") =>
        {
            DecodedBinding::Valid(binding)
        }
        Ok(_) | Err(_) => DecodedBinding::Invalid,
    }
}

fn invalid_binding_error(record: &SessionRecord) -> CliError {
    CliError::data(
        "codex-account-binding-invalid",
        "Codex account binding state is invalid and must be repaired explicitly",
        Some(json!({ "id": record.id })),
    )
}

fn store_binding(record: &mut SessionRecord, binding: &DurableBinding) -> Result<(), CliError> {
    let value = serde_json::to_value(binding).map_err(|_| {
        CliError::runtime(
            "codex-account-binding-encode-failed",
            "failed to encode Codex account binding state",
            Some(json!({ "id": record.id })),
        )
    })?;
    record.extra.insert(BINDING_KEY.to_string(), value);
    Ok(())
}

fn ensure_runtime(record: &SessionRecord, expected_launch_id: &str) -> Result<(), CliError> {
    if record.agent == "codex"
        && crate::codex_app_server::runtime_is_supported(record)
        && record
            .runtime
            .as_ref()
            .is_some_and(|runtime| runtime.launch_id == expected_launch_id)
    {
        return Ok(());
    }
    Err(CliError::runtime(
        "codex-account-runtime-changed",
        "Codex session runtime changed while applying its account binding",
        Some(json!({ "id": record.id })),
    ))
}

fn validate_account(account: &str) -> Result<(), CliError> {
    if account.is_empty()
        || account.len() > MAX_ACCOUNT_BYTES
        || !account
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(CliError::usage(
            "invalid-codex-account",
            "Codex account must be a short configured nickname",
            None,
        ));
    }
    Ok(())
}

fn validate_optional_public_string(value: &Option<String>, max: usize) -> Result<(), CliError> {
    if value
        .as_ref()
        .is_some_and(|value| value.len() > max || value.contains(['\n', '\r', '\0']))
    {
        return Err(broker_error(
            "codex-account-broker-invalid-response",
            "Codex account broker returned invalid public metadata",
        ));
    }
    Ok(())
}

fn ensure_schema(schema: &str) -> Result<(), CliError> {
    if schema == BROKER_SCHEMA_VERSION {
        Ok(())
    } else {
        Err(broker_error(
            "codex-account-broker-invalid-response",
            "Codex account broker returned an unsupported schema",
        ))
    }
}

fn broker_argv() -> Result<Option<Vec<String>>, CliError> {
    let Some(raw) = env::var(BROKER_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty())
    else {
        return Ok(None);
    };
    let argv: Vec<String> = serde_json::from_str(&raw).map_err(|_| {
        broker_error(
            "codex-account-broker-invalid-config",
            "Codex account broker configuration must be a JSON argv array",
        )
    })?;
    if argv.is_empty()
        || argv.len() > MAX_BROKER_ARGV
        || argv
            .iter()
            .any(|arg| arg.is_empty() || arg.len() > MAX_BROKER_ARG_BYTES || arg.contains('\0'))
    {
        return Err(broker_error(
            "codex-account-broker-invalid-config",
            "Codex account broker configuration is invalid",
        ));
    }
    Ok(Some(argv))
}

fn run_broker(args: &[&str], timeout: Duration) -> Result<Value, CliError> {
    let argv = broker_argv()?.ok_or_else(|| {
        broker_error(
            "codex-account-unsupported",
            "Codex account switching is not configured for this daemon",
        )
    })?;
    let mut child = Command::new(&argv[0])
        .args(&argv[1..])
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(|_| {
            broker_error(
                "codex-account-broker-unavailable",
                "Codex account broker could not be started",
            )
        })?;
    let mut stdout_pipe = child.stdout.take().ok_or_else(|| {
        broker_error(
            "codex-account-broker-unavailable",
            "Codex account broker output was unavailable",
        )
    })?;
    let mut stderr_pipe = child.stderr.take().ok_or_else(|| {
        broker_error(
            "codex-account-broker-unavailable",
            "Codex account broker error output was unavailable",
        )
    })?;
    let (output_tx, output_rx) = std::sync::mpsc::channel();
    let stdout_tx = output_tx.clone();
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stdout_pipe
            .by_ref()
            .take(BROKER_OUTPUT_LIMIT + 1)
            .read_to_end(&mut bytes);
        let _ = stdout_tx.send((true, bytes));
    });
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stderr_pipe
            .by_ref()
            .take(BROKER_OUTPUT_LIMIT + 1)
            .read_to_end(&mut bytes);
        let _ = output_tx.send((false, bytes));
    });

    let deadline = Instant::now() + timeout;
    let mut status = None;
    let mut stdout = None;
    let mut stderr_drained = false;
    loop {
        if status.is_none() {
            match child.try_wait() {
                Ok(Some(exit)) => status = Some(exit),
                Ok(None) => {}
                Err(_) => {
                    terminate_broker(&mut child);
                    return Err(broker_error(
                        "codex-account-broker-failed",
                        "Codex account broker failed",
                    ));
                }
            }
        }
        while let Ok((is_stdout, bytes)) = output_rx.try_recv() {
            if is_stdout {
                stdout = Some(bytes);
            } else {
                stderr_drained = true;
            }
        }
        if let (Some(status), Some(stdout)) = (status.as_ref(), stdout.as_ref())
            && stderr_drained
        {
            if !status.success() {
                terminate_broker(&mut child);
                return Err(broker_error(
                    "codex-account-broker-rejected",
                    "Codex account broker rejected the request",
                ));
            }
            if stdout.len() as u64 > BROKER_OUTPUT_LIMIT {
                return Err(broker_error(
                    "codex-account-broker-invalid-response",
                    "Codex account broker output exceeded the size limit",
                ));
            }
            let decoded = serde_json::from_slice(stdout).map_err(|_| {
                broker_error(
                    "codex-account-broker-invalid-response",
                    "Codex account broker returned malformed JSON",
                )
            });
            terminate_broker(&mut child);
            return decoded;
        }
        if Instant::now() >= deadline {
            terminate_broker(&mut child);
            return Err(broker_error(
                "codex-account-broker-timeout",
                "Codex account broker timed out",
            ));
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn terminate_broker(child: &mut std::process::Child) {
    let pid = child.id();
    // SAFETY: the broker is launched as the leader of a fresh process group.
    unsafe {
        libc::kill(-(pid as i32), libc::SIGKILL);
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn broker_error(code: &'static str, message: &'static str) -> CliError {
    CliError::runtime(code, message, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nils_test_support::{EnvGuard, GlobalStateLock};
    use std::collections::BTreeMap;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    fn record_with_binding_value(value: Value) -> SessionRecord {
        SessionRecord {
            schema_version: crate::SESSION_DOCUMENT_VERSION.to_string(),
            id: "binding-fixture".to_string(),
            agent: "codex".to_string(),
            mode: "interactive".to_string(),
            title: None,
            title_state: None,
            title_revision: 0,
            cwd: "/repo".to_string(),
            tmux_session: "hs-binding-fixture".to_string(),
            prompt_file: None,
            log_file: None,
            created_at: "2030-01-01T00:00:00Z".to_string(),
            updated_at: "2030-01-01T00:00:00Z".to_string(),
            provider_resume: None,
            runtime: Some(crate::RuntimeInfo {
                kind: crate::codex_app_server::RUNTIME_KIND.to_string(),
                tmux_session: "hs-binding-fixture".to_string(),
                generation: 1,
                started_at: "2030-01-01T00:00:00Z".to_string(),
                launch_id: "runtime-binding-fixture".to_string(),
                extra: BTreeMap::from([
                    (
                        crate::codex_app_server::PROTOCOL_KEY.to_string(),
                        json!(crate::codex_app_server::PROTOCOL_VERSION),
                    ),
                    (
                        crate::codex_app_server::SOCKET_KEY.to_string(),
                        json!("/run/codex.sock"),
                    ),
                    (
                        crate::codex_app_server::PROXY_KEY.to_string(),
                        json!("/run/codex.proxy"),
                    ),
                    (
                        crate::codex_app_server::THREAD_HANDOFF_KEY.to_string(),
                        json!("/run/codex.thread"),
                    ),
                    (
                        crate::codex_app_server::THREAD_ATTACHED_KEY.to_string(),
                        json!("/run/codex.attached"),
                    ),
                ]),
            }),
            agent_args: Vec::new(),
            agent_bin: None,
            extra: BTreeMap::from([(BINDING_KEY.to_string(), value)]),
            resume_sidecar_extra: BTreeMap::new(),
        }
    }

    #[test]
    fn nickname_validation_rejects_paths_and_identity_values() {
        assert!(validate_account("gamania").is_ok());
        assert!(validate_account("team-1").is_ok());
        assert!(validate_account("../auth.json").is_err());
        assert!(validate_account("person@example.com").is_err());
        assert!(validate_account("").is_err());
    }

    #[test]
    fn malformed_broker_configuration_fails_closed() {
        let lock = GlobalStateLock::new();
        let _broker = EnvGuard::set(&lock, BROKER_ENV, "not-json");
        let error = list_accounts().unwrap_err();
        assert_eq!(error.code(), "codex-account-broker-invalid-config");
    }

    #[test]
    fn malformed_or_future_durable_bindings_fail_closed() {
        let lock = GlobalStateLock::new();
        let _broker = EnvGuard::set(&lock, BROKER_ENV, r#"["/configured/broker"]"#);
        for value in [
            json!({
                "schema_version": "agent-session.codex-account-binding.v2",
                "selected_account": "gamania",
                "revision": 1,
                "state": "bound",
                "applied_runtime_id": "runtime-binding-fixture",
                "updated_at": "2030-01-01T00:00:00Z"
            }),
            json!({ "schema_version": BINDING_SCHEMA_VERSION }),
            json!({
                "schema_version": BINDING_SCHEMA_VERSION,
                "selected_account": "../auth.json",
                "revision": 1,
                "state": "bound",
                "applied_runtime_id": "runtime-binding-fixture",
                "updated_at": "2030-01-01T00:00:00Z"
            }),
            json!({
                "schema_version": BINDING_SCHEMA_VERSION,
                "selected_account": "gamania",
                "revision": 0,
                "state": "bound",
                "applied_runtime_id": "runtime-binding-fixture",
                "updated_at": "2030-01-01T00:00:00Z"
            }),
        ] {
            let record = record_with_binding_value(value);
            assert_eq!(view_for_record(&record).state, "failed");
            assert_eq!(
                ensure_input_allowed(&record).unwrap_err().code(),
                "codex-account-not-bound"
            );
        }
    }

    #[test]
    fn bound_input_never_falls_back_when_the_broker_disappears() {
        let lock = GlobalStateLock::new();
        let _broker = EnvGuard::set(&lock, BROKER_ENV, "");
        let tmp = tempfile::TempDir::new().unwrap();
        let (context, mut record) = persist_record(&tmp, valid_binding("bound"));

        assert_eq!(
            authorize_input_locked(&context, &mut record)
                .unwrap_err()
                .code(),
            "codex-account-not-bound"
        );
    }

    #[test]
    fn bound_input_never_falls_back_when_runtime_capability_is_lost() {
        let lock = GlobalStateLock::new();
        let _broker = EnvGuard::set(&lock, BROKER_ENV, r#"["/configured/broker"]"#);
        let tmp = tempfile::TempDir::new().unwrap();
        let mut record = record_with_binding_value(valid_binding("bound"));
        record
            .runtime
            .as_mut()
            .unwrap()
            .extra
            .remove(crate::codex_app_server::PROTOCOL_KEY);
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        fs::create_dir_all(crate::session_dir(&context, &record.id)).unwrap();
        write_session_record(&context, &record).unwrap();

        assert_eq!(
            authorize_input_locked(&context, &mut record)
                .unwrap_err()
                .code(),
            "codex-account-not-bound"
        );
    }

    fn valid_binding(state: &str) -> Value {
        json!({
            "schema_version": BINDING_SCHEMA_VERSION,
            "selected_account": "gamania",
            "revision": 7,
            "state": state,
            "applied_runtime_id": if state == "bound" { Value::String("runtime-binding-fixture".into()) } else { Value::Null },
            "failure_reason": if state == "failed" { Value::String("refresh_failed".into()) } else { Value::Null },
            "updated_at": "2030-01-01T00:00:00Z"
        })
    }

    fn persist_record(tmp: &tempfile::TempDir, value: Value) -> (CliContext, SessionRecord) {
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let record = record_with_binding_value(value);
        fs::create_dir_all(crate::session_dir(&context, &record.id)).unwrap();
        write_session_record(&context, &record).unwrap();
        crate::activity::activate_runtime(&context, &record).unwrap();
        (context, record)
    }

    #[test]
    fn input_authorization_and_account_switch_are_one_winner_transitions() {
        let lock = GlobalStateLock::new();
        let _broker = EnvGuard::set(&lock, BROKER_ENV, r#"["/configured/broker"]"#);
        let tmp = tempfile::TempDir::new().unwrap();
        let (context, record) = persist_record(&tmp, valid_binding("bound"));
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));

        let input_context = context.clone();
        let input_id = record.id.clone();
        let input_barrier = barrier.clone();
        let input = std::thread::spawn(move || {
            input_barrier.wait();
            let _guard = acquire_session_record_lock(&input_context, &input_id).unwrap();
            let mut current = load_session_record(&input_context, &input_id).unwrap();
            authorize_input_locked(&input_context, &mut current).map(|_| "input")
        });

        let switch_context = context.clone();
        let switch_id = record.id.clone();
        let switch_barrier = barrier.clone();
        let switcher = std::thread::spawn(move || {
            switch_barrier.wait();
            begin_switch_binding(
                &switch_context,
                &switch_id,
                "runtime-binding-fixture",
                "poies",
            )
            .map(|_| "switch")
        });
        barrier.wait();
        let input = input.join().unwrap();
        let switcher = switcher.join().unwrap();
        assert_ne!(input.is_ok(), switcher.is_ok());
        let loser = input.err().or_else(|| switcher.err()).unwrap();
        assert!(matches!(
            loser.code(),
            "codex-account-not-bound" | "codex-account-session-busy"
        ));
    }

    #[test]
    fn reconnect_fences_bound_but_preserves_failed_until_explicit_retry() {
        let lock = GlobalStateLock::new();
        let _broker = EnvGuard::set(&lock, BROKER_ENV, r#"["/configured/broker"]"#);
        for (state, expected_rebind) in [("bound", true), ("failed", false)] {
            let tmp = tempfile::TempDir::new().unwrap();
            let (context, record) = persist_record(&tmp, valid_binding(state));
            let prepared =
                prepare_control_reconnect(&context, &record.id, "runtime-binding-fixture").unwrap();
            assert_eq!(
                account_for_control_rebind(&prepared).unwrap().is_some(),
                expected_rebind
            );
            assert_eq!(
                view_for_record(&prepared).state,
                if expected_rebind { "pending" } else { "failed" }
            );
        }
    }

    #[test]
    fn resumed_runtime_preserves_failed_binding_until_explicit_retry() {
        let lock = GlobalStateLock::new();
        let _broker = EnvGuard::set(&lock, BROKER_ENV, r#"["/configured/broker"]"#);
        let mut record = record_with_binding_value(valid_binding("failed"));

        mark_runtime_pending(&mut record).unwrap();

        assert_eq!(view_for_record(&record).state, "failed");
        assert!(account_for_control_rebind(&record).unwrap().is_none());
    }

    #[test]
    fn stale_session_incarnation_cannot_mutate_replacement_binding() {
        let lock = GlobalStateLock::new();
        let _broker = EnvGuard::set(&lock, BROKER_ENV, r#"["/configured/broker"]"#);
        let tmp = tempfile::TempDir::new().unwrap();
        let (context, record) = persist_record(&tmp, valid_binding("bound"));
        let error =
            begin_switch_binding(&context, &record.id, "stale-launch", "poies").unwrap_err();
        assert_eq!(error.code(), "codex-account-session-incarnation-conflict");
        assert_eq!(
            selected_account(&load_session_record(&context, &record.id).unwrap()).as_deref(),
            Some("gamania")
        );
    }

    #[test]
    fn broker_contract_lists_and_resolves_only_allowlisted_nicknames() {
        let lock = GlobalStateLock::new();
        let tmp = tempfile::TempDir::new().unwrap();
        let script = tmp.path().join("broker");
        let calls = tmp.path().join("calls");
        fs::write(
            &script,
            r#"#!/bin/sh
calls=$1
shift
printf '%s\n' "$*" >> "$calls"
case "$1" in
  list)
    printf '%s\n' '{"schema_version":"agent-session.codex-auth-broker.v1","accounts":[{"account":"gamania","label":"Gamania","plan":"team"}]}'
    ;;
  resolve)
    printf '%s\n' '{"schema_version":"agent-session.codex-auth-broker.v1","account":"gamania","access_token":"fixture-token","chatgpt_account_id":"workspace-fixture","plan":"team"}'
    ;;
  *) exit 2 ;;
esac
"#,
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
        let argv = serde_json::to_string(&vec![
            script.to_string_lossy().into_owned(),
            calls.to_string_lossy().into_owned(),
        ])
        .unwrap();
        let _broker = EnvGuard::set(&lock, BROKER_ENV, &argv);

        assert_eq!(
            list_accounts().unwrap(),
            vec![CodexAccountSummary {
                account: "gamania".to_string(),
                label: Some("Gamania".to_string()),
                plan: Some("team".to_string()),
            }]
        );
        let credentials = resolve_account("gamania", false).unwrap();
        assert_eq!(credentials.access_token, "fixture-token");
        assert_eq!(credentials.chatgpt_account_id, "workspace-fixture");
        assert_eq!(credentials.chatgpt_plan_type.as_deref(), Some("team"));
        let refreshed = resolve_account("gamania", true).unwrap();
        assert_eq!(refreshed.chatgpt_account_id, "workspace-fixture");

        assert_eq!(
            fs::read_to_string(calls)
                .unwrap()
                .lines()
                .collect::<Vec<_>>(),
            vec![
                "list --format json",
                "resolve --account gamania --format json",
                "resolve --account gamania --force-refresh --format json",
            ]
        );
    }

    #[test]
    fn broker_failure_contracts_are_bounded_and_fail_closed() {
        let lock = GlobalStateLock::new();
        let tmp = tempfile::TempDir::new().unwrap();
        let script = tmp.path().join("failing-broker");
        fs::write(
            &script,
            r#"#!/bin/sh
mode=$1
shift
case "$mode" in
  malformed)
    printf '{'
    ;;
  future-list)
    printf '%s\n' '{"schema_version":"agent-session.codex-auth-broker.v2","accounts":[]}'
    ;;
  mismatch-resolve)
    printf '%s\n' '{"schema_version":"agent-session.codex-auth-broker.v1","account":"poies","access_token":"fixture-token","chatgpt_account_id":"workspace-fixture"}'
    ;;
  oversized)
    dd if=/dev/zero bs=1048577 count=1 2>/dev/null | tr '\000' x
    ;;
  rejected)
    printf '%s\n' 'private broker failure' >&2
    exit 7
    ;;
  *) exit 2 ;;
esac
"#,
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();

        let cases = [
            ("malformed", "codex-account-broker-invalid-response"),
            ("oversized", "codex-account-broker-invalid-response"),
            ("rejected", "codex-account-broker-rejected"),
        ];
        for (mode, expected_code) in cases {
            let argv = serde_json::to_string(&vec![
                script.to_string_lossy().into_owned(),
                mode.to_string(),
            ])
            .unwrap();
            let broker = EnvGuard::set(&lock, BROKER_ENV, &argv);
            let error = run_broker(&["list"], Duration::from_secs(2)).unwrap_err();
            assert_eq!(error.code(), expected_code, "mode={mode}");
            drop(broker);
        }

        let future_argv = serde_json::to_string(&vec![
            script.to_string_lossy().into_owned(),
            "future-list".to_string(),
        ])
        .unwrap();
        let broker = EnvGuard::set(&lock, BROKER_ENV, &future_argv);
        assert_eq!(
            list_accounts().unwrap_err().code(),
            "codex-account-broker-invalid-response"
        );
        drop(broker);

        let mismatch_argv = serde_json::to_string(&vec![
            script.to_string_lossy().into_owned(),
            "mismatch-resolve".to_string(),
        ])
        .unwrap();
        let _broker = EnvGuard::set(&lock, BROKER_ENV, &mismatch_argv);
        let error = match resolve_account("gamania", false) {
            Ok(_) => panic!("mismatched broker account must be rejected"),
            Err(error) => error,
        };
        assert_eq!(error.code(), "codex-account-broker-invalid-response");
    }

    #[test]
    fn broker_timeout_terminates_its_process_group() {
        let lock = GlobalStateLock::new();
        let tmp = tempfile::TempDir::new().unwrap();
        let script = tmp.path().join("hanging-broker");
        let child_pid_file = tmp.path().join("child-pid");
        fs::write(
            &script,
            r#"#!/bin/sh
child_pid_file=$1
sleep 60 &
child=$!
printf '%s\n' "$child" > "$child_pid_file"
wait "$child"
"#,
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
        let argv = serde_json::to_string(&vec![
            script.to_string_lossy().into_owned(),
            child_pid_file.to_string_lossy().into_owned(),
        ])
        .unwrap();
        let _broker = EnvGuard::set(&lock, BROKER_ENV, &argv);

        let error = run_broker(&["list"], Duration::from_millis(100)).unwrap_err();
        assert_eq!(error.code(), "codex-account-broker-timeout");
        let child_pid: i32 = fs::read_to_string(child_pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        while unsafe { libc::kill(child_pid, 0) } == 0 && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(unsafe { libc::kill(child_pid, 0) }, -1);
    }
}
