# forge-cli Issue / PR Search — Implementation Handoff

- Status: decisions settled; ready for plan generation.
- Date: 2026-05-31
- Source: a feasibility discussion comparing `plan-archive`'s bidirectional
  query surface (`catalog --grep` / `--deep` / `--refs-to` and `search`,
  backed by a local snapshot corpus) against `forge-cli`, which today has no
  search dimension at all. The goal is a live-forge equivalent of two
  plan-archive capabilities: full-text issue / PR search (the metadata ↔
  body / comment axis) and reverse "what references this ref" lookup (the
  ref ↔ owner axis). All work is in `crates/forge-cli` (`nils-forge-cli`).
- Intended next step: generate the plan bundle under
  `docs/plans/2026-05-31-forge-cli-search/`. This is a source artifact, not an
  implementation plan.

## Execution

- Recommended plan: docs/plans/2026-05-31-forge-cli-search/forge-cli-search-plan.md
- Recommended execution state: docs/plans/2026-05-31-forge-cli-search/forge-cli-search-execution-state.md
- Status: decisions settled; plan generation is the next step.
- Next-task source: this document

## Purpose

Give `forge-cli` a first-class search surface over live forge issues / PRs,
covering two sub-needs that `plan-archive` already serves for archived plans:

- **B — full-text search**: find issues / PRs by a term in their title, body,
  or comments. `forge-cli` can filter by structured fields (`issue list`
  `--state` / `--label` / `--author` / `--assignee`) but cannot match free
  text, so "which issue mentioned X" has no answer today.
- **A — reverse reference lookup**: given an issue / PR / MR, find the issues
  and PRs that reference it. This is the live-forge analog of
  `plan-archive catalog --refs-to <url>`.

The key architectural difference from `plan-archive`: `plan-archive`'s
bidirectionality comes from a local, scrubbed snapshot corpus it owns and
indexes. `forge-cli` is a stateless provider-neutral wrapper over `gh` / `glab`,
so search must **delegate to live provider primitives**, not build a local
index. v1 builds no corpus and stays GitHub-only behind the existing provider
seam.

## Confirmed Facts (current crate behaviour)

- [F1] `forge-cli` is a provider-neutral wrapper. `Provider` is a three-variant
  enum `{GitHub, GitLab, Local}` (`crates/forge-cli/src/provider.rs`) mapped
  1:1 to `BackendProgram {Gh, Glab, Local}`
  (`crates/forge-cli/src/backend.rs`). `Local` is an in-process, file-backed
  backend (`crates/forge-cli/src/local/`) that rides the GitHub gh-style argv
  paths but is served by `LocalRunner` against a local store
  (`--provider local` / `FORGE_CLI_LOCAL_STORE`).
- [F2] No search surface exists. The command tree is
  `pr / issue / label / inbox / repo / auth / completion`. `issue list` filters
  by `--state` / `--label` / `--author` / `--assignee` / `--limit`; `pr list`
  by `--state` / `--author` / `--head` / `--limit`; neither matches free text.
  `issue view` / `pr view` fetch a single record by numeric id only.
- [F3] Every op follows one shape (`crates/forge-cli/src/ops/issue_list.rs`):
  `build_<verb>_call` constructs provider-specific argv, `BackendRunner::run`
  invokes the backend (or `LocalRunner`), `parse_<verb>_output` parses backend
  JSON into a normalized `serde::Serialize` payload, and `emit_success` emits a
  versioned envelope (`cli.forge-cli.<noun>.<verb>.v1`). `--dry-run`
  short-circuits to a `DryRunPayload` carrying the exact argv plan.
  `ProviderContext::push_repo_override` injects `--repo owner/name` so dry-run
  and live calls hit the same repo.
- [F4] GitHub exposes the full-text primitive that B maps onto: `gh search
  issues` / `gh search prs` support `--match title,body,comments` field
  restriction, `--repo OWNER/REPO` single-repo scoping, `--limit`, and
  `--json` structured output.
- [F5] GitHub exposes the primitive that A maps onto: cross-reference events.
  The GraphQL `issueOrPullRequest(number).timelineItems(itemTypes:
  [CROSS_REFERENCED_EVENT])` field, reachable via `gh api graphql`, returns the
  source issues / PRs that reference a given ref — including `#123`-style
  auto-links, not just raw URL mentions.
- [F6] GitLab has no clean equivalent for either need at the basic tier: issue
  / MR search covers `in=title,description` but comment (notes) full-text and
  cross-reference enumeration require Advanced Search (Elasticsearch, paid
  tier). The `Local` file backend has no search path yet.

## Decisions

1. Implement in `crates/forge-cli` (`nils-forge-cli`). Search is forge-cli's
   own missing dimension; there is no other home. Do **not** extend
   `plan-archive` — its corpus is archived plan history, a different domain
   from live forge state, and the two must not be conflated.
2. Add a new top-level `forge-cli search` command group, mirroring `gh
   search`, with three subcommands:
   - `search issues <query>` — full-text issue search (B).
   - `search prs <query>` — full-text PR search (B).
   - `search refs-to <ref>` — issues / PRs that reference the ref (A).
   A top-level group (rather than `issue search` / `pr search`) gives the
   mixed-result reverse lookup (`refs-to` returns both issues and PRs) a
   natural home and keeps the surface aligned with `gh search`.
3. GitHub backend only in v1, implemented behind the existing provider seam.
   GitLab and Local branches return a structured `provider_unsupported` error
   (an explicit "not implemented in v1", never a silent empty result) so the
   seam is real and testable and the follow-ups are obvious. The existing
   `--provider local` file store is the natural future home for a
   no-platform, pure-file search backend.
4. B maps to `gh search <issues|prs> <query> --repo <slug> --match
   <fields> --limit <n> --json <fields>`. Default `--match` is
   `title,body,comments` (parity with `plan-archive search`, which covers body
   and comments); a `--match` flag narrows it.
5. A maps to `gh api graphql` over `repository(owner,name).issueOrPullRequest(
   number).timelineItems(itemTypes:[CROSS_REFERENCED_EVENT])`. The `<ref>`
   argument accepts a full GitHub URL, `owner/name#number`, or `#number`
   (owner/name resolved from the provider context). Cross-reference events are
   chosen over text-mention search because they are semantically correct
   (they capture `#123` auto-links and exclude incidental string matches),
   matching the "do it correctly" requirement.
6. Single-repo scope only in v1, via the existing `--repo` / remote-detection
   model and `gh`'s `--repo`. No `--scope org|global`; cross-repo is explicit
   Future Work. Correctness over breadth.
7. One shared normalized `SearchItem` shape — `{kind: issue|pr, number, url,
   title, state, repo, matched_field?}` — is reused across the three
   subcommands, each with its own versioned envelope
   (`cli.forge-cli.search.issues.v1`, `...search.prs.v1`,
   `...search.refs-to.v1`).

## Scope

- In scope: the top-level `search` group (`issues` / `prs` / `refs-to`); the
  GitHub backend for all three; single-repo scoping via `--repo` / detection;
  the shared normalized `SearchItem` and three versioned envelopes;
  `--dry-run` argv-plan parity; the provider seam returning structured
  `provider_unsupported` for GitLab / Local; integration tests, the `--help` /
  completion snapshot, golden fixtures, and the `forge-cli-spec-v1.md` /
  `forge-cli-ops-v1.yaml` updates; the release and Homebrew tap bump.
- Out of scope: GitLab and Local backend implementations (seam only,
  follow-up); cross-repo / org / global scope; ranking or sorting beyond the
  provider default; a text-mention fallback for `refs-to`; any persistent
  search index or cache.

## Non-Scope

- Changing the existing `pr` / `issue` / `inbox` ops or provider detection /
  auth.
- Building a local snapshot corpus or any shared surface with `plan-archive`;
  live forge and archived plans are different domains.
- Any non-`forge-cli` nils-cli crate.

## Implementation Boundaries

- New op modules under `crates/forge-cli/src/ops/` (e.g. `search_issues.rs`,
  `search_prs.rs`, `search_refs_to.rs`, or a `search/` submodule) follow the
  `build_<verb>_call` + `parse_<verb>_output` + `emit_success` + `render_text`
  pattern established by `issue_list.rs`.
- A new CLI subtree in `crates/forge-cli/src/cli.rs` adds `Search { Issues,
  Prs, RefsTo }` with their arg structs; dispatch is wired in `lib.rs`.
- The GitHub branch is live; GitLab and Local branches return
  `ForgeError::provider_unsupported` with a clear message. The seam is
  explicit and unit-testable, not a silent no-op.
- Single-repo scoping reuses `ProviderContext::push_repo_override` and `gh`'s
  `--repo`.
- The three envelopes are released contracts: version them, add golden
  fixtures, and document them in `forge-cli-spec-v1.md` and
  `forge-cli-ops-v1.yaml`.
- Delivery follows the nils-cli flow: PR (self-gated via `gh pr checks`) →
  release → Homebrew tap.

## Requirements

1. `forge-cli search issues <query>` returns normalized issue hits scoped to
   the resolved repo, matching `title,body,comments` by default and narrowable
   via `--match`.
2. `forge-cli search prs <query>` does the same for pull requests.
3. `forge-cli search refs-to <ref>` returns the issues / PRs that
   cross-reference the ref via GitHub cross-reference events; `<ref>` accepts a
   URL, `owner/name#number`, or `#number`.
4. All three emit a documented, versioned envelope (text default, `--format
   json`), and `--dry-run` renders the exact backend argv plan.
5. GitLab and Local return a structured `provider_unsupported` v1 error rather
   than a silent empty result, keeping the seam explicit.
6. Integration tests, the `--help` / completion snapshot, golden fixtures, and
   the spec / ops docs cover the new surface.

## Acceptance Criteria

- `search issues "<term>" --format json` returns hits whose title, body, or
  comments contain the term within the current repo — including a term that
  appears only in a body, which the structured `issue list` filters cannot
  surface today.
- `search prs "<term>"` does the same for pull requests.
- `search refs-to <url-or-#n>` returns the set of issues / PRs that reference
  the ref (for example, the PR that closes an issue appears in that issue's
  result), GitHub-only; GitLab and Local emit `provider_unsupported`.
- `--dry-run` prints the exact `gh search ...` and `gh api graphql ...` argv
  plans.
- `cargo test -p nils-forge-cli`, clippy `-D warnings`, fmt, `rumdl`, and the
  `--help` / completion snapshot all pass; `gh pr checks` is green.

## Risks And Guardrails

- The provider parity gap (GitLab / Local unimplemented) could read as a bug.
  Guardrail: return an explicit `provider_unsupported` with a clear message and
  document the follow-up; never silently return an empty result.
- "search vs list vs inbox" confusion on overlapping surfaces. Guardrail:
  document the role split in command help — `list` is structured-field
  filtering, `search` is full-text / reverse-reference, `inbox` is the personal
  work queue.
- The `refs-to` GraphQL shape is GitHub-specific and brittle. Guardrail: pin
  the query, add a golden fixture, and version the envelope.
- Scope creep (cross-repo, ranking, text-mention fallback). Guardrail: v1 is
  single-repo, provider-default ordering, cross-reference-events-only;
  everything else is Future Work.

## Validation Plan

- `cargo test -p nils-forge-cli` (argv-build unit cases, JSON-parse cases, and
  integration with a fixtured `BackendRunner`), `cargo clippy`, `cargo fmt`.
- `--help` / completion snapshot updated and asserted; golden fixtures for the
  three envelope shapes.
- `rumdl check` on changed Markdown.
- The nils-cli CI gates relevant to a changed crate, self-checked via `gh pr
  checks` before merge.
- Smoke on a real GitHub repo (e.g. `sympoies/nils-cli`): the three
  subcommands return normalized output and `--dry-run` renders the expected
  argv.

## Read First

- forge-cli contract: `crates/forge-cli/docs/specs/forge-cli-spec-v1.md`,
  `crates/forge-cli/docs/specs/forge-cli-ops-v1.yaml`.
- Op pattern to mirror: `crates/forge-cli/src/ops/issue_list.rs`
  (`build_list_call` / `parse_list_output` / `emit_success` / `render_text`),
  `crates/forge-cli/src/ops/issue_view.rs`.
- Provider seam: `crates/forge-cli/src/provider.rs` (`Provider`,
  `ProviderContext::push_repo_override`, `detect`),
  `crates/forge-cli/src/backend.rs` (`BackendProgram`, `BackendRunner`,
  `DryRunPayload`).
- Local backend (future search home):
  `crates/forge-cli/src/local/{mod,runner,store}.rs`.
- CLI wiring: `crates/forge-cli/src/cli.rs`; dispatch in
  `crates/forge-cli/src/lib.rs`; envelope helper
  `crates/forge-cli/src/envelope.rs`.
- The plan-archive analog this mirrors:
  `docs/plans/plan-archive-body-search/`.

## Retention Intent

Plan-source coordination document. Cleanup-eligible after the search surface
ships and the tracker closes and archives.
