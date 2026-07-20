use std::fs::OpenOptions;
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::cli::{
    MessageAckArgs, MessageInboxArgs, MessageReplyArgs, MessageSendArgs, MessageShowArgs,
    MessageWaitArgs,
};
use crate::{CliContext, CliError};

use super::{
    Registry, authenticate_from_file, clean_expired, idempotency_replay, incarnation, json_value,
    lock_registry, now_epoch, request_digest, revalidate_capability_file, store_receipt, timestamp,
};

const MESSAGE_VERSION: &str = "agent-session.message.v1";
const BODY_MAX_BYTES: usize = 16 * 1024;
const DEFAULT_EXPIRY_SECS: i64 = 24 * 60 * 60;
const MAX_EXPIRY_SECS: i64 = 7 * 24 * 60 * 60;
const MAX_SESSION_MESSAGES: usize = 256;
const MAX_SESSION_BYTES: usize = 4 * 1024 * 1024;
const PAIR_RATE_PER_MINUTE: usize = 30;
const PAIR_BURST: usize = 10;
const CURSOR_TTL_SECS: i64 = 60 * 60;
const MAX_CURSORS: usize = 4_096;
const DEFAULT_PAGE: usize = 50;
const MAX_PAGE: usize = 100;
const MAX_WAIT_SECS: u64 = 60;
const MAX_REPLY_DEPTH: u8 = 16;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct StoredMessage {
    pub schema_version: String,
    pub message_id: String,
    pub sender_session_id: String,
    pub sender_incarnation: String,
    pub recipient_session_id: String,
    pub recipient_incarnation: String,
    pub state: String,
    pub revision: u64,
    pub reply_to: Option<String>,
    pub reply_depth: u8,
    pub created_at: String,
    pub created_at_epoch: i64,
    #[serde(default)]
    pub created_at_epoch_millis: i64,
    pub expires_at: String,
    pub expires_at_epoch: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_at_epoch: Option<i64>,
    pub body_bytes: usize,
    pub body: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct InboxCursor {
    pub recipient_session_id: String,
    pub recipient_incarnation: String,
    pub state: Option<String>,
    pub after_created_at_epoch: i64,
    pub after_message_id: String,
    pub expires_at_epoch: i64,
}

#[derive(Clone, Debug, Serialize)]
struct MessageMetadata {
    schema_version: String,
    message_id: String,
    sender: SenderProjection,
    recipient_session_id: String,
    state: String,
    revision: u64,
    reply_to: Option<String>,
    created_at: String,
    expires_at: String,
    body_bytes: usize,
}

#[derive(Clone, Debug, Serialize)]
struct SenderProjection {
    session_id: String,
    authenticated: bool,
}

#[derive(Clone, Debug, Serialize)]
struct MessageBodyView {
    #[serde(flatten)]
    metadata: MessageMetadata,
    body: UntrustedBody,
}

#[derive(Clone, Debug, Serialize)]
struct UntrustedBody {
    classification: &'static str,
    text: String,
}

pub(crate) fn send(context: &CliContext, args: MessageSendArgs) -> Result<Value, CliError> {
    let capability_file = resolve_capability_file(args.capability_file.as_deref())?;
    let (record, sender_incarnation) =
        authenticate_from_file(context, &args.from_session, Some(&capability_file))?;
    let body = read_body(&args.body_file)?;
    send_authenticated(
        context,
        &record.id,
        &sender_incarnation,
        &args.to_session,
        body,
        args.reply_to,
        args.expires_in.as_deref(),
        args.idempotency_key,
        "message-send",
        &capability_file,
        None,
        None,
        None,
        None,
    )
}

pub(crate) fn inbox(context: &CliContext, args: MessageInboxArgs) -> Result<Value, CliError> {
    let capability_file = resolve_capability_file(args.capability_file.as_deref())?;
    let (record, recipient_incarnation) =
        authenticate_from_file(context, &args.session, Some(&capability_file))?;
    let limit = args.limit.unwrap_or(DEFAULT_PAGE);
    if limit == 0 || limit > MAX_PAGE {
        return Err(CliError::usage(
            "cursor-invalid",
            "inbox limit must be between 1 and 100",
            None,
        ));
    }
    if args
        .state
        .as_deref()
        .is_some_and(|state| !matches!(state, "unread" | "read" | "acknowledged" | "expired"))
    {
        return Err(CliError::usage(
            "cursor-invalid",
            "inbox state filter is invalid",
            None,
        ));
    }
    let now = now_epoch();
    let mut locked = lock_registry(context)?;
    clean_expired(&mut locked.registry, now);
    revalidate_capability_file(
        context,
        &locked.registry,
        &record,
        &recipient_incarnation,
        &capability_file,
    )?;
    let mut messages: Vec<_> = locked
        .registry
        .messages
        .iter()
        .filter(|message| {
            message.recipient_session_id == record.id
                && message.recipient_incarnation == recipient_incarnation
                && args
                    .state
                    .as_deref()
                    .is_none_or(|state| message.state == state)
        })
        .collect();
    messages.sort_by(|left, right| {
        (left.created_at_epoch, &left.message_id).cmp(&(right.created_at_epoch, &right.message_id))
    });
    let start = match args.cursor.as_deref() {
        Some(cursor) => {
            let cursor =
                locked.registry.cursors.get(cursor).ok_or_else(|| {
                    CliError::data("cursor-invalid", "inbox cursor is invalid", None)
                })?;
            if cursor.recipient_session_id != record.id
                || cursor.recipient_incarnation != recipient_incarnation
                || cursor.state != args.state
                || cursor.expires_at_epoch <= now
            {
                return Err(CliError::data(
                    "cursor-invalid",
                    "inbox cursor is invalid",
                    None,
                ));
            }
            messages
                .iter()
                .position(|message| {
                    (message.created_at_epoch, &message.message_id)
                        > (cursor.after_created_at_epoch, &cursor.after_message_id)
                })
                .unwrap_or(messages.len())
        }
        None => 0,
    };
    let total = messages.len();
    let page: Vec<_> = messages.into_iter().skip(start).take(limit).collect();
    let next_cursor = if start.saturating_add(page.len()) < total {
        let last = page.last().expect("a remaining page has a predecessor");
        let desired = InboxCursor {
            recipient_session_id: record.id.clone(),
            recipient_incarnation: recipient_incarnation.clone(),
            state: args.state.clone(),
            after_created_at_epoch: last.created_at_epoch,
            after_message_id: last.message_id.clone(),
            expires_at_epoch: now.saturating_add(CURSOR_TTL_SECS),
        };
        let existing = locked
            .registry
            .cursors
            .iter()
            .find(|(_, cursor)| {
                cursor.recipient_session_id == desired.recipient_session_id
                    && cursor.recipient_incarnation == desired.recipient_incarnation
                    && cursor.state == desired.state
                    && cursor.after_created_at_epoch == desired.after_created_at_epoch
                    && cursor.after_message_id == desired.after_message_id
            })
            .map(|(key, _)| key.clone());
        let opaque = if let Some(existing) = existing {
            locked
                .registry
                .cursors
                .get_mut(&existing)
                .expect("existing cursor remains present")
                .expires_at_epoch = desired.expires_at_epoch;
            existing
        } else {
            let principal_cursors = locked
                .registry
                .cursors
                .values()
                .filter(|cursor| {
                    cursor.recipient_session_id == record.id
                        && cursor.recipient_incarnation == recipient_incarnation
                })
                .count();
            if locked.registry.cursors.len() >= MAX_CURSORS || principal_cursors >= 128 {
                return Err(CliError::data(
                    "quota-exceeded",
                    "coordination cursor quota exceeded",
                    None,
                ));
            }
            let opaque = uuid::Uuid::new_v4().simple().to_string();
            locked.registry.cursors.insert(opaque.clone(), desired);
            opaque
        };
        Some(opaque)
    } else {
        None
    };
    let rows: Vec<_> = page.into_iter().map(metadata).collect();
    locked.save()?;
    Ok(json!({
        "schema_version": "agent-session.message-inbox.v1",
        "messages": rows,
        "next_cursor": next_cursor,
    }))
}

pub(crate) fn show(context: &CliContext, args: MessageShowArgs) -> Result<Value, CliError> {
    let capability_file = resolve_capability_file(args.capability_file.as_deref())?;
    let (record, recipient_incarnation) =
        authenticate_from_file(context, &args.session, Some(&capability_file))?;
    let now = now_epoch();
    let mut locked = lock_registry(context)?;
    clean_expired(&mut locked.registry, now);
    revalidate_capability_file(
        context,
        &locked.registry,
        &record,
        &recipient_incarnation,
        &capability_file,
    )?;
    let message = find_recipient_message_mut(
        &mut locked.registry,
        &record.id,
        &recipient_incarnation,
        &args.message,
    )?;
    if message.state == "expired" {
        return Err(message_expired());
    }
    if message.state == "unread" {
        message.state = "read".to_string();
        message.revision = message.revision.saturating_add(1);
    }
    let result = MessageBodyView {
        metadata: metadata(message),
        body: UntrustedBody {
            classification: "untrusted_peer_data",
            text: message.body.clone(),
        },
    };
    locked.save()?;
    json_value(result)
}

pub(crate) fn ack(context: &CliContext, args: MessageAckArgs) -> Result<Value, CliError> {
    let capability_file = resolve_capability_file(args.capability_file.as_deref())?;
    let (record, recipient_incarnation) =
        authenticate_from_file(context, &args.session, Some(&capability_file))?;
    let digest = request_digest(
        "message-ack",
        &json!({
            "message": args.message,
            "if_revision": args.if_revision,
        }),
    );
    let now = now_epoch();
    let mut locked = lock_registry(context)?;
    clean_expired(&mut locked.registry, now);
    revalidate_capability_file(
        context,
        &locked.registry,
        &record,
        &recipient_incarnation,
        &capability_file,
    )?;
    if let Some(replay) = idempotency_replay(
        &locked.registry,
        &args.idempotency_key,
        &record.id,
        &recipient_incarnation,
        "message-ack",
        &digest,
    )? {
        return Ok(replay);
    }
    let message = find_recipient_message_mut(
        &mut locked.registry,
        &record.id,
        &recipient_incarnation,
        &args.message,
    )?;
    if message.state == "expired" {
        return Err(message_expired());
    }
    if message.revision != args.if_revision {
        return Err(message_revision_conflict());
    }
    message.state = "acknowledged".to_string();
    message.revision = message.revision.saturating_add(1);
    message.terminal_at_epoch = Some(now);
    let outcome = json_value(metadata(message))?;
    store_receipt(
        &mut locked.registry,
        args.idempotency_key,
        record.id,
        recipient_incarnation,
        "message-ack".to_string(),
        digest,
        outcome.clone(),
        now,
    )?;
    locked.save()?;
    Ok(outcome)
}

pub(crate) fn reply(context: &CliContext, args: MessageReplyArgs) -> Result<Value, CliError> {
    let capability_file = resolve_capability_file(args.capability_file.as_deref())?;
    let (record, sender_incarnation) =
        authenticate_from_file(context, &args.session, Some(&capability_file))?;
    let body = read_body(&args.body_file)?;
    let digest = reply_request_digest(&record.id, &args.message, &body, args.if_revision);
    let _sender_lock = crate::acquire_session_record_lock(context, &record.id)
        .map_err(|_| super::unauthorized())?;
    let sender =
        crate::load_session_record(context, &record.id).map_err(|_| super::unauthorized())?;
    let mut locked = lock_registry(context)?;
    let registry_changed = clean_expired(&mut locked.registry, now_epoch());
    revalidate_capability_file(
        context,
        &locked.registry,
        &sender,
        &sender_incarnation,
        &capability_file,
    )?;
    if let Some(replay) = idempotency_replay(
        &locked.registry,
        &args.idempotency_key,
        &record.id,
        &sender_incarnation,
        "message-reply",
        &digest,
    )? {
        if registry_changed {
            locked.save()?;
        }
        return Ok(replay);
    }
    let original = locked
        .registry
        .messages
        .iter()
        .find(|message| {
            message.message_id == args.message
                && message.recipient_session_id == record.id
                && message.recipient_incarnation == sender_incarnation
        })
        .cloned();
    if registry_changed {
        locked.save()?;
    }
    drop(locked);
    drop(_sender_lock);
    let original = original.ok_or_else(message_not_found)?;
    if original.state == "expired" {
        return Err(message_expired());
    }
    if original.reply_depth >= MAX_REPLY_DEPTH {
        return Err(CliError::data(
            "reply-depth-exceeded",
            "message reply depth limit exceeded",
            None,
        ));
    }
    send_authenticated(
        context,
        &record.id,
        &sender_incarnation,
        &original.sender_session_id,
        body,
        Some(original.message_id),
        None,
        args.idempotency_key,
        "message-reply",
        &capability_file,
        Some(original.reply_depth.saturating_add(1)),
        Some(&original.sender_incarnation),
        Some(args.if_revision),
        Some(digest),
    )
}

fn reply_request_digest(
    sender_session_id: &str,
    message_id: &str,
    body: &str,
    if_revision: u64,
) -> String {
    request_digest(
        "message-reply",
        &json!({
            "sender": sender_session_id,
            "reply_to": message_id,
            "body_digest": super::digest_bytes(body.as_bytes()),
            "if_revision": if_revision,
        }),
    )
}

pub(crate) fn wait(context: &CliContext, args: MessageWaitArgs) -> Result<Value, CliError> {
    wait_with_cancellation(context, args, None)
}

pub(crate) fn wait_with_cancellation(
    context: &CliContext,
    args: MessageWaitArgs,
    cancelled: Option<&std::sync::atomic::AtomicBool>,
) -> Result<Value, CliError> {
    let capability_file = resolve_capability_file(args.capability_file.as_deref())?;
    let (record, recipient_incarnation) =
        authenticate_from_file(context, &args.session, Some(&capability_file))?;
    let timeout = parse_wait(&args.timeout)?;
    let started = Instant::now();
    loop {
        if cancelled.is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Acquire)) {
            return Err(CliError::runtime(
                "wait-cancelled",
                "message wait was cancelled",
                None,
            ));
        }
        let mut locked = lock_registry(context)?;
        let registry_changed = clean_expired(&mut locked.registry, now_epoch());
        revalidate_capability_file(
            context,
            &locked.registry,
            &record,
            &recipient_incarnation,
            &capability_file,
        )?;
        let message = locked
            .registry
            .messages
            .iter()
            .find(|message| {
                message.message_id == args.message
                    && message.recipient_session_id == record.id
                    && message.recipient_incarnation == recipient_incarnation
            })
            .cloned();
        let Some(message) = message else {
            if registry_changed {
                locked.save()?;
            }
            return Err(message_not_found());
        };
        if message.state == "expired" {
            if registry_changed {
                locked.save()?;
            }
            return Err(message_expired());
        }
        if message.revision != args.if_revision {
            let result = json_value(MessageBodyView {
                metadata: metadata(&message),
                body: UntrustedBody {
                    classification: "untrusted_peer_data",
                    text: message.body.clone(),
                },
            });
            if registry_changed {
                locked.save()?;
            }
            return result;
        }
        if registry_changed {
            locked.save()?;
        }
        drop(locked);
        if started.elapsed() >= timeout {
            return Err(CliError::runtime(
                "wait-timeout",
                "message wait reached its bounded timeout",
                None,
            ));
        }
        thread::sleep(Duration::from_millis(50));
    }
}

#[allow(clippy::too_many_arguments)]
fn send_authenticated(
    context: &CliContext,
    sender_session_id: &str,
    sender_incarnation: &str,
    recipient_session_id: &str,
    body: String,
    reply_to: Option<String>,
    expires_in: Option<&str>,
    idempotency_key: String,
    operation: &'static str,
    capability_file: &Path,
    explicit_reply_depth: Option<u8>,
    expected_recipient_incarnation: Option<&str>,
    expected_parent_revision: Option<u64>,
    request_digest_override: Option<String>,
) -> Result<Value, CliError> {
    if sender_session_id == recipient_session_id && reply_to.is_some() {
        return Err(CliError::data(
            "reply-depth-exceeded",
            "self-recursive replies are not allowed",
            None,
        ));
    }
    let expiry_secs = parse_expiry(expires_in)?;
    let digest = request_digest_override.unwrap_or_else(|| {
        request_digest(
            operation,
            &json!({
                "sender": sender_session_id,
                "recipient": recipient_session_id,
                "body_digest": super::digest_bytes(body.as_bytes()),
                "reply_to": reply_to,
                "expiry_secs": expiry_secs,
                "if_revision": expected_parent_revision,
            }),
        )
    });
    {
        let _sender_lock = crate::acquire_session_record_lock(context, sender_session_id)
            .map_err(|_| super::unauthorized())?;
        let sender = crate::load_session_record(context, sender_session_id)
            .map_err(|_| super::unauthorized())?;
        let mut locked = lock_registry(context)?;
        clean_expired(&mut locked.registry, now_epoch());
        revalidate_capability_file(
            context,
            &locked.registry,
            &sender,
            sender_incarnation,
            capability_file,
        )?;
        if let Some(replay) = idempotency_replay(
            &locked.registry,
            &idempotency_key,
            sender_session_id,
            sender_incarnation,
            operation,
            &digest,
        )? {
            return Ok(replay);
        }
    }
    let mut lifecycle_ids = vec![sender_session_id, recipient_session_id];
    lifecycle_ids.sort_unstable();
    lifecycle_ids.dedup();
    let mut _lifecycle_locks = Vec::with_capacity(lifecycle_ids.len());
    for session_id in lifecycle_ids {
        _lifecycle_locks.push(
            crate::acquire_session_record_lock(context, session_id)
                .map_err(|_| message_not_found())?,
        );
    }
    let sender = crate::load_session_record(context, sender_session_id)
        .map_err(|_| super::unauthorized())?;
    let recipient = crate::load_session_record(context, recipient_session_id)
        .map_err(|_| message_not_found())?;
    let recipient_incarnation = incarnation(&recipient).map_err(|_| message_not_found())?;
    if expected_recipient_incarnation.is_some_and(|expected| expected != recipient_incarnation) {
        return Err(CliError::data(
            "session-incarnation-conflict",
            "message target session was replaced",
            None,
        ));
    }
    let now = now_epoch();
    let mut locked = lock_registry(context)?;
    clean_expired(&mut locked.registry, now);
    revalidate_capability_file(
        context,
        &locked.registry,
        &sender,
        sender_incarnation,
        capability_file,
    )?;
    if let Some(replay) = idempotency_replay(
        &locked.registry,
        &idempotency_key,
        sender_session_id,
        sender_incarnation,
        operation,
        &digest,
    )? {
        return Ok(replay);
    }
    let broker = locked
        .registry
        .brokers
        .get(&recipient.id)
        .filter(|broker| {
            broker.incarnation == recipient_incarnation
                && broker.state == "ready"
                && super::broker::capability_available(
                    context,
                    &recipient.id,
                    &recipient_incarnation,
                    &broker.capability_digest,
                )
                && super::broker::heartbeat_fresh(
                    context,
                    &recipient.id,
                    &recipient_incarnation,
                    broker.heartbeat_epoch,
                )
        })
        .ok_or_else(|| {
            CliError::runtime(
                "coordination-unavailable",
                "recipient coordination broker is unavailable",
                None,
            )
        })?;
    let recipient_incarnation = broker.incarnation.clone();
    let pair_recent = locked
        .registry
        .messages
        .iter()
        .filter(|message| {
            message.sender_session_id == sender_session_id
                && message.recipient_session_id == recipient.id
                && message.created_at_epoch > now.saturating_sub(60)
        })
        .count();
    if pair_recent >= PAIR_RATE_PER_MINUTE {
        return Err(CliError::data(
            "rate-limited",
            "coordination message rate limit exceeded",
            None,
        ));
    }
    let now_millis = now_epoch_millis();
    let pair_burst = locked
        .registry
        .messages
        .iter()
        .filter(|message| {
            message.sender_session_id == sender_session_id
                && message.recipient_session_id == recipient.id
                && message.created_at_epoch_millis > now_millis.saturating_sub(1_000)
        })
        .count();
    if pair_burst >= PAIR_BURST {
        return Err(CliError::data(
            "rate-limited",
            "coordination message burst limit exceeded",
            None,
        ));
    }
    let live_for_recipient: Vec<_> = locked
        .registry
        .messages
        .iter()
        .filter(|message| {
            message.recipient_session_id == recipient.id && message.state != "deleted"
        })
        .collect();
    let recipient_bytes: usize = live_for_recipient
        .iter()
        .map(|message| message.body_bytes)
        .sum();
    let registry_bytes: usize = locked
        .registry
        .messages
        .iter()
        .filter(|message| message.state != "deleted")
        .map(|message| message.body_bytes)
        .sum();
    if live_for_recipient.len() >= MAX_SESSION_MESSAGES
        || recipient_bytes.saturating_add(body.len()) > MAX_SESSION_BYTES
        // Mirror the enforced whole-registry cap (`super::MAX_REGISTRY_BYTES`,
        // 68 MiB) so a send is refused before the persisted registry can exceed it.
        || registry_bytes.saturating_add(body.len()) > super::MAX_REGISTRY_BYTES as usize
    {
        return Err(CliError::data(
            "quota-exceeded",
            "coordination mailbox quota exceeded",
            None,
        ));
    }
    let reply_depth = match reply_to.as_deref() {
        Some(parent_id) => {
            let parent = locked
                .registry
                .messages
                .iter()
                .find(|message| {
                    message.message_id == parent_id
                        && message.recipient_session_id == sender_session_id
                        && message.recipient_incarnation == sender_incarnation
                })
                .ok_or_else(message_not_found)?;
            if parent.state == "expired" || parent.expires_at_epoch <= now {
                return Err(message_expired());
            }
            if expected_parent_revision.is_some_and(|revision| parent.revision != revision) {
                return Err(message_revision_conflict());
            }
            if parent.sender_session_id != recipient.id
                || parent.sender_incarnation != recipient_incarnation
            {
                return Err(message_not_found());
            }
            let expected =
                explicit_reply_depth.unwrap_or_else(|| parent.reply_depth.saturating_add(1));
            if expected > MAX_REPLY_DEPTH {
                return Err(CliError::data(
                    "reply-depth-exceeded",
                    "message reply depth limit exceeded",
                    None,
                ));
            }
            expected
        }
        None => 0,
    };
    let message = StoredMessage {
        schema_version: MESSAGE_VERSION.to_string(),
        message_id: uuid::Uuid::new_v4().to_string(),
        sender_session_id: sender_session_id.to_string(),
        sender_incarnation: sender_incarnation.to_string(),
        recipient_session_id: recipient.id.clone(),
        recipient_incarnation: recipient_incarnation.clone(),
        state: "unread".to_string(),
        revision: 1,
        reply_to,
        reply_depth,
        created_at: timestamp(now),
        created_at_epoch: now,
        created_at_epoch_millis: now_millis,
        expires_at: timestamp(now.saturating_add(expiry_secs)),
        expires_at_epoch: now.saturating_add(expiry_secs),
        terminal_at_epoch: None,
        body_bytes: body.len(),
        body,
    };
    let outcome = json_value(metadata(&message))?;
    super::notification::mark_queue_only(
        &mut locked.registry,
        &message.message_id,
        &recipient.id,
        &recipient_incarnation,
    );
    // Materialize the fixed template here so future delivery code cannot derive
    // it from the private body. Queue-only is authoritative when no server-side
    // structured prompt controller is present.
    let _fixed_prompt = super::notification::fixed_prompt(&message.message_id, &recipient.id);
    locked.registry.messages.push(message);
    store_receipt(
        &mut locked.registry,
        idempotency_key,
        sender_session_id.to_string(),
        sender_incarnation.to_string(),
        operation.to_string(),
        digest,
        outcome.clone(),
        now,
    )?;
    locked.save()?;
    Ok(outcome)
}

fn read_body(path: &Path) -> Result<String, CliError> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|_| body_invalid())?;
    let metadata = file.metadata().map_err(|_| body_invalid())?;
    if !metadata.is_file() || metadata.len() as usize > BODY_MAX_BYTES {
        return Err(if metadata.len() as usize > BODY_MAX_BYTES {
            CliError::data(
                "mailbox-body-too-large",
                "coordination message body exceeds 16 KiB",
                None,
            )
        } else {
            body_invalid()
        });
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.by_ref()
        .take((BODY_MAX_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| body_invalid())?;
    if bytes.len() > BODY_MAX_BYTES {
        return Err(CliError::data(
            "mailbox-body-too-large",
            "coordination message body exceeds 16 KiB",
            None,
        ));
    }
    let body = String::from_utf8(bytes).map_err(|_| body_invalid())?;
    if body.is_empty()
        || body.contains('\0')
        || body
            .chars()
            .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
    {
        return Err(body_invalid());
    }
    Ok(body)
}

fn resolve_capability_file(path: Option<&Path>) -> Result<PathBuf, CliError> {
    path.map(PathBuf::from)
        .or_else(|| std::env::var_os(super::CAPABILITY_ENV).map(PathBuf::from))
        .ok_or_else(super::unauthorized)
}

fn metadata(message: &StoredMessage) -> MessageMetadata {
    MessageMetadata {
        schema_version: message.schema_version.clone(),
        message_id: message.message_id.clone(),
        sender: SenderProjection {
            session_id: message.sender_session_id.clone(),
            authenticated: true,
        },
        recipient_session_id: message.recipient_session_id.clone(),
        state: message.state.clone(),
        revision: message.revision,
        reply_to: message.reply_to.clone(),
        created_at: message.created_at.clone(),
        expires_at: message.expires_at.clone(),
        body_bytes: message.body_bytes,
    }
}

fn find_recipient_message_mut<'a>(
    registry: &'a mut Registry,
    session_id: &str,
    incarnation: &str,
    message_id: &str,
) -> Result<&'a mut StoredMessage, CliError> {
    registry
        .messages
        .iter_mut()
        .find(|message| {
            message.message_id == message_id
                && message.recipient_session_id == session_id
                && message.recipient_incarnation == incarnation
        })
        .ok_or_else(message_not_found)
}

fn parse_expiry(value: Option<&str>) -> Result<i64, CliError> {
    let Some(value) = value else {
        return Ok(DEFAULT_EXPIRY_SECS);
    };
    let seconds = parse_duration(value)?;
    if seconds == 0 || seconds > MAX_EXPIRY_SECS as u64 {
        return Err(CliError::usage(
            "mailbox-expiry-invalid",
            "message expiry must be positive and no more than 7 days",
            None,
        ));
    }
    Ok(seconds as i64)
}

fn parse_wait(value: &str) -> Result<Duration, CliError> {
    let seconds = parse_duration(value)?;
    if seconds == 0 || seconds > MAX_WAIT_SECS {
        return Err(CliError::usage(
            "wait-timeout",
            "message wait must be between 1 and 60 seconds",
            None,
        ));
    }
    Ok(Duration::from_secs(seconds))
}

fn parse_duration(value: &str) -> Result<u64, CliError> {
    let (number, multiplier) = if let Some(number) = value.strip_suffix('s') {
        (number, 1)
    } else if let Some(number) = value.strip_suffix('m') {
        (number, 60)
    } else if let Some(number) = value.strip_suffix('h') {
        (number, 60 * 60)
    } else if let Some(number) = value.strip_suffix('d') {
        (number, 24 * 60 * 60)
    } else {
        (value, 1)
    };
    number
        .parse::<u64>()
        .ok()
        .and_then(|number| number.checked_mul(multiplier))
        .ok_or_else(|| {
            CliError::usage(
                "invalid-duration",
                "duration must be an integer with optional s, m, h, or d suffix",
                None,
            )
        })
}

fn body_invalid() -> CliError {
    CliError::data(
        "mailbox-body-invalid",
        "coordination message body is invalid UTF-8 or contains forbidden controls",
        None,
    )
}

fn message_not_found() -> CliError {
    CliError::data(
        "message-not-found",
        "coordination message was not found",
        None,
    )
}

fn message_expired() -> CliError {
    CliError::data("message-expired", "coordination message has expired", None)
}

fn now_epoch_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(i64::MAX)
}

fn message_revision_conflict() -> CliError {
    CliError::data(
        "message-revision-conflict",
        "message revision fence did not match",
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_metadata_omits_body_and_incarnations() {
        let message = StoredMessage {
            schema_version: MESSAGE_VERSION.to_string(),
            message_id: "message".to_string(),
            sender_session_id: "sender".to_string(),
            sender_incarnation: "sender-private".to_string(),
            recipient_session_id: "recipient".to_string(),
            recipient_incarnation: "recipient-private".to_string(),
            state: "unread".to_string(),
            revision: 1,
            reply_to: None,
            reply_depth: 0,
            created_at: "time".to_string(),
            created_at_epoch: 0,
            created_at_epoch_millis: 0,
            expires_at: "time".to_string(),
            expires_at_epoch: 1,
            terminal_at_epoch: None,
            body_bytes: 6,
            body: "canary".to_string(),
        };
        let serialized = serde_json::to_string(&metadata(&message)).expect("serialize");
        assert!(!serialized.contains("canary"));
        assert!(!serialized.contains("sender-private"));
        assert!(!serialized.contains("recipient-private"));
    }

    #[test]
    fn numeric_duration_limits_are_closed() {
        assert_eq!(parse_expiry(None).expect("default"), 86_400);
        assert_eq!(parse_expiry(Some("7d")).expect("maximum"), 604_800);
        assert!(parse_expiry(Some("8d")).is_err());
        assert!(parse_wait("61s").is_err());
    }

    #[test]
    fn coordination_review_reply_digest_does_not_depend_on_retained_parent_metadata() {
        let first = reply_request_digest("sender", "message", "body", 1);
        let retry = reply_request_digest("sender", "message", "body", 1);
        assert_eq!(first, retry);
        assert_ne!(
            first,
            reply_request_digest("sender", "message", "changed", 1)
        );
    }
}
