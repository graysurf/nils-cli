# plan-archive Body / Full-Text Search Execution State

<!-- plan-issue-record:v2 role=state profile=tracking -->
## Execution State

- Status: Sprint 1 implemented (PR1 pending) — shared scan core and
  `catalog --deep` landed on the feature branch; Sprint 2 not started.
- Target scope: `crates/plan-archive` body / full-text search surface in
  `sympoies/nils-cli` (shared scan core + `catalog --deep` + `search`).
- Execution window: Sprint 1 (shared core + `catalog --deep`, PR1) → Sprint 2
  (`search` subcommand + delivery, PR2), serial.
- Current task: none — Sprint 1 complete locally; PR1 delivery pending.
- Next task: Task 2.1 — `plan-archive search` subcommand (Sprint 2).
- Last updated: 2026-05-30
- Branch/commit/PR: scan core `f8fdef2` + `catalog --deep` `aa107e8` on
  `feat/plan-archive-body-search`; PR1 not yet opened.
- Source document: docs/plans/plan-archive-body-search/plan-archive-body-search-plan.md
- Direct source-doc execution waiver: not applicable
- Tracking issue: sympoies/nils-cli#689
- Source snapshot: posted by `create-plan-tracking-issue` at issue open
- Plan snapshot: posted by `create-plan-tracking-issue` at issue open
- Initial state snapshot: posted by `create-plan-tracking-issue` at issue open

## Validation Plan

- Sprint 1: `cargo test -p nils-plan-archive` core-scan cases (body-only and
  comment-only hits resolve the correct `canonical_url` and matched field) and
  `catalog --deep` cases (matches a body-only term that `--grep` misses,
  composes with `--area`, output de-duplicated at record level).
- Sprint 2: `cargo test -p nils-plan-archive` search cases with a golden
  fixture for the output shape; `--help` / completion snapshot updated; clippy
  `-D warnings`, fmt, `rumdl` clean; `gh pr checks` green; release published
  and Homebrew tap bumped.
- Cross-cutting: every executed task populates its `Evidence` cell; waived
  tasks are marked `waived` with a reason. The closeout comment is preceded by
  a final `tracking run update --note "<closing summary>"` event.

## Task Ledger

| ID | Status | Task | Evidence | Notes |
| --- | --- | --- | --- | --- |
| 1.1 | done | Shared body-scan core over `data.body` + `data.comments[].body` | commit f8fdef2 on feat/plan-archive-body-search; 10 scan unit tests (red→green); clippy -D warnings + fmt clean | Build on `query::index` (`walk_index` / `parse_index_path` / `canonical_url`); return hits with url + matched field + snippet. No second scanner. |
| 1.2 | done | `catalog --deep` flag | commit aa107e8; catalog --deep unit test (body-only term + --area compose, red→green); 108 lib + integration tests green, clippy + fmt clean | Depends on 1.1. Body/comment match projected to de-duplicated records; composes with `--grep` / `--area` / `--refs-to`. PR1 with 1.1. |
| 2.1 | todo | `plan-archive search <term>` subcommand | — | Depends on 1.1. Hit-level output (plan slug + ref URL + matched field + snippet) in a documented versioned shape; substring, latest snapshot per ref, no ranking. |
| 2.2 | todo | Tests, `--help` / completion snapshot, and release | — | Depends on 2.1. Golden fixture for `search`; document `catalog --deep` vs `search` roles; PR self-gated via `gh pr checks`; release + tap bump. |

## Session Log

- 2026-05-30: Authored this bundle (discussion-source + plan +
  execution-state) for the plan-archive body / full-text search surface.
  Findings: `catalog --grep` matches catalog metadata only
  (`catalog/mod.rs:336-354`) and never reads body text; the body is already in
  every snapshot (`data.body` + `data.comments[].body`, scrub-cleaned); and the
  snapshot->ref mapping already exists in `query::index`. Conclusion: add one
  shared body-scan core and expose it as a record-level `catalog --deep` filter
  (A) and a hit-level `search` subcommand (B), sequenced as two PRs on the
  shared core. No implementation started; this state is prepared so
  `create-plan-tracking-issue` can open the tracker with a populated ledger.
  Authored in an isolated worktree off `main` to avoid disturbing the shared
  checkout.
- 2026-05-30: Implemented Sprint 1 in the worktree, test-first. Task 1.1: the
  shared body-scan core `query::scan` (`scan_entry` / `entry_matches` over a
  ref's latest snapshot `data.body` + `data.comments[].body`, returning hits
  with `canonical_url`, matched field, and a context snippet), built on
  `query::index` — commit `f8fdef2`, 10 unit tests written red-first. Task 1.2:
  `catalog --deep`, which extends `--grep` to the scan core and composes with
  `--area` / `--refs-to` — commit `aa107e8`, unit test red-first. Full crate
  gate green (108 lib + integration tests, clippy `-D warnings`, fmt). PR1
  (Sprint 1) delivery and the full workspace required-checks gate are pending
  via `deliver-plan-tracking-issue`.

## Validation

| Command | Status | Summary | Artifact |
| --- | --- | --- | --- |
| `cargo test -p nils-plan-archive` | pass | 108 lib + integration tests green, incl. 10 scan-core cases and the `catalog --deep` body-only/`--area`-compose case. | local |
| `cargo clippy -p nils-plan-archive --all-targets -- -D warnings` | pass | No warnings. | local |
| `cargo fmt -p nils-plan-archive -- --check` | pass | Clean. | local |

## Notes

- The two surfaces overlap by design but serve distinct intents: `catalog
  --deep` is a filterable record-level discovery surface (composes with the
  other catalog filters); `search` is hit-level full-text with snippets. The
  role split must be documented in command help and the `plan-archive-query`
  skill to avoid "which do I use" confusion.
- Once `search` ships, the manual three-step body grep can be retired from the
  `plan-archive-query` skill and the AGENT_HOME.md Plan Archive note.
- Authored in worktree
  `~/Project/sympoies/nils-cli-wt/plan-archive-body-search` on branch
  `feat/plan-archive-body-search`; the shared `nils-cli` main checkout was not
  disturbed.
