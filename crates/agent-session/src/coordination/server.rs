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
    pub to: String,
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

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct InboxQuery {
    pub state: Option<String>,
    pub cursor: Option<String>,
    pub limit: Option<usize>,
}

pub(crate) fn show(context: &CliContext, id: &str, token: &str) -> Result<Value, CliError> {
    let server_capability = authorize(context, id, token)?;
    claims::show(
        context,
        cli::WorkContextShowArgs {
            session: id.to_string(),
            capability_file: Some(server_capability.path.clone()),
            format: nils_common::cli_contract::OutputFormat::Json,
        },
    )
}

pub(crate) fn check(
    context: &CliContext,
    id: &str,
    token: &str,
    body: CheckBody,
) -> Result<Value, CliError> {
    let server_capability = authorize(context, id, token)?;
    with_optional_json(
        context,
        id,
        "candidate",
        body.candidate.as_ref(),
        |candidate| {
            claims::check(
                context,
                cli::WorkContextCheckArgs {
                    session: id.to_string(),
                    capability_file: Some(server_capability.path.clone()),
                    candidate,
                    allow_incomplete: body.allow_incomplete,
                    format: nils_common::cli_contract::OutputFormat::Json,
                },
            )
        },
    )
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

pub(crate) fn broker_status(
    context: &CliContext,
    id: &str,
    token: &str,
) -> Result<Value, CliError> {
    let server_capability = authorize(context, id, token)?;
    super::broker::status(
        context,
        cli::BrokerStatusArgs {
            session: id.to_string(),
            capability_file: Some(server_capability.path.clone()),
            format: nils_common::cli_contract::OutputFormat::Json,
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
    let server_capability = authorize(context, id, token)?;
    with_text(context, id, "body", &body.body, |body_file| {
        mailbox::send(
            context,
            cli::MessageSendArgs {
                from_session: id.to_string(),
                to_session: body.to,
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
    let directory = super::coordination_dir(context, id);
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

fn with_optional_json<T>(
    context: &CliContext,
    id: &str,
    label: &str,
    value: Option<&Value>,
    operation: impl FnOnce(Option<PathBuf>) -> Result<T, CliError>,
) -> Result<T, CliError> {
    match value {
        Some(value) => with_json(context, id, label, value, |path| operation(Some(path))),
        None => operation(None),
    }
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
    let directory = super::coordination_dir(context, id);
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
