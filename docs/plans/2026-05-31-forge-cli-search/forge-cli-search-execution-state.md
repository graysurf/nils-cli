# forge-cli Issue / PR Search Execution State

<!-- plan-issue-record:v2 role=state profile=tracking -->
## Execution State

- Status: decisions settled; not yet implemented. This bundle is prepared so
  `create-plan-tracking-issue` can open the tracker with a populated ledger.
- Target scope: `crates/forge-cli` (`nils-forge-cli`) `search` surface in
  `sympoies/nils-cli` — top-level `search issues` / `search prs` (full-text,
  B) + `search refs-to` (cross-reference, A), GitHub-only behind the provider
  seam, single-repo.
- Execution window: Sprint 1 (`search` subtree + seam + GitHub full-text,
  PR1) → Sprint 2 (`search refs-to` + docs + delivery, PR2), serial.
- Current task: Task 1.1 — not started.
- Next task: Task 1.1 — `search` CLI subtree and provider seam.
- Last updated: 2026-05-31
- Branch/commit/PR: authored on `feat/forge-cli-search` (worktree
  `~/Project/sympoies/nils-cli-wt/forge-cli-search`); no implementation commits
  yet; no PR opened.
- Source document: docs/plans/2026-05-31-forge-cli-search/forge-cli-search-plan.md
- Direct source-doc execution waiver: not applicable
- Tracking issue: not yet opened
- Source snapshot: to be posted by `create-plan-tracking-issue` at issue open
- Plan snapshot: to be posted by `create-plan-tracking-issue` at issue open
- Initial state snapshot: to be posted by `create-plan-tracking-issue` at issue
  open

## Validation Plan

- Sprint 1: `cargo test -p nils-forge-cli` seam cases (forced GitLab / Local
  search returns `provider_unsupported`, not an empty result), `search issues`
  / `search prs` argv-build cases (`gh search <kind> <query> --repo --match
  title,body,comments --limit --json`, plus `--repo` override), and JSON-parse
  cases (normalized `SearchItem`, incl. a body-only hit and an empty result);
  golden fixtures for both envelopes.
- Sprint 2: `cargo test -p nils-forge-cli` `refs-to` ref-string parsing (URL /
  `owner/name#n` / `#n`), graphql-argv build, and parse cases; golden fixture
  for `cli.forge-cli.search.refs-to.v1`; `--help` / completion snapshot
  updated; spec / ops docs updated; clippy `-D warnings`, fmt, `rumdl` clean;
  `gh pr checks` green; release published and Homebrew tap bumped.
- Cross-cutting: every executed task populates its `Evidence` cell; waived
  tasks are marked `waived` with a reason. The closeout comment is preceded by
  a final `tracking run update --note "<closing summary>"` event.

## Task Ledger

| ID | Status | Task | Evidence | Notes |
| --- | --- | --- | --- | --- |
| 1.1 | todo | `search` CLI subtree (`issues` / `prs`) + provider seam | — | No deps. Add `Search` command + arg structs in `cli.rs`, dispatch in `lib.rs`; GitLab / Local → structured `provider_unsupported` (explicit, not silent). PR1. |
| 1.2 | todo | GitHub `search issues` / `search prs` ops (B) | — | Depends on 1.1. Shared `SearchItem`; `gh search <kind> <query> --repo --match title,body,comments --limit --json`; normalized + versioned envelope; `--dry-run` argv parity; single-repo via `push_repo_override`. PR1 with 1.1. |
| 2.1 | todo | GitHub `search refs-to <ref>` op (A) | — | Depends on 1.1, 1.2. Parse URL / `owner/name#n` / `#n`; `gh api graphql` CROSS_REFERENCED_EVENT; normalize referencing sources to `SearchItem`; versioned envelope; GitHub-only. PR2. |
| 2.2 | todo | Docs, `--help` / completion snapshot, and release | — | Depends on 2.1. Document the three envelopes in `forge-cli-spec-v1.md` / `forge-cli-ops-v1.yaml` and the `list` vs `search` vs `inbox` role split; golden fixtures; PR self-gated via `gh pr checks`; release + tap bump. |

## Session Log

- 2026-05-31: Authored this bundle (discussion-source + plan +
  execution-state) for the `forge-cli` issue / PR search surface. Feasibility
  findings: `forge-cli` has no search dimension today — `issue list` / `pr
  list` filter structured fields only and `issue/pr view` are by id
  (`crates/forge-cli` command tree); GitHub exposes the two needed primitives
  (`gh search issues|prs --match title,body,comments`, and `timelineItems`
  CROSS_REFERENCED_EVENT via `gh api graphql`); the provider seam already
  exists (`Provider {GitHub, GitLab, Local}`, the gh-style `Local` file
  backend, and the `build_call` / `parse_output` / `emit_success` op pattern in
  `issue_list.rs`). Decisions: implement in `crates/forge-cli` (not
  `plan-archive` — different domain); a top-level `search` group with `issues`
  / `prs` (B) + `refs-to` (A); GitHub-only in v1 behind the seam with GitLab /
  Local returning `provider_unsupported`; `refs-to` via cross-reference events
  (semantically correct over text-mention); single-repo scope only. No
  implementation started; this state is prepared so `create-plan-tracking-issue`
  can open the tracker. Authored in an isolated worktree off `main` to avoid
  disturbing the shared `nils-cli` checkout.

## Validation

| Command | Status | Summary | Artifact |
| --- | --- | --- | --- |
| `cargo test -p nils-forge-cli` | pending | Not run; no implementation yet. | — |
| `cargo clippy -p nils-forge-cli --all-targets -- -D warnings` | pending | Not run; no implementation yet. | — |
| `cargo fmt -p nils-forge-cli -- --check` | pending | Not run; no implementation yet. | — |
| `--help` / completion snapshot | pending | Not run; no implementation yet. | — |
| `rumdl check` (this bundle) | pending | To run on the authored Markdown before delivery. | — |
| `gh pr checks` | pending | No PR yet. | — |

## Notes

- The three `search` subcommands overlap with `list` and `inbox` by intent but
  serve distinct roles: `list` is structured-field filtering, `search` is
  full-text / reverse-reference query, `inbox` is the personal work queue. The
  role split must be documented in command help to avoid "which do I use"
  confusion.
- v1 is GitHub-only on purpose. GitLab and the `Local` file backend return
  `provider_unsupported`; both are documented Future Work. The `--provider
  local` file store is the natural home for the no-platform, pure-file search
  product.
- Authored in worktree `~/Project/sympoies/nils-cli-wt/forge-cli-search` on
  branch `feat/forge-cli-search`; the shared `nils-cli` main checkout was not
  disturbed.
