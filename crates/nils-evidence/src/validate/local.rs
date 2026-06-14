//! Validator for the machine-local config at
//! `$XDG_CONFIG_HOME/agent-evidence-archive/config.yaml`.
//!
//! Schema (v1):
//!
//! ```yaml
//! version: 1
//! archive_clone_path: ~/.local/share/agent-evidence-archive
//! working_repo_roots:        # optional, reserved for future use
//!   - ~/Project
//! performance:
//!   migrate_batch_size: 50
//! ```
//!
//! Adapted from plan-archive's `validate/local.rs` with one deliberate
//! rename: the performance knob is `migrate_batch_size` (evidence has no
//! `refresh` command, so `refresh_batch_size` would be a dead field). The
//! default `archive_clone_path` follows the XDG data-home convention rather
//! than plan-archive's `~/Project/...` checkout.
//!
//! Special behaviour:
//!
//! - Missing file: returns the documented defaults and exit code 0.
//! - Malformed file: returns a structured parse error.
//! - `~` and `$VAR` are expanded in `archive_clone_path` and
//!   `working_repo_roots`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::ValidationWarning;

const SUPPORTED_VERSION: u32 = 1;
const DEFAULT_MIGRATE_BATCH_SIZE: u32 = 50;
const DEFAULT_ARCHIVE_CLONE_PATH: &str = "~/.local/share/agent-evidence-archive";

/// Parsed and normalized machine-local config.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct LocalConfig {
    pub version: u32,
    pub archive_clone_path: PathBuf,
    pub working_repo_roots: Vec<PathBuf>,
    pub performance: LocalPerformance,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct LocalPerformance {
    pub migrate_batch_size: u32,
}

/// Whether the validator filled in defaults because the file was absent.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub enum LocalSource {
    Defaults,
    File,
}

/// Output payload of a successful `validate-local` run.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct LocalValidationData {
    pub source: LocalSource,
    pub config: LocalConfig,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct LocalValidation {
    pub data: LocalValidationData,
    pub warnings: Vec<ValidationWarning>,
}

/// Validation failure modes for the machine-local config.
#[derive(Debug, Error)]
pub enum LocalValidationError {
    #[error("local config could not be parsed as YAML: {0}")]
    Parse(String),
    #[error("local config version {found} is not supported (expected {SUPPORTED_VERSION})")]
    UnsupportedVersion { found: u32 },
    #[error("local config `performance.migrate_batch_size` must be > 0")]
    InvalidBatchSize,
    #[error("local config could not be read: {0}")]
    Io(String),
}

impl LocalValidationError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Parse(_) => "local-parse-error",
            Self::UnsupportedVersion { .. } => "local-unsupported-version",
            Self::InvalidBatchSize => "local-invalid-batch-size",
            Self::Io(_) => "local-io-error",
        }
    }
}

/// Validate the raw machine-local config content.
pub fn validate_local_yaml(input: &str) -> Result<LocalValidation, LocalValidationError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Ok(default_validation(LocalSource::Defaults));
    }

    let raw: RawLocalFile = serde_yaml_ng::from_str(input)
        .map_err(|err| LocalValidationError::Parse(err.to_string()))?;

    let version = raw.version.unwrap_or(SUPPORTED_VERSION);
    if version != SUPPORTED_VERSION {
        return Err(LocalValidationError::UnsupportedVersion { found: version });
    }

    let archive_clone_path = raw
        .archive_clone_path
        .as_deref()
        .map(expand_path)
        .unwrap_or_else(expanded_default_archive_clone_path);

    let working_repo_roots = raw
        .working_repo_roots
        .unwrap_or_default()
        .iter()
        .map(|p| expand_path(p))
        .collect();

    let migrate_batch_size = raw
        .performance
        .as_ref()
        .and_then(|p| p.migrate_batch_size)
        .unwrap_or(DEFAULT_MIGRATE_BATCH_SIZE);
    if migrate_batch_size == 0 {
        return Err(LocalValidationError::InvalidBatchSize);
    }

    Ok(LocalValidation {
        data: LocalValidationData {
            source: LocalSource::File,
            config: LocalConfig {
                version,
                archive_clone_path,
                working_repo_roots,
                performance: LocalPerformance { migrate_batch_size },
            },
        },
        warnings: Vec::new(),
    })
}

/// Convenience: validate a config file at the given path, falling back to
/// documented defaults when the file does not exist.
pub fn validate_local_path(path: &Path) -> Result<LocalValidation, LocalValidationError> {
    match std::fs::read_to_string(path) {
        Ok(contents) => validate_local_yaml(&contents),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            Ok(default_validation(LocalSource::Defaults))
        }
        Err(err) => Err(LocalValidationError::Io(err.to_string())),
    }
}

fn default_validation(source: LocalSource) -> LocalValidation {
    LocalValidation {
        data: LocalValidationData {
            source,
            config: LocalConfig {
                version: SUPPORTED_VERSION,
                archive_clone_path: expanded_default_archive_clone_path(),
                working_repo_roots: Vec::new(),
                performance: LocalPerformance {
                    migrate_batch_size: DEFAULT_MIGRATE_BATCH_SIZE,
                },
            },
        },
        warnings: vec![ValidationWarning::new(
            "local-defaults-used",
            "local config file is missing or empty; falling back to documented defaults",
        )],
    }
}

/// Expand a single `~` prefix and `$VAR` / `${VAR}` references.
/// The no-config default archive clone path:
/// `${XDG_DATA_HOME:-$HOME/.local/share}/agent-evidence-archive`.
///
/// Honors `XDG_DATA_HOME` when set and non-empty (matching the documented
/// default and the crate's `XDG_CONFIG_HOME` handling for the config file);
/// otherwise expands the `~/.local/share` fallback via `$HOME`.
fn expanded_default_archive_clone_path() -> PathBuf {
    if let Some(xdg) = std::env::var_os("XDG_DATA_HOME")
        && !xdg.is_empty()
    {
        return PathBuf::from(xdg).join("agent-evidence-archive");
    }
    expand_path(DEFAULT_ARCHIVE_CLONE_PATH)
}

fn expand_path(input: &str) -> PathBuf {
    let mut s = input.trim().to_string();
    if let Some(rest) = s.strip_prefix("~/")
        && let Some(home) = std::env::var_os("HOME")
    {
        let mut p = PathBuf::from(home);
        p.push(rest);
        return p;
    } else if s == "~"
        && let Some(home) = std::env::var_os("HOME")
    {
        return PathBuf::from(home);
    }

    // Expand `$VAR` and `${VAR}`. Unknown variables are left as-is.
    let mut out = String::with_capacity(s.len());
    while let Some(idx) = s.find('$') {
        out.push_str(&s[..idx]);
        let after = &s[idx + 1..];
        let (var_name, rest_start) = if let Some(rest) = after.strip_prefix('{') {
            let close = rest.find('}').map(|c| c + 1);
            match close {
                Some(end) => (&rest[..end - 1], end + 1),
                None => {
                    out.push('$');
                    s = after.to_string();
                    continue;
                }
            }
        } else {
            let end = after
                .find(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                .unwrap_or(after.len());
            (&after[..end], end)
        };
        if var_name.is_empty() {
            out.push('$');
            s = after.to_string();
            continue;
        }
        match std::env::var(var_name) {
            Ok(value) => out.push_str(&value),
            Err(_) => {
                out.push('$');
                if after.starts_with('{') {
                    out.push('{');
                    out.push_str(var_name);
                    out.push('}');
                } else {
                    out.push_str(var_name);
                }
            }
        }
        s = after[rest_start..].to_string();
    }
    out.push_str(&s);
    PathBuf::from(out)
}

#[derive(Debug, Deserialize)]
struct RawLocalFile {
    #[serde(default)]
    version: Option<u32>,
    #[serde(default)]
    archive_clone_path: Option<String>,
    #[serde(default)]
    working_repo_roots: Option<Vec<String>>,
    #[serde(default)]
    performance: Option<RawPerformance>,
    #[allow(dead_code)]
    #[serde(default)]
    schema: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawPerformance {
    #[serde(default)]
    migrate_batch_size: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_returns_defaults() {
        let v = validate_local_yaml("").expect("defaults");
        assert!(matches!(v.data.source, LocalSource::Defaults));
        assert!(v.data.config.working_repo_roots.is_empty());
        assert_eq!(v.data.config.performance.migrate_batch_size, 50);
        assert!(!v.warnings.is_empty());
    }

    #[test]
    fn explicit_overrides_validate() {
        unsafe { std::env::set_var("HOME", "/Users/test") };
        let input = r"
version: 1
archive_clone_path: ~/.local/share/agent-evidence-archive
working_repo_roots:
  - ~/Project
  - /workspace/src
performance:
  migrate_batch_size: 25
";
        let v = validate_local_yaml(input).expect("validation");
        assert!(matches!(v.data.source, LocalSource::File));
        assert_eq!(v.data.config.performance.migrate_batch_size, 25);
        assert_eq!(v.data.config.working_repo_roots.len(), 2);
        assert_eq!(
            v.data.config.archive_clone_path,
            PathBuf::from("/Users/test/.local/share/agent-evidence-archive")
        );
    }

    #[test]
    fn default_archive_clone_path_honours_xdg_data_home() {
        use nils_test_support::{EnvGuard, GlobalStateLock};
        let lock = GlobalStateLock::new();
        let _xdg = EnvGuard::set(&lock, "XDG_DATA_HOME", "/xdg-data");
        // No config file -> documented default, which must live under
        // $XDG_DATA_HOME (the `${XDG_DATA_HOME:-$HOME/.local/share}` contract).
        let v = validate_local_path(&PathBuf::from(
            "/nonexistent/agent-evidence-archive/config.yaml",
        ))
        .expect("defaults");
        assert_eq!(
            v.data.config.archive_clone_path,
            PathBuf::from("/xdg-data/agent-evidence-archive")
        );
    }

    #[test]
    fn default_archive_clone_path_falls_back_to_home_local_share() {
        use nils_test_support::{EnvGuard, GlobalStateLock};
        let lock = GlobalStateLock::new();
        let _no_xdg = EnvGuard::remove(&lock, "XDG_DATA_HOME");
        let _home = EnvGuard::set(&lock, "HOME", "/home/me");
        let v = validate_local_path(&PathBuf::from(
            "/nonexistent/agent-evidence-archive/config.yaml",
        ))
        .expect("defaults");
        assert_eq!(
            v.data.config.archive_clone_path,
            PathBuf::from("/home/me/.local/share/agent-evidence-archive")
        );
    }

    #[test]
    fn unsupported_version_rejected() {
        let err = validate_local_yaml("version: 5\n").expect_err("unsupported");
        assert_eq!(err.code(), "local-unsupported-version");
    }

    #[test]
    fn zero_batch_rejected() {
        let input = r"
version: 1
performance:
  migrate_batch_size: 0
";
        let err = validate_local_yaml(input).expect_err("zero batch");
        assert_eq!(err.code(), "local-invalid-batch-size");
        assert!(
            err.to_string().contains("migrate_batch_size"),
            "message names the renamed field: {err}"
        );
    }

    #[test]
    fn missing_file_returns_defaults() {
        let path = PathBuf::from("/nonexistent/agent-evidence-archive/config.yaml");
        let v = validate_local_path(&path).expect("defaults");
        assert!(matches!(v.data.source, LocalSource::Defaults));
    }

    #[test]
    fn malformed_yaml_is_parse_error() {
        let err = validate_local_yaml("version: : :\n  - bad").expect_err("parse error");
        assert_eq!(err.code(), "local-parse-error");
    }

    #[test]
    fn env_var_expansion() {
        unsafe { std::env::set_var("EV_TEST_ROOT", "/var/data") };
        let input = "version: 1\narchive_clone_path: $EV_TEST_ROOT/archive\n";
        let v = validate_local_yaml(input).expect("validation");
        assert_eq!(
            v.data.config.archive_clone_path,
            PathBuf::from("/var/data/archive")
        );
    }
}
