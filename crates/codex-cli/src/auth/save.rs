use anyhow::Result;
use serde_json::json;
use std::io::{self, IsTerminal, Write};
use std::path::Path;

use crate::auth;
use crate::auth::output::{self, AuthSaveResult};
use crate::paths;
use nils_common::fs;

pub fn run(target: &str, yes: bool) -> Result<i32> {
    run_with_json(target, yes, false)
}

pub fn run_with_json(target: &str, yes: bool, output_json: bool) -> Result<i32> {
    if target.is_empty() {
        return usage_error(
            output_json,
            "codex-save: usage: codex-save [--yes] <secret|secret.json>",
        );
    }

    if auth::is_invalid_secret_target(target) {
        if output_json {
            output::emit_error(
                "auth save",
                "invalid-secret-file-name",
                format!("codex-save: invalid secret file name: {target}"),
                Some(json!({ "target": target })),
            )?;
        } else {
            eprintln!("codex-save: invalid secret file name: {target}");
        }
        return Ok(64);
    }

    let secret_dir = match paths::resolve_secret_dir_from_env() {
        Some(path) => path,
        None => {
            if output_json {
                output::emit_error(
                    "auth save",
                    "secret-dir-not-configured",
                    "codex-save: CODEX_SECRET_DIR is not configured",
                    None,
                )?;
            } else {
                eprintln!("codex-save: CODEX_SECRET_DIR is not configured");
                print_secret_dir_setup_hint("codex-save");
            }
            return Ok(1);
        }
    };

    if !secret_dir.is_dir() {
        if output_json {
            output::emit_error(
                "auth save",
                "secret-dir-not-found",
                format!(
                    "codex-save: CODEX_SECRET_DIR not found: {}",
                    secret_dir.display()
                ),
                Some(json!({
                    "secret_dir": secret_dir.display().to_string(),
                })),
            )?;
        } else {
            eprintln!(
                "codex-save: CODEX_SECRET_DIR not found: {}",
                secret_dir.display()
            );
            print_secret_dir_setup_hint("codex-save");
        }
        return Ok(1);
    }

    let auth_file = match paths::resolve_auth_file() {
        Some(path) => path,
        None => {
            if output_json {
                output::emit_error(
                    "auth save",
                    "auth-file-not-configured",
                    "codex-save: CODEX_AUTH_FILE is not configured",
                    None,
                )?;
            } else {
                eprintln!("codex-save: CODEX_AUTH_FILE is not configured");
            }
            return Ok(1);
        }
    };

    if !auth_file.is_file() {
        if output_json {
            output::emit_error(
                "auth save",
                "auth-file-not-found",
                format!("codex-save: auth file not found: {}", auth_file.display()),
                Some(json!({
                    "auth_file": auth_file.display().to_string(),
                })),
            )?;
        } else {
            eprintln!("codex-save: auth file not found: {}", auth_file.display());
        }
        return Ok(1);
    }

    let secret_name = auth::normalize_secret_file_name(target);
    let target_file = secret_dir.join(&secret_name);
    let mut overwritten = false;
    if target_file.exists() {
        if yes {
            overwritten = true;
        } else if output_json {
            output::emit_error(
                "auth save",
                "overwrite-confirmation-required",
                format!(
                    "codex-save: {} exists; rerun with --yes to overwrite",
                    target_file.display()
                ),
                Some(json!({
                    "target_file": target_file.display().to_string(),
                    "overwritten": false,
                })),
            )?;
            return Ok(1);
        } else if !interactive_io_available() {
            eprintln!(
                "codex-save: {} exists; rerun with --yes to overwrite",
                target_file.display()
            );
            return Ok(1);
        } else {
            match confirm_overwrite(&target_file)? {
                true => {
                    overwritten = true;
                }
                false => {
                    eprintln!(
                        "codex-save: overwrite declined for {}",
                        target_file.display()
                    );
                    return Ok(1);
                }
            }
        }
    }

    let content = match std::fs::read(&auth_file) {
        Ok(content) => content,
        Err(_) => {
            if output_json {
                output::emit_error(
                    "auth save",
                    "auth-file-read-failed",
                    format!(
                        "codex-save: failed to read auth file: {}",
                        auth_file.display()
                    ),
                    Some(json!({
                        "auth_file": auth_file.display().to_string(),
                    })),
                )?;
            } else {
                eprintln!(
                    "codex-save: failed to read auth file: {}",
                    auth_file.display()
                );
            }
            return Ok(1);
        }
    };

    if let Err(err) = fs::write_atomic(&target_file, &content, fs::SECRET_FILE_MODE) {
        if output_json {
            output::emit_error(
                "auth save",
                "save-write-failed",
                format!(
                    "codex-save: failed to write target file {}",
                    target_file.display()
                ),
                Some(json!({
                    "target_file": target_file.display().to_string(),
                    "error": err.to_string(),
                })),
            )?;
        } else {
            eprintln!(
                "codex-save: failed to write target file {}",
                target_file.display()
            );
        }
        return Ok(1);
    }

    let _ = write_target_timestamp(&target_file, &auth_file);

    if output_json {
        output::emit_result(
            "auth save",
            AuthSaveResult {
                auth_file: auth_file.display().to_string(),
                target_file: target_file.display().to_string(),
                saved: true,
                overwritten,
            },
        )?;
    } else {
        println!(
            "codex: saved {} to {}{}",
            auth_file.display(),
            target_file.display(),
            if overwritten { " (overwritten)" } else { "" }
        );
    }

    Ok(0)
}

fn usage_error(output_json: bool, message: &str) -> Result<i32> {
    if output_json {
        output::emit_error("auth save", "invalid-usage", message, None)?;
    } else {
        eprintln!("{message}");
    }
    Ok(64)
}

fn interactive_io_available() -> bool {
    io::stdin().is_terminal() && io::stdout().is_terminal()
}

fn print_secret_dir_setup_hint(prefix: &str) {
    eprintln!("{prefix}: hint: export CODEX_SECRET_DIR=\"$HOME/.config/codex_secrets\"");
    eprintln!("{prefix}: hint: mkdir -p \"$CODEX_SECRET_DIR\" && chmod 700 \"$CODEX_SECRET_DIR\"");
}

fn confirm_overwrite(target: &Path) -> Result<bool> {
    let _ = target;
    eprint!("codex-save: target file exists. overwrite? [y/N]: ");
    io::stderr().flush()?;

    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    let normalized = line.trim().to_ascii_lowercase();
    Ok(matches!(normalized.as_str(), "y" | "yes"))
}

fn write_target_timestamp(target_file: &Path, auth_file: &Path) -> Result<()> {
    let Some(timestamp_file) = paths::resolve_secret_timestamp_path(target_file) else {
        return Ok(());
    };
    let iso = auth::last_refresh_from_auth_file(auth_file).unwrap_or(None);
    fs::write_timestamp(&timestamp_file, iso.as_deref())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::auth::is_invalid_secret_target;
    use crate::paths;
    use nils_test_support::{EnvGuard, GlobalStateLock};

    #[test]
    fn invalid_target_rejects_paths_and_traversal() {
        assert!(is_invalid_secret_target("../a.json"));
        assert!(is_invalid_secret_target("a/b.json"));
        assert!(is_invalid_secret_target(r"a\b.json"));
        assert!(!is_invalid_secret_target("alpha.json"));
    }

    #[test]
    fn resolve_secret_dir_uses_codex_secret_dir_only() {
        let lock = GlobalStateLock::new();
        let _set = EnvGuard::set(&lock, "CODEX_SECRET_DIR", "/tmp/secrets");
        assert_eq!(
            paths::resolve_secret_dir_from_env().expect("secret dir"),
            std::path::PathBuf::from("/tmp/secrets")
        );
    }
}
