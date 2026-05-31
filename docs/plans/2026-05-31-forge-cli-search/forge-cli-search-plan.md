# Plan: forge-cli Issue / PR Search

## Overview

Give `forge-cli` a first-class `search` surface over live forge issues / PRs,
covering two sub-needs that `plan-archive` already serves for archived plans:
full-text search (B) and reverse "what references this ref" lookup (A). Today
`forge-cli` can filter issues / PRs by structured fields only and has no
free-text or cross-reference query; `gh` exposes both primitives
(`gh search issues|prs --match`, and `timelineItems` CROSS_REFERENCED_EVENT via
`gh api graphql`). Unlike `plan-archive`, which queries a local snapshot
corpus, `forge-cli` delegates to these live provider primitives and builds no
index.

This plan adds a top-level `forge-cli search` group with three subcommands —
`search issues`, `search prs` (B), and `search refs-to` (A) — implemented for
GitHub only in v1 behind the existing provider seam, single-repo scoped,
sequenced as two PRs. All work is in `crates/forge-cli`; no change to existing
ops, provider detection, or auth.

## Read First

- Primary source:
  `docs/plans/2026-05-31-forge-cli-search/forge-cli-search-discussion-source.md`
- Source type: discussion-to-implementation-doc
- Repo anchors:
  - `crates/forge-cli/src/ops/issue_list.rs` (the `build_call` / `parse_output`
    / `emit_success` / `render_text` op pattern to mirror)
  - `crates/forge-cli/src/provider.rs` (`Provider`, `ProviderContext`,
    `push_repo_override`, `detect`)
  - `crates/forge-cli/src/backend.rs` (`BackendProgram`, `BackendRunner`,
    `DryRunPayload`)
  - `crates/forge-cli/src/cli.rs` (command surface), `lib.rs` (dispatch),
    `envelope.rs` (`emit_success`)
  - `crates/forge-cli/src/local/` (file backend; future search home)
  - `crates/forge-cli/docs/specs/forge-cli-spec-v1.md`,
    `forge-cli-ops-v1.yaml` (released contract docs)
  - `crates/forge-cli/tests/` (integration tests + completion snapshot)
- Key decisions carried into execution:
  - Implement in `crates/forge-cli`; do not extend `plan-archive` (different
    domain: live forge vs archived plans).
  - Top-level `search` group: `search issues`, `search prs` (B), `search
    refs-to` (A); mirrors `gh search` and gives mixed-result `refs-to` a home.
  - GitHub-only in v1 behind the provider seam; GitLab / Local return
    structured `provider_unsupported` (explicit, never a silent empty result).
  - B → `gh search issues|prs <query> --repo <slug> --match title,body,comments
    --limit <n> --json ...`; A → `gh api graphql` CROSS_REFERENCED_EVENT.
  - Single-repo scope only; one shared `SearchItem` shape; three versioned
    envelopes.
- Open questions carried into execution: none. Release cadence (one release
  after PR2 vs a release after each PR) follows the default single release
  after PR2; revisit at delivery only if PR1 is independently useful.

## Scope

- In scope:
  - **Sprint 1**: `search` CLI subtree + provider seam + GitHub `search issues`
    / `search prs` (B), single-repo, normalized + versioned.
  - **Sprint 2**: GitHub `search refs-to` (A) via cross-reference events +
    spec / ops docs + delivery.
- Out of scope (all Future Work): GitLab and Local backends; cross-repo / org /
  global `--scope`; ranking / sort beyond provider default; text-mention
  fallback for `refs-to`; any persistent index or cache.

## Assumptions

1. `gh search` and `gh api graphql` are available and authenticated wherever
   the forge-cli GitHub backend already runs; no new binary dependency.
2. Single-repo scope via `--repo` / remote detection is sufficient for v1;
   cross-repo is Future Work.
3. `cargo test`, `cargo clippy`, `cargo fmt`, the `--help` / completion
   snapshot, golden fixtures, and `rumdl` remain the gating validation surface,
   self-checked via `gh pr checks` before merge.
4. Returning `provider_unsupported` for GitLab / Local in v1 is acceptable —
   the seam is explicit and the follow-ups are documented.

## Sprint 1: `search` Surface And GitHub Full-Text (B)

**Goal**: Stand up the `search` command group and the provider seam, and ship
GitHub full-text `search issues` / `search prs`.

**PR grouping intent**: group (PR1)
**Execution Profile**: serial

### Task 1.1: `search` CLI subtree and provider seam

- **Location**:
  - `crates/forge-cli/src/cli.rs`
  - `crates/forge-cli/src/lib.rs`
- **Description**: Add a top-level `Search` command with `issues <query>` and
  `prs <query>` subcommands and their arg structs (`--match`, `--limit`,
  `--repo` via the global flag, `--format`). Wire dispatch in `lib.rs`. Route
  GitLab and Local providers to a structured `ForgeError::provider_unsupported`
  with a clear "search is GitHub-only in v1" message. (`refs-to` is added in
  Sprint 2.)
- **Dependencies**:
  - none
- **Complexity**: 2
- **Acceptance criteria**:
  - `forge-cli search issues --help` and `search prs --help` render; a forced
    `--provider gitlab` (or `local`) search returns `provider_unsupported`
    (unit test on the seam), not a silent empty result.
- **Validation**:
  - `cargo test -p nils-forge-cli` seam + arg-parse cases.

### Task 1.2: GitHub `search issues` / `search prs` ops (B)

- **Location**:
  - `crates/forge-cli/src/ops/search_issues.rs`,
    `crates/forge-cli/src/ops/search_prs.rs` (or a `search/` submodule)
  - `crates/forge-cli/src/ops/mod.rs`
- **Description**: Add the shared normalized `SearchItem` (`kind`, `number`,
  `url`, `title`, `state`, `repo`, `matched_field`) and the two GitHub ops.
  `build_<verb>_call` constructs `gh search <issues|prs> <query> --repo <slug>
  --match <fields> --limit <n> --json <fields>` (default `--match
  title,body,comments`); `parse_<verb>_output` normalizes the JSON; emit a
  versioned envelope (`cli.forge-cli.search.issues.v1` /
  `...search.prs.v1`). `--dry-run` renders the exact argv via `DryRunPayload`.
  Reuse `ProviderContext::push_repo_override` for single-repo scoping.
- **Dependencies**:
  - Task 1.1
- **Complexity**: 3
- **Acceptance criteria**:
  - `search issues "<term>" --format json` returns normalized hits within the
    current repo, including a body-only match; `--dry-run` renders the
    `gh search issues ...` plan; an empty result is a well-formed empty
    envelope. Same for `search prs`.
- **Validation**:
  - `cargo test -p nils-forge-cli` argv-build + parse cases; golden fixtures
    for both envelopes.

## Sprint 2: GitHub Reverse Reference (A) And Delivery

**Goal**: Add `search refs-to` on cross-reference events, document the surface,
then ship.

**PR grouping intent**: group (PR2)
**Execution Profile**: serial

### Task 2.1: GitHub `search refs-to <ref>` op (A)

- **Location**:
  - `crates/forge-cli/src/ops/search_refs_to.rs`
  - `crates/forge-cli/src/cli.rs` (add the `RefsTo` subcommand)
- **Description**: Add `search refs-to <ref>`. Parse `<ref>` as a GitHub URL,
  `owner/name#number`, or `#number` (owner/name from the provider context).
  Build a `gh api graphql` call over `repository(owner,name).issueOrPullRequest(
  number).timelineItems(itemTypes:[CROSS_REFERENCED_EVENT], first:<n>)` and
  normalize the referencing sources into `SearchItem`s. Emit
  `cli.forge-cli.search.refs-to.v1` (text default, `--format json`);
  `--dry-run` renders the graphql argv. GitHub-only; GitLab / Local hit the
  Task 1.1 seam.
- **Dependencies**:
  - Task 1.1, Task 1.2 (shared `SearchItem`)
- **Complexity**: 3
- **Acceptance criteria**:
  - `search refs-to <url-or-#n> --format json` returns the issues / PRs that
    reference the ref (e.g. the closing PR appears in the issue's result);
    ref-string parsing covers URL / `owner/name#n` / `#n`; `--dry-run` renders
    the graphql plan; GitLab / Local return `provider_unsupported`.
- **Validation**:
  - `cargo test -p nils-forge-cli` ref-parse + graphql-argv + parse cases;
    golden fixture for the envelope.

### Task 2.2: Docs, snapshot, and release

- **Location**:
  - `crates/forge-cli/docs/specs/forge-cli-spec-v1.md`,
    `crates/forge-cli/docs/specs/forge-cli-ops-v1.yaml`
  - `crates/forge-cli/tests/`
- **Description**: Document the three `search` envelopes in the spec / ops
  contract and the `list` vs `search` vs `inbox` role split in command help;
  update / add the `--help` / completion snapshot; run `cargo test` / clippy /
  fmt / `rumdl` and the relevant CI gates (self-gated via `gh pr checks`); cut
  the release and bump the Homebrew tap.
- **Dependencies**:
  - Task 2.1
- **Complexity**: 2
- **Acceptance criteria**:
  - All gates pass; the release ships and the tap is bumped; spec / ops docs
    and help document the surface and the role split.
- **Validation**:
  - `gh pr checks` green; release published; `brew` resolves the new version.

## Issue Closeout Gate

The tracking issue is complete when:

- `forge-cli search issues` / `search prs` return normalized, versioned hits
  scoped to the current repo, matching `title,body,comments` by default and
  narrowable via `--match`; a body-only term the structured `issue list`
  cannot surface is matched.
- `forge-cli search refs-to <ref>` returns the issues / PRs that reference the
  ref via cross-reference events, with URL / `owner/name#n` / `#n` parsing.
- All three emit documented, versioned envelopes (text + `--format json`) and
  `--dry-run` renders the exact backend argv; an empty result is a well-formed
  empty envelope.
- GitLab and Local return a structured `provider_unsupported` v1 error, not a
  silent empty result; the seam has a unit test.
- The `list` vs `search` vs `inbox` role split is documented in help, and the
  three envelopes are documented in `forge-cli-spec-v1.md` /
  `forge-cli-ops-v1.yaml` with golden fixtures.
- `cargo test -p nils-forge-cli`, clippy `-D warnings`, fmt, `rumdl`, and the
  `--help` / completion snapshot are green; `gh pr checks` is green.
- The release ships and the Homebrew tap is bumped.
- The `execution-state.md` ledger has every executed row at `done` with a
  non-empty `Evidence` cell; waived rows are marked `waived` with a reason.
- The closeout comment is preceded by a final
  `tracking run update --note "<closing summary>"` event.

## Future Work (Out Of Scope For This Tracker)

- GitLab backend: full-text via the search API (`in=title,description`),
  comments / cross-references via Advanced Search where available.
- Local file-store search backend — the no-platform, pure-file search product
  on `--provider local`.
- Cross-repo / org / global `--scope` (GitHub search supports org / global
  scoping; GitLab needs group scope).
- Ranking / sort beyond provider default, and a text-mention fallback for
  `refs-to` when cross-reference events miss a raw-URL-only mention.
- A persistent search cache or index if live provider latency / rate limits
  warrant it.

## Retention Intent

Plan-source coordination document. Cleanup-eligible after the search surface
ships and the tracker closes and archives.
