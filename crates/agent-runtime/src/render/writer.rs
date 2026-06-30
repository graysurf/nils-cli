//! Per-product render writer. Walks the skills declared for the active
//! product, renders each via Tera + helpers when its input hash differs
//! from the recorded cache entry, and writes the output under
//! `<source-root>/build/<product>/`. Skills with a cache hit are left
//! on disk verbatim — the determinism contract guarantees the cache-hit
//! path is byte-identical to a fresh render of the same source.
//!
//! Render only opens paths under `<source-root>/`. Every read is rooted
//! through [`source_path`](Self::source_path) and joined to the
//! canonical source root, so a malicious `skill.source` like
//! `../../etc` lands outside the source root and is rejected before any
//! I/O happens.

use crate::render::cache::{AGENTS_CACHE_FILE, CACHE_FILE, CacheEntry, RenderCache};
use crate::render::helpers::{HelperContext, register_all};
use crate::render::manifest::{Agent, ManifestSet, Skill, SourceRoot};
use anyhow::{Context, Result, anyhow};
use nils_markdown::Engine;
use sha2::{Digest, Sha256};
use std::fs;
use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

pub const SKILL_TEMPLATE_FILE: &str = "SKILL.md.tera";
/// Required canonical template under each `core/agents/<name>/` source
/// dir. Mirrors [`SKILL_TEMPLATE_FILE`]; the rendered output lands at the
/// product's `render_to` (e.g. `agents/<name>.toml`).
pub const AGENT_TEMPLATE_FILE: &str = "AGENT.md.tera";
pub const HOME_PROMPT_FILE: &str = "AGENT_HOME.md";
pub const NEUTRAL_HOME_PRODUCT: &str = "neutral";
const TERA_EXT: &str = "tera";

/// One file under a skill source directory. The path is relative to the
/// skill source root (e.g. `SKILL.md.tera`, `bin/topic_radar.py`); the
/// caller joins it against the canonical source dir before opening.
#[derive(Debug)]
struct SourceFile {
    rel: PathBuf,
    abs: PathBuf,
    /// Unix permission bits (mode & 0o777), used to preserve the
    /// executable bit on shell scripts when copying. Absent under
    /// `#[cfg(not(unix))]` builds; the hash and copy paths fall back
    /// to a constant on those platforms.
    #[cfg(unix)]
    mode: u32,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct RenderReport {
    pub product: String,
    pub output_root: PathBuf,
    pub rendered: Vec<String>,
    pub cached: Vec<String>,
    pub skipped: Vec<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct HomePromptReport {
    pub product: String,
    pub output_path: PathBuf,
    pub rendered: bool,
}

/// Render every skill declared for `product` from manifests rooted at
/// `root` into the default `<source-root>/build/<product>/` tree.
///
/// For renders that need a custom output destination (the
/// audit-drift rendered-target diff class renders into a scratch
/// dir to diff against the live build), use [`write_product_to`].
pub fn write_product(
    root: &SourceRoot,
    manifests: Arc<ManifestSet>,
    product: &str,
) -> Result<RenderReport> {
    let output_root = default_product_output_root(root, product);
    reject_unsafe_default_output_root(root, &output_root)?;
    write_product_to(root, manifests, product, &output_root)
}

/// Render variant that writes into `output_root` rather than the
/// default `<source-root>/build/<product>/`. The output root must
/// exist or be creatable; the symlink-escape and `..`-traversal
/// guards apply *relative to* the caller-provided `output_root`, so
/// the caller is responsible for choosing a safe root (audit-drift
/// uses a fresh `TempDir`).
///
/// Renders the skills surface and the optional agents surface into the
/// same `output_root`. The two surfaces keep independent cache files
/// ([`CACHE_FILE`] / [`AGENTS_CACHE_FILE`]) so neither reconciles away the
/// other's outputs on save. The returned report merges the rendered,
/// cached, and skipped ids from both surfaces.
pub(crate) fn write_product_to(
    root: &SourceRoot,
    manifests: Arc<ManifestSet>,
    product: &str,
    output_root: &Path,
) -> Result<RenderReport> {
    let mut report = write_skills_to(root, manifests.clone(), product, output_root)?;
    let agents = write_agents_to(root, manifests, product, output_root)?;
    report.rendered.extend(agents.rendered);
    report.cached.extend(agents.cached);
    report.skipped.extend(agents.skipped);
    write_home_prompt_to(root, product, output_root, false)?;
    Ok(report)
}

pub fn write_home_prompt(
    root: &SourceRoot,
    product: &str,
    require_source: bool,
) -> Result<HomePromptReport> {
    let output_root = default_product_output_root(root, product);
    reject_unsafe_default_output_root(root, &output_root)?;
    write_home_prompt_to(root, product, &output_root, require_source)
}

fn default_product_output_root(root: &SourceRoot, product: &str) -> PathBuf {
    root.path().join("build").join(product)
}

fn reject_unsafe_default_output_root(root: &SourceRoot, output_root: &Path) -> Result<()> {
    let source_root = root.path();
    let build_root = source_root.join("build");
    reject_existing_symlink(&build_root, "default render build directory")?;
    reject_existing_symlink(output_root, "default render output root")?;

    if let Some(canonical_build_root) = canonicalize_if_exists(&build_root)? {
        if !canonical_build_root.starts_with(source_root) {
            return Err(anyhow!(
                "default render build directory {} resolves outside the source root \
                 ({} not under {}) — refusing to write",
                build_root.display(),
                canonical_build_root.display(),
                source_root.display(),
            ));
        }

        if let Some(canonical_output_root) = canonicalize_if_exists(output_root)?
            && !canonical_output_root.starts_with(&canonical_build_root)
        {
            return Err(anyhow!(
                "default render output root {} resolves outside the build directory \
                 ({} not under {}) — refusing to write",
                output_root.display(),
                canonical_output_root.display(),
                canonical_build_root.display(),
            ));
        }
    }

    Ok(())
}

fn reject_existing_symlink(path: &Path, label: &str) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(anyhow!(
            "{label} {} is a symlink; refusing to use it as a render root",
            path.display()
        )),
        Ok(_) => Ok(()),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| format!("stat {label} {}", path.display())),
    }
}

fn canonicalize_if_exists(path: &Path) -> Result<Option<PathBuf>> {
    match path.canonicalize() {
        Ok(path) => Ok(Some(path)),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err).with_context(|| format!("canonicalize {}", path.display())),
    }
}

fn write_home_prompt_to(
    root: &SourceRoot,
    product: &str,
    output_root: &Path,
    require_source: bool,
) -> Result<HomePromptReport> {
    let source = root.path().join(HOME_PROMPT_FILE);
    let output_root = output_root.to_path_buf();
    let output_path = output_root.join(HOME_PROMPT_FILE);
    if !source.exists() {
        if require_source {
            return Err(anyhow!(
                "home prompt source {} is missing",
                source.display()
            ));
        }
        remove_stale_home_prompt(&output_root, &output_path)?;
        return Ok(HomePromptReport {
            product: product.to_string(),
            output_path,
            rendered: false,
        });
    }

    fs::create_dir_all(&output_root)
        .with_context(|| format!("create_dir_all {}", output_root.display()))?;
    let canonical_source_root = root.path().to_path_buf();
    let canonical_output_root = output_root
        .canonicalize()
        .with_context(|| format!("canonicalize output root {}", output_root.display()))?;
    let source = canonicalize_under(&canonical_source_root, &source)?;
    let body = fs::read_to_string(&source)
        .with_context(|| format!("read home prompt {}", source.display()))?;
    let rendered = render_home_prompt_template(product, &body)?;
    let output_path = guard_write_under(&canonical_output_root, &output_path)?;
    reject_leaf_symlink(&output_path)?;
    fs::write(&output_path, rendered.as_bytes())
        .with_context(|| format!("write {}", output_path.display()))?;

    Ok(HomePromptReport {
        product: product.to_string(),
        output_path,
        rendered: true,
    })
}

fn remove_stale_home_prompt(output_root: &Path, output_path: &Path) -> Result<()> {
    let metadata = match fs::symlink_metadata(output_path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(()),
        Err(err) => {
            return Err(err)
                .with_context(|| format!("stat stale home prompt {}", output_path.display()));
        }
    };
    let canonical_output_root = output_root
        .canonicalize()
        .with_context(|| format!("canonicalize output root {}", output_root.display()))?;
    let guarded_output = guard_write_under(&canonical_output_root, output_path)?;
    if !metadata.file_type().is_symlink() {
        let canonical_output = guarded_output
            .canonicalize()
            .with_context(|| format!("canonicalize stale home prompt {}", output_path.display()))?;
        if !canonical_output.starts_with(&canonical_output_root) {
            return Err(anyhow!(
                "stale home prompt output {} resolves outside the build root \
                 ({} not under {}) — refusing to remove",
                output_path.display(),
                canonical_output.display(),
                canonical_output_root.display(),
            ));
        }
    }
    fs::remove_file(&guarded_output)
        .with_context(|| format!("remove stale home prompt {}", guarded_output.display()))?;
    prune_empty_dirs_upward(output_root, &canonical_output_root, HOME_PROMPT_FILE);
    Ok(())
}

/// Render every skill declared for `product` into `output_root`. This is
/// the original per-product writer; the optional agents surface renders
/// separately through [`write_agents_to`] against its own cache file.
fn write_skills_to(
    root: &SourceRoot,
    manifests: Arc<ManifestSet>,
    product: &str,
    output_root: &Path,
) -> Result<RenderReport> {
    require_known_product(&manifests, product)?;
    let output_root = output_root.to_path_buf();
    fs::create_dir_all(&output_root)
        .with_context(|| format!("create_dir_all {}", output_root.display()))?;

    let cache_path = output_root.join(CACHE_FILE);
    let prior_cache = RenderCache::load_or_empty(&cache_path);
    let manifest_bytes = read_manifest_bundle(root)?;
    let mut next_cache = RenderCache::empty();
    let mut report = RenderReport {
        product: product.to_string(),
        output_root: output_root.clone(),
        ..RenderReport::default()
    };

    let canonical_source_root = root.path().to_path_buf();
    let canonical_output_root = output_root
        .canonicalize()
        .with_context(|| format!("canonicalize output root {}", output_root.display()))?;

    for skill in &manifests.skills.skills {
        let Some(render) = skill.products.get(product) else {
            report.skipped.push(skill.id.clone());
            continue;
        };
        // `render_to` is the output path RELATIVE to `<source-root>/build/<product>/`,
        // including the filename for the rendered SKILL.md (e.g.
        // `plugins/reporting/skills/daily-brief/SKILL.md`). Manifests that include
        // a leading `build/<product>/` segment produce doubled output paths
        // (`build/<product>/build/<product>/...`) because the binary already
        // prepends `build/<product>/`. Reject that shape with a helpful error
        // so the source manifest can be fixed once, not silently broken.
        validate_render_to(&skill.id, product, &render.render_to)?;

        let source_dir = sandboxed_join(root.path(), &skill.source)?;
        let canonical_source_dir = canonicalize_under(&canonical_source_root, &source_dir)?;
        let source_files = walk_skill_source(&canonical_source_dir, &canonical_source_root)
            .with_context(|| {
                format!(
                    "walk source for skill {} at {}",
                    skill.id,
                    canonical_source_dir.display()
                )
            })?;
        let template_file = source_files
            .iter()
            .find(|f| f.rel == Path::new(SKILL_TEMPLATE_FILE))
            .ok_or_else(|| {
                anyhow!(
                    "skill {} source {} is missing required {SKILL_TEMPLATE_FILE}",
                    skill.id,
                    skill.source
                )
            })?;
        let template_body = fs::read_to_string(&template_file.abs).with_context(|| {
            format!(
                "read template {} for skill {}",
                template_file.abs.display(),
                skill.id
            )
        })?;

        // Pre-compute the set of paths this skill will write (relative
        // to `build/<product>/`). Used for the cache entry and for
        // surgical removal of stale prior outputs that two-skills-share-
        // a-dir layouts make impossible to handle with `remove_dir_all`.
        let render_to_rel = PathBuf::from(&render.render_to);
        let output_dir_rel = render_to_rel.parent().ok_or_else(|| {
            anyhow!(
                "render_to {:?} for skill {} has no parent dir",
                render.render_to,
                skill.id,
            )
        })?;
        let mut planned_outputs: Vec<String> = Vec::with_capacity(source_files.len());
        for file in &source_files {
            let rel = if file.rel == Path::new(SKILL_TEMPLATE_FILE) {
                render_to_rel.clone()
            } else {
                output_dir_rel.join(strip_tera_suffix(&file.rel))
            };
            planned_outputs.push(rel.to_string_lossy().into_owned());
        }
        planned_outputs.sort();
        planned_outputs.dedup();

        let input_hash = input_hash(
            product,
            &skill.id,
            &render.render_to,
            &source_files,
            &canonical_source_dir,
            &manifest_bytes,
        )
        .with_context(|| format!("hash source tree for skill {}", skill.id))?;
        let entry = CacheEntry {
            hash: input_hash.clone(),
            outputs: planned_outputs.clone(),
        };
        let output_path = sandboxed_join(&output_root, &render.render_to)?;
        let cache_hit = prior_cache
            .skills
            .get(&skill.id)
            .is_some_and(|prior| prior == &entry)
            && output_path.exists();

        if cache_hit {
            report.cached.push(skill.id.clone());
        } else {
            // Cache miss: remove files this skill wrote on the prior
            // run that are NOT in `planned_outputs` (renamed or deleted
            // siblings). Crucially, we only touch paths recorded in
            // *this* skill's prior cache entry — files owned by sibling
            // skills that happen to share a parent dir (e.g.
            // `sample.determinism` writes `skills/sample/SKILL.md` and
            // `sample.codex-only` writes `skills/sample/CODEX_ONLY.md`)
            // are never disturbed.
            if let Some(prior) = prior_cache.skills.get(&skill.id) {
                let planned: std::collections::BTreeSet<&String> = planned_outputs.iter().collect();
                for stale in &prior.outputs {
                    if planned.contains(stale) {
                        continue;
                    }
                    let stale_path = sandboxed_join(&output_root, stale)?;
                    if !stale_path.exists() {
                        continue;
                    }
                    // Symlink-escape guard: a hostile cache entry could
                    // record a path that — combined with a symlink
                    // pre-staged at that location — points outside the
                    // build root. Canonicalize and re-verify before any
                    // unlink.
                    let canonical_stale = stale_path.canonicalize().with_context(|| {
                        format!("canonicalize stale output {}", stale_path.display())
                    })?;
                    if !canonical_stale.starts_with(&canonical_output_root) {
                        return Err(anyhow!(
                            "stale rendered output {} resolves outside the build root \
                             ({} not under {}) — refusing to remove",
                            stale_path.display(),
                            canonical_stale.display(),
                            canonical_output_root.display(),
                        ));
                    }
                    fs::remove_file(&canonical_stale).with_context(|| {
                        format!("remove stale rendered file {}", canonical_stale.display())
                    })?;
                }
            }

            // Render the SKILL template and write it at `render_to`.
            if let Some(parent) = output_path.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("create_dir_all {}", parent.display()))?;
            }
            let rendered =
                render_template(root.path(), &manifests, product, skill, &template_body)?;
            // Same symlink-escape guard for writes: canonicalize the
            // parent (which we just ensured exists) and verify it stays
            // beneath `<source-root>/build/<product>/`. A hostile
            // `render_to` of `../../etc/passwd` is already rejected by
            // sandboxed_join; this also catches `dir-that-is-a-symlink/foo`.
            let output_path_guarded = guard_write_under(&canonical_output_root, &output_path)?;
            fs::write(&output_path_guarded, rendered.as_bytes())
                .with_context(|| format!("write {}", output_path_guarded.display()))?;

            // Walk every other source file. Sibling .tera files are
            // rendered through the same helper context (the suffix is
            // stripped from the destination filename); non-.tera files
            // are byte-copied verbatim with the original mode preserved
            // so executables stay executable on disk.
            for file in &source_files {
                if file.rel == Path::new(SKILL_TEMPLATE_FILE) {
                    continue;
                }
                let dest_rel = strip_tera_suffix(&file.rel);
                let dest = sandboxed_join(
                    &output_root,
                    &output_dir_rel.join(&dest_rel).to_string_lossy(),
                )?;
                if let Some(parent) = dest.parent() {
                    fs::create_dir_all(parent)
                        .with_context(|| format!("create_dir_all {}", parent.display()))?;
                }
                let dest = guard_write_under(&canonical_output_root, &dest)?;
                if file.rel.extension().and_then(|e| e.to_str()) == Some(TERA_EXT) {
                    let body = fs::read_to_string(&file.abs).with_context(|| {
                        format!(
                            "read sibling tera template {} for skill {}",
                            file.abs.display(),
                            skill.id
                        )
                    })?;
                    let rendered = render_template(root.path(), &manifests, product, skill, &body)?;
                    fs::write(&dest, rendered.as_bytes())
                        .with_context(|| format!("write {}", dest.display()))?;
                } else {
                    // `fs::copy` follows symlinks at the source. If the
                    // source is itself a hostile symlink that points
                    // outside the source root, we've already rejected
                    // it during the `walk_skill_source` canonicalize-
                    // under check, so this is safe.
                    fs::copy(&file.abs, &dest).with_context(|| {
                        format!("copy {} -> {}", file.abs.display(), dest.display())
                    })?;
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        let perms = fs::Permissions::from_mode(file.mode);
                        fs::set_permissions(&dest, perms).with_context(|| {
                            format!("set mode {:#o} on {}", file.mode, dest.display())
                        })?;
                    }
                }
            }
            report.rendered.push(skill.id.clone());
        }
        next_cache.skills.insert(skill.id.clone(), entry);
    }

    // Reconcile retired skills. The per-skill cleanup above only fires for
    // skills still being rendered, so render is otherwise additive: a skill
    // removed from the manifest (or moved off this product) leaves its
    // outputs behind in `build/<product>/`. That stale tree is not just
    // untidy — `prune-stale` rebuilds its "expected" set by expanding the
    // recursive link-map entry over the current `build/` tree, so a retired
    // skill that lingers in build/ is treated as still-expected and silently
    // kept in the live home (`candidates=0`). Removing the retired outputs
    // here closes both gaps from one place.
    reconcile_retired_skills(
        &prior_cache,
        &next_cache,
        &output_root,
        &canonical_output_root,
    )?;

    next_cache
        .save(&cache_path)
        .with_context(|| format!("write {}", cache_path.display()))?;
    Ok(report)
}

/// Render every agent declared in the optional `agents.yaml` for
/// `product` into `output_root`. Structurally mirrors [`write_skills_to`]
/// — same sandboxing, stale-output removal, sibling rendering, and retired
/// reconcile — but keys its cache on [`AGENTS_CACHE_FILE`], requires the
/// [`AGENT_TEMPLATE_FILE`] canonical template, and renders through
/// [`render_agent_template`]. A tree with no agents (the common case)
/// returns an empty report and only writes the agents cache file.
fn write_agents_to(
    root: &SourceRoot,
    manifests: Arc<ManifestSet>,
    product: &str,
    output_root: &Path,
) -> Result<RenderReport> {
    require_known_product(&manifests, product)?;
    let output_root = output_root.to_path_buf();
    fs::create_dir_all(&output_root)
        .with_context(|| format!("create_dir_all {}", output_root.display()))?;

    let cache_path = output_root.join(AGENTS_CACHE_FILE);
    let prior_cache = RenderCache::load_or_empty(&cache_path);
    let manifest_bytes = read_manifest_bundle(root)?;
    let mut next_cache = RenderCache::empty();
    let mut report = RenderReport {
        product: product.to_string(),
        output_root: output_root.clone(),
        ..RenderReport::default()
    };

    let canonical_source_root = root.path().to_path_buf();
    let canonical_output_root = output_root
        .canonicalize()
        .with_context(|| format!("canonicalize output root {}", output_root.display()))?;

    for agent in &manifests.agents.agents {
        let Some(render) = agent.products.get(product) else {
            report.skipped.push(agent.id.clone());
            continue;
        };
        validate_render_to(&agent.id, product, &render.render_to)?;

        let source_dir = sandboxed_join(root.path(), &agent.source)?;
        let canonical_source_dir = canonicalize_under(&canonical_source_root, &source_dir)?;
        let source_files = walk_skill_source(&canonical_source_dir, &canonical_source_root)
            .with_context(|| {
                format!(
                    "walk source for agent {} at {}",
                    agent.id,
                    canonical_source_dir.display()
                )
            })?;
        let template_file = source_files
            .iter()
            .find(|f| f.rel == Path::new(AGENT_TEMPLATE_FILE))
            .ok_or_else(|| {
                anyhow!(
                    "agent {} source {} is missing required {AGENT_TEMPLATE_FILE}",
                    agent.id,
                    agent.source
                )
            })?;
        let template_body = fs::read_to_string(&template_file.abs).with_context(|| {
            format!(
                "read template {} for agent {}",
                template_file.abs.display(),
                agent.id
            )
        })?;

        let render_to_rel = PathBuf::from(&render.render_to);
        let output_dir_rel = render_to_rel.parent().ok_or_else(|| {
            anyhow!(
                "render_to {:?} for agent {} has no parent dir",
                render.render_to,
                agent.id,
            )
        })?;
        let mut planned_outputs: Vec<String> = Vec::with_capacity(source_files.len());
        for file in &source_files {
            let rel = if file.rel == Path::new(AGENT_TEMPLATE_FILE) {
                render_to_rel.clone()
            } else {
                output_dir_rel.join(strip_tera_suffix(&file.rel))
            };
            planned_outputs.push(rel.to_string_lossy().into_owned());
        }
        planned_outputs.sort();
        planned_outputs.dedup();

        let input_hash = input_hash(
            product,
            &agent.id,
            &render.render_to,
            &source_files,
            &canonical_source_dir,
            &manifest_bytes,
        )
        .with_context(|| format!("hash source tree for agent {}", agent.id))?;
        let entry = CacheEntry {
            hash: input_hash.clone(),
            outputs: planned_outputs.clone(),
        };
        let output_path = sandboxed_join(&output_root, &render.render_to)?;
        let cache_hit = prior_cache
            .skills
            .get(&agent.id)
            .is_some_and(|prior| prior == &entry)
            && output_path.exists();

        if cache_hit {
            report.cached.push(agent.id.clone());
        } else {
            // Same surgical stale-file removal as the skills path: only
            // touch paths recorded in this agent's prior cache entry.
            if let Some(prior) = prior_cache.skills.get(&agent.id) {
                let planned: std::collections::BTreeSet<&String> = planned_outputs.iter().collect();
                for stale in &prior.outputs {
                    if planned.contains(stale) {
                        continue;
                    }
                    let stale_path = sandboxed_join(&output_root, stale)?;
                    if !stale_path.exists() {
                        continue;
                    }
                    let canonical_stale = stale_path.canonicalize().with_context(|| {
                        format!("canonicalize stale output {}", stale_path.display())
                    })?;
                    if !canonical_stale.starts_with(&canonical_output_root) {
                        return Err(anyhow!(
                            "stale rendered output {} resolves outside the build root \
                             ({} not under {}) — refusing to remove",
                            stale_path.display(),
                            canonical_stale.display(),
                            canonical_output_root.display(),
                        ));
                    }
                    fs::remove_file(&canonical_stale).with_context(|| {
                        format!("remove stale rendered file {}", canonical_stale.display())
                    })?;
                }
            }

            if let Some(parent) = output_path.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("create_dir_all {}", parent.display()))?;
            }
            let rendered =
                render_agent_template(root.path(), &manifests, product, agent, &template_body)?;
            let output_path_guarded = guard_write_under(&canonical_output_root, &output_path)?;
            fs::write(&output_path_guarded, rendered.as_bytes())
                .with_context(|| format!("write {}", output_path_guarded.display()))?;

            for file in &source_files {
                if file.rel == Path::new(AGENT_TEMPLATE_FILE) {
                    continue;
                }
                let dest_rel = strip_tera_suffix(&file.rel);
                let dest = sandboxed_join(
                    &output_root,
                    &output_dir_rel.join(&dest_rel).to_string_lossy(),
                )?;
                if let Some(parent) = dest.parent() {
                    fs::create_dir_all(parent)
                        .with_context(|| format!("create_dir_all {}", parent.display()))?;
                }
                let dest = guard_write_under(&canonical_output_root, &dest)?;
                if file.rel.extension().and_then(|e| e.to_str()) == Some(TERA_EXT) {
                    let body = fs::read_to_string(&file.abs).with_context(|| {
                        format!(
                            "read sibling tera template {} for agent {}",
                            file.abs.display(),
                            agent.id
                        )
                    })?;
                    let rendered =
                        render_agent_template(root.path(), &manifests, product, agent, &body)?;
                    fs::write(&dest, rendered.as_bytes())
                        .with_context(|| format!("write {}", dest.display()))?;
                } else {
                    fs::copy(&file.abs, &dest).with_context(|| {
                        format!("copy {} -> {}", file.abs.display(), dest.display())
                    })?;
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        let perms = fs::Permissions::from_mode(file.mode);
                        fs::set_permissions(&dest, perms).with_context(|| {
                            format!("set mode {:#o} on {}", file.mode, dest.display())
                        })?;
                    }
                }
            }
            report.rendered.push(agent.id.clone());
        }
        next_cache.skills.insert(agent.id.clone(), entry);
    }

    // `reconcile_retired_skills` is generic over the `RenderCache.skills`
    // map; here it reconciles retired *agents* against the agents cache.
    reconcile_retired_skills(
        &prior_cache,
        &next_cache,
        &output_root,
        &canonical_output_root,
    )?;

    next_cache
        .save(&cache_path)
        .with_context(|| format!("write {}", cache_path.display()))?;
    Ok(report)
}

/// Remove `build/<product>/` outputs for skills recorded in `prior_cache`
/// that are absent from this run's `next_cache` (retired from the manifest
/// or moved off this product). Each file is removed with the same
/// canonicalize-under-output-root guard the cache-miss path uses; the
/// directories the removals empty are pruned upward, stopping at
/// `output_root`. Paths still owned by a present skill (shared output) are
/// never removed, and a shared parent dir that stays non-empty is left in
/// place, so sibling skills sharing a directory are unaffected.
fn reconcile_retired_skills(
    prior_cache: &RenderCache,
    next_cache: &RenderCache,
    output_root: &Path,
    canonical_output_root: &Path,
) -> Result<()> {
    // Paths a still-present skill writes this run must never be removed,
    // even if a retired skill also recorded them.
    let live_outputs: std::collections::BTreeSet<&String> = next_cache
        .skills
        .values()
        .flat_map(|entry| entry.outputs.iter())
        .collect();

    for (skill_id, prior) in &prior_cache.skills {
        if next_cache.skills.contains_key(skill_id) {
            continue;
        }
        for rel in &prior.outputs {
            if live_outputs.contains(rel) {
                continue;
            }
            let path = sandboxed_join(output_root, rel)?;
            // May already be gone (manual cleanup, or a path another retired
            // skill removed first). `symlink_metadata` avoids following a
            // dangling symlink.
            if fs::symlink_metadata(&path).is_err() {
                continue;
            }
            // Symlink-escape guard, mirroring the cache-miss removal path: a
            // hostile cache entry combined with a pre-staged symlink could
            // resolve outside the build root. Canonicalize and re-verify
            // before any unlink.
            let canonical = path
                .canonicalize()
                .with_context(|| format!("canonicalize retired output {}", path.display()))?;
            if !canonical.starts_with(canonical_output_root) {
                return Err(anyhow!(
                    "retired rendered output {} resolves outside the build root \
                     ({} not under {}) — refusing to remove",
                    path.display(),
                    canonical.display(),
                    canonical_output_root.display(),
                ));
            }
            fs::remove_file(&canonical)
                .with_context(|| format!("remove retired rendered file {}", canonical.display()))?;
        }
        // Prune the directories the removals emptied. Done after all files
        // for this skill are gone so a leaf dir whose siblings were also
        // retire-owned collapses fully.
        for rel in &prior.outputs {
            prune_empty_dirs_upward(output_root, canonical_output_root, rel);
        }
    }
    Ok(())
}

/// Remove now-empty ancestor directories of a removed output file, walking
/// from the file's parent up toward `output_root`. Stops at the first
/// directory that is non-empty (a sibling still owns content), missing, or
/// resolves to / above `output_root`. Best-effort: a failed `remove_dir`
/// (e.g. a race) simply ends the climb without erroring the render.
fn prune_empty_dirs_upward(output_root: &Path, canonical_output_root: &Path, rel: &str) {
    let mut dir = match PathBuf::from(rel).parent() {
        Some(parent) if !parent.as_os_str().is_empty() => output_root.join(parent),
        _ => return,
    };
    // Climb ends as soon as a directory no longer canonicalizes (already
    // removed or never existed).
    while let Ok(canonical) = dir.canonicalize() {
        if canonical == *canonical_output_root || !canonical.starts_with(canonical_output_root) {
            break;
        }
        let is_empty = match fs::read_dir(&canonical) {
            Ok(mut entries) => entries.next().is_none(),
            Err(_) => break,
        };
        if !is_empty {
            break; // a sibling still owns content here
        }
        if fs::remove_dir(&canonical).is_err() {
            break;
        }
        match dir.parent() {
            Some(parent) => dir = parent.to_path_buf(),
            None => break,
        }
    }
}

/// Reject `render_to` values that start with `build/<product>/` (or the
/// generic `build/` prefix). The output root is already
/// `<source-root>/build/<product>/`; a `render_to` like
/// `build/codex/plugins/...` doubles the prefix to
/// `build/codex/build/codex/plugins/...`. The render cache happily records
/// the (undoubled) intended path but the on-disk file lives at the doubled
/// path, which produces audit-drift confusion and silently broken installs.
///
/// The canonical form is the path **relative to** `build/<product>/`,
/// **including** the rendered filename. Per the source-doc canonical
/// example for the reporting POC:
///
/// ```yaml
/// products:
///   codex:
///     render_to: plugins/reporting/skills/daily-brief/SKILL.md
///   claude:
///     render_to: plugins/reporting/skills/daily-brief/SKILL.md
/// ```
fn validate_render_to(skill_id: &str, product: &str, render_to: &str) -> Result<()> {
    let leading = render_to.split('/').next().unwrap_or(render_to);
    if leading == "build" {
        return Err(anyhow!(
            "render_to {render_to:?} for skill {skill_id} (product {product}) starts with \
             `build/`; the binary already prepends `build/{product}/` to the value, so this \
             shape would double the prefix. Use a path relative to `build/{product}/` \
             (including the rendered filename), e.g. `plugins/<plugin>/skills/<skill>/SKILL.md`.",
        ));
    }
    Ok(())
}

fn require_known_product(manifests: &ManifestSet, product: &str) -> Result<()> {
    match product {
        "codex" | "claude" | "hermes" => Ok(()),
        other => Err(anyhow!(
            "unknown --product {other:?}; supported: codex, claude, hermes. \
             schema_version={}",
            manifests.product_capabilities.schema_version
        )),
    }
}

fn render_template(
    source_root: &Path,
    manifests: &Arc<ManifestSet>,
    product: &str,
    skill: &Skill,
    template_body: &str,
) -> Result<String> {
    let ctx = HelperContext {
        source_root: source_root.to_path_buf(),
        manifests: manifests.clone(),
        current_product: product.to_string(),
        current_skill_id: skill.id.clone(),
        current_skill_required_clis: skill.required_clis.clone(),
        current_skill_state_out_mode: skill.state_out_mode,
    };
    let mut engine = Engine::builder().build();
    register_all(&mut engine, Arc::new(ctx));
    let vars = serde_json::json!({ "product": product });
    engine
        .render_str(template_body, &vars)
        .with_context(|| format!("render skill {}", skill.id))
}

/// Render an agent template. Mirrors [`render_template`] but builds the
/// helper context from an [`Agent`] (which carries no `required_clis` or
/// `state_out_mode` of its own) and exposes the active `product` and agent
/// `id` as Tera variables, so one canonical `AGENT.md.tera` can branch to
/// Codex TOML vs Claude Markdown. The skill-bound helpers stay registered
/// for `cli_ref` reuse; agent templates are not expected to call
/// `skill_ref` / `state_out`.
fn render_agent_template(
    source_root: &Path,
    manifests: &Arc<ManifestSet>,
    product: &str,
    agent: &Agent,
    template_body: &str,
) -> Result<String> {
    let ctx = HelperContext {
        source_root: source_root.to_path_buf(),
        manifests: manifests.clone(),
        current_product: product.to_string(),
        current_skill_id: agent.id.clone(),
        current_skill_required_clis: Default::default(),
        current_skill_state_out_mode: Default::default(),
    };
    let mut engine = Engine::builder().build();
    register_all(&mut engine, Arc::new(ctx));
    let vars = serde_json::json!({ "product": product, "id": agent.id });
    engine
        .render_str(template_body, &vars)
        .with_context(|| format!("render agent {}", agent.id))
}

fn render_home_prompt_template(product: &str, template_body: &str) -> Result<String> {
    let mut engine = Engine::builder().build();
    let vars = serde_json::json!({ "product": product });
    engine
        .render_str(template_body, &vars)
        .context("render home prompt")
}

struct ManifestBytes {
    skills: Vec<u8>,
    plugins: Vec<u8>,
    product_capabilities: Vec<u8>,
    runtime_roots: Vec<u8>,
    cli_tools: Vec<u8>,
    agents: Vec<u8>,
}

fn read_manifest_bundle(root: &SourceRoot) -> Result<ManifestBytes> {
    let dir = root.manifests_dir();
    let read = |name: &str| -> Result<Vec<u8>> {
        let path = dir.join(name);
        fs::read(&path).with_context(|| format!("hash-read {}", path.display()))
    };
    // `agents.yaml` is optional; absence hashes as empty bytes so a tree
    // without the file keeps a stable digest.
    let read_optional = |name: &str| -> Result<Vec<u8>> {
        let path = dir.join(name);
        if !path.exists() {
            return Ok(Vec::new());
        }
        fs::read(&path).with_context(|| format!("hash-read {}", path.display()))
    };
    Ok(ManifestBytes {
        skills: read("skills.yaml")?,
        plugins: read("plugins.yaml")?,
        product_capabilities: read("product-capabilities.yaml")?,
        runtime_roots: read("runtime-roots.yaml")?,
        cli_tools: read("cli-tools.yaml")?,
        agents: read_optional("agents.yaml")?,
    })
}

fn input_hash(
    product: &str,
    id: &str,
    render_to: &str,
    source_files: &[SourceFile],
    canonical_source_dir: &Path,
    manifests: &ManifestBytes,
) -> Result<String> {
    // Hash version bumped to v3 when the agents render surface landed:
    // the manifest bundle now folds in `agents.yaml`, so every prior
    // cache entry is auto-invalidated by the version tag (v2 entries no
    // longer match) and re-rendered byte-identically. v2 itself landed
    // with multi-file render; v0.13 entries were invalidated then.
    let mut hasher = Sha256::new();
    hasher.update(b"agent-runtime-cli render v3\0");
    hasher.update(product.as_bytes());
    hasher.update(b"\0");
    hasher.update(id.as_bytes());
    hasher.update(b"\0");
    hasher.update(render_to.as_bytes());
    hasher.update(b"\0");
    // Hash every file under the skill source dir. The walk returns the
    // entries sorted by relative path so the digest is reproducible
    // across processes and filesystems with non-deterministic readdir
    // ordering.
    for file in source_files {
        let rel = file.rel.to_string_lossy();
        hasher.update(rel.as_bytes());
        hasher.update(b"\0");
        // Include the mode so a chmod-only change still invalidates the
        // cache. On non-unix platforms the field is absent; fold a
        // constant in so the hash space stays identical across builds
        // of the same platform.
        #[cfg(unix)]
        {
            hasher.update(file.mode.to_le_bytes());
        }
        #[cfg(not(unix))]
        {
            hasher.update([0u8; 4]);
        }
        hasher.update(b"\0");
        let bytes = fs::read(&file.abs).with_context(|| {
            format!(
                "hash-read {} (skill source dir {})",
                file.abs.display(),
                canonical_source_dir.display()
            )
        })?;
        hasher.update(&bytes);
        hasher.update(b"\0");
    }
    // Whole-file manifest bytes — keeps the hash sensitive to any change
    // in a file the helpers might consume, at the cost of a coarse cache
    // invalidation when an unrelated manifest line shifts.
    hasher.update(&manifests.skills);
    hasher.update(b"\0");
    hasher.update(&manifests.plugins);
    hasher.update(b"\0");
    hasher.update(&manifests.product_capabilities);
    hasher.update(b"\0");
    hasher.update(&manifests.runtime_roots);
    hasher.update(b"\0");
    hasher.update(&manifests.cli_tools);
    hasher.update(b"\0");
    hasher.update(&manifests.agents);
    let digest = hasher.finalize();
    let mut out = String::with_capacity(7 + digest.len() * 2);
    out.push_str("sha256:");
    for byte in digest.iter() {
        use std::fmt::Write;
        let _ = write!(&mut out, "{byte:02x}");
    }
    Ok(out)
}

/// Walk a skill source directory recursively and return every file with
/// its path relative to the skill root. Directories are descended in
/// sorted order so the resulting entry list (and the hash derived from
/// it) is deterministic across filesystems with arbitrary readdir order.
///
/// Symlinks are followed via the same `canonicalize_under` guard used
/// for the SKILL template read: a hostile sibling symlink that points
/// outside the canonical source root is rejected before any I/O.
fn walk_skill_source(skill_dir: &Path, canonical_source_root: &Path) -> Result<Vec<SourceFile>> {
    let mut out = Vec::new();
    walk_dir(skill_dir, skill_dir, canonical_source_root, &mut out)?;
    out.sort_by(|a, b| a.rel.cmp(&b.rel));
    Ok(out)
}

fn walk_dir(
    skill_root: &Path,
    dir: &Path,
    canonical_source_root: &Path,
    out: &mut Vec<SourceFile>,
) -> Result<()> {
    let entries = fs::read_dir(dir).with_context(|| format!("read_dir {}", dir.display()))?;
    let mut paths: Vec<PathBuf> = entries
        .map(|e| e.map(|entry| entry.path()))
        .collect::<std::io::Result<_>>()
        .with_context(|| format!("read_dir entries under {}", dir.display()))?;
    paths.sort();
    for path in paths {
        // Each entry — file or directory — gets the canonical-under
        // guard so a hostile symlink under, say, `bin/` can't escape
        // the source root.
        let canonical = canonicalize_under(canonical_source_root, &path)?;
        let meta = fs::metadata(&canonical)
            .with_context(|| format!("metadata {}", canonical.display()))?;
        if meta.is_dir() {
            walk_dir(skill_root, &canonical, canonical_source_root, out)?;
            continue;
        }
        if !meta.is_file() {
            continue;
        }
        let rel = canonical.strip_prefix(skill_root).map_err(|err| {
            anyhow!(
                "source file {} is not under skill root {}: {err}",
                canonical.display(),
                skill_root.display(),
            )
        })?;
        #[cfg(unix)]
        let mode = {
            use std::os::unix::fs::PermissionsExt;
            meta.permissions().mode() & 0o777
        };
        let source = SourceFile {
            rel: rel.to_path_buf(),
            abs: canonical.clone(),
            #[cfg(unix)]
            mode,
        };
        out.push(source);
    }
    Ok(())
}

/// Strip a trailing `.tera` extension from `rel` so a sibling like
/// `prompts/intro.md.tera` lands as `prompts/intro.md` in the rendered
/// tree. Files without a `.tera` extension pass through unchanged.
fn strip_tera_suffix(rel: &Path) -> PathBuf {
    if rel.extension().and_then(|e| e.to_str()) == Some(TERA_EXT) {
        rel.with_extension("")
    } else {
        rel.to_path_buf()
    }
}

/// Resolve a candidate read path and assert the canonical result is
/// still under `canonical_base`. Defeats symlink-based sandbox escape
/// where a hostile `core/skills/<x>/SKILL.md.tera` symlinks to a file
/// outside the source root (e.g. `/etc/passwd`). The path must exist;
/// the caller is reading it.
pub(crate) fn canonicalize_under(canonical_base: &Path, candidate: &Path) -> Result<PathBuf> {
    let resolved = candidate
        .canonicalize()
        .with_context(|| format!("canonicalize {}", candidate.display()))?;
    if !resolved.starts_with(canonical_base) {
        return Err(anyhow!(
            "path {candidate} resolves outside the source root \
             ({resolved} not under {canonical_base}) — likely a symlink escape",
            candidate = candidate.display(),
            resolved = resolved.display(),
            canonical_base = canonical_base.display(),
        ));
    }
    Ok(resolved)
}

/// Resolve a candidate write path. The file itself may not exist yet,
/// so we canonicalize the parent (which the caller created via
/// `create_dir_all`) and assert it sits under `canonical_base`. A
/// hostile parent symlink — e.g. `build/<product>/foo` is a symlink to
/// `/etc/` — gets rejected before we open the file for write.
pub(crate) fn guard_write_under(canonical_base: &Path, candidate: &Path) -> Result<PathBuf> {
    let parent = candidate
        .parent()
        .ok_or_else(|| anyhow!("render output path {} has no parent", candidate.display()))?;
    let canonical_parent = parent
        .canonicalize()
        .with_context(|| format!("canonicalize parent of {}", candidate.display()))?;
    if !canonical_parent.starts_with(canonical_base) {
        return Err(anyhow!(
            "render output {} resolves outside the build root \
             ({} not under {}) — likely a symlink escape",
            candidate.display(),
            canonical_parent.display(),
            canonical_base.display(),
        ));
    }
    let file_name = candidate.file_name().ok_or_else(|| {
        anyhow!(
            "render output path {} has no file name",
            candidate.display()
        )
    })?;
    Ok(canonical_parent.join(file_name))
}

fn reject_leaf_symlink(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(anyhow!(
            "render output {} is a symlink; refusing to follow a leaf symlink",
            path.display()
        )),
        Ok(_) => Ok(()),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| format!("stat render output {}", path.display())),
    }
}

/// Join `relative` onto `base` after rejecting any `..` segments. Used
/// for every render-time path so we cannot escape `<source-root>/` via
/// a hostile `skill.source`, `render_to`, or `--source-root` value.
pub(crate) fn sandboxed_join(base: &Path, relative: &str) -> Result<PathBuf> {
    let rel = PathBuf::from(relative);
    for component in rel.components() {
        match component {
            Component::Normal(_) | Component::CurDir => {}
            Component::ParentDir => {
                return Err(anyhow!(
                    "path {relative:?} contains a `..` segment; render must stay under {base}",
                    base = base.display(),
                ));
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(anyhow!(
                    "path {relative:?} is absolute; render must stay under {base}",
                    base = base.display(),
                ));
            }
        }
    }
    Ok(base.join(rel))
}

/// Map of the rendered output bytes keyed by path-under-output-root.
/// Test helper for cache-hit-vs-cache-miss byte equality assertions.
#[cfg(test)]
pub(crate) fn snapshot_outputs(output_root: &Path) -> std::collections::BTreeMap<String, Vec<u8>> {
    let mut out = std::collections::BTreeMap::new();
    walk(output_root, output_root, &mut out);
    out
}

#[cfg(test)]
fn walk(base: &Path, dir: &Path, out: &mut std::collections::BTreeMap<String, Vec<u8>>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|e| e.path());
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            walk(base, &path, out);
            continue;
        }
        if path.file_name().and_then(|n| n.to_str()) == Some(CACHE_FILE) {
            continue;
        }
        let bytes = fs::read(&path).unwrap();
        let rel = path
            .strip_prefix(base)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        out.insert(rel, bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::cache::RenderCache;
    use tempfile::TempDir;

    fn write(path: &Path, body: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, body).unwrap();
    }

    const SKILLS_FIXTURE: &str = r#"
schema_version: 1
skills:
  - id: market.favorites
    domain: market
    source: core/skills/market/favorites
    products:
      codex:
        name: /market-favorites
        render_to: skills/market/favorites/SKILL.md
      claude:
        name: market:favorites
        render_to: plugins/market/skills/favorites/SKILL.md
    required_clis:
      agent-out: ">=0.5.0"
      market-cli: ">=0.4.0"
"#;

    /// Build a working source root with one skill that exercises every
    /// helper (script / skill_ref / state_out / cli_ref).
    fn fixture_source_root(tmp: &TempDir) -> SourceRoot {
        let root = tmp.path();
        write(&root.join("manifests/skills.yaml"), SKILLS_FIXTURE);
        write(
            &root.join("manifests/plugins.yaml"),
            "schema_version: 1\nplugins: []\n",
        );
        write(
            &root.join("manifests/product-capabilities.yaml"),
            PRODUCT_CAPS,
        );
        write(&root.join("manifests/runtime-roots.yaml"), RUNTIME_ROOTS);
        write(&root.join("manifests/cli-tools.yaml"), CLI_TOOLS);

        // Skill template exercises every helper. Trailing newline is
        // intentional so the rendered file ends with one.
        write(
            &root.join("core/skills/market/favorites/SKILL.md.tera"),
            r#"# {{ skill_ref(id="market.favorites") }}

state: {{ state_out(domain="market", topic="favorites") }}
script: {{ script(path="core/scripts/market.sh") }}
required: {{ cli_ref(name="agent-out") }} via {{ cli_ref(name="market-cli") }}
"#,
        );

        SourceRoot::from_arg_or_cwd(Some(root)).unwrap()
    }

    const PRODUCT_CAPS: &str = r#"
schema_version: 1
products:
  codex:
    nested_skill_support: true
    plugin_manifest:
      path_pattern: "ignored"
      loaded_at_runtime: false
      schema_ref: "ignored"
    hooks_model:
      config_surface: "ignored"
      payload_shape: "ignored"
      supports_inline_python: false
    config_activation:
      - "$CODEX_HOME/AGENTS.md"
    runtime_state:
      state_home_env: "STATE"
      default_path: "/tmp/state"
  claude:
    nested_skill_support: true
    plugin_manifest:
      path_pattern: "ignored"
      loaded_at_runtime: true
      schema_ref: "ignored"
    hooks_model:
      config_surface: "ignored"
      payload_shape: "ignored"
      supports_inline_python: true
    config_activation:
      - "$HOME/.claude/settings.json"
    runtime_state:
      state_home_env: "STATE"
      default_path: "/tmp/state"
  hermes:
    nested_skill_support: true
    plugin_manifest:
      path_pattern: "ignored"
      loaded_at_runtime: false
      schema_ref: "ignored"
    hooks_model:
      config_surface: "n/a"
      payload_shape: "n/a"
      supports_inline_python: false
    config_activation:
      - "$HOME/.hermes/skills"
    runtime_state:
      state_home_env: "STATE"
      default_path: "/tmp/state"
"#;

    const RUNTIME_ROOTS: &str = r#"
schema_version: 1
products:
  codex:
    live_home: "$CODEX_HOME"
    docs_home: "$CODEX_HOME"
    state_home: "/tmp/state"
    plugin_root: "$CODEX_HOME/plugins"
    hook_config_strategy: managed-block
    min_version: "<TBD: pin during Phase 1>"
    recommended_version: "<TBD: pin during Phase 1>"
    min_version_effective_from: "<TBD: pin during Phase 1>"
    version_probe: "codex --version"
  claude:
    live_home: "$HOME/.claude"
    docs_home: "$HOME/.claude"
    state_home: "/tmp/state"
    plugin_root_env: "CLAUDE_PLUGIN_ROOT"
    hook_config_strategy: settings-json
    min_version: "<TBD: pin during Phase 1>"
    recommended_version: "<TBD: pin during Phase 1>"
    min_version_effective_from: "<TBD: pin during Phase 1>"
    version_probe: "claude --version"
  hermes:
    live_home: "$HOME/.hermes"
    docs_home: "$HOME/.hermes"
    state_home: "/tmp/state"
    min_version: "1.0.0"
    recommended_version: "1.0.0"
    min_version_effective_from: "<TBD>"
    version_probe: "hermes --version"
"#;

    const CLI_TOOLS: &str = r#"
schema_version: 1
profiles:
  core: [ripgrep]
  recommended: [ripgrep]
  full: [ripgrep]
formulas:
  ripgrep:
    brew: ripgrep
    command: rg
    linux_only_alternative: null
    categories: [search]
"#;

    fn load_set(root: &SourceRoot) -> Arc<ManifestSet> {
        Arc::new(crate::render::manifest::load_all(root).unwrap())
    }

    /// Drop an optional `manifests/agents.yaml` and one canonical agent
    /// source onto an existing fixture root. The single `AGENT.md.tera`
    /// branches on the `product` template variable so it can emit Codex
    /// TOML or Claude Markdown from one source.
    fn add_agent_fixture(root: &SourceRoot) {
        write(
            &root.path().join("manifests/agents.yaml"),
            r#"
schema_version: 1
agents:
  - id: reviewer-quick
    source: core/agents/reviewer-quick
    products:
      codex:
        render_to: agents/reviewer-quick.toml
      claude:
        render_to: agents/reviewer-quick.md
"#,
        );
        write(
            &root.path().join("core/agents/reviewer-quick/AGENT.md.tera"),
            "{% if product == \"codex\" %}name = \"reviewer-quick\"\n\
             {% else %}---\nname: reviewer-quick\n---\n{% endif %}",
        );
    }

    #[test]
    fn write_product_renders_codex_agent_into_build_tree() {
        let tmp = TempDir::new().unwrap();
        let root = fixture_source_root(&tmp);
        add_agent_fixture(&root);
        let set = load_set(&root);

        let report = write_product(&root, set, "codex").unwrap();

        let out = report.output_root.join("agents/reviewer-quick.toml");
        assert!(out.exists(), "expected agent render at {}", out.display());
        let body = fs::read_to_string(&out).unwrap();
        assert!(body.contains("name = \"reviewer-quick\""), "{body}");
        assert!(
            report.rendered.iter().any(|id| id == "reviewer-quick"),
            "agent id absent from rendered report: {:?}",
            report.rendered
        );
    }

    #[test]
    fn write_product_renders_claude_agent_with_product_branch() {
        let tmp = TempDir::new().unwrap();
        let root = fixture_source_root(&tmp);
        add_agent_fixture(&root);
        let set = load_set(&root);

        let report = write_product(&root, set, "claude").unwrap();

        // The one canonical AGENT.md.tera branched on `product` to the
        // Claude Markdown arm and landed at the claude `render_to`.
        let out = report.output_root.join("agents/reviewer-quick.md");
        let body = fs::read_to_string(&out).unwrap();
        assert!(body.contains("---\nname: reviewer-quick"), "{body}");
        assert!(!body.contains("name = \"reviewer-quick\""), "{body}");
        assert!(report.rendered.iter().any(|id| id == "reviewer-quick"));
    }

    #[test]
    fn agent_render_is_cached_on_second_run() {
        let tmp = TempDir::new().unwrap();
        let root = fixture_source_root(&tmp);
        add_agent_fixture(&root);
        let set = load_set(&root);

        let first = write_product(&root, set.clone(), "codex").unwrap();
        assert!(first.rendered.iter().any(|id| id == "reviewer-quick"));

        // Second run with unchanged source: the agents cache (its own
        // `.render-cache-agents.json`) reports a hit, not a re-render.
        let second = write_product(&root, set, "codex").unwrap();
        assert!(
            second.cached.iter().any(|id| id == "reviewer-quick"),
            "expected agent cache hit, got rendered={:?} cached={:?}",
            second.rendered,
            second.cached
        );
    }

    #[test]
    fn write_product_renders_codex_skill_into_build_tree() {
        let tmp = TempDir::new().unwrap();
        let root = fixture_source_root(&tmp);
        let set = load_set(&root);

        let report = write_product(&root, set, "codex").unwrap();

        assert_eq!(report.rendered, vec!["market.favorites".to_string()]);
        assert!(report.cached.is_empty());
        assert!(report.skipped.is_empty());
        let out = report.output_root.join("skills/market/favorites/SKILL.md");
        let body = fs::read_to_string(&out).unwrap();
        assert!(body.contains("# /market-favorites"), "{body}");
        assert!(
            body.contains("state: agent-out path-for --domain market --topic favorites"),
            "{body}",
        );
        assert!(
            body.contains("script: ") && body.contains("/core/scripts/market.sh"),
            "{body}",
        );
        assert!(
            body.contains("required: agent-out (>=0.5.0) via market-cli (>=0.4.0)"),
            "{body}",
        );

        // Cache file exists after the run.
        let cache = RenderCache::load_or_empty(&report.output_root.join(CACHE_FILE));
        assert!(cache.skills.contains_key("market.favorites"));
    }

    #[test]
    fn cache_hit_skips_render_and_keeps_existing_output_bytes() {
        let tmp = TempDir::new().unwrap();
        let root = fixture_source_root(&tmp);
        let set = load_set(&root);

        // First run populates output + cache.
        let first = write_product(&root, set.clone(), "codex").unwrap();
        let snapshot_first = snapshot_outputs(&first.output_root);
        assert_eq!(first.rendered, vec!["market.favorites".to_string()]);

        // Second run with no input change must hit the cache and leave
        // the output bytes identical.
        let second = write_product(&root, set.clone(), "codex").unwrap();
        assert!(second.rendered.is_empty(), "{:?}", second.rendered);
        assert_eq!(second.cached, vec!["market.favorites".to_string()]);
        let snapshot_second = snapshot_outputs(&second.output_root);
        assert_eq!(snapshot_first, snapshot_second);
    }

    #[test]
    fn cache_miss_after_cache_file_deletion_reproduces_identical_bytes() {
        let tmp = TempDir::new().unwrap();
        let root = fixture_source_root(&tmp);
        let set = load_set(&root);

        let first = write_product(&root, set.clone(), "codex").unwrap();
        let snapshot_first = snapshot_outputs(&first.output_root);

        // Delete the cache file → forces a re-render. The rendered
        // bytes must match the first run byte-for-byte.
        fs::remove_file(first.output_root.join(CACHE_FILE)).unwrap();
        let second = write_product(&root, set, "codex").unwrap();
        assert_eq!(second.rendered, vec!["market.favorites".to_string()]);
        let snapshot_second = snapshot_outputs(&second.output_root);
        assert_eq!(snapshot_first, snapshot_second);
    }

    #[test]
    fn template_change_invalidates_cache_and_re_renders() {
        let tmp = TempDir::new().unwrap();
        let root = fixture_source_root(&tmp);
        let set = load_set(&root);
        write_product(&root, set.clone(), "codex").unwrap();

        // Touch the template body.
        let tpl_path = root
            .path()
            .join("core/skills/market/favorites/SKILL.md.tera");
        let mut body = fs::read_to_string(&tpl_path).unwrap();
        body.push_str("\nextra line\n");
        fs::write(&tpl_path, body).unwrap();

        // Reload manifests (skill source bytes changed; manifest bytes
        // unchanged → cache key still differs because template body is
        // part of the hash input).
        let set = load_set(&root);
        let second = write_product(&root, set, "codex").unwrap();
        assert_eq!(second.rendered, vec!["market.favorites".to_string()]);
        let rendered =
            fs::read_to_string(second.output_root.join("skills/market/favorites/SKILL.md"))
                .unwrap();
        assert!(rendered.ends_with("extra line\n"), "{rendered}");
    }

    #[test]
    fn skill_without_product_entry_is_skipped() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        write(
            &root.join("manifests/skills.yaml"),
            r#"
schema_version: 1
skills:
  - id: codex.only
    domain: codex
    source: core/skills/codex/only
    products:
      codex:
        render_to: skills/codex-only/SKILL.md
    required_clis: {}
"#,
        );
        write(
            &root.join("manifests/plugins.yaml"),
            "schema_version: 1\nplugins: []\n",
        );
        write(
            &root.join("manifests/product-capabilities.yaml"),
            PRODUCT_CAPS,
        );
        write(&root.join("manifests/runtime-roots.yaml"), RUNTIME_ROOTS);
        write(&root.join("manifests/cli-tools.yaml"), CLI_TOOLS);
        write(
            &root.join("core/skills/codex/only/SKILL.md.tera"),
            "# codex-only\n",
        );
        let source_root = SourceRoot::from_arg_or_cwd(Some(root)).unwrap();
        let set = load_set(&source_root);
        let report = write_product(&source_root, set, "claude").unwrap();
        assert!(report.rendered.is_empty());
        assert_eq!(report.skipped, vec!["codex.only".to_string()]);
    }

    #[test]
    fn render_rejects_unknown_product() {
        let tmp = TempDir::new().unwrap();
        let root = fixture_source_root(&tmp);
        let set = load_set(&root);
        let err = write_product(&root, set, "unknown").unwrap_err();
        assert!(format!("{err:#}").contains("unknown --product"));
    }

    #[test]
    fn sandboxed_join_rejects_parent_segments_and_absolute_paths() {
        let base = Path::new("/tmp/source-root");
        sandboxed_join(base, "core/scripts/foo.sh").unwrap();
        sandboxed_join(base, "./core/scripts/foo.sh").unwrap();
        let err = sandboxed_join(base, "../etc/passwd").unwrap_err();
        assert!(format!("{err}").contains(".."));
        let err = sandboxed_join(base, "/etc/passwd").unwrap_err();
        assert!(format!("{err}").contains("absolute"));
    }

    /// A hostile `skill.source` directory could contain a symlinked
    /// SKILL.md.tera that points outside the source root. The lexical
    /// `sandboxed_join` accepts the path (no `..`, not absolute), so
    /// `canonicalize_under` catches the escape just before the read.
    /// Without that guard the renderer would expose `/etc/passwd` (or
    /// any readable file) into rendered output.
    #[cfg(unix)]
    #[test]
    fn symlinked_skill_template_outside_source_root_is_rejected() {
        let tmp = TempDir::new().unwrap();
        let root = fixture_source_root(&tmp);
        let set = load_set(&root);
        // Replace the legitimate template with a symlink pointing at
        // a file outside the source root.
        let outside = TempDir::new().unwrap();
        let target = outside.path().join("hostile.tera");
        fs::write(&target, "# captured from outside\n").unwrap();
        let template_path = root
            .path()
            .join("core/skills/market/favorites/SKILL.md.tera");
        fs::remove_file(&template_path).unwrap();
        std::os::unix::fs::symlink(&target, &template_path).unwrap();

        let err = write_product(&root, set, "codex").unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("symlink") || msg.contains("outside the source root"),
            "{msg}",
        );
    }

    /// Same threat surface as the read-path test, but for the write
    /// path: a hostile `build/<product>/` symlink could redirect render
    /// output into the user's home directory. `guard_write_under`
    /// canonicalizes the parent before fs::write opens it.
    #[cfg(unix)]
    #[test]
    fn symlinked_build_dir_outside_root_is_rejected_for_writes() {
        let tmp = TempDir::new().unwrap();
        let root = fixture_source_root(&tmp);
        let set = load_set(&root);
        // Pre-create build/<product>/, then swap one nested dir for a
        // symlink pointing outside the canonical build root.
        let build = root.path().join("build/codex");
        fs::create_dir_all(&build).unwrap();
        let dest = build.join("skills/market/favorites");
        fs::create_dir_all(dest.parent().unwrap()).unwrap();
        let outside = TempDir::new().unwrap();
        let exfil = outside.path().join("favorites");
        fs::create_dir(&exfil).unwrap();
        std::os::unix::fs::symlink(&exfil, &dest).unwrap();

        let err = write_product(&root, set, "codex").unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("outside the build root") || msg.contains("symlink"),
            "{msg}",
        );
    }

    #[test]
    fn render_rejects_render_to_with_build_prefix() {
        // A `render_to` value that starts with `build/<product>/` would
        // double the prefix because the binary already prepends
        // `build/<product>/` to the output root. The validator should
        // reject this shape with a clear pointer at the canonical form.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        write(
            &root.join("manifests/skills.yaml"),
            r#"
schema_version: 1
skills:
  - id: market.favorites
    domain: market
    source: core/skills/market/favorites
    products:
      codex:
        name: /market-favorites
        render_to: build/codex/plugins/market/skills/favorites/SKILL.md
    required_clis: {}
"#,
        );
        write(
            &root.join("manifests/plugins.yaml"),
            "schema_version: 1\nplugins: []\n",
        );
        write(
            &root.join("manifests/product-capabilities.yaml"),
            PRODUCT_CAPS,
        );
        write(&root.join("manifests/runtime-roots.yaml"), RUNTIME_ROOTS);
        write(&root.join("manifests/cli-tools.yaml"), CLI_TOOLS);
        write(
            &root.join("core/skills/market/favorites/SKILL.md.tera"),
            "# market\n",
        );
        let source_root = SourceRoot::from_arg_or_cwd(Some(root)).unwrap();
        let set = load_set(&source_root);
        let err = write_product(&source_root, set, "codex").unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("starts with `build/`"), "{msg}");
        assert!(msg.contains("market.favorites"), "{msg}");
        assert!(
            msg.contains("plugins/<plugin>/skills/<skill>/SKILL.md"),
            "{msg}"
        );
    }

    #[test]
    fn write_product_copies_sibling_files_with_executable_bit() {
        // A skill that ships `bin/`, `scripts/`, `references/` siblings
        // (the topic-radar shape) should land all of them under the
        // rendered output directory, with shell scripts keeping their
        // executable bit. Without this, the rendered SKILL points at a
        // script path that doesn't exist in the build tree.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        write(
            &root.join("manifests/skills.yaml"),
            r#"
schema_version: 1
skills:
  - id: tools.topic-radar
    domain: tools
    source: core/skills/tools/topic-radar
    products:
      codex:
        name: topic-radar
        render_to: plugins/tools/skills/topic-radar/SKILL.md
    required_clis: {}
"#,
        );
        write(
            &root.join("manifests/plugins.yaml"),
            "schema_version: 1\nplugins: []\n",
        );
        write(
            &root.join("manifests/product-capabilities.yaml"),
            PRODUCT_CAPS,
        );
        write(&root.join("manifests/runtime-roots.yaml"), RUNTIME_ROOTS);
        write(&root.join("manifests/cli-tools.yaml"), CLI_TOOLS);

        let skill_src = root.join("core/skills/tools/topic-radar");
        write(&skill_src.join("SKILL.md.tera"), "# topic-radar\n");
        write(&skill_src.join("bin/topic_radar.py"), "print('hello')\n");
        write(
            &skill_src.join("scripts/topic-radar.sh"),
            "#!/bin/sh\necho hi\n",
        );
        write(
            &skill_src.join("references/source-strategy.md"),
            "# strategy\n",
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(
                skill_src.join("scripts/topic-radar.sh"),
                fs::Permissions::from_mode(0o755),
            )
            .unwrap();
        }

        let source_root = SourceRoot::from_arg_or_cwd(Some(root)).unwrap();
        let set = load_set(&source_root);
        let report = write_product(&source_root, set, "codex").unwrap();
        assert_eq!(report.rendered, vec!["tools.topic-radar".to_string()]);

        let out_dir = report.output_root.join("plugins/tools/skills/topic-radar");
        assert!(
            out_dir.join("SKILL.md").exists(),
            "rendered SKILL.md missing"
        );
        assert!(
            out_dir.join("bin/topic_radar.py").exists(),
            "bin/topic_radar.py not copied"
        );
        assert_eq!(
            fs::read_to_string(out_dir.join("bin/topic_radar.py")).unwrap(),
            "print('hello')\n",
        );
        assert_eq!(
            fs::read_to_string(out_dir.join("scripts/topic-radar.sh")).unwrap(),
            "#!/bin/sh\necho hi\n",
        );
        assert_eq!(
            fs::read_to_string(out_dir.join("references/source-strategy.md")).unwrap(),
            "# strategy\n",
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let copied_mode = fs::metadata(out_dir.join("scripts/topic-radar.sh"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(
                copied_mode, 0o755,
                "executable bit not preserved on rendered shell script",
            );
        }
    }

    #[test]
    fn sibling_tera_file_is_rendered_through_helpers_and_drops_suffix() {
        // A `.tera` sibling (e.g. `prompts/intro.md.tera`) should be
        // rendered through the same helper context as the SKILL body
        // and land in the output as `prompts/intro.md` (no `.tera`
        // suffix) so downstream tooling consumes a plain file.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        write(
            &root.join("manifests/skills.yaml"),
            r#"
schema_version: 1
skills:
  - id: market.favorites
    domain: market
    source: core/skills/market/favorites
    products:
      codex:
        name: favorites
        render_to: skills/market/favorites/SKILL.md
    required_clis:
      agent-out: ">=0.5.0"
"#,
        );
        write(
            &root.join("manifests/plugins.yaml"),
            "schema_version: 1\nplugins: []\n",
        );
        write(
            &root.join("manifests/product-capabilities.yaml"),
            PRODUCT_CAPS,
        );
        write(&root.join("manifests/runtime-roots.yaml"), RUNTIME_ROOTS);
        write(&root.join("manifests/cli-tools.yaml"), CLI_TOOLS);

        let skill_src = root.join("core/skills/market/favorites");
        write(&skill_src.join("SKILL.md.tera"), "# favorites\n");
        write(
            &skill_src.join("prompts/intro.md.tera"),
            r#"intro for {{ skill_ref(id="market.favorites") }}"#,
        );

        let source_root = SourceRoot::from_arg_or_cwd(Some(root)).unwrap();
        let set = load_set(&source_root);
        write_product(&source_root, set, "codex").unwrap();

        let out = root.join("build/codex/skills/market/favorites/prompts/intro.md");
        assert!(
            out.exists(),
            "rendered sibling tera should land without .tera suffix"
        );
        let body = fs::read_to_string(&out).unwrap();
        assert_eq!(body, "intro for favorites");
    }

    #[test]
    fn stale_sibling_files_are_removed_on_re_render() {
        // If a sibling file is removed from source between two renders,
        // the prior rendered copy must not survive in the output. The
        // cache-miss path clears the output dir, so deleting a source
        // file is enough to make it disappear from `build/`.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        write(
            &root.join("manifests/skills.yaml"),
            r#"
schema_version: 1
skills:
  - id: tools.foo
    domain: tools
    source: core/skills/tools/foo
    products:
      codex:
        name: foo
        render_to: plugins/tools/skills/foo/SKILL.md
    required_clis: {}
"#,
        );
        write(
            &root.join("manifests/plugins.yaml"),
            "schema_version: 1\nplugins: []\n",
        );
        write(
            &root.join("manifests/product-capabilities.yaml"),
            PRODUCT_CAPS,
        );
        write(&root.join("manifests/runtime-roots.yaml"), RUNTIME_ROOTS);
        write(&root.join("manifests/cli-tools.yaml"), CLI_TOOLS);

        let skill_src = root.join("core/skills/tools/foo");
        write(&skill_src.join("SKILL.md.tera"), "# foo\n");
        write(&skill_src.join("old-helper.sh"), "echo old\n");

        let source_root = SourceRoot::from_arg_or_cwd(Some(root)).unwrap();
        let set = load_set(&source_root);
        write_product(&source_root, set.clone(), "codex").unwrap();
        let out_dir = root.join("build/codex/plugins/tools/skills/foo");
        assert!(out_dir.join("old-helper.sh").exists());

        // Delete the sibling from source; re-load + re-render.
        fs::remove_file(skill_src.join("old-helper.sh")).unwrap();
        let set = load_set(&source_root);
        write_product(&source_root, set, "codex").unwrap();
        assert!(
            !out_dir.join("old-helper.sh").exists(),
            "stale rendered sibling must be cleaned on re-render",
        );
        assert!(
            out_dir.join("SKILL.md").exists(),
            "SKILL.md should still render after sibling removal",
        );
    }

    #[test]
    fn sibling_byte_change_invalidates_cache() {
        // A pure sibling-file edit (no SKILL.md.tera change) must still
        // invalidate the cache so the rendered output picks up the new
        // bytes — otherwise users editing a helper script see stale
        // output on the next render.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        write(
            &root.join("manifests/skills.yaml"),
            r#"
schema_version: 1
skills:
  - id: tools.foo
    domain: tools
    source: core/skills/tools/foo
    products:
      codex:
        name: foo
        render_to: plugins/tools/skills/foo/SKILL.md
    required_clis: {}
"#,
        );
        write(
            &root.join("manifests/plugins.yaml"),
            "schema_version: 1\nplugins: []\n",
        );
        write(
            &root.join("manifests/product-capabilities.yaml"),
            PRODUCT_CAPS,
        );
        write(&root.join("manifests/runtime-roots.yaml"), RUNTIME_ROOTS);
        write(&root.join("manifests/cli-tools.yaml"), CLI_TOOLS);

        let skill_src = root.join("core/skills/tools/foo");
        write(&skill_src.join("SKILL.md.tera"), "# foo\n");
        write(&skill_src.join("helper.sh"), "echo v1\n");

        let source_root = SourceRoot::from_arg_or_cwd(Some(root)).unwrap();
        let set = load_set(&source_root);
        write_product(&source_root, set, "codex").unwrap();

        write(&skill_src.join("helper.sh"), "echo v2\n");
        let set = load_set(&source_root);
        let second = write_product(&source_root, set, "codex").unwrap();
        assert_eq!(
            second.rendered,
            vec!["tools.foo".to_string()],
            "sibling byte change should trigger a re-render (cache miss)",
        );
        let body = fs::read_to_string(root.join("build/codex/plugins/tools/skills/foo/helper.sh"))
            .unwrap();
        assert_eq!(body, "echo v2\n");
    }

    #[test]
    fn write_product_runs_against_empty_skills_manifest() {
        // Real agent-runtime-kit ships skills.yaml empty in Plan 01.
        // Render must not blow up — it should produce an empty cache.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        write(
            &root.join("manifests/skills.yaml"),
            "schema_version: 1\nskills: []\n",
        );
        write(
            &root.join("manifests/plugins.yaml"),
            "schema_version: 1\nplugins: []\n",
        );
        write(
            &root.join("manifests/product-capabilities.yaml"),
            PRODUCT_CAPS,
        );
        write(&root.join("manifests/runtime-roots.yaml"), RUNTIME_ROOTS);
        write(&root.join("manifests/cli-tools.yaml"), CLI_TOOLS);
        let source_root = SourceRoot::from_arg_or_cwd(Some(root)).unwrap();
        let set = load_set(&source_root);
        let report = write_product(&source_root, set, "codex").unwrap();
        assert!(report.rendered.is_empty());
        assert!(report.cached.is_empty());
        assert!(report.skipped.is_empty());
        assert!(report.output_root.exists());
    }
}
