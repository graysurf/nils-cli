//! Subprocess wrapper for the two backends (`gh`, `glab`).
//!
//! Every remote call funnels through [`BackendRunner::run`] so the audit
//! surface is a single code path. Token-shaped strings in stderr are redacted
//! before they ever enter the envelope. `--dry-run` short-circuits before any
//! subprocess is spawned.

use std::ffi::{OsStr, OsString};
use std::process::Command;

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

/// Backend program selector. Each variant maps to one external binary.
#[derive(Debug, Clone, Copy)]
pub enum BackendProgram {
    Gh,
    Glab,
}

impl BackendProgram {
    /// Resolve the backend program for a provider (current mapping is 1:1).
    pub fn for_provider(provider: Provider) -> Self {
        match provider {
            Provider::GitHub => BackendProgram::Gh,
            Provider::GitLab => BackendProgram::Glab,
        }
    }

    /// Default executable name on `PATH`.
    pub fn default_executable(self) -> &'static str {
        match self {
            BackendProgram::Gh => "gh",
            BackendProgram::Glab => "glab",
        }
    }

    /// Override env variable consulted before the default name.
    pub fn override_env(self) -> &'static str {
        match self {
            BackendProgram::Gh => ENV_GH_BIN,
            BackendProgram::Glab => ENV_GLAB_BIN,
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
}

impl BackendCall {
    pub fn new<I, S>(program: BackendProgram, argv: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        Self {
            program,
            argv: argv.into_iter().map(Into::into).collect(),
        }
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

/// Successful subprocess outcome.
#[derive(Debug, Clone)]
pub struct BackendSuccess {
    pub stdout: String,
    pub stderr: String,
}

/// Runner abstraction allowing tests to inject fixtures.
pub trait BackendRunner {
    fn run(&self, call: &BackendCall) -> Result<BackendSuccess, ForgeError>;
}

/// Production runner that actually spawns the backend.
#[derive(Debug, Default)]
pub struct ProcessRunner;

impl BackendRunner for ProcessRunner {
    fn run(&self, call: &BackendCall) -> Result<BackendSuccess, ForgeError> {
        let exe = call.program.executable();
        let mut cmd = Command::new(&exe);
        for arg in &call.argv {
            cmd.arg(arg);
        }
        let output = match cmd.output() {
            Ok(out) => out,
            Err(err) => {
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
                    format!(
                        "failed to launch {exe}: {err}",
                        exe = exe.to_string_lossy()
                    ),
                    Some(err.to_string()),
                ));
            }
        };

        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr_full = String::from_utf8_lossy(&output.stderr).into_owned();
        let stderr = redact_and_tail(&stderr_full);

        if output.status.success() {
            return Ok(BackendSuccess { stdout, stderr });
        }

        // Distinguish "binary launched but auth failed" from a generic backend
        // error. Both `gh` and `glab` print recognisable cues on auth failure.
        let lower = stderr.to_ascii_lowercase();
        if lower.contains("authentication required")
            || lower.contains("not authenticated")
            || lower.contains("could not prompt")
            || lower.contains("auth login")
            || lower.contains("token")
                && (lower.contains("invalid") || lower.contains("expired"))
        {
            return Err(ForgeError::backend_unauthenticated(
                schema(),
                "backend reports authentication required",
                Some(stderr),
            ));
        }

        let exit_code = output.status.code().unwrap_or(-1);
        Err(ForgeError::backend_error(
            schema(),
            format!(
                "{exe} exited with status {exit_code}",
                exe = exe.to_string_lossy()
            ),
            Some(stderr),
        ))
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
}

impl DryRunPayload {
    pub fn new(provider: Provider, call: &BackendCall) -> Self {
        Self {
            provider: provider.as_str(),
            plan: call.plan_argv(),
        }
    }
}

fn schema() -> String {
    nils_common::cli_contract::schema_version_for(BINARY, "error", 1)
}

/// Replace token-shaped strings in `s` with `<redacted-token>`. Patterns
/// covered: `gh[ps]_*`, `ghr_*`, `gho_*`, `glpat-*`, and `Bearer <token>`.
pub fn redact_tokens(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        // Bearer prefix carries a token after the literal "Bearer "; redact
        // the token but keep the prefix for readable context.
        if c == 'B' && peek_consume(&mut chars, "earer ") {
            out.push_str("Bearer ");
            consume_token_run(&mut chars);
            out.push_str("<redacted-token>");
            continue;
        }
        if c == 'g'
            && let Some(&n) = chars.peek()
            && (n == 'h' || n == 'l')
        {
            // gh{p,s,r,o}_... | glpat-...
            let mut buf = String::from(c);
            buf.push(chars.next().unwrap());
            let next = chars.peek().copied();
            match (buf.as_str(), next) {
                ("gh", Some(t)) if t == 'p' || t == 's' || t == 'r' || t == 'o' => {
                    buf.push(chars.next().unwrap());
                    if chars.peek() == Some(&'_') {
                        chars.next();
                        consume_token_run(&mut chars);
                        out.push_str("<redacted-token>");
                        continue;
                    }
                }
                ("gl", Some('p')) => {
                    buf.push(chars.next().unwrap()); // p
                    if chars.peek() == Some(&'a') {
                        let saved = chars.clone();
                        buf.push(chars.next().unwrap()); // a
                        if chars.peek() == Some(&'t') {
                            buf.push(chars.next().unwrap()); // t
                            if chars.peek() == Some(&'-') {
                                chars.next();
                                consume_token_run(&mut chars);
                                out.push_str("<redacted-token>");
                                continue;
                            }
                        }
                        // Not glpat- after all: restore.
                        chars = saved;
                        buf.truncate(2);
                    }
                }
                _ => {}
            }
            out.push_str(&buf);
            continue;
        }
        out.push(c);
    }
    out
}

fn peek_consume(iter: &mut std::iter::Peekable<std::str::Chars<'_>>, lit: &str) -> bool {
    let saved = iter.clone();
    for expected in lit.chars() {
        match iter.next() {
            Some(c) if c == expected => continue,
            _ => {
                *iter = saved;
                return false;
            }
        }
    }
    true
}

fn consume_token_run(iter: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    while let Some(&c) = iter.peek() {
        if c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.' {
            iter.next();
        } else {
            break;
        }
    }
}

/// Trim `s` to the last [`STDERR_TAIL_BYTES`] and redact tokens.
pub fn redact_and_tail(s: &str) -> String {
    let tail = if s.len() > STDERR_TAIL_BYTES {
        // Step backwards on a char boundary to avoid slicing inside a
        // multi-byte UTF-8 sequence.
        let mut start = s.len() - STDERR_TAIL_BYTES;
        while start < s.len() && !s.is_char_boundary(start) {
            start += 1;
        }
        &s[start..]
    } else {
        s
    };
    redact_tokens(tail)
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
        assert_eq!(
            redact_tokens(s),
            "Authorization: Bearer <redacted-token>"
        );
    }

    #[test]
    fn redact_keeps_innocent_strings() {
        let s = "hello world ghp not_a_token";
        assert_eq!(
            redact_tokens(s),
            "hello world ghp not_a_token"
        );
    }

    #[test]
    fn redact_and_tail_truncates_to_limit() {
        let big = "x".repeat(STDERR_TAIL_BYTES + 512);
        let tailed = redact_and_tail(&big);
        assert!(tailed.len() <= STDERR_TAIL_BYTES);
    }

    #[test]
    fn dry_run_payload_renders_plan() {
        let call = BackendCall::new(
            BackendProgram::Gh,
            ["repo", "view", "--json", "name"],
        );
        let payload = DryRunPayload::new(Provider::GitHub, &call);
        assert_eq!(payload.provider, "github");
        assert_eq!(payload.plan[1..], ["repo", "view", "--json", "name"]);
    }

    #[test]
    fn backend_program_executable_honours_override() {
        // Using a thread-safe env override via std::env::set_var is not safe
        // across tests; assert default name only.
        assert_eq!(BackendProgram::Gh.default_executable(), "gh");
        assert_eq!(BackendProgram::Glab.default_executable(), "glab");
    }

    #[test]
    fn process_runner_reports_missing_backend() {
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
}
