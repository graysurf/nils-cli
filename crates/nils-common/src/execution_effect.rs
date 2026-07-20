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
            capability_class: CapabilityClass::ToolContract,
            producer: ProducerIdentity {
                tool: input.tool.to_string(),
                release: input.release.to_string(),
                executable_digest: executable_digest(&executable)?,
            },
            operation: input.operation.to_string(),
            effect: input.effect,
            provider_effect: input.provider_effect,
            managed_state_reads: input.managed_state_reads,
            binding: InvocationBinding {
                argv_digest: argv_digest(input.argv),
                cwd_digest: digest_parts([cwd.as_os_str().as_encoded_bytes()]),
                target_digest: digest_parts(target_material.iter().map(String::as_bytes)),
            },
            issued_at_epoch: jiff::Timestamp::now().as_second(),
        })
    }
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
