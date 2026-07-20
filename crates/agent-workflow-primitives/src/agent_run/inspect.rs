use std::collections::BTreeSet;
use std::env;
use std::ffi::OsString;
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use nils_common::execution_effect::{
    BackendIdentity, OS_ENFORCEMENT_MAX_MEMORY_BYTES, OS_ENFORCEMENT_MAX_OUTPUT_BYTES,
    OS_ENFORCEMENT_MAX_PROCESSES, OS_ENFORCEMENT_MAX_WALL_TIME_MS, OsEnforcement, digest_parts,
    executable_digest,
};

use crate::common::{EXIT_SOFTWARE, EXIT_UNAVAILABLE};

const BWRAP: &str = "/usr/bin/bwrap";
const SYSTEMD_RUN: &str = "/usr/bin/systemd-run";
const SYSTEMCTL: &str = "/usr/bin/systemctl";
const DEFAULT_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug)]
pub(super) struct InspectError {
    pub(super) code: &'static str,
    pub(super) message: String,
    pub(super) exit_code: i32,
}

impl InspectError {
    fn unavailable(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            exit_code: EXIT_UNAVAILABLE,
        }
    }

    fn software(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            exit_code: EXIT_SOFTWARE,
        }
    }
}

#[derive(Debug)]
struct Backend {
    bwrap: PathBuf,
    systemd_run: PathBuf,
    systemctl: PathBuf,
    runtime_dir: PathBuf,
    identity: BackendIdentity,
}

#[derive(Clone, Copy)]
struct RunLimits {
    wall_time: Duration,
    runtime_max: &'static str,
    output_bytes: usize,
}

impl RunLimits {
    fn normal() -> Self {
        Self {
            wall_time: Duration::from_millis(OS_ENFORCEMENT_MAX_WALL_TIME_MS),
            runtime_max: "30s",
            output_bytes: OS_ENFORCEMENT_MAX_OUTPUT_BYTES as usize,
        }
    }

    fn probe() -> Self {
        Self {
            wall_time: PROBE_TIMEOUT,
            runtime_max: "2s",
            output_bytes: 64 * 1024,
        }
    }
}

pub(super) fn run(cwd: &Path, argv: &[OsString]) -> Result<i32, InspectError> {
    let backend = Backend::discover()?;
    let output = backend.execute(cwd, argv, RunLimits::normal())?;
    std::io::stdout()
        .write_all(&output.stdout)
        .map_err(|error| InspectError::software("sandbox-output-failed", error.to_string()))?;
    std::io::stderr()
        .write_all(&output.stderr)
        .map_err(|error| InspectError::software("sandbox-output-failed", error.to_string()))?;
    Ok(output.status.code().unwrap_or(EXIT_SOFTWARE))
}

pub(super) fn probe_enforcement(cwd: &Path) -> Result<OsEnforcement, InspectError> {
    let backend = Backend::discover()?;
    let output = backend.execute(cwd, &[OsString::from("/usr/bin/true")], RunLimits::probe())?;
    if !output.status.success() || !output.stdout.is_empty() || !output.stderr.is_empty() {
        return Err(InspectError::unavailable(
            "sandbox-backend-unavailable",
            "Linux sandbox backend self-test did not return a clean success",
        ));
    }
    Ok(OsEnforcement::strict_v1(backend.identity))
}

impl Backend {
    fn discover() -> Result<Self, InspectError> {
        let bwrap = trusted_system_executable(BWRAP)?;
        let systemd_run = trusted_system_executable(SYSTEMD_RUN)?;
        let systemctl = trusted_system_executable(SYSTEMCTL)?;
        let runtime_dir = PathBuf::from(format!("/run/user/{}", unsafe { libc::geteuid() }));
        let runtime_metadata = fs::metadata(&runtime_dir).map_err(|_| {
            InspectError::unavailable(
                "sandbox-backend-unavailable",
                "the systemd user runtime directory is unavailable",
            )
        })?;
        if !runtime_metadata.is_dir()
            || runtime_metadata.uid() != unsafe { libc::geteuid() }
            || runtime_metadata.permissions().mode() & 0o077 != 0
            || !runtime_dir.join("bus").exists()
        {
            return Err(InspectError::unavailable(
                "sandbox-backend-untrusted",
                "the systemd user runtime directory has an unsafe identity or mode",
            ));
        }
        let bwrap_release = first_version_line(&bwrap, &["--version"])?;
        let systemd_release = first_version_line(&systemd_run, &["--version"])?;
        let bwrap_digest = executable_digest(&bwrap)
            .map_err(|error| InspectError::unavailable("sandbox-backend-untrusted", error))?;
        let systemd_run_digest = executable_digest(&systemd_run)
            .map_err(|error| InspectError::unavailable("sandbox-backend-untrusted", error))?;
        let systemctl_digest = executable_digest(&systemctl)
            .map_err(|error| InspectError::unavailable("sandbox-backend-untrusted", error))?;
        let executable_digest = digest_parts([
            bwrap_digest.as_bytes(),
            systemd_run_digest.as_bytes(),
            systemctl_digest.as_bytes(),
        ]);
        Ok(Self {
            bwrap,
            systemd_run,
            systemctl,
            runtime_dir,
            identity: BackendIdentity {
                kind: "linux.bubblewrap-systemd.v1".to_string(),
                release: format!("{bwrap_release}; {systemd_release}"),
                executable_digest,
            },
        })
    }

    fn execute(
        &self,
        cwd: &Path,
        argv: &[OsString],
        limits: RunLimits,
    ) -> Result<std::process::Output, InspectError> {
        let unit = unique_unit()?;
        let mut command = self.systemd_command(&unit, limits);
        command.arg(&self.bwrap);
        append_bwrap_args(&mut command, cwd, argv);
        let output = run_bounded(command, limits, || self.stop_unit(&unit))?;
        self.ensure_unit_inactive(&unit)?;
        Ok(output)
    }

    fn systemd_command(&self, unit: &str, limits: RunLimits) -> Command {
        let mut command = Command::new(&self.systemd_run);
        command
            .args(["--user", "--quiet", "--wait", "--collect", "--pipe"])
            .arg(format!("--unit={unit}"))
            .args(["-p", &format!("TasksMax={OS_ENFORCEMENT_MAX_PROCESSES}")])
            .args([
                "-p",
                &format!("MemoryMax={OS_ENFORCEMENT_MAX_MEMORY_BYTES}"),
            ])
            .args(["-p", &format!("RuntimeMaxSec={}", limits.runtime_max)])
            .args(["-p", "KillMode=control-group"])
            .args(["-p", "TimeoutStopSec=1s"])
            .args(["-p", "UMask=0077"])
            .env_clear()
            .env("XDG_RUNTIME_DIR", &self.runtime_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        unsafe {
            command.pre_exec(|| {
                let result = libc::syscall(libc::SYS_close_range, 3_u32, u32::MAX, 1_u32 << 2);
                if result == 0 {
                    Ok(())
                } else {
                    Err(std::io::Error::last_os_error())
                }
            });
        }
        command
    }

    fn stop_unit(&self, unit: &str) -> Result<(), InspectError> {
        self.systemctl_command()
            .args(["stop", unit])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|error| InspectError::software("sandbox-cleanup-failed", error.to_string()))?;
        self.ensure_unit_inactive(unit)
    }

    fn ensure_unit_inactive(&self, unit: &str) -> Result<(), InspectError> {
        let deadline = Instant::now() + CLEANUP_TIMEOUT;
        loop {
            let output = self
                .systemctl_command()
                .args(["is-active", unit])
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .output()
                .map_err(|error| {
                    InspectError::software("sandbox-cleanup-failed", error.to_string())
                })?;
            let state = String::from_utf8_lossy(&output.stdout);
            if matches!(output.status.code(), Some(3 | 4))
                || matches!(state.trim(), "inactive" | "failed" | "unknown")
            {
                return Ok(());
            }
            if !output.status.success()
                && !matches!(state.trim(), "active" | "activating" | "deactivating")
            {
                return Err(InspectError::software(
                    "sandbox-cleanup-failed",
                    "sandbox service state could not be verified",
                ));
            }
            if Instant::now() >= deadline {
                return Err(InspectError::software(
                    "sandbox-cleanup-failed",
                    "sandbox service remained active after cleanup",
                ));
            }
            thread::sleep(Duration::from_millis(20));
        }
    }

    fn systemctl_command(&self) -> Command {
        let mut command = Command::new(&self.systemctl);
        command
            .arg("--user")
            .env_clear()
            .env("XDG_RUNTIME_DIR", &self.runtime_dir)
            .stdin(Stdio::null());
        command
    }
}

fn append_bwrap_args(command: &mut Command, cwd: &Path, argv: &[OsString]) {
    command.args([
        "--unshare-all",
        "--unshare-user",
        "--disable-userns",
        "--assert-userns-disabled",
        "--die-with-parent",
        "--new-session",
        "--cap-drop",
        "ALL",
        "--ro-bind",
        "/",
        "/",
        "--tmpfs",
        "/run",
        "--proc",
        "/proc",
        "--dev",
        "/dev",
        "--clearenv",
        "--setenv",
        "PATH",
    ]);
    command.arg(sanitized_path());
    for (name, value) in [
        ("HOME", "/run/agent-scratch/home"),
        ("TMPDIR", "/run/agent-scratch/tmp"),
        ("XDG_CACHE_HOME", "/run/agent-scratch/xdg-cache"),
        ("XDG_CONFIG_HOME", "/run/agent-scratch/xdg-config"),
        ("XDG_DATA_HOME", "/run/agent-scratch/xdg-data"),
        ("XDG_RUNTIME_DIR", "/run/agent-scratch/xdg-runtime"),
        ("XDG_STATE_HOME", "/run/agent-scratch/xdg-state"),
        ("LANG", "C.UTF-8"),
    ] {
        command.args(["--setenv", name, value]);
    }
    for path in [
        "/run/agent-scratch",
        "/run/agent-scratch/home",
        "/run/agent-scratch/tmp",
        "/run/agent-scratch/xdg-cache",
        "/run/agent-scratch/xdg-config",
        "/run/agent-scratch/xdg-data",
        "/run/agent-scratch/xdg-runtime",
        "/run/agent-scratch/xdg-state",
    ] {
        command.args(["--dir", path, "--chmod", "0700", path]);
    }
    command.arg("--chdir").arg(cwd).arg("--").args(argv);
}

fn trusted_system_executable(path: &str) -> Result<PathBuf, InspectError> {
    let expected = PathBuf::from(path);
    let canonical = fs::canonicalize(&expected).map_err(|_| {
        InspectError::unavailable(
            "sandbox-backend-unavailable",
            format!("required Linux sandbox component is unavailable: {path}"),
        )
    })?;
    if canonical != expected {
        return Err(InspectError::unavailable(
            "sandbox-backend-untrusted",
            format!("Linux sandbox component is not canonical: {path}"),
        ));
    }
    for candidate in [
        Path::new("/usr"),
        Path::new("/usr/bin"),
        canonical.as_path(),
    ] {
        let metadata = fs::symlink_metadata(candidate).map_err(|_| {
            InspectError::unavailable(
                "sandbox-backend-untrusted",
                format!("Linux sandbox component metadata is unavailable: {path}"),
            )
        })?;
        if metadata.file_type().is_symlink()
            || metadata.uid() != 0
            || metadata.permissions().mode() & 0o022 != 0
        {
            return Err(InspectError::unavailable(
                "sandbox-backend-untrusted",
                format!("Linux sandbox component ownership or mode is unsafe: {path}"),
            ));
        }
    }
    let metadata = fs::metadata(&canonical).map_err(|_| {
        InspectError::unavailable("sandbox-backend-untrusted", "backend metadata unavailable")
    })?;
    if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
        return Err(InspectError::unavailable(
            "sandbox-backend-untrusted",
            format!("Linux sandbox component is not executable: {path}"),
        ));
    }
    Ok(canonical)
}

fn first_version_line(path: &Path, args: &[&str]) -> Result<String, InspectError> {
    let output = Command::new(path)
        .args(args)
        .env_clear()
        .output()
        .map_err(|error| {
            InspectError::unavailable("sandbox-backend-unavailable", error.to_string())
        })?;
    let line = String::from_utf8_lossy(&output.stdout)
        .lines()
        .find(|line| !line.trim().is_empty())
        .map(str::trim)
        .unwrap_or_default()
        .to_string();
    if !output.status.success() || line.is_empty() || line.len() > 96 {
        return Err(InspectError::unavailable(
            "sandbox-backend-untrusted",
            "Linux sandbox component version is unavailable",
        ));
    }
    Ok(line)
}

fn sanitized_path() -> OsString {
    let mut seen = BTreeSet::new();
    let paths = env::var_os("PATH")
        .into_iter()
        .flat_map(|value| env::split_paths(&value).collect::<Vec<_>>())
        .filter(|path| path.is_absolute() && path.is_dir())
        .filter(|path| seen.insert(path.clone()))
        .collect::<Vec<_>>();
    env::join_paths(paths).unwrap_or_else(|_| OsString::from(DEFAULT_PATH))
}

fn unique_unit() -> Result<String, InspectError> {
    let mut random = [0_u8; 12];
    getrandom::fill(&mut random).map_err(|error| {
        InspectError::unavailable("sandbox-randomness-unavailable", error.to_string())
    })?;
    let suffix = random
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Ok(format!("agent-run-inspect-{suffix}.service"))
}

fn run_bounded(
    mut command: Command,
    limits: RunLimits,
    cleanup: impl FnOnce() -> Result<(), InspectError>,
) -> Result<std::process::Output, InspectError> {
    let mut child = command.spawn().map_err(|error| {
        InspectError::unavailable("sandbox-backend-unavailable", error.to_string())
    })?;
    let total = Arc::new(AtomicUsize::new(0));
    let overflowed = Arc::new(AtomicBool::new(false));
    let stdout = child.stdout.take().map(|pipe| {
        let total = Arc::clone(&total);
        let overflowed = Arc::clone(&overflowed);
        thread::spawn(move || read_capped(pipe, limits.output_bytes, total, overflowed))
    });
    let stderr = child.stderr.take().map(|pipe| {
        let total = Arc::clone(&total);
        let overflowed = Arc::clone(&overflowed);
        thread::spawn(move || read_capped(pipe, limits.output_bytes, total, overflowed))
    });
    let deadline = Instant::now() + limits.wall_time + Duration::from_secs(1);
    let status = loop {
        if overflowed.load(Ordering::Acquire) {
            terminate_process_group(&mut child);
            cleanup()?;
            let _ = join_output(stdout);
            let _ = join_output(stderr);
            return Err(InspectError::software(
                "sandbox-output-limit-exceeded",
                "sandbox output exceeded the fixed 8 MiB limit",
            ));
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(5)),
            Ok(None) => {
                terminate_process_group(&mut child);
                cleanup()?;
                let _ = join_output(stdout);
                let _ = join_output(stderr);
                return Err(InspectError::software(
                    "sandbox-time-limit-exceeded",
                    "sandbox execution exceeded the fixed wall-time limit",
                ));
            }
            Err(error) => {
                terminate_process_group(&mut child);
                cleanup()?;
                let _ = join_output(stdout);
                let _ = join_output(stderr);
                return Err(InspectError::software(
                    "sandbox-wait-failed",
                    error.to_string(),
                ));
            }
        }
    };
    let stdout = join_output(stdout)?;
    let stderr = join_output(stderr)?;
    if overflowed.load(Ordering::Acquire) {
        cleanup()?;
        return Err(InspectError::software(
            "sandbox-output-limit-exceeded",
            "sandbox output exceeded the fixed 8 MiB limit",
        ));
    }
    Ok(std::process::Output {
        status,
        stdout,
        stderr,
    })
}

fn read_capped(
    mut pipe: impl Read,
    limit: usize,
    total: Arc<AtomicUsize>,
    overflowed: Arc<AtomicBool>,
) -> Result<Vec<u8>, std::io::Error> {
    let mut retained = Vec::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let read = pipe.read(&mut buffer)?;
        if read == 0 {
            return Ok(retained);
        }
        let before = total.fetch_add(read, Ordering::AcqRel);
        if before.saturating_add(read) > limit {
            overflowed.store(true, Ordering::Release);
        }
        if before < limit {
            retained.extend_from_slice(&buffer[..read.min(limit - before)]);
        }
    }
}

fn join_output(
    handle: Option<thread::JoinHandle<Result<Vec<u8>, std::io::Error>>>,
) -> Result<Vec<u8>, InspectError> {
    match handle {
        Some(handle) => handle
            .join()
            .map_err(|_| InspectError::software("sandbox-output-failed", "output reader panicked"))?
            .map_err(|error| InspectError::software("sandbox-output-failed", error.to_string())),
        None => Ok(Vec::new()),
    }
}

fn terminate_process_group(child: &mut std::process::Child) {
    unsafe {
        libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL);
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_and_untrusted_backend_components_fail_closed() {
        let missing = trusted_system_executable("/usr/bin/agent-run-missing-backend")
            .expect_err("missing component must fail");
        assert_eq!(missing.code, "sandbox-backend-unavailable");
        assert_eq!(missing.exit_code, EXIT_UNAVAILABLE);

        let temp = tempfile::TempDir::new().expect("tempdir");
        let executable = temp.path().join("component");
        fs::write(&executable, b"component").expect("component");
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).expect("mode");
        let untrusted = trusted_system_executable(executable.to_str().expect("UTF-8 path"))
            .expect_err("user-owned component must fail");
        assert_eq!(untrusted.code, "sandbox-backend-untrusted");
        assert_eq!(untrusted.exit_code, EXIT_UNAVAILABLE);
    }
}
