use std::collections::BTreeSet;
use std::fs;
use std::path::{Component, Path};

use serde::{Deserialize, Serialize};

use crate::error::CliError;
use crate::test_mode;

const EMBEDDED_LOCK: &str = include_str!("../peekaboo-lock.json");

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PeekabooLock {
    pub schema_version: u8,
    pub repository: String,
    pub tag: String,
    pub commit: String,
    pub released_at: String,
    pub license: LicenseLock,
    pub minimum_macos: String,
    pub assets: Vec<AssetLock>,
    pub archive_policy: ArchivePolicy,
    pub required_capability_probes: Vec<CapabilityProbe>,
    #[serde(default)]
    pub rollback_releases: Vec<RollbackReleaseLock>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RollbackReleaseLock {
    pub tag: String,
    pub commit: String,
    pub minimum_macos: String,
    pub assets: Vec<AssetLock>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LicenseLock {
    pub spdx: String,
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AssetLock {
    pub kind: String,
    pub name: String,
    pub url: String,
    pub sha256: String,
    pub archive_root: String,
    pub executable: String,
    pub executable_sha256: String,
    pub bridge_build: String,
    pub architectures: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bundle_id: Option<String>,
    pub signing_authority: String,
    pub team_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArchivePolicy {
    pub reject_absolute_paths: bool,
    pub reject_parent_traversal: bool,
    pub allow_internal_symlinks: bool,
    pub reject_symlink_escape: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CapabilityProbe {
    pub id: String,
    pub argv: Vec<String>,
}

impl PeekabooLock {
    pub fn embedded() -> Result<Self, CliError> {
        let raw = match test_mode::lock_path_override() {
            Some(path) => fs::read_to_string(path).map_err(|error| {
                CliError::backend(format!("test Peekaboo lock is unavailable: {error}"))
                    .with_operation("backend.lock")
            })?,
            None => EMBEDDED_LOCK.to_string(),
        };
        let lock: Self = serde_json::from_str(&raw).map_err(|error| {
            CliError::backend(format!("embedded Peekaboo lock is invalid: {error}"))
                .with_operation("backend.lock")
        })?;
        lock.validate()?;
        Ok(lock)
    }

    pub fn cli_asset(&self) -> &AssetLock {
        self.assets
            .iter()
            .find(|asset| asset.kind == "cli")
            .expect("validated lock has a CLI asset")
    }

    pub fn app_asset(&self) -> &AssetLock {
        self.assets
            .iter()
            .find(|asset| asset.kind == "app")
            .expect("validated lock has an app asset")
    }

    pub fn rollback_release(&self, tag: &str, commit: &str) -> Option<&RollbackReleaseLock> {
        self.rollback_releases
            .iter()
            .find(|release| release.tag == tag && release.commit == commit)
    }

    fn validate(&self) -> Result<(), CliError> {
        if self.schema_version != 1 {
            return Err(lock_error("unsupported lock schema version"));
        }
        if self.repository != "https://github.com/openclaw/Peekaboo" {
            return Err(lock_error("unexpected Peekaboo repository"));
        }
        if !self.tag.starts_with('v') || self.commit.len() != 40 || !is_lower_hex(&self.commit) {
            return Err(lock_error("tag or immutable commit is malformed"));
        }
        if self.minimum_macos != "15.0" {
            return Err(lock_error("the supported macOS floor must be exactly 15.0"));
        }
        if !self.archive_policy.reject_absolute_paths
            || !self.archive_policy.reject_parent_traversal
            || !self.archive_policy.allow_internal_symlinks
            || !self.archive_policy.reject_symlink_escape
        {
            return Err(lock_error(
                "archive policy must retain every reviewed extraction guard",
            ));
        }
        let kinds = self
            .assets
            .iter()
            .map(|asset| asset.kind.as_str())
            .collect::<BTreeSet<_>>();
        if kinds != BTreeSet::from(["app", "cli"]) || self.assets.len() != 2 {
            return Err(lock_error(
                "lock must contain exactly one CLI and one app asset",
            ));
        }
        validate_assets(&self.assets, &self.tag)?;
        if self.required_capability_probes.is_empty()
            || self
                .required_capability_probes
                .iter()
                .any(|probe| probe.id.trim().is_empty() || probe.argv.is_empty())
        {
            return Err(lock_error("required capability probes are incomplete"));
        }
        let probe_ids = self
            .required_capability_probes
            .iter()
            .map(|probe| probe.id.as_str())
            .collect::<BTreeSet<_>>();
        let mandatory = BTreeSet::from(["bridge", "permissions", "tools", "version"]);
        if !mandatory.is_subset(&probe_ids)
            || probe_ids.len() != self.required_capability_probes.len()
        {
            return Err(lock_error(
                "lock must contain each mandatory capability probe exactly once",
            ));
        }
        let mut rollback_tags = BTreeSet::new();
        for release in &self.rollback_releases {
            if !release.tag.starts_with('v')
                || release.commit.len() != 40
                || !is_lower_hex(&release.commit)
                || release.tag == self.tag
                || release.minimum_macos != "15.0"
                || !rollback_tags.insert(release.tag.as_str())
            {
                return Err(lock_error(
                    "rollback release identity is malformed or duplicated",
                ));
            }
            validate_assets(&release.assets, &release.tag)?;
        }
        Ok(())
    }
}

impl RollbackReleaseLock {
    pub fn cli_asset(&self) -> &AssetLock {
        self.assets
            .iter()
            .find(|asset| asset.kind == "cli")
            .expect("validated rollback lock has a CLI asset")
    }

    pub fn app_asset(&self) -> &AssetLock {
        self.assets
            .iter()
            .find(|asset| asset.kind == "app")
            .expect("validated rollback lock has an app asset")
    }
}

fn validate_assets(assets: &[AssetLock], tag: &str) -> Result<(), CliError> {
    let kinds = assets
        .iter()
        .map(|asset| asset.kind.as_str())
        .collect::<BTreeSet<_>>();
    if kinds != BTreeSet::from(["app", "cli"]) || assets.len() != 2 {
        return Err(lock_error(
            "release lock must contain exactly one CLI and one app asset",
        ));
    }
    for asset in assets {
        if !asset
            .url
            .starts_with("https://github.com/openclaw/Peekaboo/releases/download/")
            || asset.sha256.len() != 64
            || !is_lower_hex(&asset.sha256)
            || asset.executable_sha256.len() != 64
            || !is_lower_hex(&asset.executable_sha256)
            || !safe_asset_name(&asset.name)
            || !safe_relative_path(&asset.archive_root)
            || !safe_relative_path(&asset.executable)
            || bridge_build_number(&asset.bridge_build, tag).is_none()
            || asset.architectures.is_empty()
            || asset.signing_authority.trim().is_empty()
            || asset.team_id.trim().is_empty()
        {
            return Err(lock_error(format!("asset `{}` is malformed", asset.kind)));
        }
    }
    Ok(())
}

pub(crate) fn bridge_build_number<'a>(value: &'a str, tag: &str) -> Option<&'a str> {
    let version = tag.strip_prefix('v')?;
    let prefix = format!("{version} (");
    let build = value.strip_prefix(&prefix)?.strip_suffix(')')?;
    (!build.is_empty()
        && build.len() <= 64
        && build
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_' | b'+')))
    .then_some(build)
}

fn safe_asset_name(value: &str) -> bool {
    safe_relative_path(value) && !value.contains('/')
}

fn safe_relative_path(value: &str) -> bool {
    if value.is_empty()
        || Path::new(value).is_absolute()
        || value
            .split('/')
            .any(|part| part.is_empty() || matches!(part, "." | ".."))
    {
        return false;
    }
    let mut components = Path::new(value).components().peekable();
    components.peek().is_some()
        && components.all(|component| matches!(component, Component::Normal(_)))
}

fn is_lower_hex(value: &str) -> bool {
    value
        .bytes()
        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn lock_error(message: impl Into<String>) -> CliError {
    CliError::backend(message).with_operation("backend.lock")
}

#[cfg(test)]
mod tests {
    use super::PeekabooLock;

    #[test]
    fn embedded_lock_is_complete_and_immutable() {
        let lock = PeekabooLock::embedded().expect("embedded lock");
        assert_eq!(lock.tag, "v3.9.3");
        assert_eq!(lock.assets.len(), 2);
        assert_eq!(lock.cli_asset().architectures, ["arm64", "x86_64"]);
        assert_eq!(lock.cli_asset().bridge_build, "3.9.3 (3.9.3)");
        assert_eq!(lock.app_asset().bridge_build, "3.9.3 (3090399)");
        assert_eq!(
            lock.app_asset().bundle_id.as_deref(),
            Some("boo.peekaboo.mac")
        );
    }

    #[test]
    fn locked_asset_paths_are_confined_to_safe_relative_components() {
        for unsafe_name in [
            "",
            ".",
            "../escape",
            "nested/asset",
            "/private/asset",
            "asset/",
            "./asset",
        ] {
            let mut lock = PeekabooLock::embedded().expect("embedded lock");
            lock.assets[0].name = unsafe_name.into();
            assert!(
                lock.validate().is_err(),
                "unsafe asset name was admitted: {unsafe_name:?}"
            );
        }
        for unsafe_path in [
            "",
            ".",
            "../escape",
            "safe/../escape",
            "/private/escape",
            "safe/./binary",
            "safe//binary",
            "safe/",
        ] {
            let mut archive_root = PeekabooLock::embedded().expect("embedded lock");
            archive_root.assets[0].archive_root = unsafe_path.into();
            assert!(
                archive_root.validate().is_err(),
                "unsafe archive root was admitted: {unsafe_path:?}"
            );

            let mut executable = PeekabooLock::embedded().expect("embedded lock");
            executable.assets[0].executable = unsafe_path.into();
            assert!(
                executable.validate().is_err(),
                "unsafe executable path was admitted: {unsafe_path:?}"
            );
        }
    }

    #[test]
    fn bridge_builds_are_exact_tag_scoped_immutable_identities() {
        for unsafe_build in [
            "",
            "3.9.2 (3090399)",
            "3.9.3",
            "3.9.3 ()",
            "3.9.3 (build value)",
            "3.9.3 (3090399) trailing",
        ] {
            let mut lock = PeekabooLock::embedded().expect("embedded lock");
            lock.assets[0].bridge_build = unsafe_build.into();
            assert!(
                lock.validate().is_err(),
                "unsafe bridge build was admitted: {unsafe_build:?}"
            );
        }
    }
}
