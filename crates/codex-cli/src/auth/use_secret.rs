use anyhow::Result;
use serde_json::json;
use std::path::{Path, PathBuf};

use crate::auth;
use crate::auth::output::{self, AuthUseResult};
use crate::paths;
use nils_common::fs;
use nils_common::provider_runtime::auth::{SecretFileResolution, resolve_secret_file_by_email};

pub fn run(target: &str) -> Result<i32> {
    run_with_json(target, false)
}

pub fn run_with_json(target: &str, output_json: bool) -> Result<i32> {
    if target.is_empty() {
        if output_json {
            output::emit_error(
                "auth use",
                "invalid-usage",
                "codex-use: usage: codex-use <name|name.json|email>",
                None,
            )?;
        } else {
            eprintln!("codex-use: usage: codex-use <name|name.json|email>");
        }
        return Ok(64);
    }

    if auth::is_invalid_secret_target(target) {
        if output_json {
            output::emit_error(
                "auth use",
                "invalid-secret-name",
                format!("codex-use: invalid secret name: {target}"),
                Some(json!({
                    "target": target,
                })),
            )?;
        } else {
            eprintln!("codex-use: invalid secret name: {target}");
        }
        return Ok(64);
    }

    let secret_dir = match paths::resolve_secret_dir() {
        Some(dir) => dir,
        None => {
            if output_json {
                output::emit_error(
                    "auth use",
                    "secret-not-found",
                    format!("codex-use: secret not found: {target}"),
                    Some(json!({
                        "target": target,
                    })),
                )?;
            } else {
                eprintln!("codex-use: secret not found: {target}");
            }
            return Ok(1);
        }
    };

    let is_email = target.contains('@');
    let secret_name = if is_email {
        target.to_string()
    } else {
        auth::normalize_secret_file_name(target)
    };

    if secret_dir.join(&secret_name).is_file() {
        let (code, auth_file) = apply_secret(&secret_dir, &secret_name, output_json)?;
        if output_json && code == 0 {
            output::emit_result(
                "auth use",
                AuthUseResult {
                    target: target.to_string(),
                    matched_secret: Some(secret_name),
                    applied: true,
                    auth_file: auth_file.unwrap_or_default(),
                },
            )?;
        }
        return Ok(code);
    }

    match resolve_secret_file_by_email(&secret_dir, target) {
        SecretFileResolution::Exact(name) => {
            let (code, auth_file) = apply_secret(&secret_dir, &name, output_json)?;
            if output_json && code == 0 {
                output::emit_result(
                    "auth use",
                    AuthUseResult {
                        target: target.to_string(),
                        matched_secret: Some(name),
                        applied: true,
                        auth_file: auth_file.unwrap_or_default(),
                    },
                )?;
            }
            Ok(code)
        }
        SecretFileResolution::Ambiguous { candidates } => {
            if output_json {
                output::emit_error(
                    "auth use",
                    "ambiguous-secret",
                    format!("codex-use: identifier matches multiple secrets: {target}"),
                    Some(json!({
                        "target": target,
                        "candidates": candidates,
                    })),
                )?;
            } else {
                eprintln!("codex-use: identifier matches multiple secrets: {target}");
                eprintln!("codex-use: candidates: {}", candidates.join(", "));
            }
            Ok(2)
        }
        SecretFileResolution::NotFound => {
            if output_json {
                output::emit_error(
                    "auth use",
                    "secret-not-found",
                    format!("codex-use: secret not found: {target}"),
                    Some(json!({
                        "target": target,
                    })),
                )?;
            } else {
                eprintln!("codex-use: secret not found: {target}");
            }
            Ok(1)
        }
    }
}

fn apply_secret(
    secret_dir: &Path,
    secret_name: &str,
    output_json: bool,
) -> Result<(i32, Option<String>)> {
    let source_file = secret_dir.join(secret_name);
    if !source_file.is_file() {
        if !output_json {
            eprintln!("codex: requested secret file not found");
        }
        return Ok((1, None));
    }

    let auth_file = match paths::resolve_auth_file() {
        Some(path) => path,
        None => return Ok((1, None)),
    };

    if auth_file.is_file() {
        let sync_result = crate::auth::sync::run_with_json(false)?;
        if sync_result != 0 {
            if !output_json {
                eprintln!("codex: failed to sync current auth before switching secrets");
            }
            return Ok((1, None));
        }
    }

    let contents = std::fs::read(&source_file)?;
    fs::write_atomic(&auth_file, &contents, fs::SECRET_FILE_MODE)?;

    let iso = auth::last_refresh_from_auth_file(&auth_file).unwrap_or(None);
    let timestamp_path = secret_timestamp_path(&auth_file)?;
    fs::write_timestamp(&timestamp_path, iso.as_deref())?;

    if !output_json {
        println!("codex: applied stored secret to {}", auth_file.display());
    }
    Ok((0, Some(auth_file.display().to_string())))
}

fn secret_timestamp_path(target_file: &Path) -> Result<PathBuf> {
    paths::resolve_secret_timestamp_path(target_file)
        .ok_or_else(|| anyhow::anyhow!("CODEX_SECRET_CACHE_DIR not resolved"))
}

#[cfg(test)]
mod tests {
    use super::secret_timestamp_path;
    use nils_test_support::{EnvGuard, GlobalStateLock};
    use pretty_assertions::assert_eq;
    use std::path::Path;

    #[test]
    fn secret_timestamp_path_uses_cache_dir_and_default_file_name() {
        let lock = GlobalStateLock::new();
        let dir = tempfile::TempDir::new().expect("tempdir");
        let cache = dir.path().join("cache");
        std::fs::create_dir_all(&cache).expect("cache");
        let cache_value = cache.to_string_lossy().to_string();
        let _guard = EnvGuard::set(&lock, "CODEX_SECRET_CACHE_DIR", &cache_value);

        let with_name =
            secret_timestamp_path(Path::new("/tmp/demo-auth.json")).expect("timestamp path");
        assert_eq!(with_name, cache.join("demo-auth.json.timestamp"));

        let without_name = secret_timestamp_path(Path::new("")).expect("timestamp path");
        assert_eq!(without_name, cache.join("auth.json.timestamp"));
    }
}
