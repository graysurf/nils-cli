pub mod applescript;
pub mod cliclick;
pub mod hammerspoon;
pub mod hammerspoon_input;
pub mod input_source;
pub mod process;

use crate::backend::hammerspoon::HammerspoonAxBackend;
use crate::backend::process::ProcessRunner;
use crate::error::CliError;
use crate::model::{
    AxActionPerformRequest, AxActionPerformResult, AxAttrGetRequest, AxAttrGetResult,
    AxAttrSetRequest, AxAttrSetResult, AxClickRequest, AxClickResult, AxListRequest, AxListResult,
    AxSessionListResult, AxSessionStartRequest, AxSessionStartResult, AxSessionStopRequest,
    AxSessionStopResult, AxTypeRequest, AxTypeResult, AxWatchPollRequest, AxWatchPollResult,
    AxWatchStartRequest, AxWatchStartResult, AxWatchStopRequest, AxWatchStopResult,
};
use crate::test_mode;

const AX_EXTENDED_CAPABILITY_HINT: &str =
    "AX attr/action/session/watch commands require Hammerspoon backend (`hs`).";
const AX_EXTENDED_CAPABILITY_ACTION_HINT: &str = "Use `AGENTS_MACOS_AGENT_AX_BACKEND=hammerspoon|auto` and run `macos-agent preflight --include-probes` to verify readiness.";

pub trait AxBackendAdapter {
    fn list(
        &self,
        runner: &dyn ProcessRunner,
        request: &AxListRequest,
        timeout_ms: u64,
    ) -> Result<AxListResult, CliError>;

    fn click(
        &self,
        runner: &dyn ProcessRunner,
        request: &AxClickRequest,
        timeout_ms: u64,
    ) -> Result<AxClickResult, CliError>;

    fn type_text(
        &self,
        runner: &dyn ProcessRunner,
        request: &AxTypeRequest,
        timeout_ms: u64,
    ) -> Result<AxTypeResult, CliError>;

    fn attr_get(
        &self,
        _runner: &dyn ProcessRunner,
        _request: &AxAttrGetRequest,
        _timeout_ms: u64,
    ) -> Result<AxAttrGetResult, CliError> {
        Err(
            CliError::runtime("AX attribute get is not supported by this backend")
                .with_operation("ax.attr.get")
                .with_hint(AX_EXTENDED_CAPABILITY_HINT)
                .with_hint(AX_EXTENDED_CAPABILITY_ACTION_HINT),
        )
    }

    fn attr_set(
        &self,
        _runner: &dyn ProcessRunner,
        _request: &AxAttrSetRequest,
        _timeout_ms: u64,
    ) -> Result<AxAttrSetResult, CliError> {
        Err(
            CliError::runtime("AX attribute set is not supported by this backend")
                .with_operation("ax.attr.set")
                .with_hint(AX_EXTENDED_CAPABILITY_HINT)
                .with_hint(AX_EXTENDED_CAPABILITY_ACTION_HINT),
        )
    }

    fn action_perform(
        &self,
        _runner: &dyn ProcessRunner,
        _request: &AxActionPerformRequest,
        _timeout_ms: u64,
    ) -> Result<AxActionPerformResult, CliError> {
        Err(
            CliError::runtime("AX action perform is not supported by this backend")
                .with_operation("ax.action.perform")
                .with_hint(AX_EXTENDED_CAPABILITY_HINT)
                .with_hint(AX_EXTENDED_CAPABILITY_ACTION_HINT),
        )
    }

    fn session_start(
        &self,
        _runner: &dyn ProcessRunner,
        _request: &AxSessionStartRequest,
        _timeout_ms: u64,
    ) -> Result<AxSessionStartResult, CliError> {
        Err(
            CliError::runtime("AX session start is not supported by this backend")
                .with_operation("ax.session.start")
                .with_hint(AX_EXTENDED_CAPABILITY_HINT)
                .with_hint(AX_EXTENDED_CAPABILITY_ACTION_HINT),
        )
    }

    fn session_list(
        &self,
        _runner: &dyn ProcessRunner,
        _timeout_ms: u64,
    ) -> Result<AxSessionListResult, CliError> {
        Err(
            CliError::runtime("AX session list is not supported by this backend")
                .with_operation("ax.session.list")
                .with_hint(AX_EXTENDED_CAPABILITY_HINT)
                .with_hint(AX_EXTENDED_CAPABILITY_ACTION_HINT),
        )
    }

    fn session_stop(
        &self,
        _runner: &dyn ProcessRunner,
        _request: &AxSessionStopRequest,
        _timeout_ms: u64,
    ) -> Result<AxSessionStopResult, CliError> {
        Err(
            CliError::runtime("AX session stop is not supported by this backend")
                .with_operation("ax.session.stop")
                .with_hint(AX_EXTENDED_CAPABILITY_HINT)
                .with_hint(AX_EXTENDED_CAPABILITY_ACTION_HINT),
        )
    }

    fn watch_start(
        &self,
        _runner: &dyn ProcessRunner,
        _request: &AxWatchStartRequest,
        _timeout_ms: u64,
    ) -> Result<AxWatchStartResult, CliError> {
        Err(
            CliError::runtime("AX watch start is not supported by this backend")
                .with_operation("ax.watch.start")
                .with_hint(AX_EXTENDED_CAPABILITY_HINT)
                .with_hint(AX_EXTENDED_CAPABILITY_ACTION_HINT),
        )
    }

    fn watch_poll(
        &self,
        _runner: &dyn ProcessRunner,
        _request: &AxWatchPollRequest,
        _timeout_ms: u64,
    ) -> Result<AxWatchPollResult, CliError> {
        Err(
            CliError::runtime("AX watch poll is not supported by this backend")
                .with_operation("ax.watch.poll")
                .with_hint(AX_EXTENDED_CAPABILITY_HINT)
                .with_hint(AX_EXTENDED_CAPABILITY_ACTION_HINT),
        )
    }

    fn watch_stop(
        &self,
        _runner: &dyn ProcessRunner,
        _request: &AxWatchStopRequest,
        _timeout_ms: u64,
    ) -> Result<AxWatchStopResult, CliError> {
        Err(
            CliError::runtime("AX watch stop is not supported by this backend")
                .with_operation("ax.watch.stop")
                .with_hint(AX_EXTENDED_CAPABILITY_HINT)
                .with_hint(AX_EXTENDED_CAPABILITY_ACTION_HINT),
        )
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct AppleScriptAxBackend;

impl AxBackendAdapter for AppleScriptAxBackend {
    fn list(
        &self,
        runner: &dyn ProcessRunner,
        request: &AxListRequest,
        timeout_ms: u64,
    ) -> Result<AxListResult, CliError> {
        applescript::ax_list(runner, request, timeout_ms)
    }

    fn click(
        &self,
        runner: &dyn ProcessRunner,
        request: &AxClickRequest,
        timeout_ms: u64,
    ) -> Result<AxClickResult, CliError> {
        applescript::ax_click(runner, request, timeout_ms)
    }

    fn type_text(
        &self,
        runner: &dyn ProcessRunner,
        request: &AxTypeRequest,
        timeout_ms: u64,
    ) -> Result<AxTypeResult, CliError> {
        applescript::ax_type(runner, request, timeout_ms)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AxBackendPreference {
    Auto,
    Hammerspoon,
    AppleScript,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AxBackendCapabilityCheck {
    pub message: String,
    pub hint: Option<String>,
}

impl AxBackendPreference {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Hammerspoon => "hammerspoon",
            Self::AppleScript => "applescript",
        }
    }

    pub fn resolve() -> Self {
        if let Ok(raw) = std::env::var("AGENTS_MACOS_AGENT_AX_BACKEND") {
            match raw.trim().to_ascii_lowercase().as_str() {
                "hammerspoon" | "hs" => return Self::Hammerspoon,
                "applescript" | "jxa" => return Self::AppleScript,
                "auto" => return Self::Auto,
                _ => {}
            }
        }

        if test_mode::enabled() {
            // Tests rely on deterministic osascript stubs.
            Self::AppleScript
        } else {
            Self::Auto
        }
    }

    fn capability_message(self) -> &'static str {
        match self {
            Self::Auto => {
                "AX backend preference=auto; list/click/type use Hammerspoon first and may fallback to AppleScript (JXA). attr/action/session/watch remain Hammerspoon-only."
            }
            Self::Hammerspoon => {
                "AX backend preference=hammerspoon; list/click/type and attr/action/session/watch all use Hammerspoon."
            }
            Self::AppleScript => {
                "AX backend preference=applescript; list/click/type use AppleScript (JXA), while attr/action/session/watch still require Hammerspoon."
            }
        }
    }
}

pub fn preflight_capability_check() -> AxBackendCapabilityCheck {
    let preference = AxBackendPreference::resolve();
    let hint = if preference == AxBackendPreference::Hammerspoon {
        None
    } else {
        Some(AX_EXTENDED_CAPABILITY_ACTION_HINT.to_string())
    };
    AxBackendCapabilityCheck {
        message: preference.capability_message().to_string(),
        hint,
    }
}

#[derive(Debug, Clone, Copy)]
pub struct AutoAxBackend {
    preference: AxBackendPreference,
}

impl Default for AutoAxBackend {
    fn default() -> Self {
        Self {
            preference: AxBackendPreference::resolve(),
        }
    }
}

impl AutoAxBackend {
    fn fallback_with_hint(primary_error: CliError, fallback_error: CliError) -> CliError {
        fallback_error.with_hint(format!(
            "Hammerspoon backend failed first: {}",
            primary_error.message()
        ))
    }
}

impl AxBackendAdapter for AutoAxBackend {
    fn list(
        &self,
        runner: &dyn ProcessRunner,
        request: &AxListRequest,
        timeout_ms: u64,
    ) -> Result<AxListResult, CliError> {
        match self.preference {
            AxBackendPreference::Hammerspoon => {
                HammerspoonAxBackend.list(runner, request, timeout_ms)
            }
            AxBackendPreference::AppleScript => {
                AppleScriptAxBackend.list(runner, request, timeout_ms)
            }
            AxBackendPreference::Auto => match HammerspoonAxBackend
                .list(runner, request, timeout_ms)
            {
                Ok(result) => Ok(result),
                Err(primary_error) if hammerspoon::is_backend_unavailable_error(&primary_error) => {
                    AppleScriptAxBackend
                        .list(runner, request, timeout_ms)
                        .map_err(|fallback_error| {
                            Self::fallback_with_hint(primary_error, fallback_error)
                        })
                }
                Err(error) => Err(error),
            },
        }
    }

    fn click(
        &self,
        runner: &dyn ProcessRunner,
        request: &AxClickRequest,
        timeout_ms: u64,
    ) -> Result<AxClickResult, CliError> {
        match self.preference {
            AxBackendPreference::Hammerspoon => {
                HammerspoonAxBackend.click(runner, request, timeout_ms)
            }
            AxBackendPreference::AppleScript => {
                AppleScriptAxBackend.click(runner, request, timeout_ms)
            }
            AxBackendPreference::Auto => match HammerspoonAxBackend
                .click(runner, request, timeout_ms)
            {
                Ok(result) => Ok(result),
                Err(primary_error) if hammerspoon::is_backend_unavailable_error(&primary_error) => {
                    AppleScriptAxBackend
                        .click(runner, request, timeout_ms)
                        .map_err(|fallback_error| {
                            Self::fallback_with_hint(primary_error, fallback_error)
                        })
                }
                Err(error) => Err(error),
            },
        }
    }

    fn type_text(
        &self,
        runner: &dyn ProcessRunner,
        request: &AxTypeRequest,
        timeout_ms: u64,
    ) -> Result<AxTypeResult, CliError> {
        match self.preference {
            AxBackendPreference::Hammerspoon => {
                HammerspoonAxBackend.type_text(runner, request, timeout_ms)
            }
            AxBackendPreference::AppleScript => {
                AppleScriptAxBackend.type_text(runner, request, timeout_ms)
            }
            AxBackendPreference::Auto => {
                match HammerspoonAxBackend.type_text(runner, request, timeout_ms) {
                    Ok(result) => Ok(result),
                    Err(primary_error)
                        if hammerspoon::is_backend_unavailable_error(&primary_error) =>
                    {
                        AppleScriptAxBackend
                            .type_text(runner, request, timeout_ms)
                            .map_err(|fallback_error| {
                                Self::fallback_with_hint(primary_error, fallback_error)
                            })
                    }
                    Err(error) => Err(error),
                }
            }
        }
    }

    fn attr_get(
        &self,
        runner: &dyn ProcessRunner,
        request: &AxAttrGetRequest,
        timeout_ms: u64,
    ) -> Result<AxAttrGetResult, CliError> {
        HammerspoonAxBackend.attr_get(runner, request, timeout_ms)
    }

    fn attr_set(
        &self,
        runner: &dyn ProcessRunner,
        request: &AxAttrSetRequest,
        timeout_ms: u64,
    ) -> Result<AxAttrSetResult, CliError> {
        HammerspoonAxBackend.attr_set(runner, request, timeout_ms)
    }

    fn action_perform(
        &self,
        runner: &dyn ProcessRunner,
        request: &AxActionPerformRequest,
        timeout_ms: u64,
    ) -> Result<AxActionPerformResult, CliError> {
        HammerspoonAxBackend.action_perform(runner, request, timeout_ms)
    }

    fn session_start(
        &self,
        runner: &dyn ProcessRunner,
        request: &AxSessionStartRequest,
        timeout_ms: u64,
    ) -> Result<AxSessionStartResult, CliError> {
        HammerspoonAxBackend.session_start(runner, request, timeout_ms)
    }

    fn session_list(
        &self,
        runner: &dyn ProcessRunner,
        timeout_ms: u64,
    ) -> Result<AxSessionListResult, CliError> {
        HammerspoonAxBackend.session_list(runner, timeout_ms)
    }

    fn session_stop(
        &self,
        runner: &dyn ProcessRunner,
        request: &AxSessionStopRequest,
        timeout_ms: u64,
    ) -> Result<AxSessionStopResult, CliError> {
        HammerspoonAxBackend.session_stop(runner, request, timeout_ms)
    }

    fn watch_start(
        &self,
        runner: &dyn ProcessRunner,
        request: &AxWatchStartRequest,
        timeout_ms: u64,
    ) -> Result<AxWatchStartResult, CliError> {
        HammerspoonAxBackend.watch_start(runner, request, timeout_ms)
    }

    fn watch_poll(
        &self,
        runner: &dyn ProcessRunner,
        request: &AxWatchPollRequest,
        timeout_ms: u64,
    ) -> Result<AxWatchPollResult, CliError> {
        HammerspoonAxBackend.watch_poll(runner, request, timeout_ms)
    }

    fn watch_stop(
        &self,
        runner: &dyn ProcessRunner,
        request: &AxWatchStopRequest,
        timeout_ms: u64,
    ) -> Result<AxWatchStopResult, CliError> {
        HammerspoonAxBackend.watch_stop(runner, request, timeout_ms)
    }
}

#[cfg(test)]
mod tests {
    use nils_test_support::{EnvGuard, GlobalStateLock};
    use pretty_assertions::assert_eq;
    use serde_json::json;

    use crate::backend::process::RealProcessRunner;
    use crate::backend::{
        AppleScriptAxBackend, AutoAxBackend, AxBackendAdapter, AxBackendPreference,
    };
    use crate::model::{
        AxActionPerformRequest, AxAttrGetRequest, AxAttrSetRequest, AxClickRequest, AxListRequest,
        AxSelector, AxSessionStartRequest, AxSessionStopRequest, AxTarget, AxTypeRequest,
        AxWatchPollRequest, AxWatchStartRequest, AxWatchStopRequest,
    };

    fn node_selector() -> AxSelector {
        AxSelector {
            node_id: Some("1.1".to_string()),
            ..AxSelector::default()
        }
    }

    #[test]
    fn backend_preference_defaults_to_applescript_in_test_mode() {
        let lock = GlobalStateLock::new();
        let _test_mode = EnvGuard::set(&lock, "AGENTS_MACOS_AGENT_TEST_MODE", "1");
        let _backend = EnvGuard::remove(&lock, "AGENTS_MACOS_AGENT_AX_BACKEND");
        assert_eq!(
            AxBackendPreference::resolve(),
            AxBackendPreference::AppleScript
        );
    }

    #[test]
    fn backend_preference_env_overrides_test_mode_default() {
        let lock = GlobalStateLock::new();
        let _test_mode = EnvGuard::set(&lock, "AGENTS_MACOS_AGENT_TEST_MODE", "1");
        let _backend = EnvGuard::set(&lock, "AGENTS_MACOS_AGENT_AX_BACKEND", "hammerspoon");
        assert_eq!(
            AxBackendPreference::resolve(),
            AxBackendPreference::Hammerspoon
        );
    }

    #[test]
    fn applescript_backend_reports_unsupported_for_ax_extension_methods() {
        let runner = RealProcessRunner;
        let request_target = AxTarget::default();
        let selector = node_selector();

        let attr_get = AppleScriptAxBackend.attr_get(
            &runner,
            &AxAttrGetRequest {
                target: request_target.clone(),
                selector: selector.clone(),
                name: "AXRole".to_string(),
            },
            1000,
        );
        assert!(attr_get.is_err());

        let attr_set = AppleScriptAxBackend.attr_set(
            &runner,
            &AxAttrSetRequest {
                target: request_target.clone(),
                selector: selector.clone(),
                name: "AXValue".to_string(),
                value: json!("hello"),
            },
            1000,
        );
        assert!(attr_set.is_err());

        let action = AppleScriptAxBackend.action_perform(
            &runner,
            &AxActionPerformRequest {
                target: request_target,
                selector,
                name: "AXPress".to_string(),
            },
            1000,
        );
        assert!(action.is_err());
    }

    #[test]
    fn auto_backend_hammerspoon_preference_routes_list_click_type() {
        let lock = GlobalStateLock::new();
        let _test_mode = EnvGuard::set(&lock, "AGENTS_MACOS_AGENT_TEST_MODE", "1");
        let _backend = EnvGuard::set(&lock, "AGENTS_MACOS_AGENT_AX_BACKEND", "hammerspoon");
        let _list = EnvGuard::set(
            &lock,
            "AGENTS_MACOS_AGENT_AX_LIST_JSON",
            r#"{"nodes":[{"node_id":"1.1","role":"AXButton","enabled":true,"focused":false,"actions":[],"path":["1","1"]}],"warnings":[]}"#,
        );
        let _click = EnvGuard::set(
            &lock,
            "AGENTS_MACOS_AGENT_AX_CLICK_JSON",
            r#"{"node_id":"1.1","matched_count":1,"action":"ax-press","used_coordinate_fallback":false}"#,
        );
        let _typ = EnvGuard::set(
            &lock,
            "AGENTS_MACOS_AGENT_AX_TYPE_JSON",
            r#"{"node_id":"1.1","matched_count":1,"applied_via":"ax-set-value","text_length":4,"submitted":false,"used_keyboard_fallback":false}"#,
        );

        let backend = AutoAxBackend::default();
        let runner = RealProcessRunner;
        let list = backend
            .list(&runner, &AxListRequest::default(), 1000)
            .expect("list should succeed");
        assert_eq!(list.nodes.len(), 1);

        let click = backend
            .click(
                &runner,
                &AxClickRequest {
                    target: AxTarget::default(),
                    selector: node_selector(),
                    allow_coordinate_fallback: false,
                    reselect_before_click: false,
                    fallback_order: Vec::new(),
                },
                1000,
            )
            .expect("click should succeed");
        assert_eq!(click.matched_count, 1);

        let typ = backend
            .type_text(
                &runner,
                &AxTypeRequest {
                    target: AxTarget::default(),
                    selector: node_selector(),
                    text: "test".to_string(),
                    clear_first: false,
                    submit: false,
                    paste: false,
                    allow_keyboard_fallback: false,
                },
                1000,
            )
            .expect("type should succeed");
        assert_eq!(typ.text_length, 4);
    }

    #[test]
    fn auto_backend_auto_preference_uses_hammerspoon_first_when_available() {
        let lock = GlobalStateLock::new();
        let _test_mode = EnvGuard::set(&lock, "AGENTS_MACOS_AGENT_TEST_MODE", "1");
        let _backend = EnvGuard::set(&lock, "AGENTS_MACOS_AGENT_AX_BACKEND", "auto");
        let _list = EnvGuard::set(
            &lock,
            "AGENTS_MACOS_AGENT_AX_LIST_JSON",
            r#"{"nodes":[{"node_id":"9.9","role":"AXButton","enabled":true,"focused":false,"actions":[],"path":["9","9"]}],"warnings":[]}"#,
        );

        let backend = AutoAxBackend::default();
        let runner = RealProcessRunner;
        let list = backend
            .list(&runner, &AxListRequest::default(), 1000)
            .expect("list should succeed");
        assert_eq!(list.nodes[0].node_id, "9.9");
    }

    #[test]
    fn auto_backend_ax_extension_methods_route_through_hammerspoon() {
        let lock = GlobalStateLock::new();
        let _test_mode = EnvGuard::set(&lock, "AGENTS_MACOS_AGENT_TEST_MODE", "1");
        let _backend = EnvGuard::set(&lock, "AGENTS_MACOS_AGENT_AX_BACKEND", "applescript");

        let _attr_get = EnvGuard::set(
            &lock,
            "AGENTS_MACOS_AGENT_AX_ATTR_GET_JSON",
            r#"{"node_id":"1.1","matched_count":1,"name":"AXRole","value":"AXButton"}"#,
        );
        let _attr_set = EnvGuard::set(
            &lock,
            "AGENTS_MACOS_AGENT_AX_ATTR_SET_JSON",
            r#"{"node_id":"1.1","matched_count":1,"name":"AXValue","applied":true,"value_type":"string"}"#,
        );
        let _action = EnvGuard::set(
            &lock,
            "AGENTS_MACOS_AGENT_AX_ACTION_PERFORM_JSON",
            r#"{"node_id":"1.1","matched_count":1,"name":"AXPress","performed":true}"#,
        );
        let _session_start = EnvGuard::set(
            &lock,
            "AGENTS_MACOS_AGENT_AX_SESSION_START_JSON",
            r#"{"session_id":"axs-1","app":"Arc","bundle_id":"company.thebrowser.Browser","pid":1001,"created_at_ms":1700000000000,"created":true}"#,
        );
        let _session_list = EnvGuard::set(
            &lock,
            "AGENTS_MACOS_AGENT_AX_SESSION_LIST_JSON",
            r#"{"sessions":[{"session_id":"axs-1","app":"Arc","bundle_id":"company.thebrowser.Browser","pid":1001,"created_at_ms":1700000000000}]}"#,
        );
        let _session_stop = EnvGuard::set(
            &lock,
            "AGENTS_MACOS_AGENT_AX_SESSION_STOP_JSON",
            r#"{"session_id":"axs-1","removed":true}"#,
        );
        let _watch_start = EnvGuard::set(
            &lock,
            "AGENTS_MACOS_AGENT_AX_WATCH_START_JSON",
            r#"{"watch_id":"axw-1","session_id":"axs-1","events":["AXTitleChanged"],"max_buffer":64,"started":true}"#,
        );
        let _watch_poll = EnvGuard::set(
            &lock,
            "AGENTS_MACOS_AGENT_AX_WATCH_POLL_JSON",
            r#"{"watch_id":"axw-1","events":[],"dropped":0,"running":true}"#,
        );
        let _watch_stop = EnvGuard::set(
            &lock,
            "AGENTS_MACOS_AGENT_AX_WATCH_STOP_JSON",
            r#"{"watch_id":"axw-1","stopped":true,"drained":0}"#,
        );

        let backend = AutoAxBackend::default();
        let runner = RealProcessRunner;

        let attr_get = backend
            .attr_get(
                &runner,
                &AxAttrGetRequest {
                    target: AxTarget::default(),
                    selector: node_selector(),
                    name: "AXRole".to_string(),
                },
                1000,
            )
            .expect("attr get should succeed");
        assert_eq!(attr_get.name, "AXRole");

        let attr_set = backend
            .attr_set(
                &runner,
                &AxAttrSetRequest {
                    target: AxTarget::default(),
                    selector: node_selector(),
                    name: "AXValue".to_string(),
                    value: json!("hello"),
                },
                1000,
            )
            .expect("attr set should succeed");
        assert!(attr_set.applied);

        let action = backend
            .action_perform(
                &runner,
                &AxActionPerformRequest {
                    target: AxTarget::default(),
                    selector: node_selector(),
                    name: "AXPress".to_string(),
                },
                1000,
            )
            .expect("action should succeed");
        assert!(action.performed);

        let start = backend
            .session_start(
                &runner,
                &AxSessionStartRequest {
                    target: AxTarget::default(),
                    session_id: Some("axs-1".to_string()),
                },
                1000,
            )
            .expect("session start should succeed");
        assert_eq!(start.session.session_id, "axs-1");

        let listed = backend
            .session_list(&runner, 1000)
            .expect("session list should succeed");
        assert_eq!(listed.sessions.len(), 1);

        let stop = backend
            .session_stop(
                &runner,
                &AxSessionStopRequest {
                    session_id: "axs-1".to_string(),
                },
                1000,
            )
            .expect("session stop should succeed");
        assert!(stop.removed);

        let watch_start = backend
            .watch_start(
                &runner,
                &AxWatchStartRequest {
                    session_id: "axs-1".to_string(),
                    events: vec!["AXTitleChanged".to_string()],
                    max_buffer: 64,
                    watch_id: Some("axw-1".to_string()),
                },
                1000,
            )
            .expect("watch start should succeed");
        assert_eq!(watch_start.watch_id, "axw-1");

        let watch_poll = backend
            .watch_poll(
                &runner,
                &AxWatchPollRequest {
                    watch_id: "axw-1".to_string(),
                    limit: 10,
                    drain: true,
                },
                1000,
            )
            .expect("watch poll should succeed");
        assert!(watch_poll.running);

        let watch_stop = backend
            .watch_stop(
                &runner,
                &AxWatchStopRequest {
                    watch_id: "axw-1".to_string(),
                },
                1000,
            )
            .expect("watch stop should succeed");
        assert!(watch_stop.stopped);
    }
}
