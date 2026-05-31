//! Documented product-difference drift class.
//!
//! `product-capabilities.yaml` records product-specific plugin manifest
//! fields. When the target plugin manifests actually diverge along
//! those documented fields, audit-drift reports the surface as
//! informational instead of treating the difference as hidden drift.

use crate::audit_drift::{DriftReport, Finding, Severity};
use crate::render::manifest::{ManifestSet, PluginManifestDiff, SourceRoot};
use anyhow::{Context, Result};
use serde_json::{Map, Value};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

pub const CLASS: &str = "intentional-difference";

pub fn check(
    root: &SourceRoot,
    manifests: Option<&ManifestSet>,
    report: &mut DriftReport,
) -> Result<()> {
    let Some(manifests) = manifests else {
        return Ok(());
    };
    let Some(diff) = manifests.product_capabilities.plugin_manifest_diff.as_ref() else {
        return Ok(());
    };
    if diff.codex_only_fields.is_empty() && diff.claude_only_fields.is_empty() {
        return Ok(());
    }

    for domain in plugin_domains(root.path())? {
        let codex_path = plugin_manifest_path(root.path(), "codex", &domain);
        let claude_path = plugin_manifest_path(root.path(), "claude", &domain);
        let Some(codex) = read_object(&codex_path)? else {
            continue;
        };
        let Some(claude) = read_object(&claude_path)? else {
            continue;
        };

        let ctx = PluginDiffContext {
            source_root: root.path(),
            domain: &domain,
            diff,
        };

        push_documented_fields(&ctx, "codex", report, &codex_path, &codex, &claude);
        push_documented_fields(&ctx, "claude", report, &claude_path, &claude, &codex);
    }
    Ok(())
}

struct PluginDiffContext<'a> {
    source_root: &'a Path,
    domain: &'a str,
    diff: &'a PluginManifestDiff,
}

fn push_documented_fields(
    ctx: &PluginDiffContext<'_>,
    product: &str,
    report: &mut DriftReport,
    path: &Path,
    product_manifest: &Map<String, Value>,
    other_manifest: &Map<String, Value>,
) {
    let fields = match product {
        "codex" => &ctx.diff.codex_only_fields,
        "claude" => &ctx.diff.claude_only_fields,
        _ => return,
    };

    for field in fields {
        if product_manifest.contains_key(field) && !other_manifest.contains_key(field) {
            report.push(Finding {
                class: CLASS,
                severity: Severity::Info,
                product: Some(product.to_string()),
                path: rel(ctx.source_root, path),
                message: format!(
                    "documented plugin manifest divergence for domain `{domain}`: field `{field}` is {product}-only",
                    domain = ctx.domain
                ),
            });
        }
    }
}

fn plugin_domains(root: &Path) -> Result<BTreeSet<String>> {
    let mut out = BTreeSet::new();
    for product in ["codex", "claude"] {
        let dir = root.join("targets").join(product).join("plugins");
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries {
            let entry = entry.with_context(|| format!("read {}", dir.display()))?;
            if entry.file_type().is_ok_and(|kind| kind.is_dir())
                && let Some(name) = entry.file_name().to_str()
            {
                out.insert(name.to_string());
            }
        }
    }
    Ok(out)
}

fn plugin_manifest_path(root: &Path, product: &str, domain: &str) -> PathBuf {
    root.join("targets")
        .join(product)
        .join("plugins")
        .join(domain)
        .join(format!(".{product}-plugin"))
        .join("plugin.json")
}

fn read_object(path: &Path) -> Result<Option<Map<String, Value>>> {
    if !path.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let value: Value =
        serde_json::from_str(&raw).with_context(|| format!("parse {}", path.display()))?;
    Ok(value.as_object().cloned())
}

fn rel(source_root: &Path, path: &Path) -> PathBuf {
    path.strip_prefix(source_root).unwrap_or(path).to_path_buf()
}
