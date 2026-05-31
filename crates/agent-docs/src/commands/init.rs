//! `init` — emit an annotated, human-editable project-local override stub.
//!
//! The stub lists the inherited defaults as comments, ships commented
//! ready-to-uncomment examples plus inline schema/`when` syntax, and declares
//! NO required entries by default. It never writes a full copy of the inherited
//! defaults (that would fork and drift). When the project looks like a known
//! ecosystem (`Cargo.toml` / `package.json`), it pre-fills matching `when`
//! examples in comments.

use std::fs;
use std::io;
use std::path::Path;

use crate::config::{config_path_for_root, load_catalog_from_roots};
use crate::env::ResolvedRoots;
use crate::model::{InitMode, InitReport, ScopeCatalog};

#[derive(Debug)]
pub enum InitError {
    AlreadyExists(std::path::PathBuf),
    Io(io::Error),
}

impl std::fmt::Display for InitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AlreadyExists(path) => write!(
                f,
                "{} already exists; pass --force to overwrite or --print to preview",
                path.display()
            ),
            Self::Io(err) => write!(f, "{err}"),
        }
    }
}

pub fn run_init(
    roots: &ResolvedRoots,
    mode: InitMode,
    force: bool,
) -> Result<InitReport, InitError> {
    let stub = render_stub(roots);
    let target_path = config_path_for_root(&roots.project_path);

    let wrote = match mode {
        InitMode::Print | InitMode::DryRun => false,
        InitMode::Write => {
            if target_path.exists() && !force {
                return Err(InitError::AlreadyExists(target_path));
            }
            if let Some(parent) = target_path.parent() {
                fs::create_dir_all(parent).map_err(InitError::Io)?;
            }
            fs::write(&target_path, &stub).map_err(InitError::Io)?;
            true
        }
    };

    Ok(InitReport {
        mode,
        target_path,
        wrote,
        stub,
    })
}

fn render_stub(roots: &ResolvedRoots) -> String {
    let mut out = String::new();
    out.push_str(STUB_HEADER);

    out.push_str("#\n# Inherited defaults (from the docs-home catalog");
    out.push_str(&format!(" at {}):\n", roots.docs_home.display()));
    match load_catalog_from_roots(roots) {
        Ok(catalog) => match catalog.home.as_ref() {
            Some(home) if !home.documents.is_empty() || !home.validations.is_empty() => {
                render_inherited(&mut out, home);
            }
            _ => out.push_str("#   (the docs-home declares no defaults)\n"),
        },
        Err(_) => out.push_str("#   (could not read the docs-home catalog)\n"),
    }

    render_examples(&mut out, &roots.project_path);

    out.push_str("\n# (no overrides declared — this project inherits the defaults above)\n");
    out
}

fn render_inherited(out: &mut String, home: &ScopeCatalog) {
    for entry in &home.documents {
        out.push_str(&format!(
            "#   document: context={} scope={} path={} required={} when=\"{}\"\n",
            entry.context,
            entry.scope,
            entry.path.display(),
            entry.required,
            entry.when_raw,
        ));
    }
    for validation in &home.validations {
        out.push_str(&format!(
            "#   validation: context={} commands={:?}\n",
            validation.context, validation.commands,
        ));
    }
}

fn render_examples(out: &mut String, project_path: &Path) {
    out.push_str("#\n# Ready-to-uncomment examples for THIS repo:\n");

    let mut matched = false;
    if project_path.join("Cargo.toml").exists() {
        matched = true;
        out.push_str(RUST_EXAMPLE);
    }
    if project_path.join("package.json").exists() {
        matched = true;
        out.push_str(NODE_EXAMPLE);
    }
    if !matched {
        out.push_str(GENERIC_EXAMPLE);
    }
}

const STUB_HEADER: &str = "\
# AGENT_DOCS.toml — project-local override for agent-docs.
#
# This file is OPTIONAL. The docs-home catalog already declares the defaults
# your repo inherits (listed below). Add an entry here only to:
#   - require a project-specific document,
#   - change `when` / `marker` for an inherited document, or
#   - declare this repo's validation contract.
#
# A project that needs no override can leave this file with no entries; a fresh
# `init` adds zero new requirements.
#
# Schema
# ------
# [[document]]
# context  = \"project-dev\"           # the intent that needs this document
# scope    = \"project\"               # home | project | global
# path     = \"DEVELOPMENT.md\"        # relative to the scope root
# required = true                     # default: false
# when     = \"path-exists:Cargo.toml\" # default: always (see grammar below)
# marker   = \"## Validation\"         # optional: content must contain this string
# last-reviewed-within-days = 180     # optional freshness window
# notes    = \"why this document matters\"
#
# [[validation]]
# context     = \"project-dev\"
# commands    = [\"bash scripts/ci/all.sh\"]  # run before declaring done
# marker      = \"target/.agent-validation-ok\" # optional finish-line marker
# description = \"Run the full check stack before delivery.\"
#
# [skills]                              # optional: opt in to skill-name linting
# enforce_name_prefix = true           # audit flags non-conforming skill dirs
# allowed_prefixes    = [\"project\", \"private\"] # default; matched as \"<prefix>-\"
# dir                 = \".agents/skills\"       # default skills directory
#
# `when` grammar: `path-exists:<glob>` atoms composed with `||` and `&&`
# (`&&` binds tighter). Globs support `*`, `?`, `[...]`, and `**`.
";

const RUST_EXAMPLE: &str = "\
# # Detected Cargo.toml — Rust project:
# [[document]]
# context  = \"project-dev\"
# scope    = \"project\"
# path     = \"DEVELOPMENT.md\"
# required = true
# when     = \"path-exists:Cargo.toml\"
# marker   = \"## Validation\"
#
# [[validation]]
# context  = \"project-dev\"
# commands = [\"cargo test --workspace\", \"cargo clippy --all-targets -- -D warnings\"]
";

const NODE_EXAMPLE: &str = "\
# # Detected package.json — Node project:
# [[document]]
# context  = \"project-dev\"
# scope    = \"project\"
# path     = \"DEVELOPMENT.md\"
# required = true
# when     = \"path-exists:package.json\"
#
# [[validation]]
# context  = \"project-dev\"
# commands = [\"npm test\"]
";

const GENERIC_EXAMPLE: &str = "\
# # Example project-dev override (uncomment and adapt):
# [[document]]
# context  = \"project-dev\"
# scope    = \"project\"
# path     = \"DEVELOPMENT.md\"
# required = true
# when     = \"path-exists:Cargo.toml || path-exists:package.json\"
";
