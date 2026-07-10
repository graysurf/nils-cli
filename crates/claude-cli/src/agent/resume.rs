//! `claude-cli agent resume <SESSION_ID>` — a foreground convenience wrapper.
//!
//! Claude Code stores sessions per project (keyed by the working directory), so
//! `claude --resume <id>` only finds a session when launched from that project.
//! The shared `nils-provider-resume` resolver recovers the recorded cwd from the
//! project transcripts, and this module launches the native interactive
//! `claude --resume` there.
//!
//! Unlike Codex, the `claude` binary has no `--cd` flag, so the recorded cwd is
//! applied as the child process working directory rather than a launch argument.

use std::path::PathBuf;

use nils_common::cli_contract::exit;
use nils_common::process as shared_process;
use nils_provider_resume::{
    ResumeIdError, ResumeProvider, ResumeResolveError, normalize_resume_id, resolve_resume_source,
};

const CLAUDE_BIN: &str = "claude";
const ERROR_PREFIX: &str = "claude-cli agent resume";

/// Options for `claude-cli agent resume`.
pub struct ResumeOptions {
    /// The Claude session id to resume.
    pub session_id: String,
    /// Optional explicit working-directory override, bypassing auto-resolution
    /// for a repository that moved after the session was created.
    pub cwd: Option<PathBuf>,
}

/// Resolve the recorded cwd for `session_id` and launch `claude --resume` there,
/// propagating the child exit status.
pub fn run(options: &ResumeOptions) -> i32 {
    let session_id = match normalize_resume_id(&options.session_id) {
        Ok(session_id) => session_id,
        Err(err) => {
            eprintln!("{ERROR_PREFIX}: {}", id_error_message(err));
            return exit::USAGE;
        }
    };

    let cwd = match &options.cwd {
        Some(cwd) => cwd.clone(),
        None => match resolve_resume_source(ResumeProvider::Claude, &session_id) {
            Ok(resolved) => resolved.cwd,
            Err(err) => {
                eprintln!(
                    "{ERROR_PREFIX}: {}",
                    resolve_error_message(err, &session_id)
                );
                return resolve_error_exit_code(err);
            }
        },
    };

    if !cwd.is_dir() {
        eprintln!(
            "{ERROR_PREFIX}: working directory is not an existing directory: {}",
            cwd.display()
        );
        return exit::RUNTIME;
    }

    let args = resume_argv(&session_id);
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    // `claude` has no `--cd`; resume in the recorded directory by launching there.
    match shared_process::run_status_inherit_in(CLAUDE_BIN, &arg_refs, &cwd) {
        Ok(status) => status.code().unwrap_or(exit::RUNTIME),
        Err(err) => {
            eprintln!("{ERROR_PREFIX}: failed to launch {CLAUDE_BIN}: {err}");
            exit::RUNTIME
        }
    }
}

/// Build the argv passed to the native `claude` binary. Kept argv-based (never
/// shell-interpolated) so the session id reaches Claude as an exact value.
fn resume_argv(session_id: &str) -> Vec<String> {
    vec!["--resume".to_string(), session_id.to_string()]
}

fn id_error_message(err: ResumeIdError) -> &'static str {
    match err {
        ResumeIdError::Empty => "session id must not be empty",
        ResumeIdError::ControlChar => "session id must not contain control characters",
    }
}

fn resolve_error_message(err: ResumeResolveError, session_id: &str) -> String {
    match err {
        ResumeResolveError::NotFound => {
            format!("no Claude session history records id: {session_id}")
        }
        ResumeResolveError::Ambiguous { cwd_count } => format!(
            "id {session_id} matches {cwd_count} distinct working directories; pass --cd <dir> to choose one"
        ),
        ResumeResolveError::Truncated => format!(
            "Claude history scan was truncated before id {session_id} could be resolved; pass --cd <dir> to resume it directly"
        ),
    }
}

fn resolve_error_exit_code(err: ResumeResolveError) -> i32 {
    match err {
        ResumeResolveError::NotFound | ResumeResolveError::Ambiguous { .. } => exit::DATA,
        ResumeResolveError::Truncated => exit::RUNTIME,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resume_argv_is_resume_flag_plus_id() {
        assert_eq!(
            resume_argv("sess-1"),
            vec!["--resume".to_string(), "sess-1".to_string()]
        );
    }

    #[test]
    fn resume_argv_keeps_session_id_as_a_single_argument() {
        // A pathological id with a space is still one argv slot, never split.
        let argv = resume_argv("weird id");
        assert_eq!(argv, vec!["--resume".to_string(), "weird id".to_string()]);
    }

    #[test]
    fn resolve_error_exit_codes_match_contract() {
        assert_eq!(
            resolve_error_exit_code(ResumeResolveError::NotFound),
            exit::DATA
        );
        assert_eq!(
            resolve_error_exit_code(ResumeResolveError::Ambiguous { cwd_count: 2 }),
            exit::DATA
        );
        assert_eq!(
            resolve_error_exit_code(ResumeResolveError::Truncated),
            exit::RUNTIME
        );
    }

    #[test]
    fn resolve_error_messages_are_actionable() {
        assert!(
            resolve_error_message(ResumeResolveError::NotFound, "abc")
                .contains("no Claude session")
        );
        assert!(
            resolve_error_message(ResumeResolveError::Ambiguous { cwd_count: 3 }, "abc")
                .contains("--cd")
        );
        assert!(resolve_error_message(ResumeResolveError::Truncated, "abc").contains("--cd"));
    }
}
