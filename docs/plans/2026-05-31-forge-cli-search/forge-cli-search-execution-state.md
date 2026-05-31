# forge-cli Issue / PR Search Execution State

<!-- plan-issue-record:v2 role=state profile=tracking -->
## Execution State

- Status: complete; tracking issue #716 closed on 2026-05-31 after merged
  PRs #722 and #723 and the v0.31.7 release.
- Target scope: `crates/forge-cli` (`nils-forge-cli`) `search` surface in
  `sympoies/nils-cli` — top-level `search issues` / `search prs` (full-text,
  B) + `search refs-to` (cross-reference, A), GitHub-only behind the provider
  seam, single-repo.
- Execution window: Sprint 1 (`search` subtree + seam + GitHub full-text,
  PR1) -> Sprint 2 (`search refs-to` + docs + delivery, PR2), completed
  serially.
- Current task: complete.
- Next task: archive the closed plan bundle.
- Last updated: 2026-05-31
- Branch/commit/PR: delivered through PR sympoies/nils-cli#722
  (`b6121ad`) and PR sympoies/nils-cli#723 (`4506b3c`); released as
  nils-cli v0.31.7.
- Source document: docs/plans/2026-05-31-forge-cli-search/forge-cli-search-plan.md
- Direct source-doc execution waiver: not applicable
- Tracking issue: <https://github.com/sympoies/nils-cli/issues/716>
- Source snapshot:
  <https://github.com/sympoies/nils-cli/issues/716#issuecomment-4586133498>
- Plan snapshot:
  <https://github.com/sympoies/nils-cli/issues/716#issuecomment-4586133560>
- Initial state snapshot:
  <https://github.com/sympoies/nils-cli/issues/716#issuecomment-4587045156>

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
| 1.1 | done | `search` CLI subtree (`issues` / `prs`) + provider seam | PR sympoies/nils-cli#722 (merged b6121ad); cargo test -p nils-forge-cli green, local-fast passed | No deps. Add `Search` command + arg structs in `cli.rs`, dispatch in `lib.rs`; GitLab / Local → structured `provider_unsupported` (explicit, not silent). PR1. |
| 1.2 | done | GitHub `search issues` / `search prs` ops (B) | PR sympoies/nils-cli#722 (merged b6121ad); search.issues.v1/search.prs.v1 envelopes + golden integration tests | Depends on 1.1. Shared `SearchItem`; `gh search <kind> <query> --repo --match title,body,comments --limit --json`; normalized + versioned envelope; `--dry-run` argv parity; single-repo via `push_repo_override`. PR1 with 1.1. |
| 2.1 | done | GitHub `search refs-to <ref>` op (A) | Sprint 2 / PR2: search refs-to graphql op (CROSS_REFERENCED_EVENT), ref parsing (URL/owner-name#n/#n/n), unit + golden integration tests | Depends on 1.1, 1.2. Parse URL / `owner/name#n` / `#n`; `gh api graphql` CROSS_REFERENCED_EVENT; normalize referencing sources to `SearchItem`; versioned envelope; GitHub-only. PR2. |
| 2.2 | done | Docs, `--help` / completion snapshot, and release | PR sympoies/nils-cli#723 merged `4506b3c`; closeout linked release v0.31.7 (#724, tag v0.31.7). | Depends on 2.1. Documented the three envelopes in `forge-cli-spec-v1.md` / `forge-cli-ops-v1.yaml` and the `list` vs `search` vs `inbox` role split; golden fixtures; PR self-gated via `gh pr checks`; release + tap bump completed. |

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
- 2026-05-31: Tracking issue #716 was completed and closed. Lifecycle audit
  found source, plan, state, session, validation, review, and closeout records
  visible and complete. Delivery landed through PR #722 and PR #723, followed
  by release v0.31.7.

## Validation

| Command | Status | Summary | Artifact |
| --- | --- | --- | --- |
| `cargo test -p nils-forge-cli` | pass | Covered by PR #722 / #723 validation and issue #716 validation record. | <https://github.com/sympoies/nils-cli/issues/716#issuecomment-4586929592> |
| `cargo clippy -p nils-forge-cli --all-targets -- -D warnings` | pass | Covered by PR #722 / #723 validation and issue #716 validation record. | <https://github.com/sympoies/nils-cli/issues/716#issuecomment-4586929592> |
| `cargo fmt -p nils-forge-cli -- --check` | pass | Covered by local-fast validation and merged PR checks. | <https://github.com/sympoies/nils-cli/issues/716#issuecomment-4586929592> |
| `--help` / completion snapshot | pass | Search help/completion surfaces were updated and merged in PR #723. | <https://github.com/sympoies/nils-cli/pull/723> |
| `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast` | pass | Issue #716 validation record shows the local-fast gate passed. | <https://github.com/sympoies/nils-cli/issues/716#issuecomment-4586929592> |
| `gh pr checks` | pass | Closeout records PR #722 and PR #723 checks as passing. | <https://github.com/sympoies/nils-cli/issues/716#issuecomment-4587049026> |

## Closeout

- Status: complete.
- Closed issue:
  <https://github.com/sympoies/nils-cli/issues/716>
- Closeout comment:
  <https://github.com/sympoies/nils-cli/issues/716#issuecomment-4587049026>
- Review decision: approve at
  <https://github.com/sympoies/nils-cli/issues/716#issuecomment-4587045211>.
- Final validation:
  <https://github.com/sympoies/nils-cli/issues/716#issuecomment-4586929592>.

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
