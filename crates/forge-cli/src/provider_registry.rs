//! User-only registry for named forge providers.
//!
//! Records contain connection metadata plus the *name* of an environment
//! variable holding the token. Token values are never accepted or persisted.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use nils_common::cli_contract::schema_version_for;
use serde::{Deserialize, Serialize};
use url::{Host, Url};

use crate::cli::BINARY;
use crate::error::ForgeError;

const REGISTRY_SCHEMA: &str = "forge-cli.providers.v1";
const MAX_REGISTRY_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderKind {
    Forgejo,
}

impl ProviderKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Forgejo => "forgejo",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderRecord {
    pub kind: ProviderKind,
    pub base_url: String,
    pub token_env: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegistryFile {
    schema_version: String,
    #[serde(default)]
    providers: BTreeMap<String, ProviderRecord>,
}

impl Default for RegistryFile {
    fn default() -> Self {
        Self {
            schema_version: REGISTRY_SCHEMA.to_string(),
            providers: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProviderPayload {
    pub name: String,
    pub kind: &'static str,
    pub base_url: String,
    pub token_env: String,
}

impl ProviderPayload {
    fn from_record(name: &str, record: &ProviderRecord) -> Self {
        Self {
            name: name.to_string(),
            kind: record.kind.as_str(),
            base_url: record.base_url.clone(),
            token_env: record.token_env.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProviderListPayload {
    pub providers: Vec<ProviderPayload>,
}

pub fn add(
    name: &str,
    kind: ProviderKind,
    base_url: &str,
    token_env: &str,
) -> Result<ProviderPayload, ForgeError> {
    validate_name(name)?;
    let base_url = normalize_base_url(base_url)?;
    validate_token_env(token_env)?;

    let path = registry_path()?;
    let mut registry = load_from(&path)?;
    if registry.providers.contains_key(name) {
        return Err(validation(
            "provider_exists",
            format!("named provider '{name}' already exists"),
        ));
    }
    let record = ProviderRecord {
        kind,
        base_url,
        token_env: token_env.to_string(),
    };
    registry.providers.insert(name.to_string(), record.clone());
    persist(&path, &registry)?;
    Ok(ProviderPayload::from_record(name, &record))
}

pub fn list() -> Result<ProviderListPayload, ForgeError> {
    let registry = load_from(&registry_path()?)?;
    Ok(ProviderListPayload {
        providers: registry
            .providers
            .iter()
            .map(|(name, record)| ProviderPayload::from_record(name, record))
            .collect(),
    })
}

pub fn view(name: &str) -> Result<ProviderPayload, ForgeError> {
    validate_name(name)?;
    let registry = load_from(&registry_path()?)?;
    let record = registry.providers.get(name).ok_or_else(|| {
        validation(
            "provider_not_found",
            format!("named provider '{name}' does not exist"),
        )
    })?;
    Ok(ProviderPayload::from_record(name, record))
}

pub fn get(name: &str) -> Result<ProviderRecord, ForgeError> {
    validate_name(name)?;
    let registry = load_from(&registry_path()?)?;
    registry.providers.get(name).cloned().ok_or_else(|| {
        validation(
            "provider_not_found",
            format!("named provider '{name}' does not exist"),
        )
    })
}

fn registry_path() -> Result<PathBuf, ForgeError> {
    let config_home = match std::env::var_os("XDG_CONFIG_HOME").filter(|value| !value.is_empty()) {
        Some(path) => PathBuf::from(path),
        None => {
            let home = std::env::var_os("HOME")
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    ForgeError::unavailable(
                        schema(),
                        "provider_registry_unavailable",
                        "cannot resolve the provider registry without XDG_CONFIG_HOME or HOME",
                        None,
                    )
                })?;
            PathBuf::from(home).join(".config")
        }
    };
    Ok(config_home.join("forge-cli/providers.toml"))
}

fn load_from(path: &Path) -> Result<RegistryFile, ForgeError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(RegistryFile::default());
        }
        Err(error) => return Err(registry_io("inspect", path, error)),
    };
    if !metadata.file_type().is_file() {
        return Err(validation(
            "provider_registry_unsafe",
            "provider registry must be a regular file",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(validation(
                "provider_registry_permissions",
                "provider registry must not be readable or writable by group or other users",
            ));
        }
    }
    if metadata.len() > MAX_REGISTRY_BYTES as u64 {
        return Err(validation(
            "provider_registry_too_large",
            format!("provider registry exceeds {MAX_REGISTRY_BYTES} bytes"),
        ));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    File::open(path)
        .and_then(|file| {
            file.take((MAX_REGISTRY_BYTES + 1) as u64)
                .read_to_end(&mut bytes)
        })
        .map_err(|error| registry_io("read", path, error))?;
    if bytes.len() > MAX_REGISTRY_BYTES {
        return Err(validation(
            "provider_registry_too_large",
            format!("provider registry exceeds {MAX_REGISTRY_BYTES} bytes"),
        ));
    }
    let text = String::from_utf8(bytes).map_err(|error| {
        validation(
            "provider_registry_invalid",
            format!("provider registry is not valid UTF-8: {error}"),
        )
    })?;
    let registry: RegistryFile = toml::from_str(&text).map_err(|error| {
        validation(
            "provider_registry_invalid",
            format!("provider registry is not valid TOML: {error}"),
        )
    })?;
    if registry.schema_version != REGISTRY_SCHEMA {
        return Err(validation(
            "provider_registry_version_unsupported",
            format!(
                "unsupported provider registry schema '{}'",
                registry.schema_version
            ),
        ));
    }
    for (name, record) in &registry.providers {
        validate_name(name)?;
        normalize_base_url(&record.base_url)?;
        validate_token_env(&record.token_env)?;
    }
    Ok(registry)
}

fn persist(path: &Path, registry: &RegistryFile) -> Result<(), ForgeError> {
    let parent = path.parent().ok_or_else(|| {
        ForgeError::software(schema(), "provider registry path has no parent", None)
    })?;
    fs::create_dir_all(parent).map_err(|error| registry_io("create directory for", path, error))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
            .map_err(|error| registry_io("secure directory for", path, error))?;
    }
    let text = toml::to_string_pretty(registry).map_err(|error| {
        ForgeError::software(
            schema(),
            "failed to serialize provider registry",
            Some(error.to_string()),
        )
    })?;
    let temp_path = parent.join(format!(".providers.toml.{}.tmp", std::process::id()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&temp_path)
        .map_err(|error| registry_io("create temporary", path, error))?;
    let write_result = (|| {
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        fs::rename(&temp_path, path)?;
        Ok::<(), std::io::Error>(())
    })();
    if let Err(error) = write_result {
        let _ = fs::remove_file(&temp_path);
        return Err(registry_io("write", path, error));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .map_err(|error| registry_io("secure", path, error))?;
    }
    Ok(())
}

fn validate_name(name: &str) -> Result<(), ForgeError> {
    if matches!(name, "github" | "gitlab" | "local") {
        return Err(validation(
            "provider_name_reserved",
            format!("provider name '{name}' is reserved"),
        ));
    }
    let valid = (1..=64).contains(&name.len())
        && name.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
        && name.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || (index > 0 && matches!(byte, b'-' | b'_'))
        });
    if valid {
        Ok(())
    } else {
        Err(validation(
            "provider_name_invalid",
            "provider name must start with a lowercase letter and contain only lowercase letters, digits, '-' or '_'",
        ))
    }
}

fn validate_token_env(value: &str) -> Result<(), ForgeError> {
    let valid = !value.is_empty()
        && value.len() <= 128
        && value
            .as_bytes()
            .first()
            .is_some_and(|byte| byte.is_ascii_uppercase() || *byte == b'_')
        && value
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_');
    if valid {
        Ok(())
    } else {
        Err(validation(
            "provider_token_env_invalid",
            "token environment-variable name must contain only uppercase ASCII letters, digits, and '_'",
        ))
    }
}

fn normalize_base_url(value: &str) -> Result<String, ForgeError> {
    let mut url = Url::parse(value).map_err(|_| invalid_base_url())?;
    if url.cannot_be_a_base()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !matches!(url.scheme(), "http" | "https")
    {
        return Err(invalid_base_url());
    }
    if url.scheme() == "http" {
        let loopback = match url.host() {
            Some(Host::Domain(host)) => host.eq_ignore_ascii_case("localhost"),
            Some(Host::Ipv4(address)) => address.is_loopback(),
            Some(Host::Ipv6(address)) => address.is_loopback(),
            None => false,
        };
        if !loopback {
            return Err(invalid_base_url());
        }
    }
    let normalized_path = url.path().trim_end_matches('/').to_string();
    url.set_path(&normalized_path);
    Ok(url.to_string().trim_end_matches('/').to_string())
}

fn invalid_base_url() -> ForgeError {
    validation(
        "provider_base_url_invalid",
        "provider base URL must be absolute HTTPS (HTTP is allowed only for loopback), without userinfo, query, or fragment",
    )
}

fn registry_io(action: &str, path: &Path, error: std::io::Error) -> ForgeError {
    ForgeError::unavailable(
        schema(),
        "provider_registry_unavailable",
        format!("failed to {action} provider registry '{}'", path.display()),
        Some(error.to_string()),
    )
}

fn validation(kind: &'static str, message: impl Into<String>) -> ForgeError {
    ForgeError::validation(schema(), kind, message, None)
}

fn schema() -> String {
    schema_version_for(BINARY, "error", 1)
}
