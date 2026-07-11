//! Portable installed-runtime provenance receipts.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::plan::{InstallPlan, PlanAction};

pub const RECEIPT_SCHEMA: &str = "agent-runtime.install-receipt.v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InstallReceipt {
    pub schema: String,
    pub product: String,
    pub source_revision: String,
    pub source_dirty: bool,
    pub install_plan_digest: String,
    pub managed_entries: Vec<ManagedEntryReceipt>,
    pub producer_version: String,
    pub recorded_at_unix_seconds: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ManagedEntryReceipt {
    pub id: String,
    pub digest: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ReceiptError {
    #[error("failed to inspect source revision: {0}")]
    Source(String),
    #[error("failed to hash install content: {0}")]
    Hash(String),
    #[error("failed to render receipt: {0}")]
    Render(#[from] serde_json::Error),
    #[error("failed to write receipt: {0}")]
    Write(String),
    #[error("failed to read receipt: {0}")]
    Read(String),
}

pub fn path(state_home: &Path, product: &str) -> PathBuf {
    state_home.join("receipts").join(format!("{product}.json"))
}

pub fn write(plan: &InstallPlan, now: SystemTime) -> Result<InstallReceipt, ReceiptError> {
    let receipt = build(plan, now)?;
    let target = path(&plan.state_home, &plan.product);
    let mut bytes = serde_json::to_vec_pretty(&receipt)?;
    bytes.push(b'\n');
    nils_common::fs::write_atomic(&target, &bytes, 0o600)
        .map_err(|err| ReceiptError::Write(err.to_string()))?;
    Ok(receipt)
}

pub fn read(state_home: &Path, product: &str) -> Result<InstallReceipt, ReceiptError> {
    let target = path(state_home, product);
    let raw = std::fs::read_to_string(&target)
        .map_err(|err| ReceiptError::Read(format!("{}: {err}", target.display())))?;
    serde_json::from_str(&raw).map_err(ReceiptError::Render)
}

pub fn build(plan: &InstallPlan, now: SystemTime) -> Result<InstallReceipt, ReceiptError> {
    let (source_revision, source_dirty) = source_provenance(&plan.source_root);
    let managed_entries = managed_entry_receipts(plan)?;
    let mut plan_hasher = Sha256::new();
    plan_hasher.update(plan.product.as_bytes());
    for entry in &managed_entries {
        plan_hasher.update(entry.id.as_bytes());
        plan_hasher.update(entry.digest.as_bytes());
    }
    let recorded_at_unix_seconds = now.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    Ok(InstallReceipt {
        schema: RECEIPT_SCHEMA.to_string(),
        product: plan.product.clone(),
        source_revision,
        source_dirty,
        install_plan_digest: digest_result(plan_hasher),
        managed_entries,
        producer_version: env!("CARGO_PKG_VERSION").to_string(),
        recorded_at_unix_seconds,
    })
}

fn source_provenance(root: &Path) -> (String, bool) {
    let revision = git(root, &["rev-parse", "HEAD"]);
    let status = git(root, &["status", "--porcelain", "--untracked-files=normal"]);
    match (revision, status) {
        (Some(revision), Some(status)) => (revision, !status.trim().is_empty()),
        _ => ("unavailable".to_string(), true),
    }
}

fn git(root: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn managed_entry_receipts(plan: &InstallPlan) -> Result<Vec<ManagedEntryReceipt>, ReceiptError> {
    let mut entries: BTreeMap<String, Sha256> = BTreeMap::new();
    for action in &plan.actions {
        let (id, canonical, content) = match action {
            PlanAction::Symlink {
                entry_id,
                source,
                dest,
                link_mode,
                ..
            } => {
                let source_rel = source.strip_prefix(&plan.source_root).unwrap_or(source);
                let dest_rel = dest.strip_prefix(&plan.home).unwrap_or(dest);
                let content = if source.is_file() {
                    std::fs::read(source).map_err(|err| ReceiptError::Hash(err.to_string()))?
                } else {
                    Vec::new()
                };
                (
                    entry_id,
                    format!(
                        "symlink:{}:{}:{}",
                        source_rel.display(),
                        dest_rel.display(),
                        link_mode.label()
                    ),
                    content,
                )
            }
            PlanAction::ManagedBlock {
                entry_id,
                config_file,
                surface,
                body,
                ..
            } => {
                let dest_rel = config_file.strip_prefix(&plan.home).unwrap_or(config_file);
                (
                    entry_id,
                    format!("managed-block:{}:{surface}", dest_rel.display()),
                    body.as_bytes().to_vec(),
                )
            }
        };
        let hasher = entries.entry(id.clone()).or_default();
        hasher.update(canonical.as_bytes());
        hasher.update(&content);
    }
    Ok(entries
        .into_iter()
        .map(|(id, hasher)| ManagedEntryReceipt {
            id,
            digest: digest_result(hasher),
        })
        .collect())
}

fn digest_result(hasher: Sha256) -> String {
    let digest = hasher.finalize();
    format!(
        "sha256:{}",
        digest
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    )
}
