# Plan: agent-docs Engine Redesign

## Overview

Rebuild the `agent-docs` crate (`crates/agent-docs`) so policy is data the
consuming repo declares and the binary is a generic resolver and auditor.
Today contexts, scopes, and required docs are hardcoded Rust; `when` is inert;
the surface only reports presence and never emits content; and the docs-home
must come from `AGENT_DOCS_HOME`. The redesign makes the catalog data-driven,
adds real `when` predicates and content validation, collapses the command
surface to `audit` / `preflight` / `init` / `explain` / `list` / `remove`,
derives the docs-home from the install symlink, and makes `preflight` emit doc
content plus the per-repo validation contract so a consuming hook can inject
and enforce it.

This crate work is the upstream dependency for graysurf/agent-runtime-kit#181:
the kit's Sprints 2-4 consume this release. No backward compatibility is
required.

## Read First

- Primary source:
  `docs/plans/agent-docs-engine-redesign/agent-docs-engine-redesign-discussion-source.md`
- Source type: discussion-to-implementation-doc
- Authoritative full design (cross-repo): `graysurf/agent-runtime-kit`:
  `docs/plans/2026-05-30-agent-docs-redesign/2026-05-30-agent-docs-redesign-discussion-source.md`
- Consuming tracker (cross-repo): graysurf/agent-runtime-kit#181
- Repo anchors:
  - `crates/agent-docs/src/model.rs` (contexts, scopes, `when`, output types)
  - `crates/agent-docs/src/config.rs` (catalog schema + parsing)
  - `crates/agent-docs/src/resolver.rs` (builtins, resolution, dedupe)
  - `crates/agent-docs/src/commands/baseline.rs` (-> `audit`)
  - `crates/agent-docs/src/env.rs` (docs-home resolution)
  - `crates/agent-docs/src/output.rs` (presence report -> content emit)
  - `crates/agent-docs/src/cli.rs` (command surface)
  - `crates/agent-docs/tests/` (integration tests, `--help` snapshot)
- Key decisions carried into execution (engine subset; full set in the kit
  source):
  - Data-driven catalog; no hardcoded builtins.
  - `when` predicates replace the `required=false` opt-out.
  - Content validation, not just presence.
  - Collapsed command surface; dedupe by resolved path; retire `startup`.
  - Symlink-derived docs-home; keep `--docs-home`.
  - `preflight` emits doc content + the per-repo validation contract.
  - `init` annotated override stub.
- Open questions carried into execution:
  - none — the design is settled in the kit source; the `preflight` output
    JSON shape is defined during Sprint 4 and documented as the cross-repo
    contract.

## Scope

- In scope:
  - **Sprint 1**: data-driven catalog schema + parser; remove hardcoded
    builtins.
  - **Sprint 2**: `when` predicate evaluator; content validation.
  - **Sprint 3**: collapsed command surface, symlink-derived docs-home,
    dedupe by resolved path, `init` stub; retire `startup` / `resolve` /
    `baseline` / `scaffold-*`.
  - **Sprint 4**: content-emitting `preflight` + validation-contract
    resolution; integration tests + `--help` snapshot; release.
- Out of scope: all kit-side consumers (catalog content, cue inlining,
  hooks, Codex enforcement) — graysurf/agent-runtime-kit#181.

## Assumptions

1. No backward compatibility is required; breaking the CLI surface is
   acceptable as long as in-repo callers, fixtures, and snapshots are updated
   in the same release.
2. The released `preflight` output shape becomes a cross-repo contract pinned
   by the kit via `required_clis`.
3. `cargo test`, `cargo clippy`, `cargo fmt`, the `--help` snapshot, and
   `rumdl` remain the gating validation surface, self-checked via
   `gh pr checks` before merge.

## Sprint 1: Data-Driven Catalog Foundation

**Goal**: Replace hardcoded Rust builtins with a catalog schema so contexts
and required docs are declared as data.

**PR grouping intent**: group
**Execution Profile**: serial

### Task 1.1: Define the catalog schema and model

- **Location**:
  - `crates/agent-docs/src/model.rs`
  - `crates/agent-docs/src/config.rs`
- **Description**: Define the data model for contexts and required docs as
  catalog entries (context id, scope, path, required, `when`, marker, notes)
  and extend the TOML parser to load a full catalog, including a default
  catalog the consuming repo inherits or overrides. Keep validation errors
  precise (line/column, field).
- **Dependencies**:
  - none
- **Complexity**: 3
- **Acceptance criteria**:
  - The catalog model and parser load contexts and docs from data; invalid
    catalogs produce precise errors.
- **Validation**:
  - `cargo test -p agent-docs` config/model cases.

### Task 1.2: Remove hardcoded builtins from resolution and baseline

- **Location**:
  - `crates/agent-docs/src/resolver.rs`
  - `crates/agent-docs/src/commands/baseline.rs`
- **Description**: Drive resolution and the baseline/audit pass from the
  catalog instead of the hardcoded `startup` / `skill-dev` / `task-tools` /
  `project-dev` builtins and the `required=false` opt-out path.
- **Dependencies**:
  - Task 1.1
- **Complexity**: 3
- **Acceptance criteria**:
  - No hardcoded required-doc constants remain; resolution reflects the
    catalog.
- **Validation**:
  - `cargo test -p agent-docs` resolver/baseline cases.

## Sprint 2: Conditional And Content Validation

**Goal**: Make requirements conditional and validate content, not just
existence.

**PR grouping intent**: group
**Execution Profile**: serial

### Task 2.1: Implement the when predicate evaluator

- **Location**:
  - `crates/agent-docs/src/config.rs`
  - `crates/agent-docs/src/resolver.rs`
- **Description**: Replace the inert `DocumentWhen::Always` with a
  `path-exists:<glob>` predicate composed with `||` and `&&`, evaluated
  against the resolved project root. A requirement whose predicate is false is
  not required.
- **Dependencies**:
  - Task 1.2
- **Complexity**: 3
- **Acceptance criteria**:
  - A docs-only project (no `Cargo.toml` / `package.json` / `src/**`)
    auto-skips a code doc with no opt-out; a project with the marker requires
    it.
- **Validation**:
  - `cargo test -p agent-docs` `when` evaluation cases (true/false/compose).

### Task 2.2: Add content validation

- **Location**:
  - `crates/agent-docs/src/resolver.rs`
  - `crates/agent-docs/src/model.rs`
- **Description**: Beyond existence, validate non-empty content, a required
  marker, and an optional `last-reviewed` freshness signal. Expose the result
  so a placeholder fails.
- **Dependencies**:
  - Task 1.2
- **Complexity**: 2
- **Acceptance criteria**:
  - A zero-byte or marker-less required doc is reported invalid.
- **Validation**:
  - `cargo test -p agent-docs` content-validation cases.

## Sprint 3: Command Surface And Resolution

**Goal**: Collapse the command surface, derive the docs-home from the install
symlink, dedupe, add `init`, and retire the old commands.

**PR grouping intent**: group
**Execution Profile**: serial

### Task 3.1: Collapse the command surface and retire old commands

- **Location**:
  - `crates/agent-docs/src/cli.rs`
  - `crates/agent-docs/src/lib.rs`
- **Description**: Replace `resolve` / `baseline` / `scaffold-agents` /
  `scaffold-baseline` with `audit`, `preflight`, `init`, `explain`, `list`,
  `remove`. Retire the `startup` per-task context. Dedupe resolved docs by
  resolved path.
- **Dependencies**:
  - Task 1.2
- **Complexity**: 3
- **Acceptance criteria**:
  - `agent-docs --help` shows only the new surface; no doc is listed twice.
- **Validation**:
  - `cargo test -p agent-docs`; `--help` snapshot updated.

### Task 3.2: Symlink-derived docs-home

- **Location**:
  - `crates/agent-docs/src/env.rs`
- **Description**: When `--docs-home` is absent, derive the docs-home from the
  install symlink (`dirname(readlink ~/.claude/CLAUDE.md)`, with the Codex
  equivalent). Keep `--docs-home` as the explicit override; an unresolvable
  home is a clear error.
- **Dependencies**:
  - Task 3.1
- **Complexity**: 2
- **Acceptance criteria**:
  - With `AGENT_DOCS_HOME` unset and no `--docs-home`, the engine resolves the
    docs-home via the symlink; an unresolvable case errors clearly.
- **Validation**:
  - `cargo test -p agent-docs` env-resolution cases.

### Task 3.3: init annotated override stub

- **Location**:
  - `crates/agent-docs/src/commands/scaffold_agents.rs`
  - `crates/agent-docs/src/cli.rs`
- **Description**: Implement `init` to emit an annotated, editable
  project-local override stub (`--print` to stdout; `--dry-run` / `--force` to
  write) that lists inherited defaults as comments and never dumps a full copy
  of them; optionally pre-fill `when` examples from detected `Cargo.toml` /
  `package.json`.
- **Dependencies**:
  - Task 3.1
- **Complexity**: 2
- **Acceptance criteria**:
  - `agent-docs init --print` outputs a valid, lint-clean stub with no
    required entries by default.
- **Validation**:
  - `cargo test -p agent-docs` init cases; `rumdl` on a generated stub.

## Sprint 4: Hook-Consumable Output And Delivery

**Goal**: Make `preflight` emit doc content and the validation contract, then
ship the release the kit consumes.

**PR grouping intent**: group
**Execution Profile**: serial

### Task 4.1: Content-emitting preflight + validation-contract resolution

- **Location**:
  - `crates/agent-docs/src/output.rs`
  - `crates/agent-docs/src/resolver.rs`
- **Description**: Make `preflight --intent X` emit the non-auto-loaded doc
  set, each doc's content, and the per-repo validation contract in a
  documented, versioned JSON shape a hook consumes. This is the cross-repo
  contract the kit pins.
- **Dependencies**:
  - Task 2.2
  - Task 3.1
- **Complexity**: 3
- **Acceptance criteria**:
  - `preflight --intent project-dev --format json` emits the resolved content
    and the validation contract, not a bare presence report; the shape is
    documented.
- **Validation**:
  - `cargo test -p agent-docs` preflight-output cases against a fixture repo.

### Task 4.2: Tests, snapshot, and release

- **Location**:
  - `crates/agent-docs/tests/`
- **Description**: Update integration tests and the `--help` snapshot to the
  new surface; run `cargo test` / `clippy` / `fmt` and `rumdl`; open the PR
  (self-gated via `gh pr checks`), cut the release, and bump the Homebrew tap.
  Signal graysurf/agent-runtime-kit#181 to bump `required_clis` and start its
  Sprints 2-4.
- **Dependencies**:
  - Task 4.1
- **Complexity**: 2
- **Acceptance criteria**:
  - All gates pass; the release ships and the tap is bumped; the kit tracker
    is notified with the release tag.
- **Validation**:
  - `gh pr checks` green; release published; `brew` resolves the new version.

## Issue Closeout Gate

The tracking issue is complete when:

- The catalog is data-driven with no hardcoded builtins; `when` predicates and
  content validation work.
- The command surface is `audit` / `preflight` / `init` / `explain` / `list` /
  `remove`; `resolve` / `baseline` / `scaffold-*` / `startup` per-task are
  gone; no doc is listed twice.
- `preflight --intent` emits doc content and the validation contract in a
  documented JSON shape; docs-home derives from the install symlink.
- `cargo test -p agent-docs`, clippy, fmt, the `--help` snapshot, and `rumdl`
  are green; `gh pr checks` is green.
- The release ships, the Homebrew tap is bumped, and
  graysurf/agent-runtime-kit#181 is notified to bump `required_clis`.
- The `execution-state.md` ledger has every executed row at `done` with a
  non-empty `Evidence` cell; waived rows are marked `waived` with a reason.
- The closeout comment is preceded by a final
  `tracking run update --note "<closing summary>"` event.

## Future Work (Out Of Scope For This Tracker)

- All kit-side consumption (default catalog, cue inlining, awareness +
  finish-line hooks, Codex enforcement): graysurf/agent-runtime-kit#181.
- A general-purpose `when` expression language beyond `path-exists`, glob, and
  boolean composition.

## Retention Intent

Plan-source coordination document. Cleanup-eligible after the engine release
ships and the tracker closes and archives.
