use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::backend::{self, BackendPaths, RuntimeContract, VerifiedBackend};
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

#[derive(Debug, Clone)]
pub struct RuntimeBinding {
    socket: Option<PathBuf>,
}

impl RuntimeBinding {
    fn remote(socket: PathBuf) -> Self {
        Self {
            socket: Some(socket),
        }
    }

    fn process() -> Self {
        Self { socket: None }
    }

    fn for_mode(runtime: RuntimeMode, identity: &str) -> Self {
        let socket = match runtime {
            RuntimeMode::App => Some(runtime_socket_dir().join("bridge.sock")),
            RuntimeMode::Daemon => {
                Some(runtime_socket_dir().join(format!("daemon-{identity}.sock")))
            }
            RuntimeMode::Auto => Some(runtime_socket_dir().join(format!("auto-{identity}.sock"))),
            RuntimeMode::Process => None,
        };
        Self { socket }
    }

    pub fn argv(&self, argv: &[String]) -> Vec<String> {
        let mut result = Vec::with_capacity(argv.len() + 2);
        result.extend_from_slice(argv);
        if let Some(socket) = self.socket.as_ref() {
            result.push("--bridge-socket".into());
            result.push(socket.to_string_lossy().into_owned());
        } else {
            result.push("--no-remote".into());
        }
        result
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BridgeProbe {
    Ready,
    Unavailable,
    Mismatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AutoRuntimeChoice {
    App,
    Daemon,
}

fn auto_runtime_choice(probe: BridgeProbe) -> AutoRuntimeChoice {
    match probe {
        BridgeProbe::Ready => AutoRuntimeChoice::App,
        BridgeProbe::Unavailable | BridgeProbe::Mismatch => AutoRuntimeChoice::Daemon,
    }
}

pub fn prepare_runtime(
    runtime: RuntimeMode,
    binary: &VerifiedBackend,
) -> Result<RuntimeBinding, CliError> {
    if test_mode::enabled() {
        return Ok(RuntimeBinding::for_mode(runtime, binary.runtime_identity()));
    }
    if runtime == RuntimeMode::Process {
        return Ok(RuntimeBinding::process());
    }
    let paths = BackendPaths::resolve()?;
    let socket_dir = runtime_socket_dir();
    retire_obsolete_daemons_at(binary.path(), &socket_dir, binary.obsolete_runtimes())?;
    let app_socket = socket_dir.join("bridge.sock");
    match runtime {
        RuntimeMode::App => {
            if !paths.stable_app().is_dir() {
                return Err(CliError::backend("the stable Peekaboo app is unavailable")
                    .with_operation("runtime.app"));
            }
            let expected_build = binary.app_bridge_build().ok_or_else(|| {
                CliError::backend("the verified Peekaboo app build identity is unavailable")
                    .with_operation("runtime.app")
            })?;
            if probe_runtime_bridge_once(
                binary.path(),
                &app_socket,
                "gui",
                expected_build,
                "runtime.app",
            )? != BridgeProbe::Ready
            {
                launch_stable_app(&paths)?;
                verify_runtime_bridge(
                    binary.path(),
                    &app_socket,
                    "gui",
                    expected_build,
                    "runtime.app",
                )?;
            }
            Ok(RuntimeBinding::remote(app_socket))
        }
        RuntimeMode::Daemon => {
            let expected_build = binary.cli_bridge_build().ok_or_else(|| {
                CliError::backend("the verified Peekaboo CLI build identity is unavailable")
                    .with_operation("runtime.daemon")
            })?;
            let socket = socket_dir.join(format!("daemon-{}.sock", binary.runtime_identity()));
            start_daemon(binary.path(), &socket, expected_build, "runtime.daemon")?;
            Ok(RuntimeBinding::remote(socket))
        }
        RuntimeMode::Auto => {
            let app_build = binary.app_bridge_build().ok_or_else(|| {
                CliError::backend("the verified Peekaboo app build identity is unavailable")
                    .with_operation("runtime.auto")
            })?;
            let app_probe = probe_runtime_bridge_once(
                binary.path(),
                &app_socket,
                "gui",
                app_build,
                "runtime.auto",
            )?;
            if auto_runtime_choice(app_probe) == AutoRuntimeChoice::App {
                return Ok(RuntimeBinding::remote(app_socket));
            }
            let cli_build = binary.cli_bridge_build().ok_or_else(|| {
                CliError::backend("the verified Peekaboo CLI build identity is unavailable")
                    .with_operation("runtime.auto")
            })?;
            let socket = socket_dir.join(format!("auto-{}.sock", binary.runtime_identity()));
            start_daemon(binary.path(), &socket, cli_build, "runtime.auto")?;
            Ok(RuntimeBinding::remote(socket))
        }
        RuntimeMode::Process => unreachable!(),
    }
}

fn launch_stable_app(paths: &BackendPaths) -> Result<(), CliError> {
    let args = vec![
        "-g".into(),
        "-n".into(),
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
    Ok(())
}

fn start_daemon(
    binary: &Path,
    socket: &Path,
    expected_build: &str,
    operation: &str,
) -> Result<(), CliError> {
    match probe_runtime_bridge_once(binary, socket, "onDemand", expected_build, operation)? {
        BridgeProbe::Ready => return Ok(()),
        BridgeProbe::Mismatch => {
            return Err(CliError::backend(
                "the active Peekaboo daemon does not match the verified release",
            )
            .with_operation(operation));
        }
        BridgeProbe::Unavailable => {}
    }
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
            CliError::backend("the pinned Peekaboo daemon did not start").with_operation(operation),
        );
    }
    verify_runtime_bridge(binary, socket, "onDemand", expected_build, operation)
}

fn verify_runtime_bridge(
    binary: &Path,
    socket: &Path,
    expected_host: &str,
    expected_build: &str,
    operation: &str,
) -> Result<(), CliError> {
    let started = Instant::now();
    loop {
        if probe_runtime_bridge_once(binary, socket, expected_host, expected_build, operation)?
            == BridgeProbe::Ready
        {
            return Ok(());
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

fn probe_runtime_bridge_once(
    binary: &Path,
    socket: &Path,
    expected_host: &str,
    expected_build: &str,
    operation: &str,
) -> Result<BridgeProbe, CliError> {
    let args = vec![
        "bridge".into(),
        "status".into(),
        "--json".into(),
        "--bridge-socket".into(),
        socket.to_string_lossy().into_owned(),
    ];
    let (envs, removed_envs) = hardened_env(None);
    let output = process::run(
        binary,
        &args,
        &envs,
        &removed_envs,
        None,
        Duration::from_secs(5),
    )
    .map_err(|_| {
        CliError::backend("failed to probe the pinned Peekaboo runtime").with_operation(operation)
    })?;
    if output.exit_code != 0 || output.timed_out || output.stdout_truncated {
        return Ok(BridgeProbe::Unavailable);
    }
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&output.stdout) else {
        return Ok(BridgeProbe::Unavailable);
    };
    if bridge_identity_matches(&value, expected_host, expected_build) {
        Ok(BridgeProbe::Ready)
    } else if value
        .pointer("/data/selected/source")
        .and_then(serde_json::Value::as_str)
        == Some("remote")
    {
        Ok(BridgeProbe::Mismatch)
    } else {
        Ok(BridgeProbe::Unavailable)
    }
}

fn bridge_identity_matches(
    value: &serde_json::Value,
    expected_host: &str,
    expected_build: &str,
) -> bool {
    backend::bridge_handshake_matches(value, expected_host, expected_build)
}

pub(crate) fn retire_obsolete_daemons_at(
    binary: &Path,
    socket_dir: &Path,
    contracts: &[RuntimeContract],
) -> Result<(), CliError> {
    for contract in contracts {
        for prefix in ["daemon", "auto"] {
            let socket = socket_dir.join(format!("{prefix}-{}.sock", contract.identity()));
            if !socket.exists()
                || probe_runtime_bridge_once(
                    binary,
                    &socket,
                    "onDemand",
                    contract.bridge_build(),
                    "runtime.retire",
                )? != BridgeProbe::Ready
            {
                continue;
            }
            let args = vec![
                "daemon".into(),
                "stop".into(),
                "--bridge-socket".into(),
                socket.to_string_lossy().into_owned(),
            ];
            let (envs, removed_envs) = hardened_env(None);
            let output = process::run(
                binary,
                &args,
                &envs,
                &removed_envs,
                None,
                Duration::from_secs(15),
            )
            .map_err(|_| {
                CliError::backend("failed to retire an obsolete Peekaboo daemon")
                    .with_operation("runtime.retire")
            })?;
            if output.exit_code != 0 || output.timed_out {
                return Err(CliError::backend(
                    "an owned obsolete Peekaboo daemon could not be stopped",
                )
                .with_operation("runtime.retire"));
            }
            let started = Instant::now();
            while socket.exists() && started.elapsed() < Duration::from_secs(5) {
                std::thread::sleep(Duration::from_millis(50));
            }
            if socket.exists() {
                return Err(CliError::backend(
                    "an owned obsolete Peekaboo daemon socket remained active",
                )
                .with_operation("runtime.retire"));
            }
        }
    }
    Ok(())
}

pub(crate) fn runtime_socket_dir() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("~"));
    home.join("Library/Application Support/Peekaboo")
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
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    use tempfile::TempDir;

    use super::{
        AutoRuntimeChoice, BridgeProbe, RuntimeBinding, auto_runtime_choice,
        bridge_identity_matches, retire_obsolete_daemons_at,
    };
    use crate::backend::RuntimeContract;
    use crate::cli::RuntimeMode;

    #[test]
    fn every_runtime_mode_has_a_distinct_upstream_selector() {
        let command = vec!["see".into(), "--json".into()];
        let app = RuntimeBinding::for_mode(RuntimeMode::App, "0123456789abcdef").argv(&command);
        let daemon =
            RuntimeBinding::for_mode(RuntimeMode::Daemon, "0123456789abcdef").argv(&command);
        let auto = RuntimeBinding::for_mode(RuntimeMode::Auto, "0123456789abcdef").argv(&command);
        let process =
            RuntimeBinding::for_mode(RuntimeMode::Process, "0123456789abcdef").argv(&command);
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
        assert!(bridge_identity_matches(&current, "gui", "3.9.3 (95)"));

        let mut stale = current.clone();
        stale["data"]["selected"]["handshake"]["build"] = "3.9.3 (91)".into();
        assert!(!bridge_identity_matches(&stale, "gui", "3.9.3 (95)"));

        let mut prefix_collision = current.clone();
        prefix_collision["data"]["selected"]["handshake"]["build"] = "3.9.30 (1)".into();
        assert!(!bridge_identity_matches(
            &prefix_collision,
            "gui",
            "3.9.3 (95)"
        ));

        let mut wrong_host = current.clone();
        wrong_host["data"]["selected"]["handshake"]["hostKind"] = "daemon".into();
        assert!(!bridge_identity_matches(&wrong_host, "gui", "3.9.3 (95)"));

        let mut missing_build = current;
        missing_build["data"]["selected"]["handshake"]
            .as_object_mut()
            .expect("handshake")
            .remove("build");
        assert!(!bridge_identity_matches(
            &missing_build,
            "gui",
            "3.9.3 (95)"
        ));
    }

    #[test]
    fn automatic_runtime_prefers_an_exact_gui_and_falls_back_closed() {
        assert_eq!(
            auto_runtime_choice(BridgeProbe::Ready),
            AutoRuntimeChoice::App
        );
        assert_eq!(
            auto_runtime_choice(BridgeProbe::Unavailable),
            AutoRuntimeChoice::Daemon
        );
        assert_eq!(
            auto_runtime_choice(BridgeProbe::Mismatch),
            AutoRuntimeChoice::Daemon
        );
    }

    #[test]
    fn obsolete_daemon_retirement_proves_ownership_and_preserves_unrelated_sockets() {
        let root = TempDir::new().expect("root");
        let socket_dir = root.path().join("sockets");
        fs::create_dir(&socket_dir).expect("socket directory");
        let old_daemon = socket_dir.join("daemon-0123456789abcdef.sock");
        let old_auto = socket_dir.join("auto-0123456789abcdef.sock");
        let unrelated = socket_dir.join("daemon-unrelated.sock");
        for path in [&old_daemon, &old_auto, &unrelated] {
            fs::write(path, b"fixture").expect("socket fixture");
        }
        let fake = root.path().join("peekaboo");
        fs::write(
            &fake,
            r#"#!/bin/sh
for last do :; done
if [ "$1 $2" = "bridge status" ]; then
  printf '%s\n' '{"success":true,"data":{"selected":{"source":"remote","handshake":{"hostKind":"onDemand","build":"3.9.2 (3.9.2)"}}}}'
  exit 0
fi
if [ "$1 $2" = "daemon stop" ]; then
  rm -- "$last"
  exit 0
fi
exit 64
"#,
        )
        .expect("fake Peekaboo");
        fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).expect("executable fake");

        retire_obsolete_daemons_at(
            &fake,
            &socket_dir,
            &[RuntimeContract::new(
                "0123456789abcdef".into(),
                "3.9.2 (3.9.2)".into(),
            )],
        )
        .expect("retire old runtimes");

        assert!(!old_daemon.exists());
        assert!(!old_auto.exists());
        assert!(unrelated.exists());
    }
}
