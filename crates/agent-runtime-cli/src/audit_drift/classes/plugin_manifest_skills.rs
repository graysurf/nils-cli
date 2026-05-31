//! Codex plugin-manifest skills consistency class (block-tier, exit 2).
//!
//! The Codex per-domain plugin manifest
//! (`targets/codex/plugins/<domain>/.codex-plugin/plugin.json`) carries a
//! hand-maintained `skills` array enumerating the plugin's skills. The
//! `intentional-difference` class records that the array is codex-only but
//! never inspects its contents, so it can silently diverge from
//! `manifests/plugins.yaml` (`contained_skills`) and `manifests/skills.yaml`
//! (`source`). A renamed or removed skill can stay advertised with a
//! `source` pointing at a directory that no longer exists
//! (graysurf/agent-runtime-kit#225; the #220 rename shipped exactly this and
//! the full CI gate stayed green).
//!
//! For every Codex `plugin.json` whose domain has a matching `plugins.yaml`
//! plugin, this class asserts that the set of advertised skill ids equals
//! the plugin's `contained_skills`, and that each advertised entry's
//! `source` matches `skills.yaml` and resolves to a directory on disk. Any
//! divergence is a `block`-tier finding. Domains without a `plugins.yaml`
//! plugin are out of scope here (an orphan-manifest concern); in the real
//! source tree every Codex plugin domain has a `plugins.yaml` entry, so this
//! class fully covers the renamed/removed-skill case.

use crate::audit_drift::{DriftReport, Finding, Severity};
use crate::render::manifest::{ManifestSet, SourceRoot};
use anyhow::{Context, Result};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

pub const CLASS: &str = "plugin-manifest-skills";

/// The `skills` array is a Codex-only plugin manifest field, so every
/// finding here is attributed to the `codex` product.
const PRODUCT: &str = "codex";

pub fn check(
    root: &SourceRoot,
    manifests: Option<&ManifestSet>,
    report: &mut DriftReport,
) -> Result<()> {
    let Some(manifests) = manifests else {
        return Ok(());
    };

    // Manifest source path keyed by full `domain.skill` id.
    let manifest_source: BTreeMap<&str, &str> = manifests
        .skills
        .skills
        .iter()
        .map(|s| (s.id.as_str(), s.source.as_str()))
        .collect();

    for plugin in &manifests.plugins.plugins {
        let domain = plugin.domain.as_str();
        let path = codex_plugin_manifest_path(root.path(), domain);
        let Some(raw_entries) = read_skills_array(&path)? else {
            // No Codex plugin.json for this declared plugin. Whether a
            // declared plugin must ship a Codex manifest is a separate
            // concern; this class only validates manifests that exist.
            continue;
        };
        let rel = rel(root.path(), &path);

        // Resolve each advertised entry to a full `domain.skill` id, flagging
        // malformed entries (missing string `id` / `source`) as blockers.
        let mut advertised: Vec<AdvertisedSkill> = Vec::new();
        for entry in &raw_entries {
            match (
                entry.get("id").and_then(Value::as_str),
                entry.get("source").and_then(Value::as_str),
            ) {
                (Some(id), Some(source)) => advertised.push(AdvertisedSkill {
                    full_id: format!("{domain}.{id}"),
                    source: source.to_string(),
                }),
                _ => report.push(block(
                    &rel,
                    format!(
                        "domain `{domain}`: plugin.json skills[] has a malformed entry (expected string `id` and `source`): {entry}"
                    ),
                )),
            }
        }

        let contained: BTreeSet<&str> =
            plugin.contained_skills.iter().map(String::as_str).collect();
        let advertised_ids: BTreeSet<&str> =
            advertised.iter().map(|s| s.full_id.as_str()).collect();

        // Set equality: advertised skills must mirror contained_skills.
        for extra in advertised_ids.difference(&contained) {
            report.push(block(
                &rel,
                format!(
                    "domain `{domain}`: plugin.json skills[] advertises `{extra}`, which is not in plugins.yaml contained_skills"
                ),
            ));
        }
        for missing in contained.difference(&advertised_ids) {
            report.push(block(
                &rel,
                format!(
                    "domain `{domain}`: plugins.yaml contained_skill `{missing}` is not advertised in plugin.json skills[]"
                ),
            ));
        }

        // Per-entry source integrity: matches skills.yaml and exists on disk.
        for skill in &advertised {
            match manifest_source.get(skill.full_id.as_str()) {
                None => report.push(block(
                    &rel,
                    format!(
                        "domain `{domain}`: plugin.json skill `{id}` has no entry in skills.yaml",
                        id = skill.full_id
                    ),
                )),
                Some(expected) if *expected != skill.source => report.push(block(
                    &rel,
                    format!(
                        "domain `{domain}`: plugin.json skill `{id}` source `{got}` does not match skills.yaml source `{expected}`",
                        id = skill.full_id,
                        got = skill.source,
                    ),
                )),
                Some(_) => {}
            }
            if !root.path().join(&skill.source).is_dir() {
                report.push(block(
                    &rel,
                    format!(
                        "domain `{domain}`: plugin.json skill `{id}` source `{src}` does not exist on disk",
                        id = skill.full_id,
                        src = skill.source,
                    ),
                ));
            }
        }
    }
    Ok(())
}

struct AdvertisedSkill {
    full_id: String,
    source: String,
}

fn block(rel: &Path, message: String) -> Finding {
    Finding {
        class: CLASS,
        severity: Severity::Block,
        product: Some(PRODUCT.to_string()),
        path: rel.to_path_buf(),
        message,
    }
}

fn codex_plugin_manifest_path(root: &Path, domain: &str) -> PathBuf {
    root.join("targets")
        .join("codex")
        .join("plugins")
        .join(domain)
        .join(".codex-plugin")
        .join("plugin.json")
}

/// Read the `skills` array of a Codex `plugin.json`. Returns `None` when the
/// file does not exist, an empty vec when the file omits `skills` (or it is
/// not an array — the resulting "missing contained_skill" findings make that
/// drift visible), and the array entries otherwise.
fn read_skills_array(path: &Path) -> Result<Option<Vec<Value>>> {
    if !path.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let value: Value =
        serde_json::from_str(&raw).with_context(|| format!("parse {}", path.display()))?;
    let skills = value
        .get("skills")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    Ok(Some(skills))
}

fn rel(source_root: &Path, path: &Path) -> PathBuf {
    path.strip_prefix(source_root).unwrap_or(path).to_path_buf()
}
