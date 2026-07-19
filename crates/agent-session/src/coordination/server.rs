use std::fs;
use std::path::PathBuf;

use nils_common::fs::{SECRET_FILE_MODE, write_atomic};
use serde::Deserialize;
use serde_json::Value;

use crate::cli;
use crate::{CliContext, CliError};

use super::{authenticate_token, claims, mailbox};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ClaimBody {
    pub candidate: Value,
    pub idempotency_key: String,
    pub if_revision: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CheckBody {
    pub candidate: Option<Value>,
    #[serde(default)]
    pub self_selector: bool,
    #[serde(default)]
    pub allow_incomplete: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ClaimMutationBody {
    pub claim: String,
    pub if_revision: u64,
    pub idempotency_key: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AdmitBody {
    pub claim: String,
    pub if_revision: u64,
    pub targets: Value,
    pub operation: String,
    pub execution_token: String,
    pub idempotency_key: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CompleteBody {
    pub lease: String,
    pub if_revision: u64,
    pub execution_token: String,
    pub outcome: String,
    pub idempotency_key: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReconcileBody {
    pub lease: String,
    pub if_revision: u64,
    pub proof: Value,
    pub idempotency_key: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SendBody {
    pub body: String,
    pub idempotency_key: String,
    pub reply_to: Option<String>,
    pub expires_in: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AckBody {
    pub if_revision: u64,
    pub idempotency_key: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReplyBody {
    pub body: String,
    pub if_revision: u64,
    pub idempotency_key: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WaitBody {
    pub if_revision: u64,
    pub timeout: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BrokerRecoveryBody {
    pub proof: Value,
    pub idempotency_key: String,
    pub operation: Option<String>,
    pub if_revision: Option<u64>,
    #[serde(default)]
    pub attest_inactive: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct InboxQuery {
    pub state: Option<String>,
    pub cursor: Option<String>,
    pub limit: Option<usize>,
}

pub(crate) fn show(context: &CliContext, id: &str) -> Result<Value, CliError> {
    claims::show(
        context,
        cli::WorkContextShowArgs {
            session: id.to_string(),
            capability_file: None,
            format: nils_common::cli_contract::OutputFormat::Json,
        },
    )
}

pub(crate) fn check(
    context: &CliContext,
    id: &str,
    token: Option<&str>,
    body: CheckBody,
) -> Result<Value, CliError> {
    validate_session_check_body(&body)?;
    let capability = if body.self_selector {
        let token = token.ok_or_else(super::unauthorized)?;
        Some(authorize(context, id, token)?)
    } else {
        None
    };
    claims::check(
        context,
        cli::WorkContextCheckArgs {
            session: (!body.self_selector).then(|| id.to_string()),
            self_selector: body.self_selector,
            capability_file: capability.as_ref().map(|value| value.path.clone()),
            candidate: None,
            allow_incomplete: body.allow_incomplete,
            format: nils_common::cli_contract::OutputFormat::Json,
        },
    )
}

fn validate_session_check_body(body: &CheckBody) -> Result<(), CliError> {
    if body.candidate.is_some() {
        return Err(CliError::usage(
            "invalid-check-selector",
            "session-level checks accept only the path session or authenticated self",
            None,
        ));
    }
    Ok(())
}

pub(crate) fn check_candidate(context: &CliContext, body: CheckBody) -> Result<Value, CliError> {
    if body.self_selector {
        return Err(CliError::usage(
            "invalid-check-selector",
            "registry-level checks accept only an explicit candidate",
            None,
        ));
    }
    let candidate = body.candidate.as_ref().ok_or_else(|| {
        CliError::usage(
            "invalid-check-selector",
            "registry-level conflict checks require a candidate",
            None,
        )
    })?;
    with_json(context, "registry", "candidate", candidate, |candidate| {
        claims::check(
            context,
            cli::WorkContextCheckArgs {
                session: None,
                self_selector: false,
                capability_file: None,
                candidate: Some(candidate),
                allow_incomplete: body.allow_incomplete,
                format: nils_common::cli_contract::OutputFormat::Json,
            },
        )
    })
}

pub(crate) fn claim(
    context: &CliContext,
    id: &str,
    token: &str,
    body: ClaimBody,
) -> Result<Value, CliError> {
    let server_capability = authorize(context, id, token)?;
    with_json(context, id, "candidate", &body.candidate, |file| {
        claims::claim(
            context,
            cli::WorkContextClaimArgs {
                session: id.to_string(),
                file,
                capability_file: Some(server_capability.path.clone()),
                idempotency_key: body.idempotency_key,
                if_revision: body.if_revision,
                format: nils_common::cli_contract::OutputFormat::Json,
            },
        )
    })
}

pub(crate) fn claim_mutation(
    context: &CliContext,
    id: &str,
    token: &str,
    body: ClaimMutationBody,
    release: bool,
) -> Result<Value, CliError> {
    let server_capability = authorize(context, id, token)?;
    let capability_file = Some(server_capability.path.clone());
    if release {
        claims::release(
            context,
            cli::WorkContextReleaseArgs {
                session: id.to_string(),
                claim: body.claim,
                if_revision: body.if_revision,
                capability_file,
                idempotency_key: body.idempotency_key,
                format: nils_common::cli_contract::OutputFormat::Json,
            },
        )
    } else {
        claims::renew(
            context,
            cli::WorkContextRenewArgs {
                session: id.to_string(),
                claim: body.claim,
                if_revision: body.if_revision,
                capability_file,
                idempotency_key: body.idempotency_key,
                format: nils_common::cli_contract::OutputFormat::Json,
            },
        )
    }
}

pub(crate) fn admit(
    context: &CliContext,
    id: &str,
    token: &str,
    body: AdmitBody,
) -> Result<Value, CliError> {
    let server_capability = authorize(context, id, token)?;
    with_json(context, id, "targets", &body.targets, |targets_file| {
        with_text(
            context,
            id,
            "execution-token",
            &body.execution_token,
            |execution_token_file| {
                claims::admit(
                    context,
                    cli::WorkContextAdmitArgs {
                        session: id.to_string(),
                        claim: body.claim,
                        if_revision: body.if_revision,
                        targets_file,
                        operation: body.operation,
                        execution_token_file,
                        capability_file: Some(server_capability.path.clone()),
                        idempotency_key: body.idempotency_key,
                        format: nils_common::cli_contract::OutputFormat::Json,
                    },
                )
            },
        )
    })
}

pub(crate) fn complete(
    context: &CliContext,
    id: &str,
    token: &str,
    body: CompleteBody,
) -> Result<Value, CliError> {
    let server_capability = authorize(context, id, token)?;
    let outcome = match body.outcome.as_str() {
        "pass" => cli::OperationOutcome::Pass,
        "fail" => cli::OperationOutcome::Fail,
        _ => {
            return Err(CliError::usage(
                "invalid-operation-outcome",
                "operation outcome must be pass or fail",
                None,
            ));
        }
    };
    with_text(
        context,
        id,
        "execution-token",
        &body.execution_token,
        |execution_token_file| {
            claims::complete(
                context,
                cli::WorkContextCompleteArgs {
                    session: id.to_string(),
                    lease: body.lease,
                    if_revision: body.if_revision,
                    execution_token_file,
                    outcome,
                    capability_file: Some(server_capability.path.clone()),
                    idempotency_key: body.idempotency_key,
                    format: nils_common::cli_contract::OutputFormat::Json,
                },
            )
        },
    )
}

pub(crate) fn reconcile(
    context: &CliContext,
    id: &str,
    token: &str,
    body: ReconcileBody,
) -> Result<Value, CliError> {
    let server_capability = authorize(context, id, token)?;
    with_json(context, id, "proof", &body.proof, |proof_file| {
        claims::reconcile(
            context,
            cli::WorkContextReconcileArgs {
                session: id.to_string(),
                lease: body.lease,
                if_revision: body.if_revision,
                proof_file,
                capability_file: Some(server_capability.path.clone()),
                idempotency_key: body.idempotency_key,
                format: nils_common::cli_contract::OutputFormat::Json,
            },
        )
    })
}

pub(crate) fn broker_status(context: &CliContext, id: &str) -> Result<Value, CliError> {
    super::broker::status(
        context,
        cli::BrokerStatusArgs {
            session: id.to_string(),
            capability_file: None,
            format: nils_common::cli_contract::OutputFormat::Json,
        },
    )
}

pub(crate) fn broker_recover(
    context: &CliContext,
    id: &str,
    body: BrokerRecoveryBody,
    reconcile: bool,
) -> Result<Value, CliError> {
    with_json(
        context,
        id,
        "broker-recovery-proof",
        &body.proof,
        |proof_file| {
            super::broker::recover(
                context,
                cli::BrokerRecoveryArgs {
                    session: id.to_string(),
                    proof_file,
                    idempotency_key: body.idempotency_key,
                    operation: body.operation,
                    if_revision: body.if_revision,
                    attest_inactive: body.attest_inactive,
                    format: nils_common::cli_contract::OutputFormat::Json,
                },
                reconcile,
            )
        },
    )
}

pub(crate) fn inbox(
    context: &CliContext,
    id: &str,
    token: &str,
    query: InboxQuery,
) -> Result<Value, CliError> {
    let server_capability = authorize(context, id, token)?;
    mailbox::inbox(
        context,
        cli::MessageInboxArgs {
            session: id.to_string(),
            capability_file: Some(server_capability.path.clone()),
            state: query.state,
            cursor: query.cursor,
            limit: query.limit,
            format: nils_common::cli_contract::OutputFormat::Json,
        },
    )
}

pub(crate) fn send(
    context: &CliContext,
    id: &str,
    token: &str,
    body: SendBody,
) -> Result<Value, CliError> {
    let (sender, server_capability) = authorize_any(context, token)?;
    with_text(context, &sender.id, "body", &body.body, |body_file| {
        mailbox::send(
            context,
            cli::MessageSendArgs {
                from_session: sender.id.clone(),
                to_session: id.to_string(),
                body_file,
                capability_file: Some(server_capability.path.clone()),
                idempotency_key: body.idempotency_key,
                reply_to: body.reply_to,
                expires_in: body.expires_in,
                format: nils_common::cli_contract::OutputFormat::Json,
            },
        )
    })
}

pub(crate) fn message_show(
    context: &CliContext,
    id: &str,
    message: &str,
    token: &str,
) -> Result<Value, CliError> {
    let server_capability = authorize(context, id, token)?;
    mailbox::show(
        context,
        cli::MessageShowArgs {
            session: id.to_string(),
            message: message.to_string(),
            capability_file: Some(server_capability.path.clone()),
            format: nils_common::cli_contract::OutputFormat::Json,
        },
    )
}

pub(crate) fn ack(
    context: &CliContext,
    id: &str,
    message: &str,
    token: &str,
    body: AckBody,
) -> Result<Value, CliError> {
    let server_capability = authorize(context, id, token)?;
    mailbox::ack(
        context,
        cli::MessageAckArgs {
            session: id.to_string(),
            message: message.to_string(),
            if_revision: body.if_revision,
            capability_file: Some(server_capability.path.clone()),
            idempotency_key: body.idempotency_key,
            format: nils_common::cli_contract::OutputFormat::Json,
        },
    )
}

pub(crate) fn reply(
    context: &CliContext,
    id: &str,
    message: &str,
    token: &str,
    body: ReplyBody,
) -> Result<Value, CliError> {
    let server_capability = authorize(context, id, token)?;
    with_text(context, id, "reply", &body.body, |body_file| {
        mailbox::reply(
            context,
            cli::MessageReplyArgs {
                session: id.to_string(),
                message: message.to_string(),
                if_revision: body.if_revision,
                body_file,
                capability_file: Some(server_capability.path.clone()),
                idempotency_key: body.idempotency_key,
                format: nils_common::cli_contract::OutputFormat::Json,
            },
        )
    })
}

pub(crate) fn wait(
    context: &CliContext,
    id: &str,
    message: &str,
    token: &str,
    body: WaitBody,
) -> Result<Value, CliError> {
    let server_capability = authorize(context, id, token)?;
    mailbox::wait(
        context,
        cli::MessageWaitArgs {
            session: id.to_string(),
            message: message.to_string(),
            if_revision: body.if_revision,
            timeout: body.timeout,
            capability_file: Some(server_capability.path.clone()),
            format: nils_common::cli_contract::OutputFormat::Json,
        },
    )
}

struct ServerCapability {
    path: PathBuf,
}

impl Drop for ServerCapability {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn authorize(context: &CliContext, id: &str, token: &str) -> Result<ServerCapability, CliError> {
    authenticate_token(context, id, token)?;
    let directory = if id == "registry" {
        drop(super::lock_registry(context)?);
        context.state_dir.join("coordination")
    } else {
        super::coordination_dir(context, id)
    };
    let path = directory.join(format!(
        ".server-capability-{}.request",
        uuid::Uuid::new_v4()
    ));
    write_atomic(&path, token.as_bytes(), SECRET_FILE_MODE).map_err(|_| {
        CliError::runtime(
            "coordination-unavailable",
            "server capability staging failed",
            None,
        )
    })?;
    Ok(ServerCapability { path })
}

fn authorize_any(
    context: &CliContext,
    token: &str,
) -> Result<(crate::SessionRecord, ServerCapability), CliError> {
    let (record, _) = super::authenticate_any_token(context, token)?;
    let directory = super::coordination_dir(context, &record.id);
    let path = directory.join(format!(
        ".server-capability-{}.request",
        uuid::Uuid::new_v4()
    ));
    write_atomic(&path, token.as_bytes(), SECRET_FILE_MODE).map_err(|_| {
        CliError::runtime(
            "coordination-unavailable",
            "server capability staging failed",
            None,
        )
    })?;
    Ok((record, ServerCapability { path }))
}

fn with_json<T>(
    context: &CliContext,
    id: &str,
    label: &str,
    value: &Value,
    operation: impl FnOnce(PathBuf) -> Result<T, CliError>,
) -> Result<T, CliError> {
    let bytes = serde_json::to_vec(value).map_err(|_| invalid_server_body())?;
    with_bytes(context, id, label, &bytes, operation)
}

fn with_text<T>(
    context: &CliContext,
    id: &str,
    label: &str,
    value: &str,
    operation: impl FnOnce(PathBuf) -> Result<T, CliError>,
) -> Result<T, CliError> {
    with_bytes(context, id, label, value.as_bytes(), operation)
}

fn with_bytes<T>(
    context: &CliContext,
    id: &str,
    label: &str,
    bytes: &[u8],
    operation: impl FnOnce(PathBuf) -> Result<T, CliError>,
) -> Result<T, CliError> {
    let directory = if id == "registry" {
        drop(super::lock_registry(context)?);
        context.state_dir.join("coordination")
    } else {
        super::coordination_dir(context, id)
    };
    let path = directory.join(format!(".{label}-{}.request", uuid::Uuid::new_v4()));
    write_atomic(&path, bytes, SECRET_FILE_MODE).map_err(|_| {
        CliError::runtime(
            "coordination-unavailable",
            "server coordination request staging failed",
            None,
        )
    })?;
    let result = operation(path.clone());
    let cleanup = fs::remove_file(&path);
    if cleanup.is_err() && result.is_ok() {
        return Err(CliError::runtime(
            "coordination-unavailable",
            "server coordination request cleanup failed",
            None,
        ));
    }
    result
}

fn invalid_server_body() -> CliError {
    CliError::usage(
        "invalid-json-body",
        "coordination request body is invalid",
        None,
    )
}

pub(crate) fn capability_from_headers(headers: &axum::http::HeaderMap) -> Result<&str, CliError> {
    headers
        .get("x-agent-session-capability")
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty())
        .ok_or_else(super::unauthorized)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn send_body_uses_path_recipient_and_contains_no_redirect_selector() {
        let parsed = serde_json::from_value::<SendBody>(json!({
            "body": "hello",
            "idempotency_key": "send-key-0001",
            "reply_to": null,
            "expires_in": null
        }));
        assert!(
            parsed.is_ok(),
            "recipient must come only from the route path"
        );
        assert!(
            serde_json::from_value::<SendBody>(json!({
                "to": "redirect-target",
                "body": "hello",
                "idempotency_key": "send-key-0002",
                "reply_to": null,
                "expires_in": null
            }))
            .is_err()
        );
    }

    #[test]
    fn session_check_rejects_a_candidate_selector() {
        let body = CheckBody {
            candidate: Some(json!({"schema_version": "agent-session.work-context-input.v1"})),
            self_selector: false,
            allow_incomplete: false,
        };
        let error = validate_session_check_body(&body)
            .expect_err("candidate belongs only on the registry route");
        assert_eq!(error.code(), "invalid-check-selector");
    }
}
