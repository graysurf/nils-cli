use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{BufRead, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Component, Path, PathBuf};
use std::process::{Child, ExitStatus};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use base64::Engine;
use nils_common::fs::{SECRET_FILE_MODE, sha256_file, write_atomic};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::backend;
use crate::cli::{
    EvidenceMode, ExecArgs, McpArgs, OutputFormat, RuntimeMode, ScenarioArgs, ToolProfile,
};
use crate::commands;
use crate::error::{CliError, ErrorClass};
use crate::journal::ArtifactIndex;
use crate::lock::PeekabooLock;
use crate::model::SuccessEnvelope;
use crate::{process, test_mode};

const PROTOCOL_SCHEMA: &str = "macos-agent.remote.v2";
const MAX_REQUEST_BYTES: u64 = 8 * 1024 * 1024;
const MAX_TRANSFER_FILES: usize = 256;
const MAX_TRANSFER_BYTES: usize = 8 * 1024 * 1024;
const MAX_MCP_TERMINAL_BYTES: usize = 64 * 1024;
const MCP_TERMINAL_PREFIX: &str = "NILS_MACOS_AGENT_MCP_TERMINAL=";
static TOKEN_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum BackendAction {
    Install,
    Status,
    Verify,
    Rollback,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RemoteCommand {
    Backend {
        action: BackendAction,
        dry_run: bool,
        strict: bool,
    },
    Doctor {
        strict: bool,
    },
    Capabilities {
        strict: bool,
    },
    Exec {
        argv: Vec<String>,
        intent: Option<String>,
        expected: Option<String>,
        evidence_mode: EvidenceMode,
        runtime: RuntimeMode,
        timeout_seconds: u64,
    },
    Scenario {
        source_sha256: String,
        source_base64: String,
        evidence_mode: EvidenceMode,
        runtime: RuntimeMode,
        timeout_seconds: u64,
    },
    Collect,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RemoteRequest {
    schema_version: String,
    adapter_version: String,
    peekaboo_commit: String,
    token: String,
    command: RemoteCommand,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RemoteResponse {
    schema_version: String,
    adapter_version: String,
    token: String,
    ok: bool,
    exit_code: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<RemoteError>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    artifacts: Vec<TransferredFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RemoteError {
    class: ErrorClass,
    message: String,
    operation: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TransferredFile {
    relative_path: String,
    sha256: String,
    data_base64: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RemoteMcpControl {
    schema_version: String,
    adapter_version: String,
    peekaboo_commit: String,
    token: String,
    runtime: RuntimeMode,
    tool_profile: ToolProfile,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RemoteMcpTerminal {
    schema_version: String,
    adapter_version: String,
    token: String,
    ok: bool,
    exit_code: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<RemoteError>,
}

pub fn run_remote_control(
    host: &str,
    command: RemoteCommand,
    format: OutputFormat,
    label: &'static str,
) -> Result<u8, CliError> {
    let timeout = match &command {
        RemoteCommand::Backend {
            action: BackendAction::Install,
            ..
        } => Duration::from_secs(900),
        RemoteCommand::Backend { .. }
        | RemoteCommand::Doctor { .. }
        | RemoteCommand::Capabilities { .. } => Duration::from_secs(180),
        RemoteCommand::Exec {
            timeout_seconds, ..
        }
        | RemoteCommand::Scenario {
            timeout_seconds, ..
        } => Duration::from_secs((*timeout_seconds).clamp(1, 3600) + 30),
        RemoteCommand::Collect => Duration::from_secs(30),
    };
    let response = request(host, command, timeout)?;
    emit_remote(format, label, response)
}

pub fn run_remote_exec(args: &ExecArgs, format: OutputFormat) -> Result<u8, CliError> {
    let host = args
        .host
        .as_deref()
        .ok_or_else(|| transport_error("SSH host is required"))?;
    let response = request(
        host,
        RemoteCommand::Exec {
            argv: args.argv.clone(),
            intent: args.intent.clone(),
            expected: args.expected.clone(),
            evidence_mode: args.evidence_mode,
            runtime: args.runtime,
            timeout_seconds: args.timeout_seconds,
        },
        Duration::from_secs(args.timeout_seconds.clamp(1, 3600) + 30),
    )?;
    install_transferred_files(&args.out_dir, &response.artifacts)?;
    emit_remote(format, "exec", response)
}

pub fn run_remote_scenario(args: &ScenarioArgs, format: OutputFormat) -> Result<u8, CliError> {
    let host = args
        .host
        .as_deref()
        .ok_or_else(|| transport_error("SSH host is required"))?;
    let source = commands::scenario::validate_source(args)?;
    crate::policy::validate_scenario(
        &serde_json::from_slice(&source)
            .map_err(|_| CliError::usage("scenario is not valid JSON"))?,
    )?;
    let source_sha256 = hex(&Sha256::digest(&source));
    let response = request(
        host,
        RemoteCommand::Scenario {
            source_sha256,
            source_base64: base64::engine::general_purpose::STANDARD.encode(source),
            evidence_mode: args.evidence_mode,
            runtime: args.runtime,
            timeout_seconds: args.timeout_seconds,
        },
        Duration::from_secs(args.timeout_seconds.clamp(1, 3600) + 30),
    )?;
    install_transferred_files(&args.out_dir, &response.artifacts)?;
    emit_remote(format, "scenario", response)
}

pub fn run_remote_mcp(args: &McpArgs) -> Result<u8, CliError> {
    let host = args
        .host
        .as_deref()
        .ok_or_else(|| transport_error("SSH host is required"))?;
    validate_host(host)?;
    let lock = PeekabooLock::embedded()?;
    let token = session_token();
    let control = RemoteMcpControl {
        schema_version: PROTOCOL_SCHEMA.into(),
        adapter_version: env!("CARGO_PKG_VERSION").into(),
        peekaboo_commit: lock.commit,
        token: token.clone(),
        runtime: args.runtime,
        tool_profile: args.tool_profile,
    };
    let mut ssh = std::process::Command::new(ssh_program());
    ssh.args(ssh_args(
        host,
        &["macos-agent", "__remote-mcp", "--token", &token],
    ))
    .stdin(std::process::Stdio::piped())
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped());
    // SAFETY: `setpgid(0, 0)` is async-signal-safe and confines SSH plus any
    // transport helpers to one process group for deterministic cleanup.
    unsafe {
        ssh.pre_exec(|| {
            if libc::setpgid(0, 0) == 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        });
    }
    let child = ssh
        .spawn()
        .map_err(|_| transport_error("failed to start SSH MCP transport"))?;
    let mut process = ProcessGroupChild::new(child);
    let mut child_in = process
        .child
        .stdin
        .take()
        .ok_or_else(|| transport_error("SSH MCP stdin is unavailable"))?;
    let control = serde_json::to_vec(&control)
        .map_err(|_| transport_error("failed to encode SSH MCP control frame"))?;
    child_in
        .write_all(&control)
        .and_then(|_| child_in.write_all(b"\n"))
        .and_then(|_| child_in.flush())
        .map_err(|_| transport_error("failed to send SSH MCP control frame"))?;
    let upstream_out = process
        .child
        .stdout
        .take()
        .ok_or_else(|| transport_error("SSH MCP stdout is unavailable"))?;
    let upstream_err = process
        .child
        .stderr
        .take()
        .ok_or_else(|| transport_error("SSH MCP stderr is unavailable"))?;
    let _input_thread = thread::spawn(move || {
        let mut stdin = std::io::stdin().lock();
        let copied = std::io::copy(&mut stdin, &mut child_in);
        let _ = child_in.flush();
        copied
    });
    let error_thread = thread::spawn(move || read_capped(upstream_err, MAX_MCP_TERMINAL_BYTES));
    let mut output = std::io::stdout().lock();
    let copy_result = std::io::copy(&mut { upstream_out }, &mut output);
    let _ = output.flush();
    if copy_result.is_err() {
        process.terminate_and_reap();
        let _ = error_thread.join();
        let _ = cleanup(host, &token);
        return Err(transport_error("SSH MCP output transport failed"));
    }
    let status = match process.wait_bounded(test_mode::ssh_mcp_exit_timeout()) {
        Ok(status) => status,
        Err(error) => {
            let _ = error_thread.join();
            return match cleanup(host, &token) {
                Ok(()) => Err(error),
                Err(_) => Err(transport_error(
                    "SSH MCP transport did not exit cleanly and remote cleanup could not be confirmed",
                )),
            };
        }
    };
    let (terminal_stderr, terminal_truncated) = error_thread
        .join()
        .map_err(|_| transport_error("SSH MCP terminal-status reader failed"))?;
    let terminal = if status.success() && !terminal_truncated {
        parse_mcp_terminal(&terminal_stderr, &token).ok()
    } else {
        None
    };

    let collected = request_with_token(
        host,
        token.clone(),
        RemoteCommand::Collect,
        Duration::from_secs(30),
    );
    let collect_result = collected.and_then(|response| {
        install_transferred_files(&args.out_dir, &response.artifacts)?;
        if response.ok {
            Ok(())
        } else {
            Err(response_error(response))
        }
    });
    let Some(terminal) = terminal else {
        let _ = cleanup(host, &token);
        return Err(transport_error(
            "SSH MCP transport ended without a valid terminal status",
        ));
    };
    collect_result?;
    if !terminal.ok {
        return Err(terminal
            .error
            .map(remote_error_to_cli)
            .unwrap_or_else(|| transport_error("remote MCP failed without a typed error")));
    }
    Ok(0)
}

struct ProcessGroupChild {
    child: Child,
    pgid: i32,
    reaped: bool,
}

impl ProcessGroupChild {
    fn new(child: Child) -> Self {
        Self {
            pgid: child.id() as i32,
            child,
            reaped: false,
        }
    }

    fn wait_bounded(&mut self, timeout: Duration) -> Result<ExitStatus, CliError> {
        let started = Instant::now();
        loop {
            match self
                .child
                .try_wait()
                .map_err(|_| transport_error("failed to wait for SSH MCP transport"))?
            {
                Some(status) => {
                    // The SSH leader may exit while a transport helper keeps
                    // running in its inherited group. End the remainder before
                    // declaring the transport reaped.
                    let _ = unsafe { libc::kill(-self.pgid, libc::SIGKILL) };
                    self.reaped = true;
                    return Ok(status);
                }
                None if started.elapsed() >= timeout => {
                    self.terminate_and_reap();
                    return Err(transport_error("SSH MCP transport did not exit cleanly"));
                }
                None => thread::sleep(Duration::from_millis(10)),
            }
        }
    }

    fn terminate_and_reap(&mut self) {
        if self.reaped {
            return;
        }
        // SAFETY: the negative PID addresses the isolated SSH process group.
        let _ = unsafe { libc::kill(-self.pgid, libc::SIGTERM) };
        let started = Instant::now();
        while started.elapsed() < Duration::from_millis(200) {
            if self.child.try_wait().ok().flatten().is_some() {
                // The leader is reaped, but a TERM-ignoring helper can still
                // retain the isolated group. Escalate the remainder before
                // returning.
                let _ = unsafe { libc::kill(-self.pgid, libc::SIGKILL) };
                self.reaped = true;
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
        // SAFETY: escalation remains confined to the same process group.
        let _ = unsafe { libc::kill(-self.pgid, libc::SIGKILL) };
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.reaped = true;
    }
}

impl Drop for ProcessGroupChild {
    fn drop(&mut self) {
        self.terminate_and_reap();
    }
}

pub fn run_remote_endpoint() -> Result<u8, CliError> {
    let response = match read_request().and_then(execute_remote) {
        Ok(response) => response,
        Err(error) => RemoteResponse {
            schema_version: PROTOCOL_SCHEMA.into(),
            adapter_version: env!("CARGO_PKG_VERSION").into(),
            token: String::new(),
            ok: false,
            exit_code: error.exit_code(),
            result: None,
            error: Some(remote_error(&error)),
            artifacts: Vec::new(),
        },
    };
    let body = serde_json::to_string(&response)
        .map_err(|_| transport_error("failed to encode remote response"))?;
    println!("{body}");
    Ok(0)
}

pub fn run_remote_mcp_endpoint(token: &str) -> Result<u8, CliError> {
    let result = run_remote_mcp_endpoint_inner(token);
    let terminal = match &result {
        Ok(exit_code) => RemoteMcpTerminal {
            schema_version: PROTOCOL_SCHEMA.into(),
            adapter_version: env!("CARGO_PKG_VERSION").into(),
            token: token.into(),
            ok: *exit_code == 0,
            exit_code: *exit_code,
            error: None,
        },
        Err(error) => RemoteMcpTerminal {
            schema_version: PROTOCOL_SCHEMA.into(),
            adapter_version: env!("CARGO_PKG_VERSION").into(),
            token: token.into(),
            ok: false,
            exit_code: error.exit_code(),
            error: Some(remote_error(error)),
        },
    };
    let encoded = serde_json::to_string(&terminal)
        .map_err(|_| transport_error("failed to encode remote MCP terminal status"))?;
    eprintln!("{MCP_TERMINAL_PREFIX}{encoded}");
    Ok(0)
}

fn run_remote_mcp_endpoint_inner(token: &str) -> Result<u8, CliError> {
    validate_token(token)?;
    let stdin = std::io::stdin();
    let mut input = std::io::BufReader::new(stdin);
    let control_frame = read_bounded_line(&mut input, 64 * 1024)?;
    let control: RemoteMcpControl = serde_json::from_slice(trim_line_ending(&control_frame))
        .map_err(|_| transport_error("remote MCP control frame is malformed"))?;
    let lock = PeekabooLock::embedded()?;
    if control.schema_version != PROTOCOL_SCHEMA
        || control.adapter_version != env!("CARGO_PKG_VERSION")
        || control.peekaboo_commit != lock.commit
        || control.token != token
    {
        return Err(transport_error("remote MCP control frame is incompatible"));
    }
    let root = remote_session_root(token)?;
    create_private_dir(&root)?;
    let args = McpArgs {
        host: None,
        out_dir: root.join("journal"),
        tool_profile: control.tool_profile,
        runtime: control.runtime,
    };
    let stdout = std::io::stdout();
    commands::mcp::run_with_io(&args, "ssh", input, stdout.lock())
}

fn read_capped(mut reader: impl Read, limit: usize) -> (Vec<u8>, bool) {
    let mut body = Vec::new();
    let mut buffer = [0u8; 8192];
    let mut truncated = false;
    loop {
        let read = match reader.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(read) => read,
        };
        let keep = limit.saturating_sub(body.len()).min(read);
        body.extend_from_slice(&buffer[..keep]);
        truncated |= keep < read;
    }
    (body, truncated)
}

fn parse_mcp_terminal(raw: &[u8], token: &str) -> Result<RemoteMcpTerminal, CliError> {
    let text = std::str::from_utf8(raw)
        .map_err(|_| transport_error("SSH MCP terminal status is not UTF-8"))?;
    let encoded = text
        .lines()
        .rev()
        .find_map(|line| line.strip_prefix(MCP_TERMINAL_PREFIX))
        .ok_or_else(|| transport_error("SSH MCP terminal status is unavailable"))?;
    let terminal: RemoteMcpTerminal = serde_json::from_str(encoded)
        .map_err(|_| transport_error("SSH MCP terminal status is malformed"))?;
    if terminal.schema_version != PROTOCOL_SCHEMA
        || terminal.adapter_version != env!("CARGO_PKG_VERSION")
        || terminal.token != token
        || terminal.ok != (terminal.exit_code == 0)
        || terminal.ok == terminal.error.is_some()
    {
        return Err(transport_error("SSH MCP terminal status is incompatible"));
    }
    Ok(terminal)
}

fn read_bounded_line(reader: &mut impl BufRead, max_bytes: usize) -> Result<Vec<u8>, CliError> {
    let mut frame = Vec::new();
    loop {
        let available = reader
            .fill_buf()
            .map_err(|_| transport_error("failed to read remote MCP control frame"))?;
        if available.is_empty() {
            break;
        }
        let take = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |index| index + 1);
        if frame.len().saturating_add(take) > max_bytes {
            return Err(transport_error(
                "remote MCP control frame exceeds its bound",
            ));
        }
        let ended = available.get(take.saturating_sub(1)) == Some(&b'\n');
        frame.extend_from_slice(&available[..take]);
        reader.consume(take);
        if ended {
            break;
        }
    }
    if frame.is_empty() {
        return Err(transport_error("remote MCP control frame is unavailable"));
    }
    Ok(frame)
}

fn trim_line_ending(mut frame: &[u8]) -> &[u8] {
    if let Some(trimmed) = frame.strip_suffix(b"\n") {
        frame = trimmed;
    }
    frame.strip_suffix(b"\r").unwrap_or(frame)
}

pub fn run_remote_cleanup_endpoint(token: &str) -> Result<u8, CliError> {
    validate_token(token)?;
    remove_session(&remote_session_root(token)?)?;
    Ok(0)
}

fn request(
    host: &str,
    command: RemoteCommand,
    timeout: Duration,
) -> Result<RemoteResponse, CliError> {
    request_with_token(host, session_token(), command, timeout)
}

fn request_with_token(
    host: &str,
    token: String,
    command: RemoteCommand,
    timeout: Duration,
) -> Result<RemoteResponse, CliError> {
    validate_host(host)?;
    let lock = PeekabooLock::embedded()?;
    let request = RemoteRequest {
        schema_version: PROTOCOL_SCHEMA.into(),
        adapter_version: env!("CARGO_PKG_VERSION").into(),
        peekaboo_commit: lock.commit,
        token: token.clone(),
        command,
    };
    let mut input = serde_json::to_vec(&request)
        .map_err(|_| transport_error("failed to encode remote request"))?;
    input.push(b'\n');
    if input.len() as u64 > MAX_REQUEST_BYTES {
        return Err(CliError::usage("remote request exceeds its size bound"));
    }
    let output = process::run(
        &ssh_program(),
        &ssh_args(host, &["macos-agent", "__remote"]),
        &[],
        &[],
        Some(&input),
        timeout,
    )
    .map_err(|_| transport_error("failed to start SSH transport"))?;
    if output.timed_out {
        let _ = cleanup(host, &token);
        return Err(transport_error(
            "SSH operation timed out; mutation state may be unknown",
        ));
    }
    if output.exit_code != 0 || output.stdout_truncated {
        let _ = cleanup(host, &token);
        return Err(transport_error(
            "SSH transport failed before a complete response",
        ));
    }
    let response: RemoteResponse = serde_json::from_slice(&output.stdout).map_err(|_| {
        let _ = cleanup(host, &token);
        transport_error("remote endpoint returned malformed protocol output")
    })?;
    if response.schema_version != PROTOCOL_SCHEMA
        || response.adapter_version != env!("CARGO_PKG_VERSION")
        || response.token != token
    {
        let _ = cleanup(host, &token);
        return Err(transport_error(
            "remote endpoint version or correlation mismatch",
        ));
    }
    Ok(response)
}

fn execute_remote(request: RemoteRequest) -> Result<RemoteResponse, CliError> {
    let lock = PeekabooLock::embedded()?;
    validate_token(&request.token)?;
    if request.schema_version != PROTOCOL_SCHEMA
        || request.adapter_version != env!("CARGO_PKG_VERSION")
        || request.peekaboo_commit != lock.commit
    {
        return Err(transport_error(
            "remote request version or backend lock is incompatible",
        ));
    }
    let token = request.token;
    let root = remote_session_root(&token)?;
    let operation = execute_remote_operation(&root, request.command);
    let response = match operation {
        Ok((result, exit_code, collect, cleanup_after)) => {
            let artifacts = if collect {
                match collect_files(&root.join("journal")) {
                    Ok(artifacts) => artifacts,
                    Err(error) => {
                        let cleanup_error = remove_session(&root).err();
                        return Ok(remote_failure(
                            token,
                            cleanup_error.unwrap_or(error),
                            Vec::new(),
                        ));
                    }
                }
            } else {
                Vec::new()
            };
            if cleanup_after && let Err(error) = remove_session(&root) {
                let _ =
                    crate::journal::record_system_failure(&root.join("journal"), "remote_cleanup");
                let artifacts = collect_files(&root.join("journal")).unwrap_or(artifacts);
                return Ok(remote_failure(token, error, artifacts));
            }
            RemoteResponse {
                schema_version: PROTOCOL_SCHEMA.into(),
                adapter_version: env!("CARGO_PKG_VERSION").into(),
                token,
                ok: true,
                exit_code,
                result: Some(result),
                error: None,
                artifacts,
            }
        }
        Err(error) => {
            let artifacts = collect_files(&root.join("journal")).unwrap_or_default();
            let cleanup_error = remove_session(&root).err();
            remote_failure(token, cleanup_error.unwrap_or(error), artifacts)
        }
    };
    Ok(response)
}

fn remote_failure(
    token: String,
    error: CliError,
    artifacts: Vec<TransferredFile>,
) -> RemoteResponse {
    RemoteResponse {
        schema_version: PROTOCOL_SCHEMA.into(),
        adapter_version: env!("CARGO_PKG_VERSION").into(),
        token,
        ok: false,
        exit_code: error.exit_code(),
        result: None,
        error: Some(remote_error(&error)),
        artifacts,
    }
}

fn execute_remote_operation(
    root: &Path,
    command: RemoteCommand,
) -> Result<(serde_json::Value, u8, bool, bool), CliError> {
    match command {
        RemoteCommand::Backend {
            action,
            dry_run,
            strict,
        } => {
            let value = match action {
                BackendAction::Install => serde_json::to_value(backend::install(dry_run, strict)?),
                BackendAction::Status => serde_json::to_value(backend::status(dry_run)?),
                BackendAction::Verify => serde_json::to_value(backend::verify(strict)?),
                BackendAction::Rollback => {
                    serde_json::to_value(backend::rollback(dry_run, strict)?)
                }
            }
            .map_err(|_| transport_error("failed to encode backend response"))?;
            Ok((value, 0, false, true))
        }
        RemoteCommand::Doctor { strict } => {
            let report = backend::doctor(strict)?;
            let exit_code = if strict && !report.ready { 77 } else { 0 };
            Ok((
                serde_json::to_value(report)
                    .map_err(|_| transport_error("failed to encode doctor response"))?,
                exit_code,
                false,
                true,
            ))
        }
        RemoteCommand::Capabilities { strict } => {
            Ok((crate::run::capability_report(strict)?, 0, false, true))
        }
        RemoteCommand::Exec {
            argv,
            intent,
            expected,
            evidence_mode,
            runtime,
            timeout_seconds,
        } => {
            create_private_dir(root)?;
            let args = ExecArgs {
                host: None,
                out_dir: root.join("journal"),
                intent,
                expected,
                evidence_mode,
                runtime,
                timeout_seconds,
                argv,
            };
            let outcome = commands::exec::run_local(&args, None, "ssh")?;
            Ok((
                serde_json::to_value(outcome.result)
                    .map_err(|_| transport_error("failed to encode exec response"))?,
                outcome.exit_code,
                true,
                true,
            ))
        }
        RemoteCommand::Scenario {
            source_sha256,
            source_base64,
            evidence_mode,
            runtime,
            timeout_seconds,
        } => {
            create_private_dir(root)?;
            let input_dir = root.join("input");
            create_private_dir(&input_dir)?;
            let source = base64::engine::general_purpose::STANDARD
                .decode(source_base64)
                .map_err(|_| transport_error("remote scenario payload is malformed"))?;
            if source.len() > 1024 * 1024 || hex(&Sha256::digest(&source)) != source_sha256 {
                return Err(transport_error("remote scenario payload digest mismatch"));
            }
            let source_path = input_dir.join("scenario.peekaboo.json");
            write_atomic(&source_path, &source, SECRET_FILE_MODE)
                .map_err(|_| transport_error("failed to stage remote scenario"))?;
            let args = ScenarioArgs {
                host: None,
                out_dir: root.join("journal"),
                file: source_path,
                evidence_mode,
                runtime,
                timeout_seconds,
            };
            let outcome = commands::scenario::run_local(&args, "ssh")?;
            Ok((
                serde_json::to_value(outcome.result)
                    .map_err(|_| transport_error("failed to encode scenario response"))?,
                outcome.exit_code,
                true,
                true,
            ))
        }
        RemoteCommand::Collect => {
            let artifacts = root.join("journal");
            if !artifacts.is_dir() {
                return Err(transport_error("remote session journal is unavailable"));
            }
            Ok((serde_json::json!({"collected": true}), 0, true, true))
        }
    }
}

fn read_request() -> Result<RemoteRequest, CliError> {
    let mut reader = std::io::stdin().take(MAX_REQUEST_BYTES + 1);
    let mut raw = Vec::new();
    reader
        .read_to_end(&mut raw)
        .map_err(|_| transport_error("failed to read remote request"))?;
    if raw.len() as u64 > MAX_REQUEST_BYTES {
        return Err(transport_error("remote request exceeds its size bound"));
    }
    serde_json::from_slice(&raw).map_err(|_| transport_error("remote request is malformed"))
}

fn collect_files(root: &Path) -> Result<Vec<TransferredFile>, CliError> {
    if !root.is_dir() {
        return Ok(Vec::new());
    }
    let mut allow = vec![
        "manifest.json".to_string(),
        "steps.jsonl".to_string(),
        "summary.json".to_string(),
        "redaction.json".to_string(),
        "review.json".to_string(),
        "artifacts/index.json".to_string(),
    ];
    let mut indexed_digests = BTreeMap::<String, String>::new();
    let index_path = root.join("artifacts/index.json");
    if index_path.is_file() {
        let raw = fs::read(&index_path)
            .map_err(|_| transport_error("failed to read remote artifact index"))?;
        let index: ArtifactIndex = serde_json::from_slice(&raw)
            .map_err(|_| transport_error("remote artifact index is malformed"))?;
        if index.schema_version != "macos-agent.artifact-index.v1" {
            return Err(transport_error(
                "remote artifact index schema is incompatible",
            ));
        }
        for row in index.artifacts {
            validate_relative(&row.relative_path)?;
            if indexed_digests
                .insert(row.relative_path.clone(), row.sha256)
                .is_some()
            {
                return Err(transport_error(
                    "remote artifact index contains duplicate paths",
                ));
            }
            allow.push(row.relative_path);
        }
    }
    allow.sort();
    allow.dedup();
    if allow.len() > MAX_TRANSFER_FILES {
        return Err(transport_error(
            "remote artifact manifest exceeds its file bound",
        ));
    }
    let mut total = 0usize;
    let mut files = Vec::new();
    let mut collected_indexed = BTreeSet::new();
    for relative in allow {
        validate_relative(&relative)?;
        ensure_confined_parent(root, &relative, false)?;
        let path = root.join(&relative);
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if indexed_digests.contains_key(&relative) {
                    return Err(transport_error("indexed remote artifact is unavailable"));
                }
                continue;
            }
            Err(_) => return Err(transport_error("failed to inspect remote artifact")),
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(transport_error("remote artifact is not a regular file"));
        }
        let body =
            fs::read(&path).map_err(|_| transport_error("failed to read remote artifact"))?;
        total = total.saturating_add(body.len());
        if total > MAX_TRANSFER_BYTES {
            return Err(transport_error(
                "remote artifact bundle exceeds its size bound",
            ));
        }
        let digest = hex(&Sha256::digest(&body));
        if let Some(expected) = indexed_digests.get(&relative) {
            if expected != &format!("sha256:{digest}") {
                return Err(transport_error(
                    "remote artifact no longer matches its indexed digest",
                ));
            }
            collected_indexed.insert(relative.clone());
        }
        files.push(TransferredFile {
            relative_path: relative,
            sha256: digest,
            data_base64: base64::engine::general_purpose::STANDARD.encode(body),
        });
    }
    if collected_indexed.len() != indexed_digests.len() {
        return Err(transport_error(
            "remote artifact bundle omitted an indexed artifact",
        ));
    }
    Ok(files)
}

fn install_transferred_files(root: &Path, files: &[TransferredFile]) -> Result<(), CliError> {
    if files.len() > MAX_TRANSFER_FILES {
        return Err(transport_error(
            "remote artifact response exceeds its file bound",
        ));
    }
    if let Ok(metadata) = fs::symlink_metadata(root)
        && metadata.file_type().is_symlink()
    {
        return Err(CliError::policy(
            "artifact output directory must not be a symlink",
        ));
    }
    create_private_dir(root)?;
    let mut total = 0usize;
    for file in files {
        validate_relative(&file.relative_path)?;
        let body = base64::engine::general_purpose::STANDARD
            .decode(&file.data_base64)
            .map_err(|_| transport_error("remote artifact encoding is malformed"))?;
        total = total.saturating_add(body.len());
        if total > MAX_TRANSFER_BYTES || hex(&Sha256::digest(&body)) != file.sha256 {
            return Err(transport_error(
                "remote artifact digest or bundle size is invalid",
            ));
        }
        let path = root.join(&file.relative_path);
        ensure_confined_parent(root, &file.relative_path, true)?;
        if let Ok(metadata) = fs::symlink_metadata(&path)
            && (metadata.file_type().is_symlink() || !metadata.is_file())
        {
            return Err(CliError::policy(
                "refusing to replace an unsafe local artifact path",
            ));
        }
        write_atomic(&path, &body, SECRET_FILE_MODE)
            .map_err(|_| transport_error("failed to write a transferred artifact"))?;
        if sha256_file(&path)
            .map_err(|_| transport_error("failed to verify a transferred artifact"))?
            != file.sha256
        {
            return Err(transport_error("transferred artifact verification failed"));
        }
    }
    Ok(())
}

fn emit_remote(
    format: OutputFormat,
    label: &'static str,
    response: RemoteResponse,
) -> Result<u8, CliError> {
    if !response.ok {
        return Err(response_error(response));
    }
    let envelope = SuccessEnvelope::new(label, response.result.unwrap_or(serde_json::Value::Null));
    let body = match format {
        OutputFormat::Json => serde_json::to_string(&envelope),
        OutputFormat::Text => serde_json::to_string_pretty(&envelope),
    }
    .map_err(|_| transport_error("failed to encode remote command response"))?;
    println!("{body}");
    Ok(response.exit_code)
}

fn response_error(response: RemoteResponse) -> CliError {
    match response.error {
        Some(error) => remote_error_to_cli(error),
        None => transport_error("remote endpoint failed without a typed error"),
    }
}

fn remote_error_to_cli(error: RemoteError) -> CliError {
    let mut result = CliError::new(error.class, error.message);
    if let Some(operation) = error.operation {
        result = result.with_operation(operation);
    }
    result
}

fn remote_error(error: &CliError) -> RemoteError {
    RemoteError {
        class: error.class(),
        message: crate::journal::sanitize_output(error.message(), EvidenceMode::Minimal),
        operation: error.operation().map(str::to_string),
    }
}

fn cleanup(host: &str, token: &str) -> Result<(), CliError> {
    let output = process::run(
        &ssh_program(),
        &ssh_args(host, &["macos-agent", "__remote-cleanup", "--token", token]),
        &[],
        &[],
        None,
        Duration::from_secs(15),
    )
    .map_err(|_| transport_error("failed to start remote cleanup"))?;
    if output.exit_code != 0 || output.timed_out {
        Err(transport_error("remote cleanup could not be confirmed"))
    } else {
        Ok(())
    }
}

fn ssh_program() -> PathBuf {
    test_mode::ssh_bin_override().unwrap_or_else(|| PathBuf::from("ssh"))
}

fn ssh_args(host: &str, remote_command: &[&str]) -> Vec<String> {
    let mut args = vec![
        "-o".into(),
        "BatchMode=yes".into(),
        "-o".into(),
        "ConnectTimeout=10".into(),
        "-o".into(),
        "LogLevel=ERROR".into(),
        "--".into(),
        host.into(),
    ];
    args.extend(remote_command.iter().map(|value| (*value).to_string()));
    args
}

fn validate_host(host: &str) -> Result<(), CliError> {
    if host.is_empty()
        || host.len() > 255
        || host.starts_with('-')
        || host.matches('@').count() > 1
        || !host.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(byte, b'.' | b'-' | b'_' | b'@' | b':' | b'[' | b']' | b'%')
        })
    {
        return Err(CliError::usage("SSH host syntax is unsafe").with_operation("transport.ssh"));
    }
    Ok(())
}

fn validate_token(token: &str) -> Result<(), CliError> {
    if token.len() != 32
        || !token
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(transport_error("remote session token is malformed"));
    }
    Ok(())
}

fn validate_relative(value: &str) -> Result<(), CliError> {
    let path = Path::new(value);
    if value.is_empty()
        || value.contains('\\')
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(transport_error("remote artifact path is unsafe"));
    }
    Ok(())
}

fn ensure_confined_parent(root: &Path, relative: &str, create: bool) -> Result<(), CliError> {
    let relative = Path::new(relative);
    let mut current = root.to_path_buf();
    let Some(parent) = relative.parent() else {
        return Ok(());
    };
    for component in parent.components() {
        let Component::Normal(component) = component else {
            return Err(transport_error("remote artifact path is unsafe"));
        };
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                return Err(CliError::policy(
                    "artifact path contains an unsafe ancestor component",
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && create => {
                fs::create_dir(&current)
                    .map_err(|_| transport_error("failed to create private transport storage"))?;
                fs::set_permissions(&current, fs::Permissions::from_mode(0o700))
                    .map_err(|_| transport_error("failed to secure private transport storage"))?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(transport_error("remote artifact parent is unavailable"));
            }
            Err(_) => return Err(transport_error("failed to inspect artifact path ancestry")),
        }
    }
    Ok(())
}

fn remote_session_root(token: &str) -> Result<PathBuf, CliError> {
    validate_token(token)?;
    if let Some(root) = test_mode::remote_root_override() {
        return Ok(root.join(token));
    }
    let home = std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| transport_error("HOME is required for remote session storage"))?;
    Ok(home
        .join("Library/Application Support/nils-cli/macos-agent/remote-sessions")
        .join(token))
}

fn create_private_dir(path: &Path) -> Result<(), CliError> {
    fs::create_dir_all(path)
        .map_err(|_| transport_error("failed to create private transport storage"))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|_| transport_error("failed to secure private transport storage"))
}

fn remove_session(root: &Path) -> Result<(), CliError> {
    if test_mode::cleanup_failure() {
        return Err(transport_error("remote session cleanup failed"));
    }
    match fs::remove_dir_all(root) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(transport_error("remote session cleanup failed")),
    }
}

fn session_token() -> String {
    let sequence = TOKEN_SEQUENCE.fetch_add(1, Ordering::SeqCst);
    let source = format!(
        "{}:{}:{sequence}",
        test_mode::timestamp(),
        std::process::id()
    );
    hex(&Sha256::digest(source.as_bytes()))[..32].to_string()
}

fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn transport_error(message: impl Into<String>) -> CliError {
    CliError::transport(message).with_operation("transport.ssh")
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Cursor;
    use std::os::unix::fs::symlink;
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    use sha2::Digest;
    use tempfile::TempDir;

    use super::{
        ProcessGroupChild, TransferredFile, collect_files, install_transferred_files,
        read_bounded_line, validate_host, validate_relative,
    };

    #[test]
    fn host_is_an_argv_value_but_still_rejects_option_and_shell_shapes() {
        assert!(validate_host("mac-role").is_ok());
        assert!(validate_host("operator@192.0.2.8").is_ok());
        assert!(validate_host("-oProxyCommand=evil").is_err());
        assert!(validate_host("host;touch-owned").is_err());
        assert!(validate_host("host name").is_err());
    }

    #[test]
    fn remote_mcp_control_frame_is_bounded_before_unlimited_allocation() {
        let mut valid = Cursor::new(b"{}\n".to_vec());
        assert_eq!(read_bounded_line(&mut valid, 8).expect("frame"), b"{}\n");
        let mut oversized = Cursor::new(vec![b'x'; 9]);
        assert!(read_bounded_line(&mut oversized, 8).is_err());
    }

    #[test]
    fn transferred_files_are_confined_and_hash_verified() {
        let root = TempDir::new().expect("root");
        let body = b"{}";
        let file = TransferredFile {
            relative_path: "manifest.json".into(),
            sha256: super::hex(&sha2::Sha256::digest(body)),
            data_base64: base64::Engine::encode(&base64::engine::general_purpose::STANDARD, body),
        };
        install_transferred_files(root.path(), &[file]).expect("transfer");
        assert_eq!(
            fs::read(root.path().join("manifest.json")).expect("read"),
            body
        );
        assert!(validate_relative("../escape").is_err());

        let wrong_digest = TransferredFile {
            relative_path: "summary.json".into(),
            sha256: "0".repeat(64),
            data_base64: base64::Engine::encode(&base64::engine::general_purpose::STANDARD, body),
        };
        assert!(install_transferred_files(root.path(), &[wrong_digest]).is_err());

        let unsafe_path = TransferredFile {
            relative_path: "../outside.json".into(),
            sha256: super::hex(&sha2::Sha256::digest(body)),
            data_base64: base64::Engine::encode(&base64::engine::general_purpose::STANDARD, body),
        };
        assert!(install_transferred_files(root.path(), &[unsafe_path]).is_err());
    }

    #[test]
    fn transferred_files_reject_symlinked_ancestor_components() {
        let root = TempDir::new().expect("root");
        let outside = TempDir::new().expect("outside");
        symlink(outside.path(), root.path().join("artifacts")).expect("ancestor symlink");
        let body = b"private";
        let file = TransferredFile {
            relative_path: "artifacts/result.json".into(),
            sha256: super::hex(&sha2::Sha256::digest(body)),
            data_base64: base64::Engine::encode(&base64::engine::general_purpose::STANDARD, body),
        };
        assert!(install_transferred_files(root.path(), &[file]).is_err());
        assert!(!outside.path().join("result.json").exists());
    }

    #[test]
    fn collection_rejects_artifacts_that_drift_from_the_index_digest() {
        let root = TempDir::new().expect("root");
        fs::create_dir_all(root.path().join("artifacts")).expect("artifacts");
        fs::write(
            root.path().join("artifacts/result.json"),
            b"changed-after-index",
        )
        .expect("artifact");
        fs::write(
            root.path().join("artifacts/index.json"),
            serde_json::to_vec(&crate::journal::ArtifactIndex {
                schema_version: "macos-agent.artifact-index.v1".into(),
                artifacts: vec![crate::journal::ArtifactRecord {
                    sha256: format!("sha256:{}", "0".repeat(64)),
                    mime: "application/json".into(),
                    kind: "fixture".into(),
                    producing_step: "step-000001".into(),
                    sensitivity: "private".into(),
                    redaction: "sanitized".into(),
                    retention: "debug".into(),
                    relative_path: "artifacts/result.json".into(),
                }],
            })
            .expect("index"),
        )
        .expect("write index");
        assert!(collect_files(root.path()).is_err());
    }

    #[test]
    fn process_group_cleanup_kills_helper_after_leader_exits_on_term() {
        let root = TempDir::new().expect("root");
        let marker = root.path().join("orphan");
        let mut command = Command::new("/bin/sh");
        command
            .arg("-c")
            .arg(format!(
                "(trap '' TERM; sleep 0.4; : > '{}') & sleep 5",
                marker.display()
            ))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // SAFETY: the child performs only the async-signal-safe setpgid call.
        unsafe {
            command.pre_exec(|| {
                if libc::setpgid(0, 0) == 0 {
                    Ok(())
                } else {
                    Err(std::io::Error::last_os_error())
                }
            });
        }
        let child = command.spawn().expect("child");
        let mut guard = ProcessGroupChild::new(child);
        let started = Instant::now();
        guard.terminate_and_reap();
        assert!(started.elapsed() < Duration::from_secs(1));
        std::thread::sleep(Duration::from_millis(500));
        assert!(!marker.exists(), "helper survived after its leader exited");
    }
}
