# Third-Party Artifacts Contract v1

## Purpose

This document defines the generation contract for:

- `THIRD_PARTY_LICENSES.md`
- `THIRD_PARTY_NOTICES.md`

The contract is deterministic and anchored to `cargo metadata --format-version 1 --locked` plus `Cargo.lock`.

## Scope

- In scope:
  - required sections and table schema for both generated artifacts
  - deterministic ordering and formatting keys
  - notice extraction policy and fallback wording
  - generator mode semantics for `--check` and `--write`
- Out of scope:
  - legal interpretation of individual upstream licenses or notice obligations
- non-Rust dependency ecosystems other than an explicitly locked runtime
  backend declared by a workspace crate

## Canonical Generator Entrypoint

- Script: `scripts/generate-third-party-artifacts.sh`
- Source-of-truth command: `cargo metadata --format-version 1 --locked`
- Artifacts are generated together in a single invocation.

## Artifact Requirements

### `THIRD_PARTY_LICENSES.md` (required sections)

The artifact must contain the following sections in order:

1. H1 heading: `# THIRD_PARTY_LICENSES`
2. Intro paragraph describing third-party Rust crate licenses for the workspace.
3. Metadata bullets:
   - data source command
   - `Cargo.lock` SHA256
   - third-party crate count (`source != null`)
   - workspace crate count (`source == null`, excluded)
4. `## Notes` section with generation/source notes.
5. `## License Summary` section with markdown table:
   - columns: `License Expression`, `Crate Count`
6. `## Dependency List` section with markdown table:
   - columns: `Crate`, `Version`, `License`, `Source`
7. `## Locked Runtime Backend` section generated from
   `crates/macos-agent/peekaboo-lock.json` with exact version, license, source,
   and non-embedding/on-demand install model.

### `THIRD_PARTY_NOTICES.md` (required sections)

The artifact must contain the following sections in order:

1. H1 heading: `# THIRD_PARTY_NOTICES`
2. Intro paragraph describing notice discovery for third-party Rust crates.
3. Metadata bullets:
   - data source command
   - `Cargo.lock` SHA256
   - third-party crate count (`source != null`)
4. `## Notice Extraction Policy` section:
   - deterministic file-discovery statement
   - fallback wording statement (exact phrase)
5. `## Dependency Notices` section:
   - one subsection per third-party crate (`### <crate> <version>`)
   - per-entry fields:
     - `License`
     - `Source`
     - `Source URL` (only for entries where resolved `License` includes `MPL-2.0`, derived from package source metadata when available)
     - `License text (MPL-2.0): <https://mozilla.org/MPL/2.0/>` (only for entries where resolved `License` includes `MPL-2.0`)
     - `Notice files` (discovered list or fallback wording)
     - `License file references` (deterministic list when discovered)
     - or `License file reference: none declared`
6. `## Locked Runtime Backend Notice` generated from the immutable Peekaboo
   lock with exact version, license/source links, and distribution model.

## Deterministic Rules

### Package set

- Include only third-party packages where `source != null`.
- Exclude workspace/local packages where `source == null`.

### Ordering keys

- Package entry order (both artifacts):
  - primary: `name` (ascending)
  - secondary: `version` (ascending)
  - tertiary: `source` (ascending)
  - quaternary: `id` (ascending)
- License summary order:
  - primary: `Crate Count` (descending)
  - secondary: `License Expression` (ascending)
- Notice file list order:
  - deterministic preferred filename order, then additional regex-discovered names sorted ascending.

### Stable formatting

- Output must be markdown and stable for unchanged lockfile/metadata input.
- Markdown table cells escape pipe delimiters.
- Generated output must not include wall-clock timestamps.

## Notice Extraction Policy

For each third-party crate:

1. Resolve crate directory from package `manifest_path`.
2. Discover notice files with deterministic filename matching.
3. If no notice files are discovered, emit this exact fallback wording:
   - `No explicit NOTICE file discovered.`
4. Collect license file references in this deterministic order:
   - `license_file` metadata reference when present
   - then top-level files matching `LICENSE*`, `COPYING*`, `UNLICENSE*`
5. If no license file references are discovered, emit `License file reference: none declared`.

## Regeneration Triggers

Regenerate `THIRD_PARTY_LICENSES.md` and `THIRD_PARTY_NOTICES.md` when any of the following changes:

- `Cargo.lock`
- dependency graph or crate metadata reachable from `cargo metadata --format-version 1 --locked`
- `crates/macos-agent/peekaboo-lock.json`
- `scripts/generate-third-party-artifacts.sh`
- this contract when it changes schema/ordering/output expectations

## Generator Mode Semantics

### `--write`

- Generates both artifacts and writes them in-place at repository root.
- Success condition: both files are written without generation errors.

### `--check`

- Generates both artifacts to temporary outputs and compares against repository files.
- Exit `0` only when both files exist and are byte-identical to regenerated outputs.
- Exit non-zero when either file is missing or differs.
- On drift, the script prints diagnostics and remediation command:
  - `bash scripts/generate-third-party-artifacts.sh --write`

## Validation Commands

- `test -f docs/specs/third-party-artifacts-contract-v1.md`
- `bash scripts/generate-third-party-artifacts.sh --write`
- `bash scripts/generate-third-party-artifacts.sh --check`
