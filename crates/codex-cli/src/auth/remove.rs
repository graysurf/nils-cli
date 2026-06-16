use anyhow::Result;
use serde_json::json;
use std::io::{self, IsTerminal, Write};
use std::path::Path;

use crate::auth;
use crate::auth::output::{self, AuthRemoveResult};
use crate::paths;
use nils_common::fs;

pub fn run(target: &str, yes: bool) -> Result<i32> {
    run_with_json(target, yes, false)
}

pub fn run_with_json(target: &str, yes: bool, output_json: bool) -> Result<i32> {
    if target.is_empty() {
        return usage_error(
            output_json,
            "codex-remove: usage: codex-remove [--yes] <secret|secret.json>",
        );
    }

    if auth::is_invalid_secret_target(target) {
        if output_json {
            output::emit_error(
                "auth remove",
                "invalid-secret-file-name",
                format!("codex-remove: invalid secret file name: {target}"),
                Some(json!({ "target": target })),
            )?;
        } else {
            eprintln!("codex-remove: invalid secret file name: {target}");
        }
        return Ok(64);
    }

    let secret_dir = match paths::resolve_secret_dir_from_env() {
        Some(path) => path,
        None => {
            if output_json {
                output::emit_error(
                    "auth remove",
                    "secret-dir-not-configured",
                    "codex-remove: CODEX_SECRET_DIR is not configured",
                    None,
                )?;
            } else {
                eprintln!("codex-remove: CODEX_SECRET_DIR is not configured");
                print_secret_dir_setup_hint("codex-remove");
            }
            return Ok(1);
        }
    };

    if !secret_dir.is_dir() {
        if output_json {
            output::emit_error(
                "auth remove",
                "secret-dir-not-found",
                format!(
                    "codex-remove: CODEX_SECRET_DIR not found: {}",
                    secret_dir.display()
                ),
                Some(json!({
                    "secret_dir": secret_dir.display().to_string(),
                })),
            )?;
        } else {
            eprintln!(
                "codex-remove: CODEX_SECRET_DIR not found: {}",
                secret_dir.display()
            );
            print_secret_dir_setup_hint("codex-remove");
        }
        return Ok(1);
    }

    let secret_name = auth::normalize_secret_file_name(target);
    let target_file = secret_dir.join(&secret_name);
    if !target_file.is_file() {
        if output_json {
            output::emit_error(
                "auth remove",
                "target-not-found",
                format!(
                    "codex-remove: secret file not found: {}",
                    target_file.display()
                ),
                Some(json!({
                    "target_file": target_file.display().to_string(),
                })),
            )?;
        } else {
            eprintln!(
                "codex-remove: secret file not found: {}",
                target_file.display()
            );
        }
        return Ok(1);
    }

    if !yes {
        if !interactive_io_available() {
            emit_confirmation_usage_error(output_json, &target_file)?;
            return Ok(64);
        }
        if output_json {
            emit_confirmation_usage_error(true, &target_file)?;
            return Ok(64);
        }

        if !confirm_remove(&target_file)? {
            eprintln!(
                "codex-remove: removal declined for {}",
                target_file.display()
            );
            return Ok(1);
        }
    }

    if let Err(err) = std::fs::remove_file(&target_file) {
        if output_json {
            output::emit_error(
                "auth remove",
                "remove-failed",
                format!("codex-remove: failed to remove {}", target_file.display()),
                Some(json!({
                    "target_file": target_file.display().to_string(),
                    "error": err.to_string(),
                })),
            )?;
        } else {
            eprintln!("codex-remove: failed to remove {}", target_file.display());
        }
        return Ok(1);
    }

    remove_target_timestamp(&target_file);

    if output_json {
        output::emit_result(
            "auth remove",
            AuthRemoveResult {
                target_file: target_file.display().to_string(),
                removed: true,
            },
        )?;
    } else {
        println!("codex: removed {}", target_file.display());
    }
    Ok(0)
}

fn emit_confirmation_usage_error(output_json: bool, target_file: &Path) -> Result<()> {
    if output_json {
        output::emit_error(
            "auth remove",
            "usage-error",
            format!(
                "codex-remove: {} exists; rerun with --yes to remove",
                target_file.display()
            ),
            Some(json!({
                "target_file": target_file.display().to_string(),
                "removed": false,
            })),
        )?;
    } else {
        eprintln!(
            "codex-remove: {} exists; rerun with --yes to remove",
            target_file.display()
        );
    }
    Ok(())
}

fn usage_error(output_json: bool, message: &str) -> Result<i32> {
    if output_json {
        output::emit_error("auth remove", "invalid-usage", message, None)?;
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

fn confirm_remove(target: &Path) -> Result<bool> {
    let _ = target;
    eprint!("codex-remove: remove target file? [y/N]: ");
    io::stderr().flush()?;

    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    let normalized = line.trim().to_ascii_lowercase();
    Ok(matches!(normalized.as_str(), "y" | "yes"))
}

fn remove_target_timestamp(target_file: &Path) {
    let Some(timestamp_file) = paths::resolve_secret_timestamp_path(target_file) else {
        return;
    };
    let _ = fs::write_timestamp(&timestamp_file, None);
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
