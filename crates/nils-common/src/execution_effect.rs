//! Versioned, request-bound operation effect descriptors for trusted nils CLIs.
//!
//! Owning tools classify their already-parsed command enums and call this
//! module only to bind that classification to the exact local invocation. The
//! descriptor is evidence, not authorization: consumers still verify the
//! producer executable, release, freshness, and every binding before treating
//! a `read_only` effect as an `execution.read-only.v1` capability.

use std::ffi::OsString;
use std::fs;
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const OPERATION_EFFECT_VERSION: &str = "execution.operation-effect.v1";

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Effect {
    ReadOnly,
    Mutation,
    Unknown,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityClass {
    OsEnforced,
    ToolContract,
    HostAttested,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProviderEffect {
    None,
    LocalRead,
    NetworkRead,
    NetworkWrite,
}

pub const OS_ENFORCEMENT_MAX_WALL_TIME_MS: u64 = 30_000;
pub const OS_ENFORCEMENT_MAX_OUTPUT_BYTES: u64 = 8 * 1024 * 1024;
pub const OS_ENFORCEMENT_MAX_PROCESSES: u32 = 64;
pub const OS_ENFORCEMENT_MAX_MEMORY_BYTES: u64 = 1024 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BackendIdentity {
    pub kind: String,
    pub release: String,
    pub executable_digest: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExecutionLimits {
    pub wall_time_ms: u64,
    pub output_bytes: u64,
    pub process_count: u32,
    pub memory_bytes: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OsEnforcement {
    pub backend: BackendIdentity,
    pub durable_roots_read_only: bool,
    pub network_denied: bool,
    pub private_ephemeral_roots: Vec<String>,
    pub inherited_credentials_cleared: bool,
    pub inherited_writable_fds_closed: bool,
    pub descendants_contained: bool,
    pub limits: ExecutionLimits,
}

impl OsEnforcement {
    pub fn strict_v1(backend: BackendIdentity) -> Self {
        Self {
            backend,
            durable_roots_read_only: true,
            network_denied: true,
            private_ephemeral_roots: [
                "home",
                "tmp",
                "xdg_cache",
                "xdg_config",
                "xdg_data",
                "xdg_runtime",
                "xdg_state",
            ]
            .into_iter()
            .map(str::to_string)
            .collect(),
            inherited_credentials_cleared: true,
            inherited_writable_fds_closed: true,
            descendants_contained: true,
            limits: ExecutionLimits {
                wall_time_ms: OS_ENFORCEMENT_MAX_WALL_TIME_MS,
                output_bytes: OS_ENFORCEMENT_MAX_OUTPUT_BYTES,
                process_count: OS_ENFORCEMENT_MAX_PROCESSES,
                memory_bytes: OS_ENFORCEMENT_MAX_MEMORY_BYTES,
            },
        }
    }

    pub fn satisfies_strict_v1(&self) -> bool {
        let expected_roots = [
            "home",
            "tmp",
            "xdg_cache",
            "xdg_config",
            "xdg_data",
            "xdg_runtime",
            "xdg_state",
        ];
        matches!(
            self.backend.kind.as_str(),
            "linux.bubblewrap-systemd.v1" | "macos.sandbox-exec.v1"
        ) && !self.backend.release.is_empty()
            && self.backend.release.len() <= 192
            && valid_digest(&self.backend.executable_digest)
            && self.durable_roots_read_only
            && self.network_denied
            && self.inherited_credentials_cleared
            && self.inherited_writable_fds_closed
            && self.descendants_contained
            && self
                .private_ephemeral_roots
                .iter()
                .map(String::as_str)
                .eq(expected_roots)
            && self.limits.wall_time_ms > 0
            && self.limits.wall_time_ms <= OS_ENFORCEMENT_MAX_WALL_TIME_MS
            && self.limits.output_bytes > 0
            && self.limits.output_bytes <= OS_ENFORCEMENT_MAX_OUTPUT_BYTES
            && self.limits.process_count > 0
            && self.limits.process_count <= OS_ENFORCEMENT_MAX_PROCESSES
            && self.limits.memory_bytes > 0
            && self.limits.memory_bytes <= OS_ENFORCEMENT_MAX_MEMORY_BYTES
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProducerIdentity {
    pub tool: String,
    pub release: String,
    pub executable_digest: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct InvocationBinding {
    pub argv_digest: String,
    pub cwd_digest: String,
    pub target_digest: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OperationEffectDescriptor {
    pub schema_version: String,
    pub capability_class: CapabilityClass,
    pub producer: ProducerIdentity,
    pub operation: String,
    pub effect: Effect,
    pub provider_effect: ProviderEffect,
    pub managed_state_reads: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub os_enforcement: Option<OsEnforcement>,
    pub binding: InvocationBinding,
    pub issued_at_epoch: i64,
}

pub struct OperationEffectInput<'a> {
    pub tool: &'a str,
    pub release: &'a str,
    pub operation: &'a str,
    pub effect: Effect,
    pub provider_effect: ProviderEffect,
    pub managed_state_reads: Vec<String>,
    pub argv: &'a [OsString],
    pub targets: &'a [String],
}

impl OperationEffectDescriptor {
    pub fn for_current_process(input: OperationEffectInput<'_>) -> Result<Self, String> {
        Self::for_current_process_with(input, CapabilityClass::ToolContract, None)
    }

    pub fn for_current_process_os(
        input: OperationEffectInput<'_>,
        enforcement: OsEnforcement,
    ) -> Result<Self, String> {
        if !enforcement.satisfies_strict_v1() {
            return Err("OS enforcement does not satisfy the strict v1 contract".to_string());
        }
        Self::for_current_process_with(input, CapabilityClass::OsEnforced, Some(enforcement))
    }

    fn for_current_process_with(
        input: OperationEffectInput<'_>,
        capability_class: CapabilityClass,
        os_enforcement: Option<OsEnforcement>,
    ) -> Result<Self, String> {
        let executable = std::env::current_exe()
            .and_then(fs::canonicalize)
            .map_err(|error| format!("failed to resolve current executable: {error}"))?;
        let cwd = std::env::current_dir()
            .and_then(fs::canonicalize)
            .map_err(|error| format!("failed to resolve current directory: {error}"))?;
        let mut target_material = input.targets.to_vec();
        target_material.sort();
        target_material.dedup();

        Ok(Self {
            schema_version: OPERATION_EFFECT_VERSION.to_string(),
            capability_class,
            producer: ProducerIdentity {
                tool: input.tool.to_string(),
                release: input.release.to_string(),
                executable_digest: executable_digest(&executable)?,
            },
            operation: input.operation.to_string(),
            effect: input.effect,
            provider_effect: input.provider_effect,
            managed_state_reads: input.managed_state_reads,
            os_enforcement,
            binding: InvocationBinding {
                argv_digest: argv_digest(input.argv),
                cwd_digest: digest_parts([cwd.as_os_str().as_encoded_bytes()]),
                target_digest: digest_parts(target_material.iter().map(String::as_bytes)),
            },
            issued_at_epoch: jiff::Timestamp::now().as_second(),
        })
    }
}

fn valid_digest(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    })
}

pub fn executable_digest(path: &std::path::Path) -> Result<String, String> {
    let executable =
        fs::canonicalize(path).map_err(|error| format!("failed to resolve executable: {error}"))?;
    let mut file = fs::File::open(&executable)
        .map_err(|error| format!("failed to open executable: {error}"))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("failed to inspect executable: {error}"))?;
    if !metadata.is_file() {
        return Err("executable is not a regular file".to_string());
    }
    let mut hasher = Sha256::new();
    hasher.update(b"execution.executable.v1\0");
    for part in [
        executable.as_os_str().as_encoded_bytes(),
        &metadata.dev().to_le_bytes(),
        &metadata.ino().to_le_bytes(),
        &metadata.len().to_le_bytes(),
    ] {
        hasher.update((part.len() as u64).to_le_bytes());
        hasher.update(part);
    }
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("failed to read executable: {error}"))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let after = file
        .metadata()
        .map_err(|error| format!("failed to re-inspect executable: {error}"))?;
    if metadata.dev() != after.dev()
        || metadata.ino() != after.ino()
        || metadata.len() != after.len()
        || metadata.mtime() != after.mtime()
        || metadata.mtime_nsec() != after.mtime_nsec()
    {
        return Err("executable changed while it was being digested".to_string());
    }
    Ok(hex_digest(hasher.finalize()))
}

pub fn cwd_digest(path: &std::path::Path) -> Result<String, String> {
    let cwd = fs::canonicalize(path)
        .map_err(|error| format!("failed to resolve current directory: {error}"))?;
    Ok(digest_parts([cwd.as_os_str().as_encoded_bytes()]))
}

pub fn argv_digest(argv: &[OsString]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"execution.argv.v1\0");
    for argument in argv {
        let bytes = argument.as_os_str().as_encoded_bytes();
        hasher.update((bytes.len() as u64).to_le_bytes());
        hasher.update(bytes);
    }
    hex_digest(hasher.finalize())
}

pub fn digest_parts<'a>(parts: impl IntoIterator<Item = &'a [u8]>) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"execution.binding.v1\0");
    for part in parts {
        hasher.update((part.len() as u64).to_le_bytes());
        hasher.update(part);
    }
    hex_digest(hasher.finalize())
}

fn hex_digest(bytes: impl AsRef<[u8]>) -> String {
    let hex = bytes
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("sha256:{hex}")
}

pub fn canonical_target(path: PathBuf) -> String {
    fs::canonicalize(&path)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::fs;

    use super::{argv_digest, executable_digest};

    #[test]
    fn argv_binding_preserves_argument_boundaries() {
        assert_ne!(
            argv_digest(&[OsString::from("ab"), OsString::from("c")]),
            argv_digest(&[OsString::from("a"), OsString::from("bc")])
        );
    }

    #[test]
    fn executable_binding_changes_with_file_content() {
        let temp = tempfile::TempDir::new().expect("tempdir");
        let executable = temp.path().join("producer");
        fs::write(&executable, b"first").expect("first content");
        let first = executable_digest(&executable).expect("first digest");
        fs::write(&executable, b"later").expect("later content");
        let later = executable_digest(&executable).expect("later digest");

        assert_ne!(first, later);
    }
}
