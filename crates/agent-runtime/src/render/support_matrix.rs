//! Shared support matrix renderer.
//!
//! The target consumes `<source-root>/manifests/surfaces.yaml` and writes the
//! derived human-readable view to `build/shared/SUPPORT_MATRIX.md`. The root
//! Markdown file is not parsed as input; row data comes from the manifest.

use crate::render::manifest::{SCHEMA_VERSION, SourceRoot};
use crate::render::writer::{guard_write_under, sandboxed_join};
use anyhow::{Context, Result, anyhow};
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};

const MANIFEST_NAME: &str = "surfaces.yaml";
const EXPECTED_TARGET: &str = "support-matrix";
const DEFAULT_OUTPUT: &str = "build/shared/SUPPORT_MATRIX.md";

#[derive(Debug, PartialEq, Eq)]
pub struct SupportMatrixReport {
    pub output_path: PathBuf,
    pub surfaces: usize,
    pub rows: usize,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SurfacesManifest {
    schema_version: u32,
    render: SurfaceRenderConfig,
    surfaces: Vec<Surface>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SurfaceRenderConfig {
    target: String,
    root_view: String,
    output: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Surface {
    id: String,
    ordinal: u32,
    name: String,
    products: SurfaceProducts,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SurfaceProducts {
    codex: SurfaceProduct,
    claude: SurfaceProduct,
}

impl SurfaceProducts {
    fn iter(&self) -> [(&'static str, &SurfaceProduct); 2] {
        [("codex", &self.codex), ("claude", &self.claude)]
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SurfaceProduct {
    state: SurfaceState,
    mechanism: String,
    source_artifacts: Vec<String>,
    min_product: String,
    min_nils_cli: String,
    acceptance: Vec<Acceptance>,
    source_manifest: Vec<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum SurfaceState {
    Shipped,
    Partial,
    PlannedNotShipped,
    NotShipped,
    NotApplicable,
}

impl SurfaceState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Shipped => "shipped",
            Self::Partial => "partial",
            Self::PlannedNotShipped => "planned-not-shipped",
            Self::NotShipped => "not-shipped",
            Self::NotApplicable => "not-applicable",
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Acceptance {
    kind: AcceptanceKind,
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    note: Option<String>,
    #[serde(default)]
    success: Option<AcceptanceSuccess>,
    #[serde(default)]
    descriptive_only: bool,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum AcceptanceKind {
    Ci,
    Live,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AcceptanceSuccess {
    exit_status: u8,
}

pub fn render(root: &SourceRoot) -> Result<SupportMatrixReport> {
    let manifest = load(root)?;
    validate(&manifest)?;
    let markdown = render_markdown(&manifest);
    let output_path = output_path(root, &manifest.render.output)?;
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create_dir_all {}", parent.display()))?;
    }
    let guarded = guard_write_under(root.path(), &output_path)?;
    fs::write(&guarded, markdown.as_bytes())
        .with_context(|| format!("write {}", guarded.display()))?;
    Ok(SupportMatrixReport {
        output_path: guarded,
        surfaces: manifest.surfaces.len(),
        rows: manifest.surfaces.len() * 2,
    })
}

pub fn update_golden(source_root: &Path, report: &SupportMatrixReport) -> Result<PathBuf> {
    let golden = source_root
        .join("tests")
        .join("golden")
        .join("shared")
        .join("SUPPORT_MATRIX.md");
    if let Some(parent) = golden.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create_dir_all {}", parent.display()))?;
    }
    fs::copy(&report.output_path, &golden).with_context(|| {
        format!(
            "copy {} -> {}",
            report.output_path.display(),
            golden.display()
        )
    })?;
    Ok(golden)
}

fn load(root: &SourceRoot) -> Result<SurfacesManifest> {
    let path = root.manifests_dir().join(MANIFEST_NAME);
    let raw = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    serde_yaml_ng::from_str(&raw).with_context(|| format!("parse {}", path.display()))
}

fn validate(manifest: &SurfacesManifest) -> Result<()> {
    if manifest.schema_version != SCHEMA_VERSION {
        return Err(anyhow!(
            "schema_version mismatch in manifests/{MANIFEST_NAME}: expected {SCHEMA_VERSION}, got {}",
            manifest.schema_version
        ));
    }
    if manifest.render.target != EXPECTED_TARGET {
        return Err(anyhow!(
            "render.target must be `{EXPECTED_TARGET}`, got `{}`",
            manifest.render.target
        ));
    }
    if manifest.render.root_view.trim().is_empty() {
        return Err(anyhow!("render.root_view must not be empty"));
    }
    if manifest.render.output.trim().is_empty() {
        return Err(anyhow!("render.output must not be empty"));
    }

    let mut seen = std::collections::BTreeSet::new();
    for surface in &manifest.surfaces {
        if surface.id.trim().is_empty() {
            return Err(anyhow!("surface ordinal {} has empty id", surface.ordinal));
        }
        if !seen.insert(surface.id.as_str()) {
            return Err(anyhow!("duplicate surface id `{}`", surface.id));
        }
        if surface.name.trim().is_empty() {
            return Err(anyhow!("surface `{}` has empty name", surface.id));
        }
        for (product, entry) in surface.products.iter() {
            validate_product(surface, product, entry)?;
        }
    }
    Ok(())
}

fn validate_product(surface: &Surface, product: &str, entry: &SurfaceProduct) -> Result<()> {
    if entry.mechanism.trim().is_empty() {
        return Err(anyhow!(
            "surface `{}` product `{product}` has empty mechanism",
            surface.id
        ));
    }
    if entry.min_product.trim().is_empty() {
        return Err(anyhow!(
            "surface `{}` product `{product}` has empty min_product",
            surface.id
        ));
    }
    if entry.min_nils_cli.trim().is_empty() {
        return Err(anyhow!(
            "surface `{}` product `{product}` has empty min_nils_cli",
            surface.id
        ));
    }
    for item in &entry.acceptance {
        let has_command = item
            .command
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty());
        let has_note = item
            .note
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty());
        match (has_command, has_note) {
            (true, false) => {
                if item.descriptive_only {
                    return Err(anyhow!(
                        "surface `{}` product `{product}` command acceptance cannot be descriptive_only",
                        surface.id
                    ));
                }
                if item.success.is_none() {
                    return Err(anyhow!(
                        "surface `{}` product `{product}` command acceptance requires success",
                        surface.id
                    ));
                }
            }
            (false, true) => {
                if !item.descriptive_only {
                    return Err(anyhow!(
                        "surface `{}` product `{product}` note acceptance requires descriptive_only=true",
                        surface.id
                    ));
                }
                if item.success.is_some() {
                    return Err(anyhow!(
                        "surface `{}` product `{product}` note acceptance must not include success",
                        surface.id
                    ));
                }
            }
            _ => {
                return Err(anyhow!(
                    "surface `{}` product `{product}` acceptance must contain exactly one of command or note",
                    surface.id
                ));
            }
        }
    }
    Ok(())
}

fn output_path(root: &SourceRoot, output: &str) -> Result<PathBuf> {
    let rel = if output.trim().is_empty() {
        DEFAULT_OUTPUT
    } else {
        output
    };
    sandboxed_join(root.path(), rel)
}

fn render_markdown(manifest: &SurfacesManifest) -> String {
    let mut lines = vec![
        "# SUPPORT_MATRIX".to_string(),
        String::new(),
        "<!-- Generated by `agent-runtime render --target support-matrix`; edit `manifests/surfaces.yaml`. -->".to_string(),
        String::new(),
        "Unified human-readable view of which Codex and Claude harness primitives `agent-runtime-kit` ships into today, by what mechanism, and at what version floor.".to_string(),
        String::new(),
        "## Matrix".to_string(),
        String::new(),
        "| surface | product | state | mechanism | source_artifact | min_product | min_nils_cli | ci_acceptance | live_acceptance | source_manifest |".to_string(),
        "|---|---|---|---|---|---|---|---|---|---|".to_string(),
    ];

    for surface in &manifest.surfaces {
        for (product, entry) in surface.products.iter() {
            lines.push(render_row(surface, product, entry));
        }
    }
    lines.push(String::new());
    lines.join("\n")
}

fn render_row(surface: &Surface, product: &str, entry: &SurfaceProduct) -> String {
    let surface_name = format!("{}. {}", surface.ordinal, surface.name);
    let ci = render_acceptance(&entry.acceptance, AcceptanceKind::Ci);
    let live = render_acceptance(&entry.acceptance, AcceptanceKind::Live);
    format!(
        "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
        cell(&surface_name),
        cell(product),
        cell(entry.state.as_str()),
        cell(&entry.mechanism),
        cell(&render_list(&entry.source_artifacts, true)),
        cell(&entry.min_product),
        cell(&entry.min_nils_cli),
        cell(&ci),
        cell(&live),
        cell(&render_list(&entry.source_manifest, true)),
    )
}

fn render_acceptance(items: &[Acceptance], kind: AcceptanceKind) -> String {
    let rendered = items
        .iter()
        .filter(|item| item.kind == kind)
        .map(|item| {
            if let Some(command) = item
                .command
                .as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty())
            {
                let success = item
                    .success
                    .as_ref()
                    .map(|success| format!(" (exit {})", success.exit_status))
                    .unwrap_or_default();
                format!("`{command}`{success}")
            } else {
                item.note
                    .as_deref()
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                    .unwrap_or("—")
                    .to_string()
            }
        })
        .collect::<Vec<_>>();
    render_list(&rendered, false)
}

fn render_list(items: &[String], code: bool) -> String {
    let non_empty = items
        .iter()
        .map(|item| item.trim())
        .filter(|item| !item.is_empty())
        .map(|item| {
            if code && item != "—" {
                format!("`{item}`")
            } else {
                item.to_string()
            }
        })
        .collect::<Vec<_>>();
    if non_empty.is_empty() {
        "—".to_string()
    } else {
        non_empty.join("<br>")
    }
}

fn cell(value: &str) -> String {
    value.replace('|', "\\|").replace('\n', "<br>")
}
