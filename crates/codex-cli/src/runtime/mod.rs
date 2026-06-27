use std::io::Write;
use std::path::PathBuf;

use nils_common::env as shared_env;
use nils_common::provider_runtime;

use crate::auth as codex_auth;
use crate::auth::remote::{ENV_AUTH_REMOTE_NAME, ENV_AUTH_REMOTE_SSH};
use crate::provider_profile::CODEX_PROVIDER_PROFILE;

pub use nils_common::provider_runtime::ExecOptions;
pub use nils_common::provider_runtime::{
    CoreError, CoreErrorCategory, ProviderCategoryHint, auth, json, jwt,
};

pub fn config_snapshot() -> provider_runtime::config::RuntimeConfig {
    provider_runtime::config::snapshot(&CODEX_PROVIDER_PROFILE)
}

pub fn resolve_secret_dir() -> Option<PathBuf> {
    provider_runtime::paths::resolve_secret_dir(&CODEX_PROVIDER_PROFILE)
}

pub fn resolve_auth_file() -> Option<PathBuf> {
    provider_runtime::paths::resolve_auth_file(&CODEX_PROVIDER_PROFILE)
}

pub fn resolve_secret_cache_dir() -> Option<PathBuf> {
    provider_runtime::paths::resolve_secret_cache_dir(&CODEX_PROVIDER_PROFILE)
}

pub fn resolve_feature_dir() -> Option<PathBuf> {
    provider_runtime::paths::resolve_feature_dir(&CODEX_PROVIDER_PROFILE)
}

pub fn resolve_script_dir() -> Option<PathBuf> {
    provider_runtime::paths::resolve_script_dir()
}

pub fn resolve_zdotdir() -> Option<PathBuf> {
    provider_runtime::paths::resolve_zdotdir()
}

pub fn require_allow_dangerous(caller: Option<&str>, stderr: &mut impl Write) -> bool {
    provider_runtime::exec::require_allow_dangerous(&CODEX_PROVIDER_PROFILE, caller, stderr)
}

pub fn allow_dangerous_status(caller: Option<&str>) -> (bool, Option<String>) {
    provider_runtime::exec::allow_dangerous_status(&CODEX_PROVIDER_PROFILE, caller)
}

pub fn check_allow_dangerous(caller: Option<&str>) -> Result<(), CoreError> {
    provider_runtime::exec::check_allow_dangerous(&CODEX_PROVIDER_PROFILE, caller)
}

pub fn exec_dangerous(prompt: &str, caller: &str, stderr: &mut impl Write) -> i32 {
    exec_dangerous_with_options(prompt, caller, stderr, ExecOptions::default())
}

pub fn exec_dangerous_with_options(
    prompt: &str,
    caller: &str,
    stderr: &mut impl Write,
    options: ExecOptions,
) -> i32 {
    if prompt.is_empty() {
        return provider_runtime::exec::exec_dangerous_with_options(
            &CODEX_PROVIDER_PROFILE,
            prompt,
            caller,
            stderr,
            options,
        );
    }
    if !require_allow_dangerous(Some(caller), stderr) {
        return 1;
    }

    let effective_options = ExecOptions {
        ephemeral: options.ephemeral || shared_env::env_truthy("CODEX_CLI_EPHEMERAL_ENABLED"),
    };
    refresh_remote_auth_before_exec();
    provider_runtime::exec::exec_dangerous_with_options(
        &CODEX_PROVIDER_PROFILE,
        prompt,
        caller,
        stderr,
        effective_options,
    )
}

fn refresh_remote_auth_before_exec() {
    if std::env::var(ENV_AUTH_REMOTE_SSH)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .is_none()
        && std::env::var(ENV_AUTH_REMOTE_NAME)
            .ok()
            .filter(|value| !value.trim().is_empty())
            .is_none()
    {
        return;
    }

    let _ = codex_auth::refresh::run_silent(&[]);
}
