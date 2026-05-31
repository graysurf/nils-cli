# plan-archive Body / Full-Text Search — Implementation Handoff

- Status: decisions settled; ready for plan generation.
- Date: 2026-05-30
- Source: a review of the `plan-archive` query ergonomics. The archive's body
  text (issue / PR / MR descriptions and comments) is captured in every
  snapshot but is not reachable from any first-class command; the only way to
  search it today is a manual three-step shell dance. This bundle scopes a
  two-part fix (a shared scan core feeding a `catalog --deep` filter and a
  `search` subcommand) in `crates/plan-archive`.
- Intended next step: generate the plan bundle under
  `docs/plans/plan-archive-body-search/`. This is a source artifact, not an
  implementation plan.

## Execution

- Recommended plan: docs/plans/plan-archive-body-search/plan-archive-body-search-plan.md
- Recommended execution state: docs/plans/plan-archive-body-search/plan-archive-body-search-execution-state.md
- Status: decisions settled; plan generation is the next step.
- Next-task source: this document

## Purpose

Make the archived discussion layer (issue / PR / MR bodies and comments)
searchable through first-class commands. The archive already stores this text
and already knows how to map a snapshot back to its plan; the gap is purely a
search surface. Close it without adding a second snapshot scanner: one shared
body-scan core feeds both a record-level filter (`catalog --deep`) and a
hit-level subcommand (`search`).

## Confirmed Facts (current crate behaviour)

- [F1] `catalog --grep` matches catalog metadata only. `record_matches_grep`
  (`crates/plan-archive/src/catalog/mod.rs:336-354`) scans `slug`, `title`,
  `summary`, `original_path`, `host`, `org`, `repo`, and each ref's `url` /
  `title` / `state`. It never reads issue / PR bodies or comments.
- [F2] The body text is already archived. Every `_index/**.json` snapshot
  carries `data.body` and `data.comments[].body` (each comment also has
  `url` / `author` / `created_at`). Bodies are secret-scrubbed at refresh
  time, so scanning them is safe.
- [F3] Each catalog ref carries its `latest_snapshot` relative path, and
  records already map ref -> plan, so a body hit can be resolved back to a plan
  with data the catalog already holds.
- [F4] The traversal and path->URL plumbing already exists in `query::index`
  (`crates/plan-archive/src/query/index.rs`): `walk_index()` enumerates
  snapshots, `parse_index_path()` parses a snapshot path, and
  `IndexEntry::canonical_url()` reconstructs the ref URL.
- [F5] No first-class body search exists. The only way today is a manual
  three-step dance: `grep _index/**.json` -> reconstruct the ref URL from the
  snapshot path -> `catalog --refs-to <url>`. That friction suppresses use.

## Decisions

1. Add a shared body-scan core in `crates/plan-archive`, built on
   `query::index` traversal: load a snapshot's JSON, scan `data.body` and
   `data.comments[].body` for a case-insensitive substring, and return hits
   carrying the snapshot's `canonical_url`, the matched field (`body` or
   `comment`), and a context snippet. Both surfaces below consume this core;
   there is exactly one scanner.
2. (A) Add a `catalog --deep` flag. When set, catalog filtering also matches
   body / comment text via the core and projects hits to de-duplicated catalog
   records, composing with the existing `--grep` / `--area` / `--refs-to`
   filters. Default scope is each ref's `latest_snapshot` only, matching
   catalog semantics.
3. (B) Add a `plan-archive search <term>` subcommand with hit-level output:
   the plan slug (resolved via the record map), the ref URL, the matched field,
   a snippet, and the snapshot location, in a documented, versioned output
   shape (text default, `--format json`). v1 is deliberately minimal:
   case-insensitive substring, latest snapshot per ref, no ranking.
4. Sequence the work as two PRs on the shared core: PR1 ships the core plus
   `catalog --deep` (A); PR2 ships `search` (B). A single release follows PR2.

## Scope

- In scope: the shared body-scan core, the `catalog --deep` flag, the
  `plan-archive search` subcommand and its documented output schema, updated
  integration tests, golden fixtures, the `--help` / completion snapshot, and
  the release.
- Out of scope: regex / fuzzy / ranked search; an `--all-snapshots` historical
  scan (latest-snapshot-per-ref only in v1); any persistent search index or
  caching layer.

## Non-Scope

- Changing the snapshot schema, the refresh pipeline, or the scrub patterns.
- Any non-`plan-archive` nils-cli crate.

## Implementation Boundaries

- The core lives in `crates/plan-archive` and reuses `query::index` rather than
  introducing a parallel snapshot walker.
- `search` output is a contract once released; its shape is documented and
  versioned like other plan-archive command envelopes.
- Delivery follows the nils-cli flow: PR (self-gated via `gh pr checks`) ->
  release -> Homebrew tap.

## Requirements

1. A shared core scans `data.body` + `data.comments[].body` and returns hits
   with `canonical_url`, matched field, and a snippet.
2. `catalog --deep` matches body / comment text and returns de-duplicated
   records, composable with `--grep` / `--area` / `--refs-to`.
3. `plan-archive search <term>` returns hit-level results resolving to a plan
   slug, in a documented `--format json` shape (text default).
4. Both surfaces consume the one core; there is no second snapshot scanner.
5. Integration tests and the `--help` / completion snapshot cover the new
   surface.

## Acceptance Criteria

- `catalog --deep rollback` returns the plans whose body / comments contain
  "rollback" (e.g. `2026-05-23-codex-skill-surface-acceptance-cutover`), and
  composes with `--area`.
- A keyword present only in metadata is still matched; a keyword present only
  in a body / comment — which `catalog --grep` returns zero for today — is now
  matched by `--deep` and `search`.
- `plan-archive search rollback --format json` returns per-hit records with the
  ref URL, matched field, snippet, and resolved plan slug.
- `cargo test -p nils-plan-archive` and the `--help` / completion snapshots
  pass; clippy `-D warnings`, fmt, and `rumdl` are clean; `gh pr checks` green.

## Risks And Guardrails

- Two overlapping entry points on a low-frequency tool risk "which do I use?"
  confusion. Guardrail: document the role split — `catalog --deep` is a
  filterable record-level discovery surface; `search` is hit-level full-text —
  in the command help and the `plan-archive-query` skill.
- `search` output becomes a cross-consumer contract. Guardrail: version the
  output shape and add golden fixtures.
- Scope creep on `search` (ranking, context windows, `--all-snapshots`).
  Guardrail: v1 is substring + latest-snapshot-per-ref; richer modes are
  future work.

## Validation Plan

- `cargo test -p nils-plan-archive` (unit + integration), `cargo clippy`,
  `cargo fmt`.
- `--help` / completion snapshot updated and asserted; golden fixtures for the
  `search` output shape.
- `rumdl check` on changed Markdown.
- The nils-cli CI gates relevant to a changed crate, self-checked via
  `gh pr checks` before merge.

## Read First

- Catalog metadata grep: `crates/plan-archive/src/catalog/mod.rs`
  (`record_matches_grep`, the `Catalog` CLI args).
- Snapshot traversal and URL mapping: `crates/plan-archive/src/query/index.rs`
  (`walk_index`, `parse_index_path`, `IndexEntry::canonical_url`).
- Snapshot body shape: any `_index/**.json` in the archive clone
  (`data.body`, `data.comments[].body`).
- Existing reverse lookup: `catalog --refs-to <url>`.

## Retention Intent

Plan-source coordination document. Cleanup-eligible after the search surface
ships and the tracker closes and archives.
