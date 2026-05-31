# Plan: plan-issue record restore

## Overview

Add a `plan-issue record restore` subcommand that re-materializes a
plan bundle's `source` and `plan` files from a tracking issue's frozen
snapshots. The issue embeds those two files' full content verbatim in a
`<details>` block plus a `plan-issue-record-payload:hex` trailer that
carries the canonical path; restore parses the latest snapshot of each
role and writes the files back under an output directory. This makes the
issue a durable source-of-truth, so an unmerged or pruned bundle branch
no longer loses the canonical design docs.

**Scope correction (verified during implementation, 2026-05-30):** the
`state` (execution-state) file is **out of scope**. Unlike `source` /
`plan`, the `state` comment is rendered from structured `StateData`
(not embedded verbatim) and its payload carries no path, so it is not a
restorable file snapshot. Its latest rendered form stays visible on the
issue and a fresh execution-state is regenerable. See the discussion
source doc's Confirmed facts.

Source: this bundle's discussion source doc (Read First, below). The
one design choice (extract content from the visible `<details>` block,
keyed by the payload path) is locked there; no open questions are
carried into execution.

## Read First

- Primary source:
  `docs/plans/plan-issue-record-restore/plan-issue-record-restore-discussion-source.md`
- Source type: discussion-to-implementation-doc
- Source issue: none (durability gap found 2026-05-30 while opening a
  plan-tracking issue and reasoning about unmerged bundle branches)
- Open questions carried into execution: none (the content-extraction
  source and latest-per-role selection are locked at the source doc;
  payload content-hash hardening is explicitly non-scope).
- Implementation surface:
  - `crates/plan-issue-cli` record subcommand tree (where `open`,
    `attach`, `audit`, `template` live; `restore` is added alongside).
  - The plan-issue snapshot renderer that `record open` uses — restore
    is its inverse parser and stays symmetric with it.
  - The `record audit` provider read path (`--body-file` /
    `--comments-json`) that restore reuses.
- Out of scope (tracked separately): changing the snapshot format or
  embedding content / a hash in the payload; restoring non-bundle
  lifecycle roles; auto-committing restored files.

## Read First boundary

- Keep the restore parser symmetric with the existing snapshot
  renderer; a round-trip test guards the symmetry.
- Reuse the existing provider read path and the offline
  `--body-file` / `--comments-json` inputs; do not add a second issue
  fetch path.
- No new third-party dependency, so `third-party-artifacts` and the
  `Cargo.lock` locked-build gate stay clean.

## Scope

- In scope:
  - A snapshot parser that, given an issue body + comments, extracts
    for each of `source` / `plan` the canonical path (from the hex
    payload) and the file content (from the `<details>` block),
    selecting the latest snapshot per role.
  - A `record restore --repo --issue --out` subcommand that writes the
    two files at their canonical paths under the output directory,
    non-destructive by default with `--force` (the global flag), and a
    `--format json` envelope listing restored paths and each role's
    recorded commit.
  - Offline operation via `--body-file` / `--comments-json`.
- Out of scope:
  - Restoring the `state` (execution-state) file — rendered, not a
    verbatim snapshot, and its payload carries no path.
  - Snapshot format changes or payload content/hash embedding.
  - Non-bundle lifecycle roles (session / validation / review /
    closeout).
  - Auto-commit / auto-merge of restored files.

## Assumptions

- The `source` / `plan` `<details>` snapshot block contains the file
  content verbatim (not HTML-escaped) as posted by `record open` /
  `attach`, so extraction is faithful absent manual edits to the
  comment.
- `source` / `plan` can be re-attached across the lifecycle, so
  latest-per-role selection keeps restore on the freshest snapshot.
- The provider read used by `record audit` is sufficient to obtain the
  issue body and all comments for parsing.

## Sprint 1: snapshot parser + restore command

**Goal**: `plan-issue record restore` reconstructs a bundle's `source`
and `plan` files from an issue's latest snapshots, online or from
offline JSON, with round-trip fidelity against `record open`.

**Demo/Validation**:

- Commands:
  - `cargo test -p nils-plan-issue-cli`
  - `plan-issue record restore --repo sympoies/nils-cli --issue 651 --out /tmp/restore-651 --format json`
- Verify: the `source` / `plan` bundle files appear under the out dir
  at their canonical paths and match the committed bundle; the JSON
  lists each role's recorded commit.

### Task 1.1: Snapshot parser (inverse of the renderer)

- **Location**:
  - `crates/plan-issue-cli` snapshot module (new inverse parser beside
    the existing renderer)
- **Description**: Parse an issue body + comments into per-role records.
  For each of `source` / `plan`, decode the
  `plan-issue-record-payload:hex` trailer for the canonical path and
  extract the file content from the role's `<details>` block (depth-
  tracking nested `<details>` in the content), selecting the latest
  snapshot when a role appears more than once.
- **Dependencies**: none
- **Complexity**: 3
- **Acceptance criteria**:
  - Given a known issue payload, the parser returns `source` / `plan`
    with correct paths and verbatim content.
  - When a role appears multiple times, the latest snapshot is selected.
- **Validation**:
  - `cargo test -p nils-plan-issue-cli`

### Task 1.2: `record restore` subcommand

- **Location**:
  - `crates/plan-issue-cli` record subcommand tree
- **Description**: Add `record restore --repo <owner/repo> --issue <N>
  --out <dir>` reusing the `record audit` provider read (and the
  offline `--body-file` / `--comments-json` inputs). Write each parsed
  role's content to its canonical path under `--out`; refuse to
  overwrite without `--force`; emit a `--format json` envelope listing
  restored paths and each role's recorded commit.
- **Dependencies**: Task 1.1
- **Complexity**: 3
- **Acceptance criteria**:
  - Restoring `#651` into an empty dir writes the `source` / `plan`
    files at their canonical paths.
  - Existing files are not clobbered unless `--force` is passed.
  - `--comments-json` input restores without any network call.
- **Validation**:
  - `cargo test -p nils-plan-issue-cli`
  - manual restore of `#651` then `diff` against the committed bundle

### Task 1.3: Round-trip and edge tests

- **Location**:
  - `crates/plan-issue-cli` tests
- **Description**: Add an `open`->`restore` round-trip test (render
  `source` / `plan` snapshots, restore back, compare), plus a nested-
  `<details>`-in-content case, latest-per-role selection, missing-role
  error, and overwrite / `--force` cases.
- **Dependencies**: Task 1.2
- **Complexity**: 2
- **Acceptance criteria**:
  - Round-trip reproduces the `source` / `plan` content byte-for-byte
    (modulo a documented trailing-newline normalization if any).
  - Missing a required role errors clearly; `--force` governs overwrite.
- **Validation**:
  - `cargo test -p nils-plan-issue-cli`

## Sprint 2: CLI docs and required checks

**Goal**: `record restore` is documented in help / completion and the
full required checks pass with no new dependency.

**Demo/Validation**:

- Commands:
  - `bash scripts/ci/completion-flag-parity-audit.sh --strict`
  - `bash scripts/ci/completion-asset-audit.sh --strict`
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast`
- Verify: the new subcommand and flags appear in completion assets; the
  local-fast gate passes with no `Cargo.lock` drift.

### Task 2.1: Help, completion, and full required checks

- **Location**:
  - `crates/plan-issue-cli` command help and completion definitions
- **Description**: Document the `restore` subcommand and its flags in
  help and completion, then run the completion audits and the full
  required-checks entrypoint to confirm no surface regressed and no
  `Cargo.lock` drift.
- **Dependencies**: Task 1.2
- **Complexity**: 1
- **Acceptance criteria**:
  - Completion flag-parity and asset audits pass with the new
    subcommand.
  - `nils-cli-checks-entrypoint.sh --local-fast` passes with no new
    dependency and no `Cargo.lock` drift.
- **Validation**:
  - `bash scripts/ci/completion-flag-parity-audit.sh --strict`
  - `bash scripts/ci/completion-asset-audit.sh --strict`
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast`

## Risks

- **R-1**: A format change to `record open` could silently break
  restore. Mitigation: keep the parser symmetric with the renderer and
  pin an `open`->`restore` round-trip test (Task 1.3).
- **R-2**: Extraction reads the visible `<details>` block, so a
  hand-edited snapshot restores the edited text. Mitigation: treat
  snapshots as frozen; a payload `content_sha256` integrity check is a
  noted future hardening, out of scope here.
- **R-3**: Restoring an earlier (re-attached) `source` / `plan`
  snapshot instead of the latest would resurrect stale docs.
  Mitigation: explicit latest-per-role selection (by `created_at`)
  plus a multiple-snapshot test (Task 1.3).
