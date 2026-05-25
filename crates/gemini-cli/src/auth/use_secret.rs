use std::path::{Path, PathBuf};

use crate::auth;
use crate::auth::output;
use nils_common::provider_runtime::auth::{SecretFileResolution, resolve_secret_file_by_email};

pub fn run(target: &str) -> i32 {
    run_with_json(target, false)
}

pub fn run_with_json(target: &str, output_json: bool) -> i32 {
    if target.is_empty() {
        if output_json {
            let _ = output::emit_error(
                "auth use",
                "invalid-usage",
                "gemini-use: usage: gemini-use <name|name.json|email>",
                None,
            );
        } else {
            eprintln!("gemini-use: usage: gemini-use <name|name.json|email>");
        }
        return 64;
    }

    if target.contains('/') || target.contains("..") {
        if output_json {
            let _ = output::emit_error(
                "auth use",
                "invalid-secret-name",
                format!("gemini-use: invalid secret name: {target}"),
                Some(output::obj(vec![("target", output::s(target))])),
            );
        } else {
            eprintln!("gemini-use: invalid secret name: {target}");
        }
        return 64;
    }

    let secret_dir = match crate::paths::resolve_secret_dir() {
        Some(dir) => dir,
        None => {
            if output_json {
                let _ = output::emit_error(
                    "auth use",
                    "secret-not-found",
                    format!("gemini-use: secret not found: {target}"),
                    Some(output::obj(vec![("target", output::s(target))])),
                );
            } else {
                eprintln!("gemini-use: secret not found: {target}");
            }
            return 1;
        }
    };

    let is_email = target.contains('@');
    let mut secret_name = target.to_string();
    if !secret_name.ends_with(".json") && !is_email {
        secret_name.push_str(".json");
    }

    if secret_dir.join(&secret_name).is_file() {
        let (code, auth_file) = apply_secret(&secret_dir, &secret_name, output_json);
        if output_json && code == 0 {
            let _ = output::emit_result(
                "auth use",
                output::obj(vec![
                    ("target", output::s(target)),
                    ("matched_secret", output::s(secret_name)),
                    ("applied", output::b(true)),
                    ("auth_file", output::s(auth_file.unwrap_or_default())),
                ]),
            );
        }
        return code;
    }

    match resolve_secret_file_by_email(&secret_dir, target) {
        SecretFileResolution::Exact(name) => {
            let (code, auth_file) = apply_secret(&secret_dir, &name, output_json);
            if output_json && code == 0 {
                let _ = output::emit_result(
                    "auth use",
                    output::obj(vec![
                        ("target", output::s(target)),
                        ("matched_secret", output::s(name)),
                        ("applied", output::b(true)),
                        ("auth_file", output::s(auth_file.unwrap_or_default())),
                    ]),
                );
            }
            code
        }
        SecretFileResolution::Ambiguous { candidates } => {
            if output_json {
                let _ = output::emit_error(
                    "auth use",
                    "ambiguous-secret",
                    format!("gemini-use: identifier matches multiple secrets: {target}"),
                    Some(output::obj(vec![
                        ("target", output::s(target)),
                        (
                            "candidates",
                            output::arr(candidates.into_iter().map(output::s).collect()),
                        ),
                    ])),
                );
            } else {
                eprintln!("gemini-use: identifier matches multiple secrets: {target}");
                eprintln!("gemini-use: candidates: {}", candidates.join(", "));
            }
            2
        }
        SecretFileResolution::NotFound => {
            if output_json {
                let _ = output::emit_error(
                    "auth use",
                    "secret-not-found",
                    format!("gemini-use: secret not found: {target}"),
                    Some(output::obj(vec![("target", output::s(target))])),
                );
            } else {
                eprintln!("gemini-use: secret not found: {target}");
            }
            1
        }
    }
}

fn apply_secret(secret_dir: &Path, secret_name: &str, output_json: bool) -> (i32, Option<String>) {
    let source_file = secret_dir.join(secret_name);
    if !source_file.is_file() {
        if !output_json {
            eprintln!("gemini: requested secret file not found");
        }
        return (1, None);
    }

    let auth_file = match crate::paths::resolve_auth_file() {
        Some(path) => path,
        None => return (1, None),
    };

    if auth_file.is_file() {
        let sync_result = crate::auth::sync::run_with_json(false);
        if sync_result != 0 {
            if !output_json {
                eprintln!("gemini: failed to sync current auth before switching secrets");
            }
            return (1, None);
        }
    }

    let contents = match std::fs::read(&source_file) {
        Ok(contents) => contents,
        Err(_) => return (1, None),
    };

    if auth::write_atomic(&auth_file, &contents, auth::SECRET_FILE_MODE).is_err() {
        return (1, None);
    }

    let iso = auth::last_refresh_from_auth_file(&auth_file).ok().flatten();
    if let Some(timestamp_path) = secret_timestamp_path(&auth_file) {
        let _ = auth::write_timestamp(&timestamp_path, iso.as_deref());
    }

    if !output_json {
        println!("gemini: applied stored secret to {}", auth_file.display());
    }
    (0, Some(auth_file.display().to_string()))
}

fn secret_timestamp_path(target_file: &Path) -> Option<PathBuf> {
    crate::paths::resolve_secret_timestamp_path(target_file)
}
