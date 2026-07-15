use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::process::CommandExt;

use crate::cli::McpArgs;
use crate::commands::{hardened_env, peekaboo_binary, prepare_runtime, runtime_argv};
use crate::error::CliError;
use crate::journal::{Journal, StepInput, StepStatus};
use crate::policy;
use crate::test_mode;

const MAX_FRAME_BYTES: usize = 4 * 1024 * 1024;
const DEFAULT_RESPONSE_TIMEOUT: Duration = Duration::from_secs(120);
const CLEAN_EXIT_TIMEOUT: Duration = Duration::from_secs(5);
const EVENT_QUEUE_CAPACITY: usize = 16;
const WRITE_QUEUE_CAPACITY: usize = 8;

pub fn run_local(args: &McpArgs, transport: &str) -> Result<u8, CliError> {
    run_with_io(args, transport, std::io::stdin(), std::io::stdout())
}

pub fn run_with_io(
    args: &McpArgs,
    transport: &str,
    input: impl Read + Send + 'static,
    output: impl Write,
) -> Result<u8, CliError> {
    let binary = peekaboo_binary()?;
    let mut journal = Journal::open_for_backend(
        &args.out_dir,
        args.runtime,
        transport,
        crate::cli::EvidenceMode::Sensitive,
        Some(args.tool_profile),
        &binary,
    )?;
    let result = run_session(args, input, output, &mut journal, &binary);
    let close = journal.close();
    match (result, close) {
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Ok(code), Ok(_)) => Ok(code),
    }
}

fn run_session(
    args: &McpArgs,
    input: impl Read + Send + 'static,
    mut output: impl Write,
    journal: &mut Journal,
    binary: &crate::backend::VerifiedBackend,
) -> Result<u8, CliError> {
    prepare_runtime(args.runtime, binary.path())?;
    let upstream_args = runtime_argv(
        args.runtime,
        &[
            "mcp".into(),
            "serve".into(),
            "--transport".into(),
            "stdio".into(),
        ],
    );
    let (envs, removed_envs) = hardened_env(Some(args.tool_profile));
    let mut command = Command::new(binary.path());
    command
        .args(&upstream_args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in envs {
        command.env(key, value);
    }
    for key in removed_envs {
        command.env_remove(key);
    }
    #[cfg(unix)]
    // SAFETY: `setpgid(0, 0)` is async-signal-safe and gives the MCP server and
    // any helpers one process group for deterministic cleanup.
    unsafe {
        command.pre_exec(|| {
            if libc::setpgid(0, 0) == 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        });
    }
    let mut child = command.spawn().map_err(|_| {
        CliError::upstream("failed to start Peekaboo MCP stdio server").with_operation("mcp")
    })?;
    let upstream_in = child
        .stdin
        .take()
        .ok_or_else(|| CliError::upstream("Peekaboo MCP stdin is unavailable"))?;
    let upstream_out = child
        .stdout
        .take()
        .ok_or_else(|| CliError::upstream("Peekaboo MCP stdout is unavailable"))?;
    let upstream_err = child
        .stderr
        .take()
        .ok_or_else(|| CliError::upstream("Peekaboo MCP stderr is unavailable"))?;
    let mut child = ChildGuard::new(child);

    let (events_tx, events_rx) = mpsc::sync_channel(EVENT_QUEUE_CAPACITY);
    let client_handle = spawn_pump(input, StreamSide::Client, events_tx.clone());
    let upstream_handle = spawn_pump(upstream_out, StreamSide::Upstream, events_tx.clone());
    let (writer_tx, writer_rx) = mpsc::sync_channel(WRITE_QUEUE_CAPACITY);
    let writer_handle = spawn_writer(upstream_in, writer_rx, events_tx);
    let stderr_handle = thread::spawn(move || drain(upstream_err));

    let session = proxy_events(
        args,
        journal,
        &mut output,
        writer_tx,
        &events_rx,
        &mut child,
    );
    drop(events_rx);
    child.kill_remaining_group();
    let _ = writer_handle.join();
    let _ = upstream_handle.join();
    let _ = stderr_handle.join();
    drop(client_handle);
    session
}

fn proxy_events(
    args: &McpArgs,
    journal: &mut Journal,
    output: &mut impl Write,
    upstream_writer: SyncSender<WriterCommand>,
    events: &Receiver<StreamEvent>,
    child: &mut ChildGuard,
) -> Result<u8, CliError> {
    let mut upstream_writer = UpstreamWriterState::new(upstream_writer);
    let mut pending = BTreeMap::<String, PendingRequest>::new();
    let mut client_eof = false;
    let mut upstream_eof = false;
    let mut shutdown_completed = false;
    let mut clean_exit_started = None::<Instant>;
    let response_timeout = response_timeout();

    loop {
        match events.recv_timeout(Duration::from_millis(10)) {
            Ok(StreamEvent::Client(FrameEvent::Frame(frame))) => {
                handle_client_frame(
                    args,
                    journal,
                    output,
                    &mut upstream_writer,
                    &mut pending,
                    &frame,
                )?;
            }
            Ok(StreamEvent::Client(FrameEvent::Oversized)) => {
                write_protocol_error(
                    output,
                    serde_json::Value::Null,
                    -32600,
                    "request exceeds frame bound",
                )?;
            }
            Ok(StreamEvent::Client(FrameEvent::Eof)) => {
                client_eof = true;
                upstream_writer.close();
                clean_exit_started.get_or_insert_with(Instant::now);
            }
            Ok(StreamEvent::Client(FrameEvent::IoError)) => {
                return Err(
                    CliError::upstream("failed to read MCP client frame").with_operation("mcp")
                );
            }
            Ok(StreamEvent::Upstream(FrameEvent::Frame(frame))) => {
                if handle_upstream_frame(args, journal, output, &mut pending, &frame)? {
                    shutdown_completed = true;
                }
            }
            Ok(StreamEvent::Upstream(FrameEvent::Oversized)) => {
                return Err(
                    CliError::upstream("Peekaboo MCP response exceeds frame bound")
                        .with_operation("mcp"),
                );
            }
            Ok(StreamEvent::Upstream(FrameEvent::Eof)) => {
                upstream_eof = true;
            }
            Ok(StreamEvent::Upstream(FrameEvent::IoError)) => {
                return Err(CliError::upstream("failed to read Peekaboo MCP response")
                    .with_operation("mcp"));
            }
            Ok(StreamEvent::Writer(WriterEvent::Written(id))) => {
                upstream_writer.complete(id);
            }
            Ok(StreamEvent::Writer(WriterEvent::IoError)) => {
                return Err(
                    CliError::upstream("failed to forward MCP request").with_operation("mcp")
                );
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                upstream_eof = true;
                client_eof = true;
                upstream_writer.close();
                clean_exit_started.get_or_insert_with(Instant::now);
            }
        }

        if pending
            .values()
            .any(|request| request.started.elapsed() >= response_timeout)
        {
            return Err(
                CliError::upstream("Peekaboo MCP response deadline exceeded").with_operation("mcp"),
            );
        }
        if upstream_writer.has_expired(response_timeout) {
            return Err(
                CliError::upstream("Peekaboo MCP request write deadline exceeded")
                    .with_operation("mcp"),
            );
        }
        if upstream_eof && let Some(status) = child.try_wait()? {
            if !pending.is_empty() {
                return Err(
                    CliError::upstream("Peekaboo MCP server exited with requests pending")
                        .with_operation("mcp"),
                );
            }
            if !status.success() {
                return Err(CliError::upstream(format!(
                    "Peekaboo MCP server exited with status {}",
                    status.code().unwrap_or(1)
                ))
                .with_operation("mcp"));
            }
            if !client_eof && !shutdown_completed {
                return Err(CliError::upstream("Peekaboo MCP server ended unexpectedly")
                    .with_operation("mcp"));
            }
            return Ok(0);
        }
        if upstream_eof && !pending.is_empty() {
            return Err(
                CliError::upstream("Peekaboo MCP server closed stdout").with_operation("mcp")
            );
        }
        if (client_eof || shutdown_completed)
            && clean_exit_started
                .get_or_insert_with(Instant::now)
                .elapsed()
                >= CLEAN_EXIT_TIMEOUT
        {
            return Err(
                CliError::upstream("Peekaboo MCP server did not exit cleanly")
                    .with_operation("mcp"),
            );
        }
    }
}

fn handle_client_frame(
    args: &McpArgs,
    journal: &mut Journal,
    output: &mut impl Write,
    upstream_writer: &mut UpstreamWriterState,
    pending: &mut BTreeMap<String, PendingRequest>,
    frame: &[u8],
) -> Result<(), CliError> {
    let request = match serde_json::from_slice::<serde_json::Value>(frame) {
        Ok(request) => request,
        Err(_) => {
            write_protocol_error(
                output,
                serde_json::Value::Null,
                -32700,
                "invalid JSON-RPC frame",
            )?;
            return Ok(());
        }
    };
    let Some(object) = request.as_object() else {
        record_mcp_step(
            journal,
            "invalid_batch",
            None,
            StepStatus::PolicyBlocked,
            Some("policy"),
            Duration::ZERO,
        )?;
        write_protocol_error(
            output,
            serde_json::Value::Null,
            -32600,
            "JSON-RPC batch frames are not supported",
        )?;
        return Ok(());
    };
    if object.get("jsonrpc").and_then(serde_json::Value::as_str) != Some("2.0") {
        write_protocol_error(
            output,
            request_id(&request).unwrap_or(serde_json::Value::Null),
            -32600,
            "invalid JSON-RPC request",
        )?;
        return Ok(());
    }
    if object
        .get("id")
        .is_some_and(|id| !id.is_null() && !valid_id(id))
        || object
            .get("params")
            .is_some_and(|params| !params.is_object() && !params.is_array())
    {
        write_protocol_error(
            output,
            serde_json::Value::Null,
            -32600,
            "invalid JSON-RPC request envelope",
        )?;
        return Ok(());
    }
    let Some(method) = object.get("method").and_then(serde_json::Value::as_str) else {
        write_protocol_error(
            output,
            request_id(&request).unwrap_or(serde_json::Value::Null),
            -32600,
            "JSON-RPC method is required",
        )?;
        return Ok(());
    };
    let id = request_id(&request);
    let tool = request
        .pointer("/params/name")
        .and_then(serde_json::Value::as_str)
        .map(policy::normalize_tool);
    if method == "tools/call"
        && tool
            .as_deref()
            .is_none_or(|tool| !policy::mcp_call_allowed(args.tool_profile, tool, &request))
    {
        record_mcp_step(
            journal,
            method,
            tool.as_deref(),
            StepStatus::PolicyBlocked,
            Some("policy"),
            Duration::ZERO,
        )?;
        if let Some(id) = id {
            write_protocol_error(output, id, -32001, "tool denied by adapter profile")?;
        }
        return Ok(());
    }
    if let Some(id) = id.as_ref() {
        let key = id_key(id)?;
        if pending.contains_key(&key) {
            write_protocol_error(output, id.clone(), -32600, "duplicate JSON-RPC request id")?;
            return Ok(());
        }
        pending.insert(
            key,
            PendingRequest {
                method: method.into(),
                tool: tool.clone(),
                started: Instant::now(),
            },
        );
    }
    upstream_writer.enqueue(frame)?;
    if id.is_none() {
        record_mcp_step(
            journal,
            method,
            tool.as_deref(),
            StepStatus::Passed,
            None,
            Duration::ZERO,
        )?;
    }
    Ok(())
}

fn handle_upstream_frame(
    args: &McpArgs,
    journal: &mut Journal,
    output: &mut impl Write,
    pending: &mut BTreeMap<String, PendingRequest>,
    frame: &[u8],
) -> Result<bool, CliError> {
    let mut response = serde_json::from_slice::<serde_json::Value>(frame).map_err(|_| {
        CliError::upstream("Peekaboo MCP emitted invalid JSON").with_operation("mcp")
    })?;
    let Some(object) = response.as_object() else {
        return Err(
            CliError::upstream("Peekaboo MCP emitted a non-object JSON-RPC frame")
                .with_operation("mcp"),
        );
    };
    if object.get("jsonrpc").and_then(serde_json::Value::as_str) != Some("2.0") {
        return Err(
            CliError::upstream("Peekaboo MCP emitted an invalid JSON-RPC version")
                .with_operation("mcp"),
        );
    }
    if let Some(method) = object.get("method") {
        if !method.is_string()
            || object.get("id").is_some_and(|id| !id.is_null())
            || object
                .get("params")
                .is_some_and(|params| !params.is_object() && !params.is_array())
        {
            return Err(
                CliError::upstream("Peekaboo MCP emitted an invalid server notification")
                    .with_operation("mcp"),
            );
        }
        output
            .write_all(frame)
            .and_then(|_| output.write_all(b"\n"))
            .and_then(|_| output.flush())
            .map_err(|_| {
                CliError::upstream("failed to write MCP client notification").with_operation("mcp")
            })?;
        return Ok(false);
    }
    let id = object
        .get("id")
        .filter(|id| !id.is_null() && valid_id(id))
        .ok_or_else(|| {
            CliError::upstream("Peekaboo MCP response has an invalid id").with_operation("mcp")
        })?;
    let has_result = object.contains_key("result");
    let has_error = object.contains_key("error");
    if has_result == has_error
        || (has_error
            && object.get("error").is_none_or(|error| {
                !error.is_object()
                    || error
                        .get("code")
                        .and_then(serde_json::Value::as_i64)
                        .is_none()
                    || error
                        .get("message")
                        .and_then(serde_json::Value::as_str)
                        .is_none()
            }))
    {
        return Err(CliError::upstream(
            "Peekaboo MCP emitted an invalid JSON-RPC response envelope",
        )
        .with_operation("mcp"));
    }
    let key = id_key(id)?;
    let pending_request = pending.remove(&key).ok_or_else(|| {
        CliError::upstream("Peekaboo MCP response id is not pending").with_operation("mcp")
    })?;
    if pending_request.method == "tools/list" {
        filter_tools(&mut response, args.tool_profile);
    }
    let shutdown = {
        let request = pending_request;
        let failed = response.get("error").is_some()
            || response
                .pointer("/result/isError")
                .and_then(serde_json::Value::as_bool)
                == Some(true);
        record_mcp_step(
            journal,
            &request.method,
            request.tool.as_deref(),
            if failed {
                StepStatus::Failed
            } else {
                StepStatus::Passed
            },
            failed.then_some("upstream_mcp"),
            request.started.elapsed(),
        )?;
        request.method == "shutdown" && !failed
    };
    let encoded = serde_json::to_vec(&response)
        .map_err(|_| CliError::upstream("failed to encode filtered MCP response"))?;
    output
        .write_all(&encoded)
        .and_then(|_| output.write_all(b"\n"))
        .and_then(|_| output.flush())
        .map_err(|_| {
            CliError::upstream("failed to write MCP client response").with_operation("mcp")
        })?;
    Ok(shutdown)
}

fn record_mcp_step(
    journal: &mut Journal,
    method: &str,
    tool: Option<&str>,
    status: StepStatus,
    failure_class: Option<&str>,
    duration: Duration,
) -> Result<(), CliError> {
    journal.record_step(StepInput {
        parent_id: None,
        intent: Some(if method == "tools/call" {
            "MCP tool call".into()
        } else {
            "MCP request".into()
        }),
        expected: None,
        argv: vec!["mcp".into(), method.into(), tool.unwrap_or_default().into()],
        status,
        failure_class: failure_class.map(str::to_string),
        duration_ms: duration.as_millis() as u64,
        retries: 0,
        precondition_refs: Vec::new(),
        postcondition_refs: Vec::new(),
        snapshot_lineage: None,
        artifact_refs: Vec::new(),
    })?;
    Ok(())
}

fn request_id(request: &serde_json::Value) -> Option<serde_json::Value> {
    request.get("id").filter(|id| !id.is_null()).cloned()
}

fn id_key(id: &serde_json::Value) -> Result<String, CliError> {
    serde_json::to_string(id)
        .map_err(|_| CliError::upstream("failed to correlate JSON-RPC request id"))
}

fn valid_id(id: &serde_json::Value) -> bool {
    id.is_string() || id.is_number()
}

fn response_timeout() -> Duration {
    if test_mode::enabled()
        && let Ok(value) = std::env::var("NILS_MACOS_AGENT_TEST_MCP_RESPONSE_TIMEOUT_MS")
        && let Ok(milliseconds) = value.parse::<u64>()
    {
        return Duration::from_millis(milliseconds.clamp(1, 60_000));
    }
    DEFAULT_RESPONSE_TIMEOUT
}

#[derive(Debug)]
struct PendingRequest {
    method: String,
    tool: Option<String>,
    started: Instant,
}

#[derive(Debug, Clone, Copy)]
enum StreamSide {
    Client,
    Upstream,
}

#[derive(Debug)]
enum StreamEvent {
    Client(FrameEvent),
    Upstream(FrameEvent),
    Writer(WriterEvent),
}

#[derive(Debug)]
enum WriterEvent {
    Written(u64),
    IoError,
}

struct WriterCommand {
    id: u64,
    frame: Vec<u8>,
}

struct UpstreamWriterState {
    sender: Option<SyncSender<WriterCommand>>,
    pending: BTreeMap<u64, Instant>,
    next_id: u64,
}

impl UpstreamWriterState {
    fn new(sender: SyncSender<WriterCommand>) -> Self {
        Self {
            sender: Some(sender),
            pending: BTreeMap::new(),
            next_id: 1,
        }
    }

    fn enqueue(&mut self, frame: &[u8]) -> Result<(), CliError> {
        let sender = self.sender.as_ref().ok_or_else(|| {
            CliError::upstream("Peekaboo MCP stdin is closed").with_operation("mcp")
        })?;
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        let mut encoded = Vec::with_capacity(frame.len() + 1);
        encoded.extend_from_slice(frame);
        encoded.push(b'\n');
        match sender.try_send(WriterCommand { id, frame: encoded }) {
            Ok(()) => {
                self.pending.insert(id, Instant::now());
                Ok(())
            }
            Err(TrySendError::Full(_)) => {
                Err(CliError::upstream("Peekaboo MCP request queue is full").with_operation("mcp"))
            }
            Err(TrySendError::Disconnected(_)) => {
                Err(CliError::upstream("Peekaboo MCP stdin is closed").with_operation("mcp"))
            }
        }
    }

    fn complete(&mut self, id: u64) {
        self.pending.remove(&id);
    }

    fn close(&mut self) {
        drop(self.sender.take());
    }

    fn has_expired(&self, timeout: Duration) -> bool {
        self.pending
            .values()
            .any(|started| started.elapsed() >= timeout)
    }
}

#[derive(Debug)]
enum FrameEvent {
    Frame(Vec<u8>),
    Oversized,
    Eof,
    IoError,
}

fn spawn_pump(
    reader: impl Read + Send + 'static,
    side: StreamSide,
    sender: SyncSender<StreamEvent>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut reader = BufReader::new(reader);
        loop {
            let frame = match read_bounded_frame(&mut reader, MAX_FRAME_BYTES) {
                Ok(BoundedFrame::Frame(frame)) => FrameEvent::Frame(frame),
                Ok(BoundedFrame::Oversized) => FrameEvent::Oversized,
                Ok(BoundedFrame::Eof) => FrameEvent::Eof,
                Err(_) => FrameEvent::IoError,
            };
            let terminal = matches!(frame, FrameEvent::Eof | FrameEvent::IoError);
            let event = match side {
                StreamSide::Client => StreamEvent::Client(frame),
                StreamSide::Upstream => StreamEvent::Upstream(frame),
            };
            if sender.send(event).is_err() || terminal {
                break;
            }
        }
    })
}

fn spawn_writer(
    mut writer: ChildStdin,
    receiver: Receiver<WriterCommand>,
    sender: SyncSender<StreamEvent>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        while let Ok(command) = receiver.recv() {
            if writer
                .write_all(&command.frame)
                .and_then(|_| writer.flush())
                .is_err()
            {
                let _ = sender.send(StreamEvent::Writer(WriterEvent::IoError));
                return;
            }
            if sender
                .send(StreamEvent::Writer(WriterEvent::Written(command.id)))
                .is_err()
            {
                return;
            }
        }
    })
}

#[derive(Debug, PartialEq, Eq)]
enum BoundedFrame {
    Frame(Vec<u8>),
    Oversized,
    Eof,
}

fn read_bounded_frame(reader: &mut impl BufRead, limit: usize) -> std::io::Result<BoundedFrame> {
    let mut frame = Vec::new();
    let mut oversized = false;
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return if frame.is_empty() && !oversized {
                Ok(BoundedFrame::Eof)
            } else if oversized {
                Ok(BoundedFrame::Oversized)
            } else {
                Ok(BoundedFrame::Frame(frame))
            };
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let take = newline.map_or(available.len(), |index| index + 1);
        let payload = newline.map_or(take, |_| take - 1);
        if !oversized {
            if frame.len().saturating_add(payload) > limit {
                oversized = true;
                frame.clear();
            } else {
                frame.extend_from_slice(&available[..payload]);
            }
        }
        reader.consume(take);
        if newline.is_some() {
            return if oversized {
                Ok(BoundedFrame::Oversized)
            } else {
                Ok(BoundedFrame::Frame(frame))
            };
        }
    }
}

fn drain(mut reader: impl Read) {
    let mut buffer = [0u8; 8192];
    while reader.read(&mut buffer).is_ok_and(|read| read != 0) {}
}

struct ChildGuard {
    child: Child,
    process_group: i32,
    reaped: bool,
}

impl ChildGuard {
    fn new(child: Child) -> Self {
        Self {
            process_group: child.id() as i32,
            child,
            reaped: false,
        }
    }

    fn try_wait(&mut self) -> Result<Option<ExitStatus>, CliError> {
        let status = self
            .child
            .try_wait()
            .map_err(|_| CliError::upstream("failed to wait for Peekaboo MCP server"))?;
        if status.is_some() {
            self.reaped = true;
        }
        Ok(status)
    }

    fn kill_remaining_group(&self) {
        #[cfg(unix)]
        // SAFETY: the negative PID targets only the process group created for
        // this child. ESRCH is expected after a fully clean exit.
        unsafe {
            libc::kill(-self.process_group, libc::SIGKILL);
        }
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        self.kill_remaining_group();
        if !self.reaped {
            let _ = self.child.kill();
            let _ = self.child.wait();
            self.reaped = true;
        }
    }
}

fn filter_tools(response: &mut serde_json::Value, profile: crate::cli::ToolProfile) {
    let Some(tools) = response
        .pointer_mut("/result/tools")
        .and_then(serde_json::Value::as_array_mut)
    else {
        return;
    };
    tools.retain(|tool| {
        tool.get("name")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|name| policy::tool_allowed(profile, name))
    });
}

fn write_protocol_error(
    output: &mut impl Write,
    id: serde_json::Value,
    code: i64,
    message: &str,
) -> Result<(), CliError> {
    let body = serde_json::to_vec(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {"code": code, "message": message},
    }))
    .map_err(|_| CliError::upstream("failed to encode MCP protocol error"))?;
    output
        .write_all(&body)
        .and_then(|_| output.write_all(b"\n"))
        .and_then(|_| output.flush())
        .map_err(|_| CliError::upstream("failed to write MCP protocol error"))
}

#[cfg(test)]
mod tests {
    use std::io::{BufReader, Cursor};

    use serde_json::json;

    use super::{BoundedFrame, filter_tools, read_bounded_frame};
    use crate::cli::ToolProfile;

    #[test]
    fn filters_upstream_tool_lists_even_when_upstream_config_is_hostile() {
        let mut response = json!({
            "jsonrpc":"2.0",
            "id":1,
            "result":{"tools":[
                {"name":"see"},
                {"name":"click"},
                {"name":"shell"},
                {"name":"browser"}
            ]}
        });
        filter_tools(&mut response, ToolProfile::Observe);
        let names = response["result"]["tools"]
            .as_array()
            .expect("tools")
            .iter()
            .filter_map(|tool| tool["name"].as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, ["see"]);
    }

    #[test]
    fn frame_reader_rejects_before_retaining_more_than_the_bound() {
        let mut bytes = vec![b'x'; 65];
        bytes.push(b'\n');
        bytes.extend_from_slice(b"{}\n");
        let mut reader = BufReader::new(Cursor::new(bytes));
        assert_eq!(
            read_bounded_frame(&mut reader, 64).expect("oversized"),
            BoundedFrame::Oversized
        );
        assert_eq!(
            read_bounded_frame(&mut reader, 64).expect("next"),
            BoundedFrame::Frame(b"{}".to_vec())
        );
    }
}
