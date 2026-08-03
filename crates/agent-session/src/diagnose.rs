//! Redacted control-plane diagnostic bundle.
//!
//! `agent-session logs` shows a provider pane or a one-shot run log, and
//! `/healthz` proves only that an HTTP handler answers. Neither can explain why a
//! session degraded (`sympoies/nils-cli#1409`). This command is the read side of
//! the shared `agent-session.observation.v1` plane: it aggregates the bounded
//! spool that hooks and the control plane append to, projects health, and names
//! the typed recovery a degraded runtime has available.
//!
//! It reads only the filesystem. That is deliberate — the daemon being down is
//! one of the states an operator most needs to diagnose, so the diagnostic must
//! not depend on it.

use nils_common::observation::{self, CodeSummary, Component, Event, Severity};
use nils_common::runtime_compat;
use serde::Serialize;
use serde_json::json;

use crate::cli::DiagnoseArgs;
use crate::{CliContext, CliError, render_error, render_single_success};

const BUNDLE_VERSION: &str = "agent-session.diagnostic-bundle.v1";
/// Recent events retained in the bundle when the caller does not choose.
const DEFAULT_RECENT: usize = 20;
/// Hard ceiling on the recent slice so a bundle stays reviewable.
const MAX_RECENT: usize = 200;

/// Projected control-plane health.
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum Health {
    /// No warning or worse in the window.
    Healthy,
    /// At least one degraded lane, version skew, or recoverable fault.
    Degraded,
    /// The failure domain itself reported a critical event.
    Critical,
}

impl Health {
    fn as_str(self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::Degraded => "degraded",
            Self::Critical => "critical",
        }
    }
}

#[derive(Serialize)]
struct Bundle {
    schema_version: &'static str,
    binary_version: String,
    health: Health,
    runtime: Runtime,
    observation: Observation,
}

#[derive(Serialize)]
struct Runtime {
    /// Whether this process still has the executable it started from. A fleet
    /// upgrade can leave a live process on a deleted inode while the installed
    /// symlink looks correct, so the symlink alone is not evidence.
    executable_state: &'static str,
    /// Releases published by live broker records that cross a generation
    /// boundary from this binary.
    release_skew: Vec<ReleaseSkew>,
}

#[derive(Serialize)]
struct ReleaseSkew {
    session_id: String,
    broker_release: String,
    skew: &'static str,
    recovery_action: String,
}

#[derive(Serialize)]
struct Observation {
    /// Total events retained in the spool.
    event_count: usize,
    /// Unix second of the oldest retained event.
    #[serde(skip_serializing_if = "Option::is_none")]
    first_seen_epoch: Option<i64>,
    /// Unix second of the newest retained event.
    #[serde(skip_serializing_if = "Option::is_none")]
    last_seen_epoch: Option<i64>,
    /// Per-code counters, most severe first.
    summary: Vec<CodeSummary>,
    /// Most recent events, oldest first.
    recent: Vec<Event>,
}

/// Run `agent-session diagnose`.
pub(crate) fn run_diagnose(context: &CliContext, args: DiagnoseArgs) -> i32 {
    match build(context, &args) {
        Ok(bundle) => render_single_success(DIAGNOSE_COMMAND, args.format, &bundle, text),
        Err(error) => render_error(DIAGNOSE_COMMAND, args.format, error),
    }
}

/// Command name used in the response envelope schema version.
pub(crate) const DIAGNOSE_COMMAND: &str = "diagnose";

fn build(context: &CliContext, args: &DiagnoseArgs) -> Result<Bundle, CliError> {
    let events = observation::read_recent(&context.state_dir, 0).map_err(|_| {
        CliError::data(
            "observation-spool-unreadable",
            "the observation spool could not be read",
            Some(json!({ "recovery_action": "agent-hook doctor" })),
        )
    })?;
    let summary = observation::summarize(&events);
    let retained = args.limit.unwrap_or(DEFAULT_RECENT).min(MAX_RECENT);
    let recent = events
        .iter()
        .skip(events.len().saturating_sub(retained))
        .cloned()
        .collect();
    let release_skew = release_skew(context);
    let health = health(&summary, &release_skew);
    Ok(Bundle {
        schema_version: BUNDLE_VERSION,
        binary_version: nils_build_info::long_version(env!("CARGO_PKG_VERSION")),
        health,
        runtime: Runtime {
            executable_state: runtime_compat::executable_state(std::process::id()).as_str(),
            release_skew,
        },
        observation: Observation {
            event_count: events.len(),
            first_seen_epoch: events.first().map(|event| event.recorded_at_epoch),
            last_seen_epoch: events.last().map(|event| event.recorded_at_epoch),
            summary,
            recent,
        },
    })
}

/// Collect live broker records whose release crosses a generation boundary.
///
/// An unreadable or absent registry is not an error here: the diagnostic still
/// has to work when coordination is exactly the broken subsystem.
fn release_skew(context: &CliContext) -> Vec<ReleaseSkew> {
    let Ok(Some(registry)) = nils_common::coordination_projection::load(&context.state_dir) else {
        return Vec::new();
    };
    let local = env!("CARGO_PKG_VERSION");
    let mut skewed: Vec<ReleaseSkew> = registry
        .brokers
        .values()
        .filter_map(|broker| {
            let release = broker.binary_version.as_deref()?;
            let skew = runtime_compat::classify_release(local, release);
            skew.crosses_generation().then(|| ReleaseSkew {
                session_id: broker.session_id.clone(),
                broker_release: release.to_string(),
                skew: skew.as_str(),
                recovery_action: format!(
                    "agent-session broker reconcile --session {}",
                    broker.session_id
                ),
            })
        })
        .collect();
    skewed.sort_by(|left, right| left.session_id.cmp(&right.session_id));
    skewed
}

fn health(summary: &[CodeSummary], release_skew: &[ReleaseSkew]) -> Health {
    if summary
        .iter()
        .any(|entry| entry.severity == Severity::Critical)
    {
        return Health::Critical;
    }
    if !release_skew.is_empty()
        || summary
            .iter()
            .any(|entry| matches!(entry.severity, Severity::Warn | Severity::Error))
    {
        return Health::Degraded;
    }
    Health::Healthy
}

fn text(bundle: &Bundle) -> String {
    let mut out = String::new();
    out.push_str(&format!("health: {}\n", bundle.health.as_str()));
    out.push_str(&format!("binary: {}\n", bundle.binary_version));
    out.push_str(&format!(
        "executable: {}\n",
        bundle.runtime.executable_state
    ));
    out.push_str(&format!(
        "observed events: {}\n",
        bundle.observation.event_count
    ));
    for skew in &bundle.runtime.release_skew {
        out.push_str(&format!(
            "release skew: session {} broker {} ({}) -> {}\n",
            skew.session_id, skew.broker_release, skew.skew, skew.recovery_action
        ));
    }
    if bundle.observation.summary.is_empty() {
        out.push_str("no observation events retained\n");
        return out;
    }
    out.push_str("codes:\n");
    for entry in &bundle.observation.summary {
        out.push_str(&format!(
            "  [{}] {} {} x{} first={} last={}",
            entry.severity.as_str(),
            component_label(entry.component),
            entry.code,
            entry.count,
            entry.first_seen_epoch,
            entry.last_seen_epoch
        ));
        if let Some(action) = entry.recovery_action.as_deref() {
            out.push_str(&format!(" -> {action}"));
        }
        out.push('\n');
    }
    out
}

fn component_label(component: Component) -> &'static str {
    component.as_str()
}

#[cfg(test)]
mod tests {
    use super::*;
    use nils_common::runtime_compat::ExecutableState;
    use pretty_assertions::assert_eq;

    fn summary(code: &str, severity: Severity) -> CodeSummary {
        CodeSummary {
            component: Component::AgentHook,
            code: code.to_string(),
            severity,
            count: 1,
            first_seen_epoch: 1,
            last_seen_epoch: 2,
            recovery_action: None,
        }
    }

    #[test]
    fn health_separates_healthy_degraded_and_critical() {
        assert_eq!(health(&[], &[]), Health::Healthy);
        assert_eq!(
            health(&[summary("dispatch-completed", Severity::Info)], &[]),
            Health::Healthy
        );
        assert_eq!(
            health(
                &[summary("coordination-degraded-read-only", Severity::Warn)],
                &[]
            ),
            Health::Degraded
        );
        assert_eq!(
            health(&[summary("coordination-invalid", Severity::Error)], &[]),
            Health::Degraded
        );
        assert_eq!(
            health(&[summary("spool-lost", Severity::Critical)], &[]),
            Health::Critical
        );
    }

    #[test]
    fn a_release_skew_alone_degrades_an_otherwise_quiet_plane() {
        let skew = vec![ReleaseSkew {
            session_id: "worker".to_string(),
            broker_release: "0.9.0".to_string(),
            skew: "major",
            recovery_action: "agent-session broker reconcile --session worker".to_string(),
        }];

        assert_eq!(health(&[], &skew), Health::Degraded);
    }

    #[test]
    fn the_text_projection_names_the_recovery_for_each_code() {
        let bundle = Bundle {
            schema_version: BUNDLE_VERSION,
            binary_version: "1.2.3".to_string(),
            health: Health::Degraded,
            runtime: Runtime {
                executable_state: "live",
                release_skew: Vec::new(),
            },
            observation: Observation {
                event_count: 1,
                first_seen_epoch: Some(1),
                last_seen_epoch: Some(2),
                summary: vec![CodeSummary {
                    component: Component::AgentHook,
                    code: "coordination-degraded-read-only".to_string(),
                    severity: Severity::Warn,
                    count: 4,
                    first_seen_epoch: 1,
                    last_seen_epoch: 2,
                    recovery_action: Some("agent-session broker status".to_string()),
                }],
                recent: Vec::new(),
            },
        };

        let rendered = text(&bundle);
        assert!(rendered.contains("health: degraded"), "{rendered}");
        assert!(rendered.contains("executable: live"), "{rendered}");
        assert!(
            rendered.contains("[warn] agent-hook coordination-degraded-read-only x4"),
            "{rendered}"
        );
        assert!(
            rendered.contains("-> agent-session broker status"),
            "{rendered}"
        );
    }

    #[test]
    fn an_empty_plane_renders_an_explicit_empty_projection() {
        let bundle = Bundle {
            schema_version: BUNDLE_VERSION,
            binary_version: "1.2.3".to_string(),
            health: Health::Healthy,
            runtime: Runtime {
                executable_state: ExecutableState::Unknown.as_str(),
                release_skew: Vec::new(),
            },
            observation: Observation {
                event_count: 0,
                first_seen_epoch: None,
                last_seen_epoch: None,
                summary: Vec::new(),
                recent: Vec::new(),
            },
        };

        let rendered = text(&bundle);
        assert!(
            rendered.contains("no observation events retained"),
            "{rendered}"
        );
        assert!(!rendered.contains("codes:"), "{rendered}");
    }
}
