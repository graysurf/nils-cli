//! `agent-runtime purge-state` body. Plan 04 Sprint 2 Task 2.3.
//!
//! Removes writable state under `<state_home>` per a required `--scope`:
//!
//! - `out` — clears everything under `<state_home>/out/` (renderer
//!   artifacts, dry-run output captures, agent-output cache).
//! - `backups` — clears everything under `<state_home>/backups/`
//!   (every product's per-run backup tree).
//! - `all` — both of the above.
//!
//! The runtime home is **never** touched (no `--live-home` arg even
//! accepted). `auth*`, `history*`, `sessions*`, `cache*`, and
//! `projects*` live under the product runtime home — not under
//! `<state_home>` — and are therefore outside this command's scope by
//! construction.
//!
//! Confirmation is required by default. `--yes` bypasses the prompt
//! and is logged to stderr in a single audit line containing the scope
//! value — the CLI is the only sanctioned way to set it, and the
//! library writes the audit line at the start of every invocation so
//! the trace is present even on partial failure.

use std::fs;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Required-scope selector. There is **no default**: missing `--scope`
/// exits non-zero at the CLI before reaching this module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// `<state_home>/out/` only.
    Out,
    /// `<state_home>/backups/` only.
    Backups,
    /// Both `out/` and `backups/`.
    All,
}

impl Scope {
    pub fn as_str(self) -> &'static str {
        match self {
            Scope::Out => "out",
            Scope::Backups => "backups",
            Scope::All => "all",
        }
    }
}

impl std::str::FromStr for Scope {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "out" => Ok(Scope::Out),
            "backups" => Ok(Scope::Backups),
            "all" => Ok(Scope::All),
            other => Err(format!(
                "--scope must be one of `out`, `backups`, `all` (got `{other}`)"
            )),
        }
    }
}

#[derive(Debug, Error)]
pub enum PurgeError {
    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// The operator answered the confirmation prompt with anything
    /// other than `y` / `yes`. Treated as a clean refusal — not an
    /// error to log as a stack trace.
    #[error("purge cancelled by operator")]
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PurgeOutcome {
    pub scope: Scope,
    /// Absolute paths of the top-level dirs the executor cleared
    /// (`<state_home>/out`, `<state_home>/backups`, or both). Empty
    /// when the requested dir did not exist — that is a no-op success.
    pub cleared: Vec<PathBuf>,
}

/// Confirmation policy. `Yes` bypasses the prompt and triggers the
/// `--yes` audit line on stderr; `Prompt` reads a single line from
/// the supplied reader and accepts `y` / `yes` (case-insensitive).
pub enum Confirm<'a> {
    Yes,
    Prompt {
        reader: &'a mut dyn BufRead,
        writer: &'a mut dyn Write,
    },
}

/// Execute one purge cycle. Writes the `--yes` audit line (or runs
/// the prompt) before any filesystem mutation, so the operator
/// trace lands even when the subsequent `fs::remove_dir_all` fails.
pub fn run(
    state_home: &Path,
    scope: Scope,
    confirm: Confirm<'_>,
    audit: &mut dyn Write,
) -> Result<PurgeOutcome, PurgeError> {
    match confirm {
        Confirm::Yes => {
            writeln!(
                audit,
                "agent-runtime purge-state: --yes scope={} state_home={}",
                scope.as_str(),
                state_home.display(),
            )
            .ok();
        }
        Confirm::Prompt { reader, writer } => {
            write!(
                writer,
                "agent-runtime purge-state: about to clear scope={} under {} — proceed? [y/N] ",
                scope.as_str(),
                state_home.display(),
            )
            .ok();
            writer.flush().ok();
            let mut line = String::new();
            reader
                .read_line(&mut line)
                .map_err(|source| PurgeError::Io {
                    path: PathBuf::from("<confirm-prompt>"),
                    source,
                })?;
            let answer = line.trim().to_ascii_lowercase();
            if answer != "y" && answer != "yes" {
                return Err(PurgeError::Cancelled);
            }
        }
    }

    let mut cleared = Vec::new();
    match scope {
        Scope::Out => {
            if let Some(p) = clear_dir(state_home, "out")? {
                cleared.push(p);
            }
        }
        Scope::Backups => {
            if let Some(p) = clear_dir(state_home, "backups")? {
                cleared.push(p);
            }
        }
        Scope::All => {
            if let Some(p) = clear_dir(state_home, "out")? {
                cleared.push(p);
            }
            if let Some(p) = clear_dir(state_home, "backups")? {
                cleared.push(p);
            }
        }
    }
    Ok(PurgeOutcome { scope, cleared })
}

/// Remove `<state_home>/<sub>` if present, then recreate it as an
/// empty directory. Returns `Some(path)` when the dir existed before
/// the call (so the outcome can record what was cleared), `None`
/// when it was absent (no-op success).
fn clear_dir(state_home: &Path, sub: &str) -> Result<Option<PathBuf>, PurgeError> {
    let target = state_home.join(sub);
    match fs::symlink_metadata(&target) {
        Ok(meta) if meta.file_type().is_dir() => {
            fs::remove_dir_all(&target).map_err(|source| PurgeError::Io {
                path: target.clone(),
                source,
            })?;
            // Recreate the empty dir so subsequent install / render
            // calls do not have to create it themselves. Mirrors how
            // the install pipeline assumes `<state_home>` shape on
            // entry.
            fs::create_dir_all(&target).map_err(|source| PurgeError::Io {
                path: target.clone(),
                source,
            })?;
            Ok(Some(target))
        }
        Ok(_) => {
            // A non-dir at this path (file, symlink, socket) is a
            // shape violation — refuse to destroy it. The CLI maps
            // this to a clear error message.
            Err(PurgeError::Io {
                path: target,
                source: io::Error::new(
                    io::ErrorKind::InvalidData,
                    "expected directory under <state_home>",
                ),
            })
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(PurgeError::Io {
            path: target,
            source,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use tempfile::TempDir;

    fn seed(state: &Path, sub: &str, file_name: &str, bytes: &str) {
        let dir = state.join(sub);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(file_name), bytes).unwrap();
    }

    #[test]
    fn scope_from_str_accepts_three_values_and_rejects_garbage() {
        assert_eq!("out".parse::<Scope>().unwrap(), Scope::Out);
        assert_eq!("backups".parse::<Scope>().unwrap(), Scope::Backups);
        assert_eq!("all".parse::<Scope>().unwrap(), Scope::All);
        assert!("OUT".parse::<Scope>().is_err());
        assert!("everything".parse::<Scope>().is_err());
    }

    #[test]
    fn yes_writes_audit_line_and_clears_out_only() {
        let tmp = TempDir::new().unwrap();
        let state = tmp.path();
        seed(state, "out", "render.log", "RENDER");
        seed(state, "backups/claude/123/entry", "plugin.json", "BACKUP");

        let mut audit = Vec::new();
        let outcome = run(state, Scope::Out, Confirm::Yes, &mut audit).unwrap();
        assert_eq!(outcome.scope, Scope::Out);
        assert_eq!(outcome.cleared, vec![state.join("out")]);

        let audit_text = String::from_utf8(audit).unwrap();
        assert!(
            audit_text.contains("--yes"),
            "audit must mention --yes: {audit_text}"
        );
        assert!(
            audit_text.contains("scope=out"),
            "audit must name scope: {audit_text}"
        );

        // out/ is now empty, backups/ is untouched.
        assert!(state.join("out").is_dir());
        assert!(state.join("out").read_dir().unwrap().next().is_none());
        assert_eq!(
            fs::read_to_string(state.join("backups/claude/123/entry/plugin.json")).unwrap(),
            "BACKUP"
        );
    }

    #[test]
    fn yes_with_scope_backups_clears_backups_only() {
        let tmp = TempDir::new().unwrap();
        let state = tmp.path();
        seed(state, "out", "render.log", "RENDER");
        seed(state, "backups/claude/123/entry", "plugin.json", "BACKUP");

        let mut audit = Vec::new();
        let outcome = run(state, Scope::Backups, Confirm::Yes, &mut audit).unwrap();
        assert_eq!(outcome.cleared, vec![state.join("backups")]);
        assert_eq!(
            fs::read_to_string(state.join("out/render.log")).unwrap(),
            "RENDER"
        );
        assert!(state.join("backups").is_dir());
        assert!(state.join("backups").read_dir().unwrap().next().is_none());
    }

    #[test]
    fn yes_with_scope_all_clears_both() {
        let tmp = TempDir::new().unwrap();
        let state = tmp.path();
        seed(state, "out", "render.log", "RENDER");
        seed(state, "backups/claude/123/entry", "plugin.json", "BACKUP");

        let mut audit = Vec::new();
        let outcome = run(state, Scope::All, Confirm::Yes, &mut audit).unwrap();
        assert_eq!(
            outcome.cleared,
            vec![state.join("out"), state.join("backups")]
        );
        assert!(state.join("out").read_dir().unwrap().next().is_none());
        assert!(state.join("backups").read_dir().unwrap().next().is_none());
    }

    #[test]
    fn prompt_y_proceeds_and_no_cancels() {
        let tmp = TempDir::new().unwrap();
        let state = tmp.path();
        seed(state, "out", "render.log", "RENDER");

        // First call: operator answers "y\n" — purge proceeds.
        let mut reader = Cursor::new(b"y\n".to_vec());
        let mut writer: Vec<u8> = Vec::new();
        let mut audit = Vec::new();
        let outcome = run(
            state,
            Scope::Out,
            Confirm::Prompt {
                reader: &mut reader,
                writer: &mut writer,
            },
            &mut audit,
        )
        .unwrap();
        assert_eq!(outcome.scope, Scope::Out);
        // Prompt was rendered to the writer.
        let prompt = String::from_utf8(writer).unwrap();
        assert!(
            prompt.contains("scope=out"),
            "prompt missing scope: {prompt}"
        );
        // No audit line for the prompt path.
        assert!(
            audit.is_empty(),
            "audit must stay empty in prompt path: {audit:?}"
        );

        // Second call: re-seed and answer "n" — purge cancelled.
        seed(state, "out", "render.log", "RENDER-AGAIN");
        let mut reader = Cursor::new(b"n\n".to_vec());
        let mut writer: Vec<u8> = Vec::new();
        let mut audit2 = Vec::new();
        let err = run(
            state,
            Scope::Out,
            Confirm::Prompt {
                reader: &mut reader,
                writer: &mut writer,
            },
            &mut audit2,
        )
        .unwrap_err();
        assert!(matches!(err, PurgeError::Cancelled));
        // Content survived the refusal.
        assert_eq!(
            fs::read_to_string(state.join("out/render.log")).unwrap(),
            "RENDER-AGAIN"
        );
    }

    #[test]
    fn missing_subdir_is_clean_noop_not_error() {
        let tmp = TempDir::new().unwrap();
        let state = tmp.path();
        // Neither out/ nor backups/ exists.
        let mut audit = Vec::new();
        let outcome = run(state, Scope::All, Confirm::Yes, &mut audit).unwrap();
        assert!(outcome.cleared.is_empty());
    }

    #[test]
    fn refuses_non_dir_at_scope_path() {
        let tmp = TempDir::new().unwrap();
        let state = tmp.path();
        fs::create_dir_all(state).unwrap();
        // Plant a regular file where the dir should be.
        fs::write(state.join("out"), "regular-file").unwrap();

        let mut audit = Vec::new();
        let err = run(state, Scope::Out, Confirm::Yes, &mut audit).unwrap_err();
        match err {
            PurgeError::Io { path, .. } => assert_eq!(path, state.join("out")),
            other => panic!("expected Io shape-violation error, got {other:?}"),
        }
        // The audit line still landed before the IO error.
        assert!(String::from_utf8(audit).unwrap().contains("scope=out"));
        // The operator's file survived — we refused to destroy it.
        assert_eq!(
            fs::read_to_string(state.join("out")).unwrap(),
            "regular-file"
        );
    }
}
