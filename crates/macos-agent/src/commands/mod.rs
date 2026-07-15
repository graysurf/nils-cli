use std::path::{Path, PathBuf};
use std::time::Duration;

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

pub fn prepare_runtime(runtime: RuntimeMode, binary: &Path) -> Result<(), CliError> {
    if test_mode::enabled() {
        return Ok(());
    }
    let paths = BackendPaths::resolve()?;
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
        }
        RuntimeMode::Daemon => {
            let socket = runtime_socket("daemon.sock");
            let args = vec![
                "daemon".into(),
                "start".into(),
                "--bridge-socket".into(),
                socket.to_string_lossy().into_owned(),
            ];
            let (envs, removed_envs) = hardened_env(None);
            let output =
                process::run_detached(binary, &args, &envs, &removed_envs, Duration::from_secs(30))
                    .map_err(|_| CliError::backend("failed to start the pinned Peekaboo daemon"))?;
            if output.exit_code != 0 || output.timed_out {
                return Err(
                    CliError::backend("the pinned Peekaboo daemon did not start")
                        .with_operation("runtime.daemon"),
                );
            }
        }
        RuntimeMode::Auto | RuntimeMode::Process => {}
    }
    Ok(())
}

pub fn runtime_argv(runtime: RuntimeMode, argv: &[String]) -> Vec<String> {
    match runtime {
        RuntimeMode::App | RuntimeMode::Daemon => {
            let socket = match runtime {
                RuntimeMode::App => runtime_socket("bridge.sock"),
                RuntimeMode::Daemon => runtime_socket("daemon.sock"),
                RuntimeMode::Auto | RuntimeMode::Process => unreachable!(),
            };
            let mut result = Vec::with_capacity(argv.len() + 2);
            result.extend_from_slice(argv);
            result.push("--bridge-socket".into());
            result.push(socket.to_string_lossy().into_owned());
            result
        }
        RuntimeMode::Auto => argv.to_vec(),
        RuntimeMode::Process => {
            let mut result = Vec::with_capacity(argv.len() + 1);
            result.extend_from_slice(argv);
            result.push("--no-remote".into());
            result
        }
    }
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
    use super::runtime_argv;
    use crate::cli::RuntimeMode;

    #[test]
    fn every_runtime_mode_has_a_distinct_upstream_selector() {
        let command = vec!["see".into(), "--json".into()];
        let app = runtime_argv(RuntimeMode::App, &command);
        let daemon = runtime_argv(RuntimeMode::Daemon, &command);
        let auto = runtime_argv(RuntimeMode::Auto, &command);
        let process = runtime_argv(RuntimeMode::Process, &command);
        assert_ne!(app, auto);
        assert_ne!(daemon, auto);
        assert_ne!(app, daemon);
        assert_eq!(auto, command);
        assert_eq!(process.last().map(String::as_str), Some("--no-remote"));
        assert!(
            app.iter()
                .any(|value| value.ends_with("/Peekaboo/bridge.sock"))
        );
        assert!(
            daemon
                .iter()
                .any(|value| value.ends_with("/Peekaboo/daemon.sock"))
        );
    }
}
