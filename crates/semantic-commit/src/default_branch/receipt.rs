use std::fs;
use std::io::Write;
use std::path::Path;

use nils_common::default_branch_receipt::{
    DELIVERY_WAIVER_ENV, DefaultBranchCompletion, DefaultBranchData, DefaultBranchReceipt,
    DefaultBranchRemote, SCHEMA_VERSION, normalized_delivery_waiver,
};
use serde_json::{Map, Value, json};

use super::preflight::RepositoryState;
use super::transaction::Postconditions;

pub(super) const PREVIEW_SCHEMA_VERSION: &str = "cli.semantic-commit.default-branch.preview.v1";

pub(super) fn preview(state: &RepositoryState) -> Value {
    let mut remote = Map::from_iter([
        (
            "configured_count".to_string(),
            json!(state.remote.configured_count),
        ),
        ("mode".to_string(), json!(state.remote.mode)),
        ("network_observed".to_string(), json!(false)),
        ("provider_mutated".to_string(), json!(false)),
        (
            "cached_relation".to_string(),
            json!(if state.remote.upstream.is_some() {
                "aligned"
            } else {
                "untracked"
            }),
        ),
    ]);
    if let Some(upstream) = state.remote.upstream.as_ref() {
        remote.insert("upstream".to_string(), json!(upstream));
    }
    json!({
        "schema_version": PREVIEW_SCHEMA_VERSION,
        "ok": true,
        "data": {
            "mode": "default-branch",
            "repository_fingerprint": state.fingerprint,
            "default_branch": state.default_branch,
            "head": state.head,
            "staged_file_count": state.staged_file_count,
            "remote": remote,
            "completion": {
                "default_branch_committed": false,
                "provider_delivery_attempted": false,
                "provider_delivered": false,
            },
        },
    })
}

pub(super) fn final_receipt(
    state: &RepositoryState,
    new_head: String,
    post: Postconditions,
) -> DefaultBranchReceipt {
    DefaultBranchReceipt {
        schema_version: SCHEMA_VERSION.to_string(),
        ok: true,
        data: DefaultBranchData {
            mode: "default-branch".to_string(),
            repository_fingerprint: state.fingerprint.clone(),
            default_branch: state.default_branch.clone(),
            old_head: state.head.clone(),
            new_head,
            parent_sha: post.parent,
            tree_sha: post.tree,
            signature: "verified-good".to_string(),
            staged_file_count: state.staged_file_count,
            remote: DefaultBranchRemote {
                configured_count: state.remote.configured_count,
                mode: state.remote.mode.to_string(),
                network_observed: false,
                provider_mutated: false,
                upstream: state.remote.upstream.clone(),
                cached_relation_before: if state.remote.upstream.is_some() {
                    "aligned"
                } else {
                    "untracked"
                }
                .to_string(),
                cached_relation_after: post.relation_after.to_string(),
            },
            completion: DefaultBranchCompletion {
                default_branch_committed: true,
                provider_delivery_attempted: false,
                provider_delivered: false,
            },
            delivery_waiver: stated_delivery_waiver(),
        },
    }
}

pub(super) fn verify_destination(root: &Path, path: &Path) -> Result<(), String> {
    if !path.is_absolute() {
        return Err("--receipt-out must be an absolute path".to_string());
    }
    if fs::symlink_metadata(path).is_ok() {
        return Err("--receipt-out must name a new non-symlink file".to_string());
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| "--receipt-out must have an existing parent directory".to_string())?;
    if fs::symlink_metadata(parent)
        .map(|metadata| metadata.file_type().is_symlink())
        .unwrap_or(false)
    {
        return Err("--receipt-out parent must not be a symlink".to_string());
    }
    let parent = fs::canonicalize(parent)
        .map_err(|error| format!("failed to resolve --receipt-out parent: {error}"))?;
    if !parent.is_dir() {
        return Err("--receipt-out parent is not a directory".to_string());
    }
    if parent.starts_with(root) {
        return Err("--receipt-out must be outside the repository worktree".to_string());
    }
    tempfile::NamedTempFile::new_in(&parent)
        .map_err(|error| format!("--receipt-out parent is not writable: {error}"))?;
    Ok(())
}

pub(super) fn write(path: &Path, receipt: &DefaultBranchReceipt) -> Result<(), String> {
    let parent = path.parent().expect("receipt parent preflighted");
    let mut temp = tempfile::NamedTempFile::new_in(parent)
        .map_err(|error| format!("failed to allocate receipt temp file: {error}"))?;
    serde_json::to_writer_pretty(temp.as_file_mut(), receipt)
        .map_err(|error| format!("failed to serialize default-branch receipt: {error}"))?;
    temp.as_file_mut()
        .write_all(b"\n")
        .map_err(|error| format!("failed to finalize default-branch receipt: {error}"))?;
    temp.as_file_mut()
        .sync_all()
        .map_err(|error| format!("failed to sync default-branch receipt: {error}"))?;
    temp.persist_noclobber(path).map_err(|error| {
        format!("failed to create default-branch receipt without overwrite: {error}")
    })?;
    fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| format!("failed to sync default-branch receipt directory: {error}"))?;
    Ok(())
}

fn stated_delivery_waiver() -> Option<String> {
    std::env::var(DELIVERY_WAIVER_ENV)
        .ok()
        .as_deref()
        .and_then(normalized_delivery_waiver)
}
