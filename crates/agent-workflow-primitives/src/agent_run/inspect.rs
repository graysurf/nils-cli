use std::collections::BTreeSet;
use std::env;
use std::ffi::{CString, OsString};
use std::fs;
use std::io::{Read, Write};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
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
const INSPECT_CHILD_ARG: &str = "__inspect-child";
const MOUNTINFO: &str = "/proc/self/mountinfo";

const BPF_LD_W_ABS: u16 = 0x20;
const BPF_JMP_JEQ_K: u16 = 0x15;
const BPF_JMP_JSET_K: u16 = 0x45;
const BPF_RET_K: u16 = 0x06;
const SECCOMP_DATA_NR_OFFSET: u32 = 0;
const SECCOMP_DATA_ARCH_OFFSET: u32 = 4;
const SECCOMP_RET_KILL_PROCESS: u32 = 0x8000_0000;
const SECCOMP_RET_ERRNO: u32 = 0x0005_0000;
const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;
const X32_SYSCALL_BIT: u32 = 0x4000_0000;

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

pub(super) fn run_child(argv: &[OsString]) -> i32 {
    if argv.is_empty() {
        eprintln!("agent-run inspect: error[missing-command]: sandbox command is unavailable");
        return EXIT_SOFTWARE;
    }
    if let Err(error) = audit_durable_mounts_read_only() {
        eprintln!(
            "agent-run inspect: error[{}]: {}",
            error.code, error.message
        );
        return error.exit_code;
    }
    if let Err(error) = install_network_seccomp() {
        eprintln!(
            "agent-run inspect: error[{}]: {}",
            error.code, error.message
        );
        return error.exit_code;
    }
    let error = Command::new(&argv[0]).args(&argv[1..]).exec();
    eprintln!("agent-run inspect: error[sandbox-exec-failed]: {error}");
    EXIT_SOFTWARE
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
        let launcher = fs::canonicalize(env::current_exe().map_err(|error| {
            InspectError::unavailable("sandbox-launcher-unavailable", error.to_string())
        })?)
        .map_err(|error| {
            InspectError::unavailable("sandbox-launcher-unavailable", error.to_string())
        })?;
        let mount_plan = inherited_mount_plan()?;
        let mut command = self.systemd_command(&unit, limits);
        command.arg(&self.bwrap);
        append_bwrap_args(&mut command, cwd, argv, &launcher, &mount_plan);
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

fn append_bwrap_args(
    command: &mut Command,
    cwd: &Path,
    argv: &[OsString],
    launcher: &Path,
    mount_plan: &MountPlan,
) {
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
    ]);
    for path in &mount_plan.masks {
        command
            .arg("--tmpfs")
            .arg(path)
            .arg("--remount-ro")
            .arg(path);
    }
    for path in &mount_plan.remounts {
        command.arg("--remount-ro").arg(path);
    }
    command.args([
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
    command
        .arg("--chdir")
        .arg(cwd)
        .arg("--")
        .arg(launcher)
        .arg(INSPECT_CHILD_ARG)
        .args(argv);
}

#[derive(Debug)]
struct MountRecord {
    mount_id: u64,
    mountpoint: PathBuf,
    writable: bool,
}

#[derive(Debug)]
struct MountPlan {
    masks: Vec<PathBuf>,
    remounts: Vec<PathBuf>,
}

enum MountReachability {
    Reachable,
    Mask(PathBuf),
    Absent,
}

fn inherited_mount_plan() -> Result<MountPlan, InspectError> {
    let mut mask_candidates = BTreeSet::new();
    let mut remount_candidates = BTreeSet::new();
    for record in parse_mountinfo()?
        .into_iter()
        .filter(|record| record.writable && !is_private_mount(&record.mountpoint))
    {
        match mount_reachability(&record.mountpoint)? {
            MountReachability::Reachable => {
                remount_candidates.insert(record.mountpoint);
            }
            MountReachability::Mask(path) => {
                mask_candidates.insert(path);
            }
            MountReachability::Absent => {}
        }
    }
    let masks = mask_candidates
        .iter()
        .filter(|candidate| {
            !mask_candidates
                .iter()
                .any(|other| other != *candidate && candidate.starts_with(other))
        })
        .cloned()
        .collect::<Vec<_>>();
    let remounts = remount_candidates
        .into_iter()
        .filter(|candidate| !masks.iter().any(|mask| candidate.starts_with(mask)))
        .collect();
    Ok(MountPlan { masks, remounts })
}

fn mount_reachability(path: &Path) -> Result<MountReachability, InspectError> {
    let mut current = PathBuf::from("/");
    for component in path.components() {
        let std::path::Component::Normal(component) = component else {
            continue;
        };
        current.push(component);
        match fs::metadata(&current) {
            Ok(metadata) if metadata.is_dir() => match directory_searchable(&current)? {
                true => {}
                false => return Ok(MountReachability::Mask(current)),
            },
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                return Ok(MountReachability::Mask(current));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(MountReachability::Absent);
            }
            Err(error) => {
                return Err(InspectError::unavailable(
                    "sandbox-mount-audit-failed",
                    format!("{}: {error}", current.display()),
                ));
            }
        }
    }
    Ok(MountReachability::Reachable)
}

fn directory_searchable(path: &Path) -> Result<bool, InspectError> {
    let path = CString::new(path.as_os_str().as_bytes()).map_err(|_| {
        InspectError::unavailable(
            "sandbox-mount-audit-failed",
            "sandbox mountpoint contained an interior NUL",
        )
    })?;
    let result = unsafe { libc::access(path.as_ptr(), libc::X_OK) };
    if result == 0 {
        return Ok(true);
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::EACCES) {
        Ok(false)
    } else {
        Err(InspectError::unavailable(
            "sandbox-mount-audit-failed",
            error.to_string(),
        ))
    }
}

fn audit_durable_mounts_read_only() -> Result<(), InspectError> {
    let mut writable = Vec::new();
    for record in parse_mountinfo()?
        .into_iter()
        .filter(|record| record.writable && !is_private_mount(&record.mountpoint))
    {
        if visible_mount_id(&record.mountpoint)? == Some(record.mount_id) {
            writable.push(record.mountpoint);
        }
    }
    if writable.is_empty() {
        Ok(())
    } else {
        Err(InspectError::unavailable(
            "sandbox-mount-audit-failed",
            format!(
                "{} durable sandbox mounts remained writable: {}",
                writable.len(),
                writable
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ))
    }
}

fn visible_mount_id(path: &Path) -> Result<Option<u64>, InspectError> {
    let path = CString::new(path.as_os_str().as_bytes()).map_err(|_| {
        InspectError::unavailable(
            "sandbox-mount-audit-failed",
            "sandbox mountpoint contained an interior NUL",
        )
    })?;
    let mut status = std::mem::MaybeUninit::<libc::statx>::zeroed();
    let result = unsafe {
        libc::statx(
            libc::AT_FDCWD,
            path.as_ptr(),
            0,
            libc::STATX_MNT_ID,
            status.as_mut_ptr(),
        )
    };
    if result == 0 {
        let status = unsafe { status.assume_init() };
        if status.stx_mask & libc::STATX_MNT_ID == 0 {
            return Err(InspectError::unavailable(
                "sandbox-mount-audit-failed",
                "statx did not report the visible mount identity",
            ));
        }
        return Ok(Some(status.stx_mnt_id));
    }
    let error = std::io::Error::last_os_error();
    if matches!(
        error.raw_os_error(),
        Some(libc::EACCES) | Some(libc::ENOENT) | Some(libc::ENOTDIR)
    ) {
        Ok(None)
    } else {
        Err(InspectError::unavailable(
            "sandbox-mount-audit-failed",
            error.to_string(),
        ))
    }
}

fn parse_mountinfo() -> Result<Vec<MountRecord>, InspectError> {
    let bytes = fs::read(MOUNTINFO).map_err(|error| {
        InspectError::unavailable("sandbox-mount-audit-failed", error.to_string())
    })?;
    bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| {
            let fields = line.split(|byte| *byte == b' ').take(6).collect::<Vec<_>>();
            if fields.len() != 6 {
                return Err(InspectError::unavailable(
                    "sandbox-mount-audit-failed",
                    "mountinfo contained an incomplete record",
                ));
            }
            let mount_id = std::str::from_utf8(fields[0])
                .ok()
                .and_then(|value| value.parse::<u64>().ok())
                .ok_or_else(|| {
                    InspectError::unavailable(
                        "sandbox-mount-audit-failed",
                        "mountinfo contained an invalid mount identity",
                    )
                })?;
            let mountpoint = decode_mountinfo_field(fields[4])?;
            if !mountpoint.is_absolute() {
                return Err(InspectError::unavailable(
                    "sandbox-mount-audit-failed",
                    "mountinfo contained a non-absolute mountpoint",
                ));
            }
            let writable = fields[5]
                .split(|byte| *byte == b',')
                .any(|option| option == b"rw");
            Ok(MountRecord {
                mount_id,
                mountpoint,
                writable,
            })
        })
        .collect()
}

fn decode_mountinfo_field(field: &[u8]) -> Result<PathBuf, InspectError> {
    let mut decoded = Vec::with_capacity(field.len());
    let mut index = 0;
    while index < field.len() {
        if field[index] == b'\\' {
            if index + 3 >= field.len()
                || !field[index + 1..=index + 3]
                    .iter()
                    .all(|byte| matches!(byte, b'0'..=b'7'))
            {
                return Err(InspectError::unavailable(
                    "sandbox-mount-audit-failed",
                    "mountinfo contained an invalid escaped mountpoint",
                ));
            }
            let value = (field[index + 1] - b'0') * 64
                + (field[index + 2] - b'0') * 8
                + (field[index + 3] - b'0');
            decoded.push(value);
            index += 4;
        } else {
            decoded.push(field[index]);
            index += 1;
        }
    }
    Ok(PathBuf::from(OsString::from_vec(decoded)))
}

fn is_private_mount(path: &Path) -> bool {
    [Path::new("/run"), Path::new("/proc"), Path::new("/dev")]
        .iter()
        .any(|root| path == *root || path.starts_with(root))
}

fn install_network_seccomp() -> Result<(), InspectError> {
    let audit_arch = audit_arch().ok_or_else(|| {
        InspectError::unavailable(
            "sandbox-seccomp-unavailable",
            "the Linux architecture has no inspection seccomp contract",
        )
    })?;
    let mut filter = vec![
        bpf_stmt(BPF_LD_W_ABS, SECCOMP_DATA_ARCH_OFFSET),
        bpf_jump(BPF_JMP_JEQ_K, audit_arch, 1, 0),
        bpf_stmt(BPF_RET_K, SECCOMP_RET_KILL_PROCESS),
        bpf_stmt(BPF_LD_W_ABS, SECCOMP_DATA_NR_OFFSET),
    ];
    if cfg!(target_arch = "x86_64") {
        filter.extend([
            bpf_jump(BPF_JMP_JSET_K, X32_SYSCALL_BIT, 0, 1),
            bpf_stmt(BPF_RET_K, SECCOMP_RET_KILL_PROCESS),
        ]);
    }
    for syscall in [
        libc::SYS_socket,
        libc::SYS_socketpair,
        libc::SYS_io_uring_setup,
    ] {
        filter.extend([
            bpf_jump(BPF_JMP_JEQ_K, syscall as u32, 0, 1),
            bpf_stmt(BPF_RET_K, SECCOMP_RET_ERRNO | libc::EPERM as u32),
        ]);
    }
    filter.push(bpf_stmt(BPF_RET_K, SECCOMP_RET_ALLOW));
    let mut program = libc::sock_fprog {
        len: u16::try_from(filter.len()).map_err(|_| {
            InspectError::unavailable(
                "sandbox-seccomp-unavailable",
                "the inspection seccomp filter is too large",
            )
        })?,
        filter: filter.as_mut_ptr(),
    };
    let no_new_privileges = unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) };
    if no_new_privileges != 0 {
        return Err(InspectError::unavailable(
            "sandbox-seccomp-unavailable",
            std::io::Error::last_os_error().to_string(),
        ));
    }
    let installed = unsafe {
        libc::syscall(
            libc::SYS_seccomp,
            libc::SECCOMP_SET_MODE_FILTER,
            0,
            &mut program as *mut libc::sock_fprog,
        )
    };
    if installed == 0 {
        Ok(())
    } else {
        Err(InspectError::unavailable(
            "sandbox-seccomp-unavailable",
            std::io::Error::last_os_error().to_string(),
        ))
    }
}

fn bpf_stmt(code: u16, value: u32) -> libc::sock_filter {
    libc::sock_filter {
        code,
        jt: 0,
        jf: 0,
        k: value,
    }
}

fn bpf_jump(code: u16, value: u32, jt: u8, jf: u8) -> libc::sock_filter {
    libc::sock_filter {
        code,
        jt,
        jf,
        k: value,
    }
}

#[cfg(target_arch = "x86_64")]
fn audit_arch() -> Option<u32> {
    Some(0xc000_003e)
}

#[cfg(target_arch = "aarch64")]
fn audit_arch() -> Option<u32> {
    Some(0xc000_00b7)
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
fn audit_arch() -> Option<u32> {
    None
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
