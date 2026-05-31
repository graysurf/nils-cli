# Plan: plan-archive Body / Full-Text Search

## Overview

Make the archived discussion layer (issue / PR / MR bodies and comments)
searchable from first-class `plan-archive` commands. Today `catalog --grep`
matches catalog metadata only and never reads body text, so the only way to
find a body keyword is a manual three-step dance (`grep _index/**.json` ->
reconstruct the ref URL -> `catalog --refs-to`). The body text is already in
every snapshot (`data.body` + `data.comments[].body`) and the snapshot-to-ref
mapping already exists in `query::index`; the gap is purely a search surface.

This plan adds one shared body-scan core and exposes it two ways: a
record-level `catalog --deep` filter (A) and a hit-level `search` subcommand
(B), sequenced as two PRs on the shared core. All work is in
`crates/plan-archive`; no snapshot schema, refresh, or scrub change.

## Read First

- Primary source:
  `docs/plans/plan-archive-body-search/plan-archive-body-search-discussion-source.md`
- Source type: discussion-to-implementation-doc
- Repo anchors:
  - `crates/plan-archive/src/catalog/mod.rs` (`record_matches_grep`; `Catalog`
    CLI args `--grep` / `--area` / `--refs-to`)
  - `crates/plan-archive/src/query/index.rs` (`walk_index`,
    `parse_index_path`, `IndexEntry::canonical_url`)
  - `crates/plan-archive/src/query/mod.rs` (snapshot reading)
  - `crates/plan-archive/src/cli.rs` (command surface)
  - `crates/plan-archive/tests/` (`catalog.rs`, `cli.rs`)
- Key decisions carried into execution:
  - One shared body-scan core over `data.body` + `data.comments[].body`,
    built on `query::index`; both surfaces consume it (no second scanner).
  - A = `catalog --deep`: body / comment match projected to de-duplicated
    records, composable with `--grep` / `--area` / `--refs-to`.
  - B = `plan-archive search <term>`: hit-level output (plan slug + ref URL +
    matched field + snippet) in a documented, versioned shape.
  - v1 is case-insensitive substring over each ref's latest snapshot only.
- Open questions carried into execution:
  - Whether to ship one release after PR2 or release after each PR — default is
    a single release after B; revisit at delivery if PR1 is independently
    useful.

## Scope

- In scope:
  - **Sprint 1**: shared body-scan core + `catalog --deep` (A).
  - **Sprint 2**: `plan-archive search` subcommand (B) + delivery.
- Out of scope: regex / fuzzy / ranked search; `--all-snapshots` historical
  scan; any persistent search index or caching. All kept as Future Work.

## Assumptions

1. Linear scan over the archive's snapshots is acceptable (≈50 snapshots
   today); no index is needed.
2. Latest-snapshot-per-ref scope matches catalog semantics; a keyword that
   existed only in a superseded snapshot may be missed in v1.
3. `cargo test`, `cargo clippy`, `cargo fmt`, the `--help` / completion
   snapshot, golden fixtures, and `rumdl` remain the gating validation surface,
   self-checked via `gh pr checks` before merge.

## Sprint 1: Shared Scan Core And `catalog --deep`

**Goal**: Add the one body-scan core and expose it as a `catalog --deep`
filter that resolves to de-duplicated records.

**PR grouping intent**: group (PR1)
**Execution Profile**: serial

### Task 1.1: Shared body-scan core

- **Location**:
  - `crates/plan-archive/src/query/index.rs` (or a sibling module)
  - `crates/plan-archive/src/query/mod.rs`
- **Description**: Add a helper that, given a snapshot path (or an
  `IndexEntry`) and a case-insensitive term, loads the snapshot JSON and scans
  `data.body` and `data.comments[].body`, returning hits that carry the
  snapshot's `canonical_url`, the matched field (`body` | `comment`), and a
  context snippet. Reuse `walk_index` / `parse_index_path` /
  `canonical_url`; do not add a parallel walker.
- **Dependencies**:
  - none
- **Complexity**: 3
- **Acceptance criteria**:
  - Given a fixture snapshot, the core returns a hit for a body-only term and a
    comment-only term, each with the correct `canonical_url` and matched field;
    a non-matching term returns no hits.
- **Validation**:
  - `cargo test -p nils-plan-archive` core scan cases.

### Task 1.2: `catalog --deep` flag

- **Location**:
  - `crates/plan-archive/src/catalog/mod.rs`
  - `crates/plan-archive/src/cli.rs`
- **Description**: Add a `--deep` flag to `catalog`. When set, record filtering
  also matches body / comment text via the core (per the ref's latest
  snapshot) and projects hits to de-duplicated catalog records, composing with
  the existing `--grep` / `--area` / `--refs-to` filters. Without `--deep`,
  behaviour is unchanged.
- **Dependencies**:
  - Task 1.1
- **Complexity**: 2
- **Acceptance criteria**:
  - `catalog --deep <body-only-term>` returns the matching plan(s) that plain
    `--grep` returns zero for; `--deep` composes with `--area`; output stays
    de-duplicated at record level.
- **Validation**:
  - `cargo test -p nils-plan-archive` catalog `--deep` cases.

## Sprint 2: `search` Subcommand And Delivery

**Goal**: Add the hit-level `search` subcommand on the proven core, then ship.

**PR grouping intent**: group (PR2)
**Execution Profile**: serial

### Task 2.1: `plan-archive search` subcommand

- **Location**:
  - `crates/plan-archive/src/cli.rs`
  - `crates/plan-archive/src/query/mod.rs` (or a new `search` module)
- **Description**: Add a `search <term>` subcommand that runs the core across
  the archive and emits hit-level results: the resolved plan slug (via the
  record map), the ref URL, the matched field, a snippet, and the snapshot
  location, in a documented, versioned output shape (text default,
  `--format json`). v1 is case-insensitive substring, latest snapshot per ref,
  no ranking.
- **Dependencies**:
  - Task 1.1
- **Complexity**: 3
- **Acceptance criteria**:
  - `plan-archive search <term> --format json` returns per-hit records with ref
    URL, matched field, snippet, and resolved plan slug; an empty result set is
    a well-formed empty envelope.
- **Validation**:
  - `cargo test -p nils-plan-archive` search cases; golden fixture for the
    output shape.

### Task 2.2: Tests, snapshot, and release

- **Location**:
  - `crates/plan-archive/tests/`
- **Description**: Add / update integration tests and the `--help` / completion
  snapshot for the new surface; document the `catalog --deep` vs `search` role
  split in the command help; run `cargo test` / clippy / fmt / `rumdl` and the
  relevant CI gates (self-gated via `gh pr checks`); cut the release and bump
  the Homebrew tap.
- **Dependencies**:
  - Task 2.1
- **Complexity**: 2
- **Acceptance criteria**:
  - All gates pass; the release ships and the tap is bumped; help text
    documents both surfaces and their roles.
- **Validation**:
  - `gh pr checks` green; release published; `brew` resolves the new version.

## Issue Closeout Gate

The tracking issue is complete when:

- One shared body-scan core scans `data.body` + `data.comments[].body` and is
  consumed by both surfaces; there is no second snapshot scanner.
- `catalog --deep` matches body / comment text, returns de-duplicated records,
  and composes with `--grep` / `--area` / `--refs-to`.
- `plan-archive search <term>` returns hit-level results resolving to a plan
  slug in a documented, versioned `--format json` shape; an empty result is a
  well-formed empty envelope.
- A body-only keyword that `catalog --grep` returns zero for is matched by both
  `--deep` and `search`; the role split is documented in help and the
  `plan-archive-query` skill.
- `cargo test -p nils-plan-archive`, clippy, fmt, the `--help` / completion
  snapshot, golden fixtures, and `rumdl` are green; `gh pr checks` is green.
- The release ships and the Homebrew tap is bumped.
- The `execution-state.md` ledger has every executed row at `done` with a
  non-empty `Evidence` cell; waived rows are marked `waived` with a reason.
- The closeout comment is preceded by a final
  `tracking run update --note "<closing summary>"` event.

## Future Work (Out Of Scope For This Tracker)

- Regex / fuzzy / ranked search modes.
- `--all-snapshots` historical scan across superseded snapshots.
- A persistent search index or cache if the archive outgrows linear scan.
- Simplifying the `plan-archive-query` skill and the AGENT_HOME.md note once
  `search` lands (the manual three-step body grep can be retired from the docs).

## Retention Intent

Plan-source coordination document. Cleanup-eligible after the search surface
ships and the tracker closes and archives.
