use serde::{Deserialize, Serialize};

use super::{Registry, now_epoch};
use crate::{CliContext, CliError};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct NotificationReceipt {
    pub message_id: String,
    pub target_session_id: String,
    pub target_incarnation: String,
    pub state: String,
    pub attempted_at_epoch: i64,
}

pub(crate) fn mark_queue_only(
    registry: &mut Registry,
    message_id: &str,
    target_session_id: &str,
    target_incarnation: &str,
) {
    registry
        .notifications
        .entry(message_id.to_string())
        .or_insert_with(|| NotificationReceipt {
            message_id: message_id.to_string(),
            target_session_id: target_session_id.to_string(),
            target_incarnation: target_incarnation.to_string(),
            state: "queued".to_string(),
            attempted_at_epoch: now_epoch(),
        });
}

pub(crate) fn fixed_prompt(message_id: &str, session_id: &str) -> String {
    format!(
        "Coordination message {message_id} is available; run agent-session message show --session {session_id} --message {message_id}."
    )
}

pub(crate) fn candidate(
    context: &CliContext,
    message_id: &str,
) -> Result<Option<(String, String)>, CliError> {
    let locked = super::lock_registry(context)?;
    let Some(message) = locked
        .registry
        .messages
        .iter()
        .find(|message| message.message_id == message_id)
    else {
        return Ok(None);
    };
    let Some(receipt) = locked.registry.notifications.get(message_id) else {
        return Ok(None);
    };
    if receipt.state != "queued"
        || receipt.target_session_id != message.recipient_session_id
        || receipt.target_incarnation != message.recipient_incarnation
    {
        return Ok(None);
    }
    Ok(Some((
        message.recipient_session_id.clone(),
        message.recipient_incarnation.clone(),
    )))
}

pub(crate) fn begin_attempt(
    context: &CliContext,
    message_id: &str,
    target_session_id: &str,
    target_incarnation: &str,
) -> Result<bool, CliError> {
    let now = now_epoch();
    let mut locked = super::lock_registry(context)?;
    if !transition_attempt(
        &mut locked.registry,
        message_id,
        target_session_id,
        target_incarnation,
        now,
    ) {
        return Ok(false);
    }
    locked.save()?;
    Ok(true)
}

fn transition_attempt(
    registry: &mut Registry,
    message_id: &str,
    target_session_id: &str,
    target_incarnation: &str,
    now: i64,
) -> bool {
    if registry.notifications.values().any(|receipt| {
        receipt.message_id != message_id
            && receipt.target_session_id == target_session_id
            && receipt.state == "notification_attempting"
            && receipt.attempted_at_epoch > now.saturating_sub(60)
    }) {
        return false;
    }
    let Some(receipt) = registry.notifications.get_mut(message_id) else {
        return false;
    };
    if receipt.state != "queued"
        || receipt.target_session_id != target_session_id
        || receipt.target_incarnation != target_incarnation
    {
        return false;
    }
    receipt.state = "notification_attempting".to_string();
    receipt.attempted_at_epoch = now;
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notification_prompt_is_fixed_and_contains_no_body() {
        assert_eq!(
            fixed_prompt("message-id", "target-session"),
            "Coordination message message-id is available; run agent-session message show --session target-session --message message-id."
        );
    }

    #[test]
    fn notification_attempt_is_durable_once_and_rate_limited_per_target() {
        let mut registry = Registry::default();
        mark_queue_only(&mut registry, "one", "target", "incarnation");
        mark_queue_only(&mut registry, "two", "target", "incarnation");
        assert!(transition_attempt(
            &mut registry,
            "one",
            "target",
            "incarnation",
            100
        ));
        assert!(!transition_attempt(
            &mut registry,
            "one",
            "target",
            "incarnation",
            101
        ));
        assert!(!transition_attempt(
            &mut registry,
            "two",
            "target",
            "incarnation",
            120
        ));
        assert!(transition_attempt(
            &mut registry,
            "two",
            "target",
            "incarnation",
            161
        ));
    }
}
