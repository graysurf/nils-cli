use std::io::Write;
use std::path::Path;

use crate::{Result, cli_util, env_file, jwt};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProfileTokenSource {
    None,
    Profile,
    EnvFallback { env_name: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliAuthSource {
    None,
    TokenProfile,
    EnvFallback { env_name: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CliJwtValidationEnv<'a> {
    pub enabled_var: &'a str,
    pub strict_var: &'a str,
    pub leeway_var: &'a str,
    pub tool_label: &'a str,
}

impl From<ProfileTokenSource> for CliAuthSource {
    fn from(value: ProfileTokenSource) -> Self {
        match value {
            ProfileTokenSource::None => Self::None,
            ProfileTokenSource::Profile => Self::TokenProfile,
            ProfileTokenSource::EnvFallback { env_name } => Self::EnvFallback { env_name },
        }
    }
}

pub fn validate_cli_bearer_jwt(
    bearer_token: &str,
    auth_source: &CliAuthSource,
    token_name: &str,
    env: CliJwtValidationEnv<'_>,
    stderr: &mut dyn Write,
) -> Result<()> {
    let enabled = cli_util::bool_from_env(
        std::env::var(env.enabled_var).ok(),
        env.enabled_var,
        true,
        Some(env.tool_label),
        stderr,
    );
    let strict = cli_util::bool_from_env(
        std::env::var(env.strict_var).ok(),
        env.strict_var,
        false,
        Some(env.tool_label),
        stderr,
    );
    let leeway_seconds = cli_util::parse_u64_default(std::env::var(env.leeway_var).ok(), 0, 0);

    let label = match auth_source {
        CliAuthSource::TokenProfile => format!("token profile '{token_name}'"),
        CliAuthSource::EnvFallback { env_name } => env_name.to_string(),
        CliAuthSource::None => "token".to_string(),
    };

    let opts = jwt::JwtValidationOptions {
        enabled,
        strict,
        leeway_seconds: i64::try_from(leeway_seconds).unwrap_or(i64::MAX),
    };

    match jwt::check_bearer_jwt(bearer_token, &label, opts)? {
        jwt::JwtCheck::Ok => Ok(()),
        jwt::JwtCheck::Warn(msg) => {
            let tool_label = env.tool_label.trim();
            if tool_label.is_empty() {
                let _ = writeln!(stderr, "warning: {msg}");
            } else {
                let _ = writeln!(stderr, "{tool_label}: warning: {msg}");
            }
            Ok(())
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileTokenResolution {
    pub bearer_token: Option<String>,
    pub token_name: String,
    pub source: ProfileTokenSource,
}

#[derive(Debug, Clone, Copy)]
pub struct ProfileTokenConfig<'a> {
    pub token_name_arg: Option<&'a str>,
    pub token_name_env_var: &'a str,
    pub token_name_file_var: &'a str,
    pub token_var_prefix: &'a str,
    pub tokens_env: &'a Path,
    pub tokens_local: &'a Path,
    pub tokens_files: &'a [&'a Path],
    pub missing_profile_hint: &'a str,
    pub env_fallback_keys: &'a [&'a str],
}

pub fn resolve_profile_or_env_fallback(
    config: ProfileTokenConfig<'_>,
) -> Result<ProfileTokenResolution> {
    let token_name_arg = config.token_name_arg.and_then(cli_util::trim_non_empty);
    let token_name_env = std::env::var(config.token_name_env_var)
        .ok()
        .and_then(|s| cli_util::trim_non_empty(&s));
    let token_name_file = if !config.tokens_files.is_empty() {
        env_file::read_var_last_wins(config.token_name_file_var, config.tokens_files)?
    } else {
        None
    };

    let token_profile_selected =
        token_name_arg.is_some() || token_name_env.is_some() || token_name_file.is_some();
    let token_name = token_name_arg
        .or(token_name_env)
        .or(token_name_file)
        .unwrap_or_else(|| "default".to_string())
        .to_ascii_lowercase();

    if token_profile_selected {
        let token_key = cli_util::to_env_key(&token_name);
        let token_var = format!("{}{}", config.token_var_prefix, token_key);
        let bearer_token = env_file::read_var_last_wins(&token_var, config.tokens_files)?;
        let Some(bearer_token) = bearer_token else {
            let available = available_token_profiles(
                config.tokens_env,
                config.tokens_local,
                config.token_var_prefix,
            );
            anyhow::bail!(
                "Token profile '{token_name}' is empty/missing (available: {available}). {}",
                config.missing_profile_hint
            );
        };

        return Ok(ProfileTokenResolution {
            bearer_token: Some(bearer_token),
            token_name,
            source: ProfileTokenSource::Profile,
        });
    }

    if let Some((token, env_name)) = resolve_env_fallback(config.env_fallback_keys) {
        return Ok(ProfileTokenResolution {
            bearer_token: Some(token),
            token_name,
            source: ProfileTokenSource::EnvFallback { env_name },
        });
    }

    Ok(ProfileTokenResolution {
        bearer_token: None,
        token_name,
        source: ProfileTokenSource::None,
    })
}

fn available_token_profiles(
    tokens_env: &Path,
    tokens_local: &Path,
    token_var_prefix: &str,
) -> String {
    let mut available = cli_util::list_available_suffixes(tokens_env, token_var_prefix);
    if tokens_local.is_file() {
        available.extend(cli_util::list_available_suffixes(
            tokens_local,
            token_var_prefix,
        ));
        available.sort();
        available.dedup();
    }
    available.retain(|name| name != "name");
    if available.is_empty() {
        "none".to_string()
    } else {
        available.join(" ")
    }
}

pub fn resolve_env_fallback(keys: &[&str]) -> Option<(String, String)> {
    for &key in keys {
        let Ok(value) = std::env::var(key) else {
            continue;
        };
        if let Some(token) = cli_util::trim_non_empty(&value) {
            return Some((token, key.to_string()));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use nils_test_support::{EnvGuard, GlobalStateLock};
    use tempfile::TempDir;

    fn write_file(path: &Path, contents: &str) {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, contents).expect("write");
    }

    fn test_jwt_env() -> CliJwtValidationEnv<'static> {
        CliJwtValidationEnv {
            enabled_var: "TEST_JWT_VALIDATE_ENABLED",
            strict_var: "TEST_JWT_VALIDATE_STRICT",
            leeway_var: "TEST_JWT_VALIDATE_LEEWAY_SECONDS",
            tool_label: "api-test",
        }
    }

    #[test]
    fn resolve_env_fallback_prefers_order() {
        let lock = GlobalStateLock::new();
        let _access = EnvGuard::set(&lock, "ACCESS_TOKEN", "access");
        let _service = EnvGuard::set(&lock, "SERVICE_TOKEN", "service");

        assert_eq!(
            resolve_env_fallback(&["ACCESS_TOKEN", "SERVICE_TOKEN"]),
            Some(("access".to_string(), "ACCESS_TOKEN".to_string()))
        );
    }

    #[test]
    fn cli_auth_source_maps_profile_source_variants() {
        assert_eq!(
            CliAuthSource::from(ProfileTokenSource::None),
            CliAuthSource::None
        );
        assert_eq!(
            CliAuthSource::from(ProfileTokenSource::Profile),
            CliAuthSource::TokenProfile
        );
        assert_eq!(
            CliAuthSource::from(ProfileTokenSource::EnvFallback {
                env_name: "ACCESS_TOKEN".to_string()
            }),
            CliAuthSource::EnvFallback {
                env_name: "ACCESS_TOKEN".to_string()
            }
        );
    }

    #[test]
    fn validate_cli_bearer_jwt_warns_when_non_strict() {
        let lock = GlobalStateLock::new();
        let _enabled = EnvGuard::set(&lock, "TEST_JWT_VALIDATE_ENABLED", "true");
        let _strict = EnvGuard::set(&lock, "TEST_JWT_VALIDATE_STRICT", "false");
        let _leeway = EnvGuard::remove(&lock, "TEST_JWT_VALIDATE_LEEWAY_SECONDS");

        let mut stderr = Vec::new();
        validate_cli_bearer_jwt(
            "not.a.jwt",
            &CliAuthSource::None,
            "default",
            test_jwt_env(),
            &mut stderr,
        )
        .expect("non-strict invalid token should warn");

        let msg = String::from_utf8(stderr).expect("utf8");
        assert!(msg.contains("api-test: warning:"));
        assert!(msg.contains("token for token is not a valid JWT"));
    }

    #[test]
    fn validate_cli_bearer_jwt_errors_when_strict_invalid() {
        let lock = GlobalStateLock::new();
        let _enabled = EnvGuard::set(&lock, "TEST_JWT_VALIDATE_ENABLED", "true");
        let _strict = EnvGuard::set(&lock, "TEST_JWT_VALIDATE_STRICT", "true");
        let _leeway = EnvGuard::remove(&lock, "TEST_JWT_VALIDATE_LEEWAY_SECONDS");

        let mut stderr = Vec::new();
        let err = validate_cli_bearer_jwt(
            "not.a.jwt",
            &CliAuthSource::TokenProfile,
            "svc",
            test_jwt_env(),
            &mut stderr,
        )
        .expect_err("strict invalid token should fail");

        assert!(
            err.to_string()
                .contains("invalid JWT for token profile 'svc'")
        );
    }

    #[test]
    fn resolve_env_fallback_skips_empty_and_whitespace() {
        let lock = GlobalStateLock::new();
        let _access = EnvGuard::set(&lock, "ACCESS_TOKEN", "  ");
        let _service = EnvGuard::set(&lock, "SERVICE_TOKEN", "service");

        assert_eq!(
            resolve_env_fallback(&["ACCESS_TOKEN", "SERVICE_TOKEN"]),
            Some(("service".to_string(), "SERVICE_TOKEN".to_string()))
        );
    }

    #[test]
    fn resolve_env_fallback_returns_none_when_missing() {
        let lock = GlobalStateLock::new();
        let _access = EnvGuard::remove(&lock, "ACCESS_TOKEN");
        let _service = EnvGuard::remove(&lock, "SERVICE_TOKEN");

        assert_eq!(
            resolve_env_fallback(&["ACCESS_TOKEN", "SERVICE_TOKEN"]),
            None
        );
    }

    #[test]
    fn resolve_profile_or_env_fallback_prefers_selected_profile() {
        let lock = GlobalStateLock::new();
        let _access = EnvGuard::set(&lock, "ACCESS_TOKEN", "env-token");
        let _name = EnvGuard::remove(&lock, "REST_TOKEN_NAME");

        let tmp = TempDir::new().expect("tmp");
        let tokens_env = tmp.path().join("tokens.env");
        let tokens_local = tmp.path().join("tokens.local.env");
        write_file(&tokens_env, "REST_TOKEN_SVC=svc-token\n");

        let files = [&tokens_env as &Path, &tokens_local as &Path];
        let resolved = resolve_profile_or_env_fallback(ProfileTokenConfig {
            token_name_arg: Some("svc"),
            token_name_env_var: "REST_TOKEN_NAME",
            token_name_file_var: "REST_TOKEN_NAME",
            token_var_prefix: "REST_TOKEN_",
            tokens_env: &tokens_env,
            tokens_local: &tokens_local,
            tokens_files: &files,
            missing_profile_hint: "hint",
            env_fallback_keys: &["ACCESS_TOKEN", "SERVICE_TOKEN"],
        })
        .expect("profile token resolution");

        assert_eq!(resolved.bearer_token.as_deref(), Some("svc-token"));
        assert_eq!(resolved.token_name, "svc");
        assert_eq!(resolved.source, ProfileTokenSource::Profile);
    }

    #[test]
    fn resolve_profile_or_env_fallback_uses_env_fallback_without_profile_selection() {
        let lock = GlobalStateLock::new();
        let _access = EnvGuard::set(&lock, "ACCESS_TOKEN", "env-token");
        let _name = EnvGuard::remove(&lock, "REST_TOKEN_NAME");

        let tmp = TempDir::new().expect("tmp");
        let tokens_env = tmp.path().join("tokens.env");
        let tokens_local = tmp.path().join("tokens.local.env");
        let files = [&tokens_env as &Path, &tokens_local as &Path];

        let resolved = resolve_profile_or_env_fallback(ProfileTokenConfig {
            token_name_arg: None,
            token_name_env_var: "REST_TOKEN_NAME",
            token_name_file_var: "REST_TOKEN_NAME",
            token_var_prefix: "REST_TOKEN_",
            tokens_env: &tokens_env,
            tokens_local: &tokens_local,
            tokens_files: &files,
            missing_profile_hint: "hint",
            env_fallback_keys: &["ACCESS_TOKEN", "SERVICE_TOKEN"],
        })
        .expect("fallback resolution");

        assert_eq!(resolved.bearer_token.as_deref(), Some("env-token"));
        assert_eq!(resolved.token_name, "default");
        assert_eq!(
            resolved.source,
            ProfileTokenSource::EnvFallback {
                env_name: "ACCESS_TOKEN".to_string()
            }
        );
    }

    #[test]
    fn resolve_profile_or_env_fallback_reports_available_profiles_when_missing() {
        let lock = GlobalStateLock::new();
        let _name = EnvGuard::set(&lock, "REST_TOKEN_NAME", "missing");
        let _access = EnvGuard::remove(&lock, "ACCESS_TOKEN");

        let tmp = TempDir::new().expect("tmp");
        let tokens_env = tmp.path().join("tokens.env");
        let tokens_local = tmp.path().join("tokens.local.env");
        write_file(
            &tokens_env,
            "REST_TOKEN_SVC=svc-token\nREST_TOKEN_DEV=dev-token\n",
        );
        let files = [&tokens_env as &Path, &tokens_local as &Path];

        let err = resolve_profile_or_env_fallback(ProfileTokenConfig {
            token_name_arg: None,
            token_name_env_var: "REST_TOKEN_NAME",
            token_name_file_var: "REST_TOKEN_NAME",
            token_var_prefix: "REST_TOKEN_",
            tokens_env: &tokens_env,
            tokens_local: &tokens_local,
            tokens_files: &files,
            missing_profile_hint: "Set it in setup/rest/tokens.local.env.",
            env_fallback_keys: &["ACCESS_TOKEN", "SERVICE_TOKEN"],
        })
        .expect_err("missing profile should error");

        let text = err.to_string();
        assert!(text.contains("Token profile 'missing' is empty/missing"));
        assert!(text.contains("svc dev") || text.contains("dev svc"));
        assert!(text.contains("setup/rest/tokens.local.env"));
    }

    #[test]
    fn resolve_profile_or_env_fallback_prefers_env_over_file_for_name() {
        let lock = GlobalStateLock::new();
        let _name = EnvGuard::set(&lock, "REST_TOKEN_NAME", "prod");

        let tmp = TempDir::new().expect("tmp");
        let tokens_env = tmp.path().join("tokens.env");
        let tokens_local = tmp.path().join("tokens.local.env");
        write_file(
            &tokens_env,
            "REST_TOKEN_NAME=staging\nREST_TOKEN_PROD=prod-token\n",
        );
        let files = [&tokens_env as &Path, &tokens_local as &Path];

        let resolved = resolve_profile_or_env_fallback(ProfileTokenConfig {
            token_name_arg: None,
            token_name_env_var: "REST_TOKEN_NAME",
            token_name_file_var: "REST_TOKEN_NAME",
            token_var_prefix: "REST_TOKEN_",
            tokens_env: &tokens_env,
            tokens_local: &tokens_local,
            tokens_files: &files,
            missing_profile_hint: "hint",
            env_fallback_keys: &["ACCESS_TOKEN", "SERVICE_TOKEN"],
        })
        .expect("env token name resolution");

        assert_eq!(resolved.bearer_token.as_deref(), Some("prod-token"));
        assert_eq!(resolved.token_name, "prod");
        assert_eq!(resolved.source, ProfileTokenSource::Profile);
    }
}
