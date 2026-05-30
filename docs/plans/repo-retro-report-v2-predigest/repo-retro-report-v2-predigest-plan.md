# Plan: repo-retro Report v2 — Signal-vs-Noise Pre-Digestion

## Overview

`repo-retro report` ranks its derived insight (file hotspots, areas, themes,
follow-ups) by raw changed-line count over an undifferentiated path space. In a
workflow-heavy repo, plan/discussion authoring and mass plan archival dominate
that ranking and crowd out real source movement — and the analysis even
nominated an archived (net-deleted) plan for "focused review". The raw count
layer is sound; only the derived layer overreaches.

This plan adds a deterministic **pre-digestion layer** (path-class churn split,
archival facts, commit-frequency hotspots with a `netDeleted` flag), bumps the
report schema to v2 (no backward compatibility), and rebuilds the soft-narrative
layer to read the pre-digestion instead of raw churn. Because agent-runtime-kit
pins a *released* nils-cli surface and consumes this envelope, the work is
staged: ship v2 in nils-cli (Sprint 1) → release (Sprint 2) → refresh the
agent-runtime-kit consumers and surface pin in lockstep (Sprint 3).

## Read First

- Primary source:
  `docs/plans/repo-retro-report-v2-predigest/repo-retro-report-v2-predigest-discussion-source.md`
- Source type: discussion-to-implementation-doc
- Repo anchors:
  - `crates/agent-workflow-primitives/src/repo_retro.rs` — schema consts
    (`52-53`), `FileChangeSummary` (`324-330`), hotspot/area ranking
    (`992-1017`), `top_level_area` (`1095-1097`), `is_test_path`
    (`1085-1093`), analysis builder (`~1729-1845`)
  - `crates/agent-workflow-primitives/src/bin/repo-retro.rs` — CLI surface
  - `docs/specs/cli-output-contract-v1.md` — JSON contract conventions
  - `docs/specs/completion-coverage-matrix-v1.md` — completion CI gate
- Key decisions carried into execution:
  - [D1] add a deterministic L2 pre-digestion layer.
  - [D2] path-class taxonomy is configurable; defaults degrade gracefully.
  - [D3] bump schema to v2, break backward compatibility, no dual-emit.
  - [D4] keep L3 as a noise-aware convenience layer that reads L2.
  - [D5] refresh agent-runtime-kit consumers in lockstep.
  - [D6] rank `topFiles` by commit-touch count; add `class` + `netDeleted`.
  - [D7] net-deletion is the primary archival signal; commit-scope secondary.
- Open questions carried into execution:
  - Release granularity: ship Sprint 2 as its own release once Sprint 1
    merges, or fold v2 into the next scheduled release — default is its own
    release.
  - Whether the path-class config-path flag lands in Sprint 1 (adding a
    completion-matrix update) or defaults-only ships first with the flag
    deferred — default is to ship the flag in Sprint 1.

## Scope

- In scope:
  - **Sprint 1**: path-class classifier + config, L2 fields
    (`churnByClass`, `archival`), commit-frequency hotspots, schema v2 bump,
    L3 rewrite, tests, JSON-contract + completion updates (nils-cli PR).
  - **Sprint 2**: release the surface carrying v2 + Homebrew tap bump.
  - **Sprint 3**: refresh agent-runtime-kit consumers (`meta:repo-retro`,
    `reporting:project-retro`) and bump the nils-cli surface pin in lockstep
    (agent-runtime-kit PR via `meta:nils-cli-bump`).
- Out of scope: remote API enrichment; new evidence inputs; history-comparison
  changes; new subcommands beyond `report`; any v1 compatibility shim. Kept as
  Future Work.

## Assumptions

1. The raw L1 fact layer (`CommitSummary`, `commit_types`, `authors`,
   `test_signals`) is correct and stays unchanged.
2. Per-file `commits` / `insertions` / `deletions` are already collected
   (`FileChangeSummary`), so commit-frequency ranking and `netDeleted` are
   sort-key / derived-field changes, not new data collection.
3. `cargo test`, `cargo clippy -D warnings`, `cargo fmt`, the `--help` /
   completion snapshot, `rumdl`, and
   `scripts/ci/nils-cli-checks-entrypoint.sh --local-fast` are the gating
   validation surface, self-checked via `gh pr checks` before merge.
4. agent-runtime-kit's EXACT-match surface-pin gate will block its pushes until
   the pin matches the released v2 surface, enforcing lockstep in Sprint 3.

## Sprint 1: Pre-Digestion Layer And Schema v2

**Goal**: Add the deterministic L2 pre-digestion, bump the report schema to v2,
and rebuild L3 to read L2 — shipped as one nils-cli PR.

**PR grouping intent**: group (PR1)
**Execution Profile**: serial

### Task 1.1: Path-class classifier + config

- **Location**:
  - `crates/agent-workflow-primitives/src/repo_retro.rs`
  - `crates/agent-workflow-primitives/src/bin/repo-retro.rs`
- **Description**: Replace `top_level_area` with a `classify_path` resolving
  each path to a class (`source` | `tests` | `productDocs` | `processArtifacts`
  | `other`). Ship built-in default heuristics (reuse `is_test_path` for
  `tests`; `docs/plans/**`, `docs/discussions/**`, heuristic-system inbox /
  operation-records for `processArtifacts`; README / DEVELOPMENT / `docs/specs`
  / `docs/runbooks` / durable `*.md` for `productDocs`; everything else
  `source`). Support a repo-local override (glob → class) merged over defaults,
  selectable via a config-path flag. Absent conventions yield empty classes,
  never a misclassification.
- **Dependencies**: none
- **Complexity**: 3
- **Acceptance criteria**:
  - A fixture set classifies a plan path as `processArtifacts`, a `src` path as
    `source`, a test path as `tests`, and a README as `productDocs`; a
    repo-local override deterministically reclassifies a path (AC5).
- **Validation**:
  - `cargo test -p nils-agent-workflow-primitives` classifier + override
    cases (red→green).

### Task 1.2: L2 fields — churnByClass, archival, commit-frequency hotspots

- **Location**:
  - `crates/agent-workflow-primitives/src/repo_retro.rs`
- **Description**: Emit `git.churnByClass` (per-class fileCount / commits /
  insertions / deletions / changedLines, reconciling to
  `summary.changedLines`). Add `git.archival` (netDeletedFileCount,
  netDeletedFiles[], processArtifactsDeletedLines, plansArchivedEstimate),
  with net-deletion (`insertions == 0 && deletions > 0`) as the primary signal
  ([D7]). Re-rank `fileHotspots.topFiles` by `commits` desc then
  `changedLines` desc; add `class` + `netDeleted` to each entry; tag
  `topAreas` entries with a dominant `class`.
- **Dependencies**: Task 1.1
- **Complexity**: 3
- **Acceptance criteria**:
  - `churnByClass` per-class `changedLines` sums to `summary.changedLines`
    (AC2); `topFiles` is ordered by `commits` desc and every entry carries both
    `class` and `netDeleted` (AC3); `archival.netDeletedFiles` lists each
    pure-deletion file.
- **Validation**:
  - `cargo test -p nils-agent-workflow-primitives` class-reconciliation +
    ranking + flag cases (red→green).

### Task 1.3: Schema v2 bump + noise-aware L3 rewrite

- **Location**:
  - `crates/agent-workflow-primitives/src/repo_retro.rs`
- **Description**: Bump `REPORT_ENVELOPE_SCHEMA_VERSION` /
  `REPORT_SCHEMA_VERSION` to `...report.v2` with no v1 dual-emit ([D3]).
  Rebuild `analysis.themes` to lead with source/tests churn from
  `churnByClass` and report process-doc churn separately (drop the bare
  "`<area>` had the most lines"). Rebuild `analysis.followUpQuestions` to
  nominate the top non-`netDeleted` iteration hotspot and never a net-deleted
  file, emitting an archival summary line instead. Keep L3 present as a
  convenience layer ([D4]); update the Markdown renderer to match.
- **Dependencies**: Task 1.2
- **Complexity**: 3
- **Acceptance criteria**:
  - Schema strings are `...report.v2` and v1 is absent (AC6); on an
    archival-dominated window no `netDeleted` file is nominated for review
    (AC1); `themes` emits a class-aware split, not the bare docs-largest line
    (AC4).
- **Validation**:
  - `cargo test -p nils-agent-workflow-primitives` analysis-rewrite +
    schema-version cases (red→green).

### Task 1.4: Contract, completion, tests, and PR delivery

- **Location**:
  - `crates/agent-workflow-primitives/` (tests)
  - `docs/specs/cli-output-contract-v1.md`,
    `docs/specs/completion-coverage-matrix-v1.md`
- **Description**: Update the JSON-contract spec for the v2 envelope; if a
  config-path flag is added, update the completion-coverage matrix and
  completion scripts (CI gate). Add/refresh integration tests and the `--help`
  snapshot. Run the full local-fast gate; deliver PR1 self-gated via
  `gh pr checks`.
- **Dependencies**: Task 1.3
- **Complexity**: 2
- **Acceptance criteria**:
  - JSON-contract + completion updated for v2; all gates pass; PR1 merged.
- **Validation**:
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast` green;
    `gh pr checks` green.

## Sprint 2: Release

**Goal**: Ship a released nils-cli surface carrying repo-retro v2 so the pinned
consumer can adopt it.

**PR grouping intent**: group (PR2 / release)
**Execution Profile**: serial

### Task 2.1: Cut release + tap bump

- **Location**: workspace release flow
- **Description**: Run the version bump → tag → release → Homebrew tap flow so a
  published version carries repo-retro v2; confirm `brew` resolves it.
- **Dependencies**: Task 1.4
- **Complexity**: 2
- **Acceptance criteria**:
  - The release publishes; `brew upgrade` resolves the new version with
    repo-retro v2 output.
- **Validation**:
  - Release workflow green; `repo-retro --version` and a v2 report confirmed on
    the published binary.

## Sprint 3: agent-runtime-kit Consumer Refresh + Pin Bump

**Goal**: Adopt v2 in agent-runtime-kit and bump the surface pin in lockstep.

**PR grouping intent**: group (PR3, agent-runtime-kit)
**Execution Profile**: serial

### Task 3.1: Refresh consumers + bump pin

- **Location**: `agent-runtime-kit` (`meta:repo-retro`, `reporting:project-retro`
  skill surfaces; nils-cli surface pin)
- **Description**: Update the `meta:repo-retro` and `reporting:project-retro`
  consumers for the v2 envelope (no removed v1 fields referenced). Bump the
  pinned nils-cli surface via `meta:nils-cli-bump`. The EXACT-match pin gate
  forces this to land together.
- **Dependencies**: Task 2.1
- **Complexity**: 2
- **Acceptance criteria**:
  - Consumers read v2 without referencing removed v1 fields (AC7); the pin
    matches the released surface; agent-runtime-kit validation passes.
- **Validation**:
  - `agent-runtime doctor --class version-alignment` clean; agent-runtime-kit
    project-dev validation green.

## Issue Closeout Gate

The tracking issue is complete when:

- `repo-retro report` emits schema v2 (`...report.v2`) with no v1 dual-emit.
- `git.churnByClass` separates source / tests / productDocs / processArtifacts
  and per-class `changedLines` reconcile to `summary.changedLines`.
- `fileHotspots.topFiles` is ranked by commit-touch count with `class` +
  `netDeleted` on each entry; `git.archival` reports net-deleted files.
- On an archival-dominated window, no `netDeleted` file is nominated for
  "focused review" and `themes` emits a class-aware split (the [F3]/[F4]
  regressions are gone).
- The path-class taxonomy is configurable and degrades gracefully on a repo
  with no plan/discussion convention.
- JSON-contract + completion matrix updated; `nils-cli-checks-entrypoint.sh
  --local-fast`, `rumdl`, and `gh pr checks` green; release shipped and tap
  bumped.
- agent-runtime-kit `meta:repo-retro` + `reporting:project-retro` consume v2
  and the surface pin matches the released version.
- The `execution-state.md` ledger has every executed row at `done` with a
  non-empty `Evidence` cell; waived rows are marked `waived` with a reason.
- The closeout comment is preceded by a final `tracking run update
  --note "<closing summary>"` event.

## Future Work (Out Of Scope For This Tracker)

- Per-class trend / history comparison across windows.
- A first-class `repo-retro init-config` to scaffold the path-class override.
- Ranked / weighted hotspot scoring beyond raw commit-touch count.
- Folding the durable v2 contract back into a versioned crate-local spec and
  retiring the transient handoff note.

## Retention Intent

Plan-source coordination document. Cleanup-eligible after the v2 surface ships,
consumers are refreshed, and the tracker closes and archives.
