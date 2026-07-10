//! `codex-cli agent resume <SESSION_ID>` — a foreground convenience wrapper.
//!
//! Codex records the original working directory in each session's
//! `session_meta`, so a known session id can be resumed from any directory: the
//! shared `nils-provider-resume` resolver recovers the cwd, and this module
//! launches the native interactive `codex resume` in that workspace.

use std::path::{Path, PathBuf};

use nils_common::cli_contract::exit;
use nils_common::process as shared_process;
use nils_provider_resume::{
    ResumeIdError, ResumeProvider, ResumeResolveError, normalize_resume_id, resolve_resume_source,
};

const CODEX_BIN: &str = "codex";
const ERROR_PREFIX: &str = "codex-cli agent resume";

/// Options for `codex-cli agent resume`.
pub struct ResumeOptions {
    /// The Codex session id to resume.
    pub session_id: String,
    /// Optional explicit working-directory override, bypassing auto-resolution
    /// for a repository that moved after the session was created.
    pub cwd: Option<PathBuf>,
}

/// Resolve the recorded cwd for `session_id` and launch `codex resume` there,
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
        None => match resolve_resume_source(ResumeProvider::Codex, &session_id) {
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

    let args = resume_argv(&session_id, &cwd);
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    match shared_process::run_status_inherit(CODEX_BIN, &arg_refs) {
        Ok(status) => status.code().unwrap_or(exit::RUNTIME),
        Err(err) => {
            eprintln!("{ERROR_PREFIX}: failed to launch {CODEX_BIN}: {err}");
            exit::RUNTIME
        }
    }
}

/// Build the argv passed to the native `codex` binary. Kept argv-based (never
/// shell-interpolated) so the session id and cwd reach Codex as exact values.
fn resume_argv(session_id: &str, cwd: &Path) -> Vec<String> {
    vec![
        "resume".to_string(),
        session_id.to_string(),
        "--cd".to_string(),
        cwd.to_string_lossy().into_owned(),
        "--no-alt-screen".to_string(),
    ]
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
            format!("no Codex session history records id: {session_id}")
        }
        ResumeResolveError::Ambiguous { cwd_count } => format!(
            "id {session_id} matches {cwd_count} distinct working directories; pass --cd <dir> to choose one"
        ),
        ResumeResolveError::Truncated => format!(
            "Codex history scan was truncated before id {session_id} could be resolved; pass --cd <dir> to resume it directly"
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
    fn resume_argv_is_positional_and_cd_scoped() {
        let argv = resume_argv("sess-1", Path::new("/repo/with space"));
        assert_eq!(
            argv,
            vec![
                "resume".to_string(),
                "sess-1".to_string(),
                "--cd".to_string(),
                "/repo/with space".to_string(),
                "--no-alt-screen".to_string(),
            ]
        );
    }

    #[test]
    fn resume_argv_keeps_session_id_as_a_single_argument() {
        // A pathological id with a space is still one argv slot, never split.
        let argv = resume_argv("weird id", Path::new("/repo"));
        assert_eq!(argv[1], "weird id");
        assert_eq!(argv.iter().filter(|a| a.as_str() == "--cd").count(), 1);
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
            resolve_error_message(ResumeResolveError::NotFound, "abc").contains("no Codex session")
        );
        assert!(
            resolve_error_message(ResumeResolveError::Ambiguous { cwd_count: 3 }, "abc")
                .contains("--cd")
        );
        assert!(resolve_error_message(ResumeResolveError::Truncated, "abc").contains("--cd"));
    }
}
