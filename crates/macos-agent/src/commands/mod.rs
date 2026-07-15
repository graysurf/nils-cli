use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::backend::{self, BackendPaths, VerifiedBackend};
use crate::cli::{RuntimeMode, ToolProfile};
use crate::error::CliError;
use crate::policy;
use crate::process;
use crate::test_mode;

pub mod exec;
pub mod mcp;
pub mod scenario;

const PROVIDER_ENV: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "AZURE_OPENAI_API_KEY",
    "GEMINI_API_KEY",
    "GOOGLE_API_KEY",
    "OPENAI_API_KEY",
    "OPENROUTER_API_KEY",
    "XAI_API_KEY",
];

pub fn peekaboo_binary() -> Result<VerifiedBackend, CliError> {
    backend::acquire_verified_backend()
}

pub fn prepare_runtime(runtime: RuntimeMode, binary: &VerifiedBackend) -> Result<(), CliError> {
    if test_mode::enabled() {
        return Ok(());
    }
    let paths = BackendPaths::resolve()?;
    let expected_version = binary.expected_version().ok_or_else(|| {
        CliError::backend("the verified Peekaboo release identity is unavailable")
            .with_operation("runtime")
    })?;
    match runtime {
        RuntimeMode::App => {
            if !paths.stable_app().is_dir() {
                return Err(CliError::backend("the stable Peekaboo app is unavailable")
                    .with_operation("runtime.app"));
            }
            let args = vec![
                "-g".into(),
                paths.stable_app().to_string_lossy().into_owned(),
            ];
            let output = process::run(
                Path::new("open"),
                &args,
                &[],
                &[],
                None,
                Duration::from_secs(15),
            )
            .map_err(|_| CliError::backend("failed to launch the stable Peekaboo app"))?;
            if output.exit_code != 0 || output.timed_out {
                return Err(CliError::backend("the stable Peekaboo app did not start")
                    .with_operation("runtime.app"));
            }
            verify_runtime_bridge(
                binary,
                &runtime_socket("bridge.sock"),
                "gui",
                expected_version,
                "runtime.app",
            )?;
        }
        RuntimeMode::Daemon | RuntimeMode::Auto => {
            let socket = runtime_socket(&format!(
                "{}-{}.sock",
                if runtime == RuntimeMode::Daemon {
                    "daemon"
                } else {
                    "auto"
                },
                binary.runtime_identity()
            ));
            let args = vec![
                "daemon".into(),
                "start".into(),
                "--bridge-socket".into(),
                socket.to_string_lossy().into_owned(),
            ];
            let (envs, removed_envs) = hardened_env(None);
            let output = process::run_detached(
                binary.path(),
                &args,
                &envs,
                &removed_envs,
                Duration::from_secs(30),
            )
            .map_err(|_| CliError::backend("failed to start the pinned Peekaboo daemon"))?;
            if output.exit_code != 0 || output.timed_out {
                return Err(
                    CliError::backend("the pinned Peekaboo daemon did not start")
                        .with_operation("runtime.daemon"),
                );
            }
            verify_runtime_bridge(
                binary,
                &socket,
                "onDemand",
                expected_version,
                "runtime.daemon",
            )?;
        }
        RuntimeMode::Process => {}
    }
    Ok(())
}

pub fn runtime_argv(runtime: RuntimeMode, argv: &[String], identity: &str) -> Vec<String> {
    match runtime {
        RuntimeMode::App | RuntimeMode::Daemon | RuntimeMode::Auto => {
            let socket = match runtime {
                RuntimeMode::App => runtime_socket("bridge.sock"),
                RuntimeMode::Daemon => runtime_socket(&format!("daemon-{identity}.sock")),
                RuntimeMode::Auto => runtime_socket(&format!("auto-{identity}.sock")),
                RuntimeMode::Process => unreachable!(),
            };
            let mut result = Vec::with_capacity(argv.len() + 2);
            result.extend_from_slice(argv);
            result.push("--bridge-socket".into());
            result.push(socket.to_string_lossy().into_owned());
            result
        }
        RuntimeMode::Process => {
            let mut result = Vec::with_capacity(argv.len() + 1);
            result.extend_from_slice(argv);
            result.push("--no-remote".into());
            result
        }
    }
}

fn verify_runtime_bridge(
    binary: &VerifiedBackend,
    socket: &Path,
    expected_host: &str,
    expected_version: &str,
    operation: &str,
) -> Result<(), CliError> {
    let args = vec![
        "bridge".into(),
        "status".into(),
        "--json".into(),
        "--bridge-socket".into(),
        socket.to_string_lossy().into_owned(),
    ];
    let (envs, removed_envs) = hardened_env(None);
    let started = Instant::now();
    loop {
        let output = process::run(
            binary.path(),
            &args,
            &envs,
            &removed_envs,
            None,
            Duration::from_secs(5),
        )
        .map_err(|_| {
            CliError::backend("failed to probe the pinned Peekaboo runtime")
                .with_operation(operation)
        })?;
        if output.exit_code == 0
            && !output.timed_out
            && let Ok(value) = serde_json::from_slice::<serde_json::Value>(&output.stdout)
        {
            if bridge_identity_matches(&value, expected_host, expected_version) {
                return Ok(());
            }
            if value
                .pointer("/data/selected/source")
                .and_then(serde_json::Value::as_str)
                == Some("remote")
            {
                return Err(CliError::backend(
                    "the active Peekaboo runtime does not match the verified release",
                )
                .with_operation(operation));
            }
        }
        if started.elapsed() >= Duration::from_secs(15) {
            return Err(CliError::backend(
                "the pinned Peekaboo runtime identity did not become ready",
            )
            .with_operation(operation));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn bridge_identity_matches(
    value: &serde_json::Value,
    expected_host: &str,
    expected_version: &str,
) -> bool {
    value.get("success").and_then(serde_json::Value::as_bool) == Some(true)
        && value
            .pointer("/data/selected/source")
            .and_then(serde_json::Value::as_str)
            == Some("remote")
        && value
            .pointer("/data/selected/handshake/hostKind")
            .and_then(serde_json::Value::as_str)
            == Some(expected_host)
        && value
            .pointer("/data/selected/handshake/build")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|build| {
                build == expected_version
                    || build
                        .strip_prefix(expected_version)
                        .is_some_and(|suffix| suffix.starts_with(" ("))
            })
}

fn runtime_socket(name: &str) -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("~"));
    home.join("Library/Application Support/Peekaboo").join(name)
}

pub fn hardened_env(profile: Option<ToolProfile>) -> (Vec<(String, String)>, Vec<&'static str>) {
    let mut envs = vec![
        ("PEEKABOO_AI_PROVIDERS".into(), String::new()),
        ("PEEKABOO_VISUALIZER_MASK_TYPED_TEXT".into(), "true".into()),
        ("PEEKABOO_DISABLE_TOOLS".into(), policy::denied_tools_csv()),
    ];
    if let Some(profile) = profile {
        envs.push((
            "PEEKABOO_ALLOW_TOOLS".into(),
            policy::allowed_tools_csv(profile),
        ));
    }
    (envs, PROVIDER_ENV.to_vec())
}

#[cfg(test)]
mod tests {
    use super::{bridge_identity_matches, runtime_argv};
    use crate::cli::RuntimeMode;

    #[test]
    fn every_runtime_mode_has_a_distinct_upstream_selector() {
        let command = vec!["see".into(), "--json".into()];
        let app = runtime_argv(RuntimeMode::App, &command, "0123456789abcdef");
        let daemon = runtime_argv(RuntimeMode::Daemon, &command, "0123456789abcdef");
        let auto = runtime_argv(RuntimeMode::Auto, &command, "0123456789abcdef");
        let process = runtime_argv(RuntimeMode::Process, &command, "0123456789abcdef");
        assert_ne!(app, auto);
        assert_ne!(daemon, auto);
        assert_ne!(app, daemon);
        assert_eq!(process.last().map(String::as_str), Some("--no-remote"));
        assert!(
            app.iter()
                .any(|value| value.ends_with("/Peekaboo/bridge.sock"))
        );
        assert!(
            daemon
                .iter()
                .any(|value| value.ends_with("/Peekaboo/daemon-0123456789abcdef.sock"))
        );
        assert!(
            auto.iter()
                .any(|value| value.ends_with("/Peekaboo/auto-0123456789abcdef.sock"))
        );
    }

    #[test]
    fn runtime_bridge_identity_rejects_stale_or_wrong_hosts() {
        let current = serde_json::json!({
            "success": true,
            "data": {"selected": {"source": "remote", "handshake": {
                "hostKind": "gui", "build": "3.9.3 (95)"
            }}}
        });
        assert!(bridge_identity_matches(&current, "gui", "3.9.3"));

        let mut stale = current.clone();
        stale["data"]["selected"]["handshake"]["build"] = "3.9.2 (91)".into();
        assert!(!bridge_identity_matches(&stale, "gui", "3.9.3"));

        let mut prefix_collision = current.clone();
        prefix_collision["data"]["selected"]["handshake"]["build"] = "3.9.30 (1)".into();
        assert!(!bridge_identity_matches(&prefix_collision, "gui", "3.9.3"));

        let mut wrong_host = current.clone();
        wrong_host["data"]["selected"]["handshake"]["hostKind"] = "daemon".into();
        assert!(!bridge_identity_matches(&wrong_host, "gui", "3.9.3"));

        let mut missing_build = current;
        missing_build["data"]["selected"]["handshake"]
            .as_object_mut()
            .expect("handshake")
            .remove("build");
        assert!(!bridge_identity_matches(&missing_build, "gui", "3.9.3"));
    }
}
