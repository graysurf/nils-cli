//! Strict default-branch commit receipt shared by semantic-commit and forge-cli.

use std::fs::OpenOptions;
use std::io::Read;
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;

use serde::{Deserialize, Serialize};

pub const SCHEMA_VERSION: &str = "cli.semantic-commit.default-branch.v1";
pub const MAX_RECEIPT_BYTES: u64 = 64 * 1024;
/// One-shot delivery waiver stated on the invoking command. The admission
/// decision belongs to the host guard; the receipt only records the reason as
/// evidence, so a value that states nothing is not recorded at all.
pub const DELIVERY_WAIVER_ENV: &str = "AGENT_RUNTIME_DEFAULT_DELIVERY_WAIVER";
/// A host guard that admits a delivery on a stated reason must measure that
/// reason exactly as [`normalized_delivery_waiver`] does, against this same
/// minimum.
pub const MIN_DELIVERY_WAIVER_LENGTH: usize = 12;
pub const MAX_DELIVERY_WAIVER_LENGTH: usize = 500;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DefaultBranchReceipt {
    pub schema_version: String,
    pub ok: bool,
    pub data: DefaultBranchData,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DefaultBranchData {
    pub mode: String,
    pub repository_fingerprint: String,
    pub default_branch: String,
    pub old_head: String,
    pub new_head: String,
    pub parent_sha: String,
    pub tree_sha: String,
    pub signature: String,
    pub staged_file_count: usize,
    pub remote: DefaultBranchRemote,
    pub completion: DefaultBranchCompletion,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery_waiver: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DefaultBranchRemote {
    pub configured_count: usize,
    pub mode: String,
    pub network_observed: bool,
    pub provider_mutated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream: Option<String>,
    pub cached_relation_before: String,
    pub cached_relation_after: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DefaultBranchCompletion {
    pub default_branch_committed: bool,
    pub provider_delivery_attempted: bool,
    pub provider_delivered: bool,
}

impl DefaultBranchReceipt {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != SCHEMA_VERSION || !self.ok {
            return Err("unsupported or unsuccessful default-branch receipt".to_string());
        }
        let data = &self.data;
        if data.mode != "default-branch"
            || data.signature != "verified-good"
            || data.parent_sha != data.old_head
            || data.new_head == data.old_head
            || !valid_object_id(&data.old_head)
            || !valid_object_id(&data.new_head)
            || !valid_object_id(&data.tree_sha)
            || !valid_digest(&data.repository_fingerprint)
            || data.default_branch.is_empty()
            || data.default_branch.len() > 255
            || data.staged_file_count == 0
            || data.remote.network_observed
            || data.remote.provider_mutated
            || !data.completion.default_branch_committed
            || data.completion.provider_delivery_attempted
            || data.completion.provider_delivered
        {
            return Err("default-branch receipt invariants are invalid".to_string());
        }
        if let Some(waiver) = data.delivery_waiver.as_deref()
            && normalized_delivery_waiver(waiver).as_deref() != Some(waiver)
        {
            return Err("default-branch delivery waiver is not a stated reason".to_string());
        }
        if data.remote.configured_count == 0 {
            if data.remote.mode != "remote-free"
                || data.remote.upstream.is_some()
                || data.remote.cached_relation_before != "untracked"
                || data.remote.cached_relation_after != "untracked"
            {
                return Err("remote-free receipt invariants are invalid".to_string());
            }
        } else if data.remote.mode != "cached-upstream"
            || data.remote.upstream.as_deref().is_none_or(str::is_empty)
            || data.remote.cached_relation_before != "aligned"
            || data.remote.cached_relation_after != "ahead-by-one"
        {
            return Err("cached default-branch transition is invalid".to_string());
        }
        Ok(())
    }
}

/// Return the waiver reason worth recording, or `None` when the value states
/// nothing. Control characters collapse to spaces so a receipt line cannot be
/// forged, and runs of whitespace normalize to one space.
pub fn normalized_delivery_waiver(value: &str) -> Option<String> {
    let reason = value
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if reason.chars().count() < MIN_DELIVERY_WAIVER_LENGTH {
        return None;
    }
    Some(reason.chars().take(MAX_DELIVERY_WAIVER_LENGTH).collect())
}

pub fn read_strict(path: &Path) -> Result<DefaultBranchReceipt, String> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    let mut file = options
        .open(path)
        .map_err(|error| format!("failed to open default-branch receipt safely: {error}"))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("failed to inspect default-branch receipt: {error}"))?;
    if !metadata.is_file() {
        return Err("default-branch receipt must be a regular non-symlink file".to_string());
    }
    #[cfg(unix)]
    if !private_receipt_access(metadata.uid(), metadata.mode(), unsafe { libc::geteuid() }) {
        return Err(
            "default-branch receipt must be owned by the effective user and private".to_string(),
        );
    }
    if metadata.len() > MAX_RECEIPT_BYTES {
        return Err("default-branch receipt exceeds the size limit".to_string());
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.by_ref()
        .take(MAX_RECEIPT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("failed to read default-branch receipt: {error}"))?;
    if bytes.len() as u64 > MAX_RECEIPT_BYTES {
        return Err("default-branch receipt exceeds the size limit".to_string());
    }
    let receipt: DefaultBranchReceipt = serde_json::from_slice(&bytes)
        .map_err(|error| format!("default-branch receipt is malformed: {error}"))?;
    receipt.validate()?;
    Ok(receipt)
}

#[cfg(unix)]
fn private_receipt_access(owner_uid: u32, mode: u32, effective_uid: u32) -> bool {
    owner_uid == effective_uid && mode & 0o077 == 0
}

fn valid_object_id(value: &str) -> bool {
    matches!(value.len(), 40 | 64)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn valid_digest(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    #[cfg(unix)]
    use std::os::unix::fs::{PermissionsExt, symlink};

    use super::{
        MAX_DELIVERY_WAIVER_LENGTH, MAX_RECEIPT_BYTES, SCHEMA_VERSION, normalized_delivery_waiver,
        private_receipt_access, read_strict, valid_object_id,
    };

    #[test]
    fn final_receipt_uses_the_default_branch_schema() {
        assert_eq!(SCHEMA_VERSION, "cli.semantic-commit.default-branch.v1");
    }

    #[test]
    fn delivery_waivers_record_only_a_stated_reason() {
        assert_eq!(
            normalized_delivery_waiver("maintainer authorized this target").as_deref(),
            Some("maintainer authorized this target")
        );
        assert_eq!(
            normalized_delivery_waiver("  cross-repo\tlocal\ncompletion  ").as_deref(),
            Some("cross-repo local completion")
        );
        assert_eq!(normalized_delivery_waiver(""), None);
        assert_eq!(normalized_delivery_waiver("1"), None);
        assert_eq!(normalized_delivery_waiver("           "), None);
        assert_eq!(
            normalized_delivery_waiver("authorized\r\nprovider_delivered: true").as_deref(),
            Some("authorized provider_delivered: true")
        );
        let long = "reason ".repeat(200);
        assert_eq!(
            normalized_delivery_waiver(&long)
                .expect("bounded reason")
                .chars()
                .count(),
            MAX_DELIVERY_WAIVER_LENGTH
        );
    }

    #[test]
    fn object_ids_are_full_lower_hex() {
        assert!(valid_object_id(&"a".repeat(40)));
        assert!(valid_object_id(&"b".repeat(64)));
        assert!(!valid_object_id("HEAD"));
        assert!(!valid_object_id(&"A".repeat(40)));
    }

    #[cfg(unix)]
    #[test]
    fn strict_reader_rejects_symlinks() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let target = directory.path().join("target.json");
        let link = directory.path().join("receipt.json");
        fs::write(&target, b"{}").expect("write target");
        symlink(&target, &link).expect("create symlink");

        let error = read_strict(&link).expect_err("symlink must be rejected");
        assert!(error.contains("safely"), "unexpected error: {error}");
    }

    #[test]
    fn strict_reader_rejects_oversized_files() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("receipt.json");
        fs::write(&path, vec![b' '; MAX_RECEIPT_BYTES as usize + 1])
            .expect("write oversized receipt");
        #[cfg(unix)]
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
            .expect("make oversized fixture private");

        let error = read_strict(&path).expect_err("oversized receipt must be rejected");
        assert!(error.contains("size limit"), "unexpected error: {error}");
    }

    #[cfg(unix)]
    #[test]
    fn strict_reader_rejects_group_or_world_permissions() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("receipt.json");
        fs::write(&path, b"{}").expect("write receipt");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640))
            .expect("make receipt group-readable");

        let error = read_strict(&path).expect_err("non-private receipt must be rejected");
        assert!(error.contains("private"), "unexpected error: {error}");
    }

    #[cfg(unix)]
    #[test]
    fn private_receipt_access_requires_effective_user_ownership() {
        assert!(private_receipt_access(501, 0o100600, 501));
        assert!(!private_receipt_access(502, 0o100600, 501));
        for shared_access in [0o040, 0o020, 0o010, 0o004, 0o002, 0o001] {
            assert!(!private_receipt_access(501, 0o100600 | shared_access, 501));
        }
    }
}
