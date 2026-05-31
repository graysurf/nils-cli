//! Extra live-surface drift class.
//!
//! The class compares the current product runtime home against the
//! install map. It only scans roots that the install map owns, so a
//! user's unrelated runtime state does not become audit input.

use crate::audit_drift::{DriftReport, Finding, Severity};
use crate::install::link_map::{LinkMap, LinkMapError};
use crate::live_surface;
use crate::render::manifest::{ManifestSet, ProductRoot, RuntimeRootsManifest, SourceRoot};
use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::path::PathBuf;

pub const CLASS: &str = "extra";

pub fn check(
    root: &SourceRoot,
    manifests: Option<&ManifestSet>,
    product: &str,
    report: &mut DriftReport,
) -> Result<()> {
    let Some(manifests) = manifests else {
        return Ok(());
    };
    let Some(product_root) = product_root(&manifests.runtime_roots, product) else {
        return Ok(());
    };
    let live_home = resolve_live_home(product_root);
    if !live_home.is_absolute() || !live_home.exists() {
        return Ok(());
    }
    let link_map = match LinkMap::load(root.path(), product) {
        Ok(link_map) => link_map,
        Err(LinkMapError::Missing { .. }) => return Ok(()),
        Err(err) => return Err(err.into()),
    };

    let expected = live_surface::expected_live_paths(root.path(), &link_map);
    let scan_roots = live_surface::scan_roots(&link_map);
    if scan_roots.is_empty() {
        return Ok(());
    }

    let live_files = live_surface::live_files_under_roots(&live_home, &scan_roots)
        .with_context(|| format!("scan live home {}", live_home.display()))?;
    for rel in live_files {
        if expected.contains(&rel) || live_surface::ignored_live_file(&rel) {
            continue;
        }
        report.push(Finding {
            class: CLASS,
            severity: Severity::Warn,
            product: Some(product.to_string()),
            path: rel.clone(),
            message: format!(
                "live runtime surface exists under an install-map root but is not tracked by the install map: {}",
                rel.display()
            ),
        });
    }

    Ok(())
}

fn product_root<'a>(
    runtime_roots: &'a RuntimeRootsManifest,
    product: &str,
) -> Option<&'a ProductRoot> {
    match product {
        "codex" => Some(&runtime_roots.products.codex),
        "claude" => Some(&runtime_roots.products.claude),
        _ => None,
    }
}

fn resolve_live_home(root: &ProductRoot) -> PathBuf {
    let env: BTreeMap<String, String> = std::env::vars().collect();
    PathBuf::from(expand_env_vars(&root.live_home, &env))
}

fn expand_env_vars(raw: &str, env: &BTreeMap<String, String>) -> String {
    let chars: Vec<char> = raw.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '$' {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        if chars.get(i + 1) == Some(&'{')
            && let Some(end) = find_matching_brace(&chars, i + 1)
        {
            let expr: String = chars[i + 2..end].iter().collect();
            out.push_str(&expand_braced_expr(&expr, env));
            i = end + 1;
            continue;
        }
        let mut end = i + 1;
        while end < chars.len() && (chars[end].is_ascii_alphanumeric() || chars[end] == '_') {
            end += 1;
        }
        if end == i + 1 {
            out.push('$');
            i += 1;
            continue;
        }
        let name: String = chars[i + 1..end].iter().collect();
        out.push_str(env.get(&name).map(String::as_str).unwrap_or(""));
        i = end;
    }
    out
}

fn find_matching_brace(chars: &[char], open_brace: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut i = open_brace + 1;
    while i < chars.len() {
        if chars[i] == '$' && chars.get(i + 1) == Some(&'{') {
            depth += 1;
            i += 2;
            continue;
        }
        if chars[i] == '}' {
            if depth == 0 {
                return Some(i);
            }
            depth -= 1;
        }
        i += 1;
    }
    None
}

fn expand_braced_expr(expr: &str, env: &BTreeMap<String, String>) -> String {
    if let Some((name, fallback)) = expr.split_once(":-") {
        if let Some(value) = env.get(name)
            && !value.is_empty()
        {
            return value.clone();
        }
        expand_env_vars(fallback, env)
    } else {
        env.get(expr).cloned().unwrap_or_default()
    }
}
