//! Hook-side adapter for the centralized `agent-session.observation.v1` plane.
//!
//! Dispatch is the one place in the control plane that sees every provider
//! lifecycle delivery, so it is also the only place that can prove a hook ran at
//! all. Recording is therefore unconditional — not gated behind `--trace`, a
//! healthy broker, or a loadable policy — and it happens on every terminal exit
//! path including the ones that fail before a payload can be normalized.
//!
//! Recording is best-effort by contract. A spool failure is never allowed to
//! change a decision: recovery-critical logging must not sit behind the
//! subsystem it is diagnosing, and it must not become a new way to block a
//! session either.

use nils_common::observation::{self, Component, Event};
use serde_json::Value;
use sha2::{Digest, Sha256};

pub(crate) use nils_common::observation::Severity;

use crate::model::{DecisionAction, Product};
use crate::paths::agent_session_state_root;

/// Bounded operator-facing next actions. These are the only recovery hints the
/// plane may carry, and none of them contains a filesystem path.
pub(crate) const RECOVERY_BROKER_STATUS: &str = "agent-session broker status --session <id>";
pub(crate) const RECOVERY_BROKER_RECONCILE: &str = "agent-session broker reconcile --session <id>";
pub(crate) const RECOVERY_DIAGNOSE: &str = "agent-session diagnose";
pub(crate) const RECOVERY_HOOK_DOCTOR: &str = "agent-hook doctor";

/// One pending observation. Fields are attached fluently and the record is
/// written by [`Record::emit`].
#[derive(Debug)]
pub(crate) struct Record<'a> {
    stage: &'a str,
    code: &'a str,
    severity: Severity,
    product: Option<Product>,
    event: Option<&'a str>,
    disposition: Option<&'a str>,
    duration_ms: Option<u64>,
    peer_version: Option<&'a str>,
    correlation: Option<String>,
    recovery_action: Option<&'a str>,
}

impl<'a> Record<'a> {
    /// Start a record for one pipeline stage and stable outcome code.
    pub(crate) fn new(stage: &'a str, code: &'a str, severity: Severity) -> Self {
        Self {
            stage,
            code,
            severity,
            product: None,
            event: None,
            disposition: None,
            duration_ms: None,
            peer_version: None,
            correlation: None,
            recovery_action: None,
        }
    }

    /// Attach the canonical provider and provider event.
    pub(crate) fn provider(mut self, product: Product, event: &'a str) -> Self {
        self.product = Some(product);
        self.event = Some(event);
        self
    }

    /// Attach the terminal disposition slug.
    pub(crate) fn disposition(mut self, disposition: &'a str) -> Self {
        self.disposition = Some(disposition);
        self
    }

    /// Attach the stage duration.
    pub(crate) fn duration_ms(mut self, duration_ms: u64) -> Self {
        self.duration_ms = Some(duration_ms);
        self
    }

    /// Attach the peer release observed across a protocol boundary.
    pub(crate) fn peer_version(mut self, peer_version: &'a str) -> Self {
        self.peer_version = Some(peer_version);
        self
    }

    /// Derive the turn correlation digest from the raw provider payload.
    pub(crate) fn correlate(mut self, product: Product, raw: &[u8]) -> Self {
        self.correlation = turn_correlation(product, raw);
        self
    }

    /// Attach one bounded safe next action.
    pub(crate) fn recovery(mut self, action: &'a str) -> Self {
        self.recovery_action = Some(action);
        self
    }

    /// Write the record. Failures are intentionally discarded.
    pub(crate) fn emit(self) {
        let _ = self.try_emit();
    }

    fn try_emit(self) -> Option<()> {
        let state_root = agent_session_state_root().ok()?;
        let mut event = Event::new(
            Component::AgentHook,
            self.stage,
            self.code,
            self.severity,
            env!("CARGO_PKG_VERSION"),
            jiff::Timestamp::now().as_second(),
        )
        .ok()?;
        if let (Some(product), Some(name)) = (self.product, self.event) {
            event = event.with_provider(product.as_str(), name).ok()?;
        }
        if let Some(disposition) = self.disposition {
            event = event.with_disposition(disposition).ok()?;
        }
        if let Some(duration_ms) = self.duration_ms {
            event = event.with_duration_ms(duration_ms);
        }
        if let Some(peer_version) = self.peer_version {
            // A peer release we cannot represent is dropped rather than
            // refusing the whole record: the outcome still matters.
            if let Ok(updated) = event.clone().with_peer_version(peer_version) {
                event = updated;
            }
        }
        if let Some(correlation) = self.correlation.as_deref() {
            event = event.with_correlation(correlation).ok()?;
        }
        if let Some(action) = self.recovery_action {
            event = event.with_recovery_action(action).ok()?;
        }
        observation::append(&state_root, &event).ok()
    }
}

/// Display severity for a terminal decision.
///
/// A block is the policy working as designed, so it is informational here. Only a
/// degraded lane is a warning, because that is the state an operator has to act
/// on.
pub(crate) fn severity_for(action: DecisionAction) -> Severity {
    match action {
        DecisionAction::Warn => Severity::Warn,
        DecisionAction::Allow
        | DecisionAction::Context
        | DecisionAction::Transform
        | DecisionAction::Block => Severity::Info,
    }
}

/// Stable disposition slug for a decision action.
pub(crate) fn disposition_for(action: DecisionAction) -> &'static str {
    match action {
        DecisionAction::Allow => "allow",
        DecisionAction::Warn => "warn",
        DecisionAction::Context => "context",
        DecisionAction::Transform => "transform",
        DecisionAction::Block => "block",
    }
}

/// Project the provider's own turn identity into an opaque correlation digest.
///
/// The digest is stable for repeated deliveries of the same turn, which is what
/// makes a degraded prompt replayable at most once, and it never carries the raw
/// provider identity it was derived from.
fn turn_correlation(product: Product, raw: &[u8]) -> Option<String> {
    let value: Value = serde_json::from_slice(raw).ok()?;
    let object = value.as_object()?;
    let turn = object
        .get("turn_id")
        .or_else(|| object.get("prompt_id"))
        .or_else(|| object.get("session_id"))
        .or_else(|| object.get("session_key"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.len() <= 256)?;
    let mut hasher = Sha256::new();
    hasher.update(b"agent-hook.observation.turn.v1");
    hasher.update([0]);
    hasher.update(product.as_str().as_bytes());
    hasher.update([0]);
    hasher.update(turn.as_bytes());
    Some(format!(
        "sha256:{}",
        hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::{assert_eq, assert_ne};

    #[test]
    fn turn_correlation_is_stable_per_turn_and_never_echoes_the_identity() {
        let payload = br#"{"hook_event_name":"UserPromptSubmit","prompt_id":"turn-secret"}"#;
        let first = turn_correlation(Product::Claude, payload).expect("correlation");
        let second = turn_correlation(Product::Claude, payload).expect("correlation");

        assert_eq!(first, second, "the same turn must share one digest");
        assert!(first.starts_with("sha256:"));
        assert_eq!(first.len(), "sha256:".len() + 64);
        assert!(!first.contains("turn-secret"));
    }

    #[test]
    fn turn_correlation_separates_turns_products_and_prefers_the_explicit_turn_id() {
        let claude = turn_correlation(
            Product::Claude,
            br#"{"hook_event_name":"Stop","prompt_id":"turn-a"}"#,
        )
        .expect("correlation");
        let codex = turn_correlation(
            Product::Codex,
            br#"{"hook_event_name":"Stop","prompt_id":"turn-a"}"#,
        )
        .expect("correlation");
        assert_ne!(
            claude, codex,
            "products must not share a correlation domain"
        );

        let other_turn = turn_correlation(
            Product::Claude,
            br#"{"hook_event_name":"Stop","prompt_id":"turn-b"}"#,
        )
        .expect("correlation");
        assert_ne!(claude, other_turn);

        // An explicit turn_id outranks the Claude prompt_id fallback.
        let explicit = turn_correlation(
            Product::Claude,
            br#"{"hook_event_name":"Stop","turn_id":"turn-b","prompt_id":"turn-a"}"#,
        )
        .expect("correlation");
        assert_eq!(explicit, other_turn);
    }

    #[test]
    fn a_payload_without_any_turn_identity_has_no_correlation() {
        assert_eq!(
            turn_correlation(Product::Codex, br#"{"hook_event_name":"Stop"}"#),
            None
        );
        assert_eq!(turn_correlation(Product::Codex, b"{"), None);
    }
}
