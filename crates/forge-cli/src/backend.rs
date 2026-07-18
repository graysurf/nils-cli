//! Subprocess wrapper for the two backends (`gh`, `glab`).
//!
//! Every remote call funnels through [`BackendRunner::run`] so the audit
//! surface is a single code path. Token-shaped strings in stderr are redacted
//! before they ever enter the envelope. `--dry-run` short-circuits before any
//! subprocess is spawned.

use std::ffi::{OsStr, OsString};
use std::io::{self, Read};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::process::CommandExt;

use serde::Serialize;

use crate::cli::BINARY;
use crate::error::ForgeError;
use crate::provider::Provider;

/// Environment variable that overrides the `gh` discovery path (testing).
pub const ENV_GH_BIN: &str = "FORGE_CLI_GH_BIN";
/// Environment variable that overrides the `glab` discovery path (testing).
pub const ENV_GLAB_BIN: &str = "FORGE_CLI_GLAB_BIN";

/// Stderr tail length captured before token redaction. Spec: ≤ 2 KiB.
pub const STDERR_TAIL_BYTES: usize = 2 * 1024;
/// Maximum bytes retained from each stream of a timed backend subprocess.
const BACKEND_CAPTURE_LIMIT_BYTES: usize = 8 * 1024 * 1024;

/// Backend program selector. Each variant maps to one external binary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendProgram {
    Gh,
    Glab,
    /// No external binary — served in-process by
    /// [`crate::local::LocalRunner`]. The executable/override-env values are
    /// placeholders never spawned (the local runner dispatches on argv).
    Local,
}

impl BackendProgram {
    /// Resolve the backend program for a provider (current mapping is 1:1).
    pub fn for_provider(provider: Provider) -> Self {
        match provider {
            Provider::GitHub => BackendProgram::Gh,
            Provider::GitLab => BackendProgram::Glab,
            Provider::Local => BackendProgram::Local,
        }
    }

    /// Default executable name on `PATH`.
    pub fn default_executable(self) -> &'static str {
        match self {
            BackendProgram::Gh => "gh",
            BackendProgram::Glab => "glab",
            BackendProgram::Local => "forge-cli-local",
        }
    }

    /// Override env variable consulted before the default name.
    pub fn override_env(self) -> &'static str {
        match self {
            BackendProgram::Gh => ENV_GH_BIN,
            BackendProgram::Glab => ENV_GLAB_BIN,
            BackendProgram::Local => "FORGE_CLI_LOCAL_BIN",
        }
    }

    /// Resolve the actual executable path, honouring the override env.
    pub fn executable(self) -> OsString {
        std::env::var_os(self.override_env())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| OsString::from(self.default_executable()))
    }
}

/// One backend invocation request.
#[derive(Debug, Clone)]
pub struct BackendCall {
    pub program: BackendProgram,
    pub argv: Vec<OsString>,
    env: Vec<(OsString, OsString)>,
}

impl BackendCall {
    pub fn new<I, S>(program: BackendProgram, argv: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        let argv = argv.into_iter().map(Into::into).collect::<Vec<_>>();
        let inferred_host = infer_backend_host(program, &argv);
        let mut call = Self {
            program,
            argv,
            env: Vec::new(),
        };
        match program {
            BackendProgram::Gh => {
                call.set_env("GH_HOST", inferred_host.as_deref().unwrap_or("github.com"));
            }
            BackendProgram::Glab => {
                call.set_env(
                    "GITLAB_HOST",
                    inferred_host.as_deref().unwrap_or("gitlab.com"),
                );
            }
            BackendProgram::Local => {}
        }
        call
    }

    /// Bind this call to the resolved provider authority. Process execution also
    /// removes both provider host variables before applying this call-local
    /// value, so ambient configuration cannot retarget the request.
    pub fn with_host(mut self, provider: Provider, host: impl Into<OsString>) -> Self {
        match provider {
            Provider::GitHub => self.set_env("GH_HOST", host),
            Provider::GitLab => self.set_env("GITLAB_HOST", host),
            Provider::Local => {}
        }
        self
    }

    pub fn resolved_host(&self) -> Option<&str> {
        let key = match self.program {
            BackendProgram::Gh => "GH_HOST",
            BackendProgram::Glab => "GITLAB_HOST",
            BackendProgram::Local => return None,
        };
        self.env
            .iter()
            .find(|(candidate, _)| candidate == OsStr::new(key))
            .and_then(|(_, value)| value.to_str())
    }

    fn set_env(&mut self, key: impl Into<OsString>, value: impl Into<OsString>) {
        let key = key.into();
        self.env.retain(|(candidate, _)| candidate != &key);
        self.env.push((key, value.into()));
    }

    /// Render the full `Vec<String>` argv for `data.plan` rendering. Non-UTF8
    /// arguments are lossy-converted; production argv we construct is ASCII.
    pub fn plan_argv(&self) -> Vec<String> {
        let mut out = Vec::with_capacity(self.argv.len() + 1);
        out.push(self.program.executable().to_string_lossy().into_owned());
        for arg in &self.argv {
            out.push(arg.to_string_lossy().into_owned());
        }
        out
    }
}

fn infer_backend_host(program: BackendProgram, argv: &[OsString]) -> Option<String> {
    for index in 0..argv.len().saturating_sub(1) {
        let Some(flag) = argv[index].to_str() else {
            continue;
        };
        if !matches!(flag, "--hostname" | "--repo" | "-R") {
            continue;
        }
        let Some(value) = argv[index + 1].to_str() else {
            continue;
        };
        if flag == "--hostname" {
            return Some(value.to_string());
        }
        if matches!(flag, "--repo" | "-R") {
            if program == BackendProgram::Gh
                && let Some((authority, slug)) = value.split_once('/')
                && slug.contains('/')
                && let Some(host) = crate::provider::parse_authority(authority)
            {
                return Some(host);
            }
            if let Some(host) = crate::provider::parse_host(value) {
                return Some(host);
            }
        }
    }
    None
}

/// Successful subprocess outcome.
#[derive(Debug, Clone)]
pub struct BackendSuccess {
    pub stdout: String,
    pub stderr: String,
}

/// Raw subprocess outcome. Some backend commands, notably `gh pr checks`,
/// can return useful machine-readable stdout while exiting non-zero for
/// pending or missing checks. Callers that understand those command-specific
/// statuses can opt into this shape.
#[derive(Debug, Clone)]
pub struct BackendOutput {
    pub stdout: String,
    pub stderr: String,
    pub status_success: bool,
    pub exit_code: i32,
}

/// Runner abstraction allowing tests to inject fixtures.
pub trait BackendRunner {
    fn run(&self, call: &BackendCall) -> Result<BackendSuccess, ForgeError>;

    fn run_with_timeout(
        &self,
        call: &BackendCall,
        _timeout: Option<Duration>,
    ) -> Result<BackendSuccess, ForgeError> {
        self.run(call)
    }

    fn run_raw(&self, call: &BackendCall) -> Result<BackendOutput, ForgeError> {
        self.run(call).map(|output| BackendOutput {
            stdout: output.stdout,
            stderr: output.stderr,
            status_success: true,
            exit_code: 0,
        })
    }

    fn run_raw_with_timeout(
        &self,
        call: &BackendCall,
        _timeout: Option<Duration>,
    ) -> Result<BackendOutput, ForgeError> {
        self.run_raw(call)
    }
}

/// Blanket impl so a shared reference to a runner is itself a runner. Lets
/// wrappers (e.g. [`crate::rate_limit::RateLimitedRunner`]) borrow an inner
/// runner without taking ownership, which tests rely on to inspect the inner
/// runner after a gated call.
impl<T: BackendRunner + ?Sized> BackendRunner for &T {
    fn run(&self, call: &BackendCall) -> Result<BackendSuccess, ForgeError> {
        (**self).run(call)
    }

    fn run_with_timeout(
        &self,
        call: &BackendCall,
        timeout: Option<Duration>,
    ) -> Result<BackendSuccess, ForgeError> {
        (**self).run_with_timeout(call, timeout)
    }

    fn run_raw(&self, call: &BackendCall) -> Result<BackendOutput, ForgeError> {
        (**self).run_raw(call)
    }

    fn run_raw_with_timeout(
        &self,
        call: &BackendCall,
        timeout: Option<Duration>,
    ) -> Result<BackendOutput, ForgeError> {
        (**self).run_raw_with_timeout(call, timeout)
    }
}

/// Production runner that actually spawns the backend.
#[derive(Debug, Default)]
pub struct ProcessRunner;

impl BackendRunner for ProcessRunner {
    fn run(&self, call: &BackendCall) -> Result<BackendSuccess, ForgeError> {
        self.run_with_timeout(call, None)
    }

    fn run_with_timeout(
        &self,
        call: &BackendCall,
        timeout: Option<Duration>,
    ) -> Result<BackendSuccess, ForgeError> {
        let output = self.run_raw_with_timeout(call, timeout)?;
        if output.status_success {
            return Ok(BackendSuccess {
                stdout: output.stdout,
                stderr: output.stderr,
            });
        }
        let exe = call.program.executable();
        Err(ForgeError::backend_error(
            schema(),
            format!(
                "{exe} exited with status {exit_code}",
                exe = exe.to_string_lossy(),
                exit_code = output.exit_code
            ),
            Some(output.stderr),
        ))
    }

    fn run_raw(&self, call: &BackendCall) -> Result<BackendOutput, ForgeError> {
        self.run_raw_with_timeout(call, None)
    }

    fn run_raw_with_timeout(
        &self,
        call: &BackendCall,
        timeout: Option<Duration>,
    ) -> Result<BackendOutput, ForgeError> {
        let exe = call.program.executable();
        let mut cmd = Command::new(&exe);
        cmd.env_remove("GH_HOST").env_remove("GITLAB_HOST");
        for (key, value) in &call.env {
            cmd.env(key, value);
        }
        for arg in &call.argv {
            cmd.arg(arg);
        }
        let output = match output_with_timeout(&mut cmd, timeout) {
            Ok(out) => out,
            Err(ProcessOutputError::Io(err)) => {
                let kind = err.kind();
                if matches!(kind, std::io::ErrorKind::NotFound) {
                    return Err(ForgeError::backend_missing(
                        schema(),
                        format!(
                            "{exe} not found on PATH; install it or set {env}",
                            exe = exe.to_string_lossy(),
                            env = call.program.override_env(),
                        ),
                        Some(err.to_string()),
                    ));
                }
                return Err(ForgeError::backend_missing(
                    schema(),
                    format!("failed to launch {exe}: {err}", exe = exe.to_string_lossy()),
                    Some(err.to_string()),
                ));
            }
            Err(ProcessOutputError::Timeout { timeout, output }) => {
                let stderr_full = String::from_utf8_lossy(&output.stderr).into_owned();
                let stderr = redact_and_tail(&stderr_full);
                return Err(ForgeError::unavailable(
                    schema(),
                    "backend_timeout",
                    format!(
                        "{exe} timed out after {timeout}",
                        exe = exe.to_string_lossy(),
                        timeout = format_duration(timeout)
                    ),
                    (!stderr.is_empty()).then_some(stderr),
                ));
            }
            Err(ProcessOutputError::OutputLimit {
                stream,
                limit,
                output,
            }) => {
                let stderr_full = String::from_utf8_lossy(&output.stderr).into_owned();
                let stderr = redact_and_tail(&stderr_full);
                return Err(ForgeError::unavailable(
                    schema(),
                    "backend_output_limit",
                    format!(
                        "{exe} {stream} exceeded the {limit}-byte capture limit",
                        exe = exe.to_string_lossy(),
                        stream = stream.as_str(),
                    ),
                    (!stderr.is_empty()).then_some(stderr),
                ));
            }
        };

        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr_full = String::from_utf8_lossy(&output.stderr).into_owned();
        let stderr = redact_and_tail(&stderr_full);

        let exit_code = output.status.code().unwrap_or(-1);
        let status_success = output.status.success();

        // Distinguish "binary launched but auth failed" from a generic backend
        // error. Both `gh` and `glab` print recognisable cues on auth failure.
        let lower = stderr.to_ascii_lowercase();
        if !status_success
            && (lower.contains("authentication required")
                || lower.contains("not authenticated")
                || lower.contains("could not prompt")
                || lower.contains("auth login")
                || lower.contains("token")
                    && (lower.contains("invalid") || lower.contains("expired")))
        {
            return Err(ForgeError::backend_unauthenticated(
                schema(),
                "backend reports authentication required",
                Some(stderr),
            ));
        }

        // Distinguish an exhausted API rate-limit budget (notably the GraphQL
        // budget, which is metered separately from REST/core) from a generic
        // backend failure. Without this, a drained GraphQL budget surfaces as a
        // misleading "not available" / not-found error and risks a wrong
        // conclusion (sympoies/nils-cli#1051). The
        // `crate::rate_limit::RateLimitedRunner` gate keys its reactive retry
        // off this `backend_rate_limited` discriminator.
        if !status_success && is_rate_limit_stderr(&stderr) {
            return Err(ForgeError::unavailable(
                schema(),
                crate::rate_limit::RATE_LIMITED_KIND,
                "backend reports the GitHub API rate limit is exhausted",
                (!stderr.is_empty()).then_some(stderr),
            ));
        }

        Ok(BackendOutput {
            stdout,
            stderr,
            status_success,
            exit_code,
        })
    }
}

fn output_with_timeout(
    cmd: &mut Command,
    timeout: Option<Duration>,
) -> Result<std::process::Output, ProcessOutputError> {
    output_with_limits(cmd, timeout, BACKEND_CAPTURE_LIMIT_BYTES)
}

pub(crate) fn output_with_limits(
    cmd: &mut Command,
    timeout: Option<Duration>,
    capture_limit: usize,
) -> Result<std::process::Output, ProcessOutputError> {
    let Some(timeout) = timeout.filter(|duration| !duration.is_zero()) else {
        return cmd.output().map_err(ProcessOutputError::Io);
    };

    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    configure_child_group(cmd);
    let mut child = cmd.spawn().map_err(ProcessOutputError::Io)?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| ProcessOutputError::Io(io::Error::other("child stdout was not piped")))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| ProcessOutputError::Io(io::Error::other("child stderr was not piped")))?;
    let (limit_tx, limit_rx) = mpsc::channel();
    let stdout_reader = spawn_output_reader(
        stdout,
        OutputStream::Stdout,
        capture_limit,
        limit_tx.clone(),
    );
    let stderr_reader = spawn_output_reader(stderr, OutputStream::Stderr, capture_limit, limit_tx);
    let started = Instant::now();
    loop {
        if let Ok(stream) = limit_rx.try_recv() {
            kill_child_group(&mut child);
            let status = child.wait().map_err(ProcessOutputError::Io)?;
            let collected = collect_child_output(status, stdout_reader, stderr_reader)
                .map_err(ProcessOutputError::Io)?;
            return Err(ProcessOutputError::OutputLimit {
                stream: collected.limit_exceeded.unwrap_or(stream),
                limit: capture_limit,
                output: collected.output,
            });
        }
        if let Some(status) = child.try_wait().map_err(ProcessOutputError::Io)? {
            let collected = collect_child_output(status, stdout_reader, stderr_reader)
                .map_err(ProcessOutputError::Io)?;
            if let Some(stream) = collected.limit_exceeded {
                return Err(ProcessOutputError::OutputLimit {
                    stream,
                    limit: capture_limit,
                    output: collected.output,
                });
            }
            return Ok(collected.output);
        }
        if started.elapsed() >= timeout {
            kill_child_group(&mut child);
            let status = child.wait().map_err(ProcessOutputError::Io)?;
            let collected = collect_child_output(status, stdout_reader, stderr_reader)
                .map_err(ProcessOutputError::Io)?;
            if let Some(stream) = collected.limit_exceeded {
                return Err(ProcessOutputError::OutputLimit {
                    stream,
                    limit: capture_limit,
                    output: collected.output,
                });
            }
            return Err(ProcessOutputError::Timeout {
                timeout,
                output: collected.output,
            });
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn spawn_output_reader<R>(
    mut reader: R,
    stream: OutputStream,
    capture_limit: usize,
    limit_tx: mpsc::Sender<OutputStream>,
) -> JoinHandle<io::Result<CapturedStream>>
where
    R: Read + Send + 'static,
{
    std::thread::spawn(move || {
        let mut output = Vec::new();
        let mut chunk = [0_u8; 8192];
        loop {
            let read = reader.read(&mut chunk)?;
            if read == 0 {
                return Ok(CapturedStream {
                    output,
                    limit_exceeded: false,
                });
            }
            let remaining = capture_limit.saturating_sub(output.len());
            if read > remaining {
                output.extend_from_slice(&chunk[..remaining]);
                let _ = limit_tx.send(stream);
                return Ok(CapturedStream {
                    output,
                    limit_exceeded: true,
                });
            }
            output.extend_from_slice(&chunk[..read]);
        }
    })
}

fn collect_child_output(
    status: ExitStatus,
    stdout_reader: JoinHandle<io::Result<CapturedStream>>,
    stderr_reader: JoinHandle<io::Result<CapturedStream>>,
) -> io::Result<CollectedOutput> {
    let stdout = stdout_reader
        .join()
        .map_err(|_| io::Error::other("child stdout reader panicked"))??;
    let stderr = stderr_reader
        .join()
        .map_err(|_| io::Error::other("child stderr reader panicked"))??;
    let limit_exceeded = stdout
        .limit_exceeded
        .then_some(OutputStream::Stdout)
        .or_else(|| stderr.limit_exceeded.then_some(OutputStream::Stderr));
    Ok(CollectedOutput {
        output: std::process::Output {
            status,
            stdout: stdout.output,
            stderr: stderr.output,
        },
        limit_exceeded,
    })
}

struct CapturedStream {
    output: Vec<u8>,
    limit_exceeded: bool,
}

struct CollectedOutput {
    output: std::process::Output,
    limit_exceeded: Option<OutputStream>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum OutputStream {
    Stdout,
    Stderr,
}

impl OutputStream {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
        }
    }
}

pub(crate) enum ProcessOutputError {
    Io(io::Error),
    Timeout {
        timeout: Duration,
        output: std::process::Output,
    },
    OutputLimit {
        stream: OutputStream,
        limit: usize,
        output: std::process::Output,
    },
}

fn configure_child_group(cmd: &mut Command) {
    #[cfg(unix)]
    {
        cmd.process_group(0);
    }
}

fn kill_child_group(child: &mut Child) {
    #[cfg(unix)]
    unsafe {
        let pgid = -(child.id() as libc::pid_t);
        let _ = libc::kill(pgid, libc::SIGKILL);
    }
    let _ = child.kill();
}

pub(crate) fn format_duration(duration: Duration) -> String {
    let millis = duration.as_millis();
    if millis < 1_000 {
        format!("{millis}ms")
    } else if millis.is_multiple_of(60_000) {
        format!("{}m", millis / 60_000)
    } else if millis.is_multiple_of(1_000) {
        format!("{}s", millis / 1_000)
    } else {
        format!("{millis}ms")
    }
}

/// Dry-run wrapper: any call returns the would-be argv plan instead of
/// invoking the backend.
pub struct DryRunRunner;

impl BackendRunner for DryRunRunner {
    fn run(&self, _call: &BackendCall) -> Result<BackendSuccess, ForgeError> {
        Err(ForgeError::software(
            schema(),
            "dry-run runner should not be invoked via run(); use plan_envelope",
            None,
        ))
    }
}

/// Envelope payload returned by `--dry-run`. Op handlers should construct one
/// of these instead of calling [`BackendRunner::run`].
#[derive(Debug, Clone, Serialize)]
pub struct DryRunPayload {
    pub provider: &'static str,
    pub plan: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review_convergence: Option<crate::config::ReviewConvergencePolicy>,
}

impl DryRunPayload {
    pub fn new(provider: Provider, call: &BackendCall) -> Self {
        Self {
            provider: provider.as_str(),
            plan: call.plan_argv(),
            review_convergence: None,
        }
    }

    pub fn with_review_convergence(
        mut self,
        policy: &crate::config::ReviewConvergencePolicy,
    ) -> Self {
        self.review_convergence = Some(policy.clone());
        self
    }
}

fn schema() -> String {
    nils_common::cli_contract::schema_version_for(BINARY, "error", 1)
}

/// Detect whether backend stderr reports an exhausted GitHub API rate-limit
/// budget (primary or secondary). Matches the phrasings `gh` surfaces for both
/// REST and GraphQL throttling, e.g. `API rate limit exceeded`,
/// `You have exceeded a secondary rate limit`, and
/// `GraphQL: API rate limit exceeded`.
///
/// The release skill re-implements the same intent as a shell `grep` in
/// `.agents/skills/project-bump-version-tag-release/scripts/project-bump-version-tag-release.sh`
/// (`assert_release_assets_available`); keep the two phrasing sets in sync.
pub fn is_rate_limit_stderr(stderr: &str) -> bool {
    let s = stderr.to_ascii_lowercase();
    s.contains("api rate limit exceeded")
        || s.contains("secondary rate limit")
        || (s.contains("rate limit") && s.contains("exceeded"))
}

/// Replace token-shaped strings and URL userinfo in `s` with redaction
/// markers. Covered token patterns include GitHub and GitLab token families
/// plus case-insensitive `Bearer <token>` authorization values.
pub fn redact_tokens(s: &str) -> String {
    let input = redact_url_userinfo(s);
    let mut out = String::with_capacity(input.len());
    let mut offset = 0;
    while offset < input.len() {
        let rest = &input[offset..];
        if let Some(bearer_scheme) = rest.get(.."Bearer".len())
            && bearer_scheme.eq_ignore_ascii_case("Bearer")
        {
            let after_scheme = &rest["Bearer".len()..];
            let whitespace_len = after_scheme
                .bytes()
                .take_while(|byte| matches!(byte, b' ' | b'\t'))
                .count();
            let token_len = token68_run_len(&after_scheme[whitespace_len..]);
            if whitespace_len > 0 && token_len > 0 {
                out.push_str(bearer_scheme);
                out.push_str(&after_scheme[..whitespace_len]);
                out.push_str("<redacted-token>");
                offset += "Bearer".len() + whitespace_len + token_len;
                continue;
            }
        }

        let mut matched = false;
        for prefix in [
            "github_pat_",
            "ghp_",
            "ghs_",
            "ghr_",
            "gho_",
            "ghu_",
            "glpat-",
        ] {
            if let Some(body) = rest.strip_prefix(prefix) {
                let token_len = token_run_len(body);
                if token_len > 0 {
                    out.push_str("<redacted-token>");
                    offset += prefix.len() + token_len;
                    matched = true;
                    break;
                }
            }
        }
        if matched {
            continue;
        }

        let ch = rest.chars().next().expect("offset is in bounds");
        out.push(ch);
        offset += ch.len_utf8();
    }
    out
}

fn redact_url_userinfo(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut copied_through = 0;
    let mut search_from = 0;
    while let Some(relative) = s[search_from..].find("://") {
        let separator = search_from + relative;
        let mut scheme_start = separator;
        while scheme_start > 0 {
            let byte = bytes[scheme_start - 1];
            if byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'-' | b'.') {
                scheme_start -= 1;
            } else {
                break;
            }
        }
        let scheme = &s[scheme_start..separator];
        if scheme.is_empty()
            || !scheme.as_bytes()[0].is_ascii_alphabetic()
            || !scheme
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'-' | b'.'))
        {
            search_from = separator + 3;
            continue;
        }

        let authority_start = separator + 3;
        let authority_end = s[authority_start..]
            .find(|ch: char| ch.is_ascii_whitespace() || matches!(ch, '/' | '?' | '#'))
            .map_or(s.len(), |relative| authority_start + relative);
        let Some(at_relative) = s[authority_start..authority_end].rfind('@') else {
            search_from = authority_end;
            continue;
        };
        let host_start = authority_start + at_relative + 1;
        out.push_str(&s[copied_through..authority_start]);
        out.push_str("<redacted-userinfo>@");
        copied_through = host_start;
        search_from = authority_end;
    }
    out.push_str(&s[copied_through..]);
    out
}

fn token68_run_len(value: &str) -> usize {
    let body_len = value
        .bytes()
        .take_while(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'+' | b'/')
        })
        .count();
    if body_len == 0 {
        return 0;
    }
    body_len
        + value[body_len..]
            .bytes()
            .take_while(|byte| *byte == b'=')
            .count()
}

fn token_run_len(value: &str) -> usize {
    value
        .bytes()
        .take_while(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
        .count()
}

/// Redact credentials in `s`, then trim the result to the last
/// [`STDERR_TAIL_BYTES`].
pub fn redact_and_tail(s: &str) -> String {
    let redacted = redact_tokens(s);
    if redacted.len() <= STDERR_TAIL_BYTES {
        return redacted;
    }

    // Step forwards to a char boundary to avoid slicing inside a multi-byte
    // UTF-8 sequence.
    let mut start = redacted.len() - STDERR_TAIL_BYTES;
    while !redacted.is_char_boundary(start) {
        start += 1;
    }
    redacted[start..].to_string()
}

/// Probe argv for use in tests / lint assertions.
pub fn argv_to_strings<I: IntoIterator<Item = S>, S: AsRef<OsStr>>(argv: I) -> Vec<String> {
    argv.into_iter()
        .map(|s| s.as_ref().to_string_lossy().into_owned())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn redact_replaces_github_personal_token() {
        let s = "header line\nuser ghp_AbCdEfGhIjKlMnOpQrStUvWxYz0123456789 trailing";
        assert_eq!(
            redact_tokens(s),
            "header line\nuser <redacted-token> trailing"
        );
    }

    #[test]
    fn redact_replaces_github_oauth_token() {
        let s = "gho_AbCdEf012345";
        assert_eq!(redact_tokens(s), "<redacted-token>");
    }

    #[test]
    fn redact_replaces_gitlab_pat() {
        let s = "glpat-Aa1Bb2Cc3";
        assert_eq!(redact_tokens(s), "<redacted-token>");
    }

    #[test]
    fn redact_replaces_bearer_token() {
        let s = "Authorization: Bearer abcd.efgh-12345";
        assert_eq!(redact_tokens(s), "Authorization: Bearer <redacted-token>");
    }

    #[test]
    fn redact_replaces_extended_tokens_and_url_userinfo() {
        for value in [
            "github_pat_Aa1Bb2Cc3",
            "ghu_Aa1Bb2Cc3",
            "bearer Aa1Bb2Cc3",
            "BEARER Aa1Bb2Cc3",
            "bEaReR Aa1Bb2Cc3",
        ] {
            let redacted = redact_tokens(value);
            assert!(!redacted.contains("Aa1Bb2Cc3"), "{redacted}");
            assert!(redacted.contains("<redacted-token>"), "{redacted}");
        }

        let url = "https://alice:credential-value@github.com/o/r";
        let redacted = redact_tokens(url);
        assert_eq!(redacted, "https://<redacted-userinfo>@github.com/o/r");
    }

    #[test]
    fn redact_replaces_bearer_token68_with_permitted_whitespace() {
        for value in [
            "Bearer  secret",
            "bEaReR   abc~def+/==",
            "BEARER\tabc~def+/==",
        ] {
            let redacted = redact_tokens(value);
            assert!(!redacted.contains("secret"), "{redacted}");
            assert!(!redacted.contains("abc~def+/=="), "{redacted}");
            assert!(redacted.contains("<redacted-token>"), "{redacted}");
        }
    }

    #[test]
    fn redact_and_tail_replaces_complete_bearer_token68() {
        let sensitive = "Bearer  abc~def+/==";
        let input = format!("{}{}", "x".repeat(STDERR_TAIL_BYTES), sensitive);
        let redacted = redact_and_tail(&input);
        assert!(!redacted.contains("abc~def+/=="), "{redacted}");
        assert!(redacted.ends_with("Bearer  <redacted-token>"), "{redacted}");
        assert!(redacted.len() <= STDERR_TAIL_BYTES);
    }

    #[test]
    fn redact_preserves_unicode_across_ascii_prefix_boundaries() {
        let input = "123456✓ authenticated";
        assert_eq!(redact_tokens(input), input);
    }

    #[test]
    fn redact_and_tail_redacts_before_truncating() {
        let sensitive = "https://alice:credential-value@github.com/o/r";
        let input = format!("{}{}", "x".repeat(STDERR_TAIL_BYTES), sensitive);
        let redacted = redact_and_tail(&input);
        assert!(!redacted.contains("alice"), "{redacted}");
        assert!(!redacted.contains("credential-value"), "{redacted}");
        assert!(redacted.contains("github.com/o/r"), "{redacted}");
        assert!(redacted.len() <= STDERR_TAIL_BYTES);
    }

    #[test]
    fn redact_keeps_innocent_strings() {
        let s = "hello world ghp not_a_token";
        assert_eq!(redact_tokens(s), "hello world ghp not_a_token");
    }

    #[test]
    fn redact_and_tail_truncates_to_limit() {
        let big = "x".repeat(STDERR_TAIL_BYTES + 512);
        let tailed = redact_and_tail(&big);
        assert!(tailed.len() <= STDERR_TAIL_BYTES);
    }

    #[test]
    fn dry_run_payload_renders_plan() {
        let call = BackendCall::new(BackendProgram::Gh, ["repo", "view", "--json", "name"]);
        let payload = DryRunPayload::new(Provider::GitHub, &call);
        assert_eq!(payload.provider, "github");
        assert_eq!(payload.plan[1..], ["repo", "view", "--json", "name"]);
    }

    #[test]
    fn backend_call_infers_non_default_port_from_github_repo_locator() {
        let call = BackendCall::new(
            BackendProgram::Gh,
            ["pr", "view", "7", "--repo", "internal.example:8443/o/r"],
        );
        assert_eq!(call.resolved_host(), Some("internal.example:8443"));
    }

    #[cfg(unix)]
    #[test]
    fn backend_call_host_inference_skips_unrelated_non_utf8_argv() {
        use std::os::unix::ffi::OsStringExt;

        for (program, flag, value, expected_host) in [
            (
                BackendProgram::Gh,
                "--repo",
                "internal.example:8443/o/r",
                "internal.example:8443",
            ),
            (
                BackendProgram::Glab,
                "--hostname",
                "gitlab.example.test",
                "gitlab.example.test",
            ),
            (
                BackendProgram::Glab,
                "-R",
                "https://gitlab.example.test/group/project",
                "gitlab.example.test",
            ),
        ] {
            let call = BackendCall::new(
                program,
                vec![
                    OsString::from("unrelated"),
                    OsString::from_vec(vec![0xff]),
                    OsString::from(flag),
                    OsString::from(value),
                ],
            );
            assert_eq!(
                call.resolved_host(),
                Some(expected_host),
                "failed to infer the host from {flag} after non-UTF8 argv"
            );
        }
    }

    #[test]
    fn backend_program_executable_honours_override() {
        // Using a thread-safe env override via std::env::set_var is not safe
        // across tests; assert default name only.
        assert_eq!(BackendProgram::Gh.default_executable(), "gh");
        assert_eq!(BackendProgram::Glab.default_executable(), "glab");
    }

    /// Serializes the tests that mutate the process-global `ENV_GH_BIN`
    /// override. `std::env::set_var` / `remove_var` are not thread-safe, so
    /// without this guard a concurrently-running env-mutating test can leave
    /// the override pointing at the wrong binary and flip the expected
    /// `backend_missing` / `backend_timeout` outcome. Poison is recovered so a
    /// panic in one test does not cascade-fail the other.
    static ENV_GH_BIN_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn process_runner_applies_call_local_host_over_ambient_hosts() {
        use std::os::unix::fs::PermissionsExt;

        let _guard = ENV_GH_BIN_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::TempDir::new().expect("tempdir");
        let stub = dir.path().join("gh");
        std::fs::write(
            &stub,
            "#!/bin/sh\nprintf '%s|%s\\n' \"$GH_HOST\" \"${GITLAB_HOST-unset}\"\n",
        )
        .expect("write stub");
        let mut perms = std::fs::metadata(&stub).expect("metadata").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&stub, perms).expect("chmod");

        unsafe {
            std::env::set_var(ENV_GH_BIN, &stub);
            std::env::set_var("GH_HOST", "ambient.ghe.example");
            std::env::set_var("GITLAB_HOST", "ambient.gitlab.example");
        }
        let call = BackendCall::new(BackendProgram::Gh, ["auth", "status"])
            .with_host(Provider::GitHub, "internal.ghe.com:8443");
        let output = ProcessRunner.run(&call).expect("stub succeeds");
        unsafe {
            std::env::remove_var(ENV_GH_BIN);
            std::env::remove_var("GH_HOST");
            std::env::remove_var("GITLAB_HOST");
        }
        assert_eq!(output.stdout.trim(), "internal.ghe.com:8443|unset");
    }

    #[test]
    fn process_runner_reports_missing_backend() {
        let _guard = ENV_GH_BIN_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let runner = ProcessRunner;
        // Use a path that definitely does not exist.
        unsafe {
            std::env::set_var(ENV_GH_BIN, "/tmp/forge-cli-nonexistent-binary-xyz");
        }
        let call = BackendCall::new(BackendProgram::Gh, ["auth", "status"]);
        let err = runner.run(&call).expect_err("missing backend");
        unsafe {
            std::env::remove_var(ENV_GH_BIN);
        }
        assert_eq!(err.kind(), "backend_missing");
    }

    #[test]
    fn process_runner_reports_backend_timeout() {
        use std::os::unix::fs::PermissionsExt;

        let _guard = ENV_GH_BIN_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::TempDir::new().expect("tempdir");
        let stub = dir.path().join("gh");
        std::fs::write(
            &stub,
            "#!/bin/sh\necho 'https://alice:credential-value@github.com/o/r' >&2\nsleep 2\necho should-not-complete\n",
        )
        .expect("write stub");
        let mut perms = std::fs::metadata(&stub).expect("metadata").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&stub, perms).expect("chmod");

        let runner = ProcessRunner;
        unsafe {
            std::env::set_var(ENV_GH_BIN, &stub);
        }
        let call = BackendCall::new(BackendProgram::Gh, ["search", "prs"]);
        let started = std::time::Instant::now();
        let err = runner
            .run_with_timeout(&call, Some(Duration::from_millis(50)))
            .expect_err("timeout");
        unsafe {
            std::env::remove_var(ENV_GH_BIN);
        }
        assert_eq!(err.kind(), "backend_timeout");
        let detail = err.detail().expect("timeout stderr detail");
        assert!(!detail.contains("alice"), "{detail}");
        assert!(!detail.contains("credential-value"), "{detail}");
        assert!(detail.contains("github.com/o/r"), "{detail}");
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "timeout should kill the child promptly"
        );
    }

    #[test]
    fn process_runner_timed_call_drains_large_stdout_without_deadlock() {
        use std::os::unix::fs::PermissionsExt;

        let _guard = ENV_GH_BIN_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::TempDir::new().expect("tempdir");
        let stub = dir.path().join("gh");
        std::fs::write(&stub, "#!/bin/sh\nyes 0123456789 | head -c 2097152\n").expect("write stub");
        let mut perms = std::fs::metadata(&stub).expect("metadata").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&stub, perms).expect("chmod");

        let runner = ProcessRunner;
        unsafe {
            std::env::set_var(ENV_GH_BIN, &stub);
        }
        let call = BackendCall::new(BackendProgram::Gh, ["api", "graphql"]);
        let output = runner
            .run_with_timeout(&call, Some(Duration::from_secs(2)))
            .expect("large output completes before timeout");
        unsafe {
            std::env::remove_var(ENV_GH_BIN);
        }
        assert_eq!(output.stdout.len(), 2 * 1024 * 1024);
    }

    #[test]
    fn process_runner_timed_call_rejects_output_above_capture_limit() {
        use std::os::unix::fs::PermissionsExt;

        let _guard = ENV_GH_BIN_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::TempDir::new().expect("tempdir");
        let stub = dir.path().join("gh");
        std::fs::write(
            &stub,
            format!(
                "#!/bin/sh\necho 'https://alice:credential-value@github.com/o/r' >&2\nyes 0123456789 | head -c {}\n",
                BACKEND_CAPTURE_LIMIT_BYTES + 1
            ),
        )
        .expect("write stub");
        let mut perms = std::fs::metadata(&stub).expect("metadata").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&stub, perms).expect("chmod");

        let runner = ProcessRunner;
        unsafe {
            std::env::set_var(ENV_GH_BIN, &stub);
        }
        let call = BackendCall::new(BackendProgram::Gh, ["api", "graphql"]);
        let started = std::time::Instant::now();
        let result = runner.run_with_timeout(&call, Some(Duration::from_secs(2)));
        unsafe {
            std::env::remove_var(ENV_GH_BIN);
        }
        let err = result.expect_err("output above the capture limit must fail");
        assert_eq!(err.kind(), "backend_output_limit");
        let detail = err.detail().expect("output-limit stderr detail");
        assert!(!detail.contains("alice"), "{detail}");
        assert!(!detail.contains("credential-value"), "{detail}");
        assert!(detail.contains("github.com/o/r"), "{detail}");
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "overflow should terminate the child promptly"
        );
    }

    #[test]
    fn is_rate_limit_stderr_matches_known_phrasings() {
        assert!(is_rate_limit_stderr("API rate limit exceeded for user"));
        assert!(is_rate_limit_stderr(
            "You have exceeded a secondary rate limit"
        ));
        assert!(is_rate_limit_stderr("GraphQL: API rate limit exceeded"));
        // Matched only by the generic third clause (contains "rate limit"
        // AND "exceeded", but neither of the two specific phrasings).
        assert!(is_rate_limit_stderr(
            "your rate limit for this resource has been exceeded"
        ));
        // Non-throttling failures must not be misclassified.
        assert!(!is_rate_limit_stderr("Could not resolve to a Repository"));
        assert!(!is_rate_limit_stderr("rate limit remaining: 4821"));
        assert!(!is_rate_limit_stderr("not found"));
        assert!(!is_rate_limit_stderr(""));
    }

    #[test]
    fn process_runner_classifies_rate_limit_exit_as_backend_rate_limited() {
        use std::os::unix::fs::PermissionsExt;

        let _guard = ENV_GH_BIN_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::TempDir::new().expect("tempdir");
        let stub = dir.path().join("gh");
        std::fs::write(
            &stub,
            "#!/bin/sh\necho 'GraphQL: API rate limit exceeded' >&2\nexit 1\n",
        )
        .expect("write stub");
        let mut perms = std::fs::metadata(&stub).expect("metadata").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&stub, perms).expect("chmod");

        let runner = ProcessRunner;
        unsafe {
            std::env::set_var(ENV_GH_BIN, &stub);
        }
        let call = BackendCall::new(BackendProgram::Gh, ["pr", "view", "1"]);
        let err = runner.run(&call).expect_err("rate limited");
        unsafe {
            std::env::remove_var(ENV_GH_BIN);
        }
        assert_eq!(err.kind(), "backend_rate_limited");
    }

    #[test]
    fn process_runner_non_rate_limit_failure_keeps_generic_kind() {
        use std::os::unix::fs::PermissionsExt;

        let _guard = ENV_GH_BIN_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::TempDir::new().expect("tempdir");
        let stub = dir.path().join("gh");
        std::fs::write(
            &stub,
            "#!/bin/sh\necho 'Could not resolve to a Repository' >&2\nexit 1\n",
        )
        .expect("write stub");
        let mut perms = std::fs::metadata(&stub).expect("metadata").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&stub, perms).expect("chmod");

        let runner = ProcessRunner;
        unsafe {
            std::env::set_var(ENV_GH_BIN, &stub);
        }
        let call = BackendCall::new(BackendProgram::Gh, ["pr", "view", "1"]);
        let err = runner.run(&call).expect_err("generic failure");
        unsafe {
            std::env::remove_var(ENV_GH_BIN);
        }
        // A non-throttling failure must not be classified as rate-limited.
        assert_eq!(err.kind(), "backend_error");
    }
}
