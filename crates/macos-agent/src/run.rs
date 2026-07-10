use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use crate::backend;
use crate::backend::process::RealProcessRunner;
use crate::cli::{
    AppsCommand, AxActionCommand, AxAttrCommand, AxCommand, AxSessionCommand, AxWatchCommand, Cli,
    CommandGroup, DebugCommand, InputCommand, InputSourceCommand, ObserveCommand, OutputFormat,
    PreflightArgs, ProfileCommand, ScenarioCommand, WaitCommand, WindowCommand, WindowsCommand,
};
use crate::commands;
use crate::error::CliError;
use crate::model::{ActionMeta, ActionPolicyResult};
use crate::preflight;
use crate::retry::RetryPolicy;
use crate::test_mode;

static ACTION_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Copy)]
pub struct ActionPolicy {
    pub dry_run: bool,
    pub retries: u8,
    pub retry_delay_ms: u64,
    pub timeout_ms: u64,
}

impl ActionPolicy {
    pub fn retry_policy(self) -> RetryPolicy {
        RetryPolicy {
            retries: self.retries,
            retry_delay_ms: self.retry_delay_ms,
        }
    }
}

pub fn command_label(cli: &Cli) -> &'static str {
    command_group_label(&cli.command)
}

pub fn command_group_label(command: &CommandGroup) -> &'static str {
    match command {
        CommandGroup::Preflight(_) => "preflight",
        CommandGroup::Windows { .. } => "windows.list",
        CommandGroup::Apps { .. } => "apps.list",
        CommandGroup::Window { command } => match command {
            WindowCommand::Activate(_) => "window.activate",
        },
        CommandGroup::Input { command } => match command {
            InputCommand::Click(_) => "input.click",
            InputCommand::Move(_) => "input.move",
            InputCommand::Drag(_) => "input.drag",
            InputCommand::Scroll(_) => "input.scroll",
            InputCommand::Type(_) => "input.type",
            InputCommand::Key(_) => "input.key",
            InputCommand::Hotkey(_) => "input.hotkey",
        },
        CommandGroup::InputSource { command } => match command {
            InputSourceCommand::Current(_) => "input-source.current",
            InputSourceCommand::Switch(_) => "input-source.switch",
        },
        CommandGroup::Ax { command } => match command {
            AxCommand::List(_) => "ax.list",
            AxCommand::Click(_) => "ax.click",
            AxCommand::Type(_) => "ax.type",
            AxCommand::Attr { command } => match command {
                AxAttrCommand::Get(_) => "ax.attr.get",
                AxAttrCommand::Set(_) => "ax.attr.set",
            },
            AxCommand::Action { command } => match command {
                AxActionCommand::Perform(_) => "ax.action.perform",
            },
            AxCommand::Session { command } => match command {
                AxSessionCommand::Start(_) => "ax.session.start",
                AxSessionCommand::List(_) => "ax.session.list",
                AxSessionCommand::Stop(_) => "ax.session.stop",
            },
            AxCommand::Watch { command } => match command {
                AxWatchCommand::Start(_) => "ax.watch.start",
                AxWatchCommand::Poll(_) => "ax.watch.poll",
                AxWatchCommand::Stop(_) => "ax.watch.stop",
            },
        },
        CommandGroup::Observe { command } => match command {
            ObserveCommand::Screenshot(_) => "observe.screenshot",
        },
        CommandGroup::Debug { command } => match command {
            DebugCommand::Bundle(_) => "debug.bundle",
        },
        CommandGroup::Wait { command } => match command {
            WaitCommand::Sleep(_) => "wait.sleep",
            WaitCommand::AppActive(_) => "wait.app-active",
            WaitCommand::WindowPresent(_) => "wait.window-present",
            WaitCommand::AxPresent(_) => "wait.ax-present",
            WaitCommand::AxUnique(_) => "wait.ax-unique",
        },
        CommandGroup::Scenario { command } => match command {
            ScenarioCommand::Run(_) => "scenario.run",
        },
        CommandGroup::Profile { command } => match command {
            ProfileCommand::Validate(_) => "profile.validate",
            ProfileCommand::Init(_) => "profile.init",
        },
        CommandGroup::Completion(_) => "completion",
    }
}

pub fn run(cli: Cli) -> Result<(), CliError> {
    ensure_supported_platform()?;
    validate_format_support(&cli)?;

    let policy = ActionPolicy {
        dry_run: cli.dry_run,
        retries: cli.retries,
        retry_delay_ms: cli.retry_delay_ms,
        timeout_ms: cli.timeout_ms,
    };
    let runner = RealProcessRunner;

    match cli.command {
        CommandGroup::Preflight(args) => run_preflight(cli.format, args),
        CommandGroup::Windows {
            command: WindowsCommand::List(args),
        } => commands::list::run_windows_list(cli.format, &args),
        CommandGroup::Apps {
            command: AppsCommand::List(args),
        } => commands::list::run_apps_list(cli.format, &args),
        CommandGroup::Observe {
            command: ObserveCommand::Screenshot(args),
        } => commands::observe::run_screenshot(cli.format, &args),
        CommandGroup::Debug {
            command: DebugCommand::Bundle(args),
        } => commands::list::run_debug_bundle(cli.format, &args, policy, &runner),
        CommandGroup::Wait {
            command: WaitCommand::Sleep(args),
        } => commands::wait::run_sleep(cli.format, &args),
        CommandGroup::Wait {
            command: WaitCommand::AppActive(args),
        } => commands::wait::run_app_active(cli.format, &args),
        CommandGroup::Wait {
            command: WaitCommand::WindowPresent(args),
        } => commands::wait::run_window_present(cli.format, &args),
        CommandGroup::Wait {
            command: WaitCommand::AxPresent(args),
        } => commands::wait::run_ax_present(cli.format, &args, policy, &runner),
        CommandGroup::Wait {
            command: WaitCommand::AxUnique(args),
        } => commands::wait::run_ax_unique(cli.format, &args, policy, &runner),
        CommandGroup::Scenario {
            command: ScenarioCommand::Run(args),
        } => commands::scenario::run(cli.format, &args),
        CommandGroup::Profile {
            command: ProfileCommand::Validate(args),
        } => commands::profile::run_validate(cli.format, &args),
        CommandGroup::Profile {
            command: ProfileCommand::Init(args),
        } => commands::profile::run_init(cli.format, &args),
        CommandGroup::Completion(_) => Ok(()),
        CommandGroup::Window {
            command: WindowCommand::Activate(args),
        } => commands::window_activate::run(cli.format, &args, policy, &runner),
        CommandGroup::Input {
            command: InputCommand::Click(args),
        } => commands::input_click::run(cli.format, &args, policy, &runner),
        CommandGroup::Input {
            command: InputCommand::Move(args),
        } => commands::input_pointer::run_move(cli.format, &args, policy, &runner),
        CommandGroup::Input {
            command: InputCommand::Drag(args),
        } => commands::input_pointer::run_drag(cli.format, &args, policy, &runner),
        CommandGroup::Input {
            command: InputCommand::Scroll(args),
        } => commands::input_pointer::run_scroll(cli.format, &args, policy, &runner),
        CommandGroup::Input {
            command: InputCommand::Type(args),
        } => commands::input_type::run(cli.format, &args, policy, &runner),
        CommandGroup::Input {
            command: InputCommand::Key(args),
        } => commands::input_key::run(cli.format, &args, policy, &runner),
        CommandGroup::Input {
            command: InputCommand::Hotkey(args),
        } => commands::input_hotkey::run(cli.format, &args, policy, &runner),
        CommandGroup::InputSource { command } => match command {
            InputSourceCommand::Current(args) => {
                commands::input_source::run_current(cli.format, &args, policy, &runner)
            }
            InputSourceCommand::Switch(args) => {
                commands::input_source::run_switch(cli.format, &args, policy, &runner)
            }
        },
        CommandGroup::Ax { command } => match command {
            AxCommand::List(args) => commands::ax_list::run(cli.format, &args, policy, &runner),
            AxCommand::Click(args) => commands::ax_click::run(cli.format, &args, policy, &runner),
            AxCommand::Type(args) => commands::ax_type::run(cli.format, &args, policy, &runner),
            AxCommand::Attr { command } => match command {
                AxAttrCommand::Get(args) => {
                    commands::ax_attr::run_get(cli.format, &args, policy, &runner)
                }
                AxAttrCommand::Set(args) => {
                    commands::ax_attr::run_set(cli.format, &args, policy, &runner)
                }
            },
            AxCommand::Action { command } => match command {
                AxActionCommand::Perform(args) => {
                    commands::ax_action::run_perform(cli.format, &args, policy, &runner)
                }
            },
            AxCommand::Session { command } => match command {
                AxSessionCommand::Start(args) => {
                    commands::ax_session::run_start(cli.format, &args, policy, &runner)
                }
                AxSessionCommand::List(args) => {
                    commands::ax_session::run_list(cli.format, &args, policy, &runner)
                }
                AxSessionCommand::Stop(args) => {
                    commands::ax_session::run_stop(cli.format, &args, policy, &runner)
                }
            },
            AxCommand::Watch { command } => match command {
                AxWatchCommand::Start(args) => {
                    commands::ax_watch::run_start(cli.format, &args, policy, &runner)
                }
                AxWatchCommand::Poll(args) => {
                    commands::ax_watch::run_poll(cli.format, &args, policy, &runner)
                }
                AxWatchCommand::Stop(args) => {
                    commands::ax_watch::run_stop(cli.format, &args, policy, &runner)
                }
            },
        },
    }
}

fn ensure_supported_platform() -> Result<(), CliError> {
    if cfg!(target_os = "macos") || test_mode::enabled() {
        Ok(())
    } else {
        Err(CliError::unsupported_platform())
    }
}

fn validate_format_support(cli: &Cli) -> Result<(), CliError> {
    if cli.format != OutputFormat::Tsv {
        return Ok(());
    }

    let tsv_allowed = matches!(
        &cli.command,
        CommandGroup::Windows {
            command: WindowsCommand::List(_),
        } | CommandGroup::Apps {
            command: AppsCommand::List(_),
        }
    );

    if tsv_allowed {
        Ok(())
    } else {
        Err(CliError::usage(
            "--format tsv is only supported for `windows list` and `apps list`",
        ))
    }
}

fn run_preflight(format: OutputFormat, args: PreflightArgs) -> Result<(), CliError> {
    let snapshot = preflight::collect_system_snapshot();
    let probes = if args.include_probes {
        preflight::run_live_probes()
    } else {
        Vec::new()
    };
    let mut report = preflight::build_report_with_probes(snapshot, args.strict, probes);
    let backend_capability = backend::preflight_capability_check();
    report.checks.push(preflight::CheckReport {
        id: "ax_backend_capabilities",
        label: "AX backend capabilities",
        status: preflight::CheckStatus::Ok,
        blocking: false,
        message: backend_capability.message,
        hint: backend_capability.hint,
    });

    match format {
        OutputFormat::Text => println!("{}", preflight::render_text(&report)),
        OutputFormat::Json => println!("{}", preflight::render_json(&report)),
        OutputFormat::Tsv => {
            return Err(CliError::usage(
                "--format tsv is only supported for `windows list` and `apps list`",
            ));
        }
    }

    Ok(())
}

pub fn next_action_id(command: &str) -> String {
    let sequence = ACTION_COUNTER.fetch_add(1, Ordering::SeqCst);
    format!("{command}-{}-{sequence}", test_mode::timestamp_token())
}

pub fn build_action_meta(action_id: String, started: Instant, policy: ActionPolicy) -> ActionMeta {
    build_action_meta_with_attempts(
        action_id,
        started,
        policy,
        if policy.dry_run { 0 } else { 1 },
    )
}

pub fn build_action_meta_with_attempts(
    action_id: String,
    started: Instant,
    policy: ActionPolicy,
    attempts_used: u8,
) -> ActionMeta {
    ActionMeta {
        action_id,
        elapsed_ms: started.elapsed().as_millis() as u64,
        dry_run: policy.dry_run,
        retries: policy.retries,
        attempts_used,
        timeout_ms: policy.timeout_ms,
    }
}

pub fn action_policy_result(policy: ActionPolicy) -> ActionPolicyResult {
    ActionPolicyResult {
        dry_run: policy.dry_run,
        retries: policy.retries,
        retry_delay_ms: policy.retry_delay_ms,
        timeout_ms: policy.timeout_ms,
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;
    use nils_test_support::{EnvGuard, GlobalStateLock};
    use pretty_assertions::assert_eq;

    use crate::cli::{Cli, OutputFormat};

    use super::{ensure_supported_platform, validate_format_support};

    #[test]
    fn rejects_tsv_for_non_list_commands() {
        let cli = Cli::try_parse_from(["macos-agent", "--format", "tsv", "preflight"])
            .expect("args should parse");
        let err = validate_format_support(&cli).expect_err("tsv should fail for preflight");
        assert_eq!(err.exit_code(), 2);
        assert!(err.to_string().contains("windows list"));
    }

    #[test]
    fn allows_tsv_for_windows_list() {
        let cli = Cli::try_parse_from(["macos-agent", "--format", "tsv", "windows", "list"])
            .expect("args should parse");
        assert_eq!(cli.format, OutputFormat::Tsv);
        validate_format_support(&cli).expect("tsv should be accepted for windows list");
    }

    #[test]
    fn platform_gate_maps_non_macos_to_usage_error_unless_test_mode() {
        let lock = GlobalStateLock::new();
        let _mode = EnvGuard::remove(&lock, "AGENTS_MACOS_AGENT_TEST_MODE");
        let result = ensure_supported_platform();
        #[cfg(target_os = "macos")]
        assert!(result.is_ok());

        #[cfg(not(target_os = "macos"))]
        {
            let err = result.expect_err("non-macos should be rejected by default");
            assert_eq!(err.exit_code(), 2);
            assert!(err.to_string().contains("macOS"));
        }
    }
}
