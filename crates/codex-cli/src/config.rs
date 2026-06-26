use crate::auth::remote;
use nils_common::shell::{SingleQuoteEscapeStyle, quote_posix_single_with_style};

pub fn show() -> i32 {
    let snapshot = crate::runtime::config_snapshot();

    println!("CODEX_CLI_MODEL={}", snapshot.model);
    println!("CODEX_CLI_REASONING={}", snapshot.reasoning);
    println!(
        "CODEX_CLI_EPHEMERAL_ENABLED={}",
        std::env::var("CODEX_CLI_EPHEMERAL_ENABLED").unwrap_or_default()
    );
    println!(
        "CODEX_ALLOW_DANGEROUS_ENABLED={}",
        snapshot.allow_dangerous_enabled_raw
    );

    if let Some(path) = snapshot.secret_dir {
        println!("CODEX_SECRET_DIR={}", path.to_string_lossy());
    } else {
        println!("CODEX_SECRET_DIR=");
    }

    if let Some(path) = snapshot.auth_file {
        println!("CODEX_AUTH_FILE={}", path.to_string_lossy());
    } else {
        println!("CODEX_AUTH_FILE=");
    }

    if let Some(path) = snapshot.secret_cache_dir {
        println!("CODEX_SECRET_CACHE_DIR={}", path.to_string_lossy());
    } else {
        println!("CODEX_SECRET_CACHE_DIR=");
    }

    println!(
        "CODEX_PROMPT_SEGMENT_ENABLED={}",
        snapshot.prompt_segment_enabled
    );
    println!(
        "CODEX_AUTO_REFRESH_ENABLED={}",
        snapshot.auto_refresh_enabled
    );
    println!(
        "CODEX_AUTO_REFRESH_MIN_DAYS={}",
        snapshot.auto_refresh_min_days
    );
    println!(
        "{}={}",
        remote::ENV_AUTH_REMOTE_SSH,
        std::env::var(remote::ENV_AUTH_REMOTE_SSH).unwrap_or_default()
    );
    println!(
        "{}={}",
        remote::ENV_AUTH_REMOTE_NAME,
        std::env::var(remote::ENV_AUTH_REMOTE_NAME).unwrap_or_default()
    );
    println!(
        "{}={}",
        remote::ENV_AUTH_REMOTE_REFRESH,
        std::env::var(remote::ENV_AUTH_REMOTE_REFRESH).unwrap_or_default()
    );

    0
}

pub fn set(key: &str, value: &str) -> i32 {
    match key {
        "model" | "CODEX_CLI_MODEL" => {
            println!(
                "export CODEX_CLI_MODEL={}",
                quote_posix_single_with_style(value, SingleQuoteEscapeStyle::DoubleQuoteBoundary)
            );
            0
        }
        "reasoning" | "reason" | "CODEX_CLI_REASONING" => {
            println!(
                "export CODEX_CLI_REASONING={}",
                quote_posix_single_with_style(value, SingleQuoteEscapeStyle::DoubleQuoteBoundary)
            );
            0
        }
        "ephemeral" | "CODEX_CLI_EPHEMERAL_ENABLED" => {
            let lowered = value.trim().to_ascii_lowercase();
            if lowered != "true" && lowered != "false" {
                eprintln!(
                    "codex-cli config: ephemeral must be true|false (got: {})",
                    value
                );
                return 64;
            }
            println!("export CODEX_CLI_EPHEMERAL_ENABLED={}", lowered);
            0
        }
        "dangerous" | "allow-dangerous" | "CODEX_ALLOW_DANGEROUS_ENABLED" => {
            let lowered = value.trim().to_ascii_lowercase();
            if lowered != "true" && lowered != "false" {
                eprintln!(
                    "codex-cli config: dangerous must be true|false (got: {})",
                    value
                );
                return 64;
            }
            println!("export CODEX_ALLOW_DANGEROUS_ENABLED={}", lowered);
            0
        }
        "remote-ssh" | "remote_ssh" | "CODEX_AUTH_REMOTE_SSH" => {
            println!(
                "export {}={}",
                remote::ENV_AUTH_REMOTE_SSH,
                quote_posix_single_with_style(value, SingleQuoteEscapeStyle::DoubleQuoteBoundary)
            );
            0
        }
        "remote-name" | "remote_name" | "CODEX_AUTH_REMOTE_NAME" => {
            println!(
                "export {}={}",
                remote::ENV_AUTH_REMOTE_NAME,
                quote_posix_single_with_style(value, SingleQuoteEscapeStyle::DoubleQuoteBoundary)
            );
            0
        }
        "remote-refresh" | "remote_refresh" | "CODEX_AUTH_REMOTE_REFRESH" => {
            let lowered = value.trim().to_ascii_lowercase();
            if lowered != "true" && lowered != "false" {
                eprintln!(
                    "codex-cli config: remote-refresh must be true|false (got: {})",
                    value
                );
                return 64;
            }
            println!("export {}={}", remote::ENV_AUTH_REMOTE_REFRESH, lowered);
            0
        }
        _ => {
            eprintln!("codex-cli config: unknown key: {key}");
            eprintln!(
                "codex-cli config: keys: model|reasoning|ephemeral|dangerous|remote-ssh|remote-name|remote-refresh"
            );
            64
        }
    }
}
