# forge-cli Inbox Implementation Handoff

- Status: ready for plan generation
- Date: 2026-05-22
- Source: user discussion about cross-repo PR / issue visibility, GitHub `gh`
  search behavior, GitLab `glab` live probes, and the current `forge-cli`
  provider-wrapper contract.
- Intended next step: create an implementation plan for `forge-cli inbox` and
  then add an Alfred workflow wrapper after the JSON CLI contract lands.

## Purpose

Add a personal forge work inbox to `forge-cli` so agents, scheduled jobs, and
Alfred can quickly answer which PRs, merge requests, issues, and to-dos need the
user's attention across GitHub and the company GitLab host.

The inbox is not a lifecycle mutation feature. It is a read-only aggregation
surface for work discovery and prioritization.

## Source Tags

- `[U1]` User asked for a fast Alfred view of cross-repo GitHub PRs and issues.
- `[U2]` User added that company work uses GitLab and asked whether both
  providers can be queried together.
- `[U3]` User wants agents to use the CLI for personal work status and future
  scheduled work discovery.
- `[F1]` `crates/forge-cli/docs/specs/forge-cli-spec-v1.md` defines
  `forge-cli` as a provider-neutral `gh` / `glab` subprocess wrapper.
- `[F2]` `crates/forge-cli/src/ops/pr_list.rs` currently implements repo-local
  PR / MR list behavior through `gh pr list` and `glab mr list`.
- `[F3]` `crates/forge-cli/src/cli.rs` currently has PR, issue, repo, auth, and
  completion command groups, with no inbox command group.
- `[A1]` Local `gh search prs` / `gh search issues` help confirms GitHub can
  search PRs and issues across visible repositories with `@me` qualifiers.
- `[A2]` Local `glab 1.99.0` help confirms `glab mr list` and
  `glab issue list` can filter by group or repo, but `glab search` is project
  code search rather than a personal cross-project inbox.
- `[A3]` Local `glab auth status` confirms authenticated access to
  `gitlab.gamania.com` as `terrylin`; `glab api user` reports user id `1435`
  and username `terrylin`.
- `[A4]` Read-only GitLab probes for assigned MRs, review-requested MRs,
  assigned issues, authored MRs / issues, and pending to-dos returned empty
  samples during the discussion.
- `[I1]` Inference from `[F1]` and `[F2]`: personal cross-repo work discovery
  needs a separate command surface from repo-local lifecycle list operations.
- `[I2]` Inference from `[U3]`: read-only discovery should land before any
  automated action or mutation model.
- `[I3]` Inference from `[U1]` and `[U3]`: CLI JSON should be the durable
  contract, while Alfred should remain a consumer.
- `[I4]` Inference from `[A3]`: non-repo agent and scheduler runs need explicit
  GitLab host selection or discovery.
- `[I5]` Inference from `[A2]`: GitLab todos should use host-aware API calls,
  not host-ambiguous high-level todo commands.

## Confirmed Facts

- GitHub already exposes the needed cross-repo search through `gh search prs`
  and `gh search issues`. Useful qualifiers include `--review-requested @me`,
  `--author @me`, `--assignee @me`, `--involves @me`, `--state open`,
  `--sort updated`, and `--order desc`. `[A1]`
- GitLab's high-level `glab mr list` / `glab issue list` is useful for group or
  repo-scoped listing, but it is not equivalent to GitHub's global issue and PR
  search. `[A2]`
- GitLab's REST API is available through `glab api`; for this inbox, it is the
  reliable source for user-scoped cross-project aggregation. `[A2][A3]`
- The local company GitLab identity is `terrylin` on `gitlab.gamania.com`, with
  user id `1435`; implementation must discover these values dynamically instead
  of hardcoding them. `[A3]`
- Current `forge-cli pr list` is a repo-local lifecycle operation. Reusing it
  for cross-repo personal work discovery would blur the existing contract.
  `[F1][F2]`
- Agents and future scheduled jobs need a headless JSON CLI contract, not an
  Alfred-only implementation. `[U3]`

## Decisions

- Implement this as a new top-level `forge-cli inbox` command group, not as a
  separate binary and not as extra flags on `forge-cli pr list` or
  `forge-cli issue list`. `[U3][F1][F2][I1]`
- Keep the first implementation read-only: list, summarize, choose a next item,
  and open/copy URLs through consumers. Do not mutate PRs, issues, MRs, or
  todos in v1. `[U3][I2]`
- Treat the CLI JSON shape as the source of truth for agents and schedulers.
  Alfred should be a thin UI wrapper that consumes `forge-cli inbox --format
  json` output and renders Alfred rows. `[U1][U3][I3]`
- Use provider adapters under `forge-cli`:
  - GitHub adapter: `gh search prs` and `gh search issues`.
  - GitLab adapter: `glab api` for user-scoped endpoints, with optional `glab`
    command fallbacks only when they keep the same normalized contract.
- Add explicit GitLab host handling for company use. The default should discover
  the authenticated GitLab host when possible, with a `--gitlab-host` override
  for non-repo contexts and scheduled jobs. `[A3][I4]`
- Preserve existing `forge-cli` lifecycle semantics. `pr list` and
  `issue view/create/edit/comment/close/reopen` remain repo-local lifecycle
  commands. `[F1][F2]`

## Scope

- Add `forge-cli inbox status` for aggregate counts and stale-work summary.
- Add `forge-cli inbox list` for normalized inbox item rows.
- Add `forge-cli inbox next` for one or more ranked candidate work items.
- Support `--provider github`, `--provider gitlab`, and a combined default that
  queries both available providers.
- Support item kinds:
  - `review`: PRs / MRs requesting user review or approval.
  - `assigned`: PRs / MRs / issues assigned to the user.
  - `todo`: GitLab pending todos and later GitHub notification-style items if a
    stable source is chosen.
  - `authored`: open PRs / MRs / issues authored by the user.
  - `involved`: optional broad GitHub involvement view for user-driven search.
- Return stable JSON for agents and human-readable text for terminal use.
- Keep test fixtures offline by stubbing `gh` / `glab` through the existing
  backend runner pattern.
- Add docs describing command semantics, provider caveats, and agent/scheduler
  use.

## Non-Scope

- Do not implement automatic work execution or mutation in this feature.
- Do not mark GitLab todos as done.
- Do not approve, merge, close, assign, label, or comment on work items.
- Do not replace existing lifecycle commands.
- Do not introduce a direct token store, OAuth flow, or separate authentication
  surface; reuse existing `gh` and `glab` auth state.
- Do not add a raw REST passthrough command.
- Do not require Alfred for agent or scheduler use.

## Implementation Boundaries

- `nils-cli` / `forge-cli` owns provider querying, normalization, ranking, JSON
  output, error handling, and fixture-backed tests.
- `nils-alfredworkflow` owns visual row rendering, query interaction, modifier
  actions, and any Alfred-specific cache behavior.
- Agents and scheduled jobs should call `forge-cli inbox status`, `list`, or
  `next` directly and consume JSON output.
- Provider subprocess calls must continue through the existing backend command
  abstraction where practical so stderr redaction, missing-backend errors, and
  dry-run behavior stay consistent with the rest of `forge-cli`.
- GitLab user identity lookup should happen through `glab api user` per host and
  be cached only within a single invocation unless the later plan explicitly
  adds a cache file.

## Requirements

- `forge-cli inbox list --format json` returns one normalized list covering all
  requested providers and kinds.
- `forge-cli inbox status --format json` returns count summaries grouped by
  provider, host, kind, and reason.
- `forge-cli inbox next --format json` returns a ranked subset suitable for an
  agent to inspect before deciding whether to act.
- Every item includes at least:
  - `provider`
  - `host`
  - `kind`
  - `reason`
  - `repo`
  - `number`
  - `title`
  - `url`
  - `updated_at`
  - `author`
  - `source`
- GitHub rows must be normalized from `gh search prs` and `gh search issues`
  JSON output.
- GitLab rows must be normalized from `glab api` endpoints and GitLab todo
  records.
- Duplicate rows from multiple reasons must collapse into one item with a
  reason list, not appear as separate rows.
- Empty provider results are successful and should report zero counts, not
  runtime failures.
- Provider auth or availability failures must be represented per provider so one
  failing provider does not hide successful results from the other provider.
- The default output must not include secrets, token fragments, or raw backend
  stderr beyond the existing redacted error detail policy.

## Provider Query Sketch

GitHub:

```text
gh search prs --review-requested @me --state open --sort updated --order desc
gh search prs --author @me --state open --sort updated --order desc
gh search prs --assignee @me --state open --sort updated --order desc
gh search issues --assignee @me --state open --sort updated --order desc
gh search issues --author @me --state open --sort updated --order desc
```

GitLab:

```text
glab api user --hostname <host>
glab api 'merge_requests?scope=assigned_to_me&state=opened&order_by=updated_at&sort=desc'
glab api 'merge_requests?reviewer_username=<username>&state=opened&order_by=updated_at&sort=desc'
glab api 'merge_requests?author_id=<user_id>&state=opened&order_by=updated_at&sort=desc'
glab api 'issues?scope=assigned_to_me&state=opened&order_by=updated_at&sort=desc'
glab api 'issues?author_id=<user_id>&state=opened&order_by=updated_at&sort=desc'
glab api 'todos?state=pending&order_by=updated_at&sort=desc'
```

The final implementation plan should verify exact GitLab endpoint parameters
against the target host before coding because self-managed GitLab versions can
lag public GitLab behavior.

## Acceptance Criteria

- `forge-cli inbox --help` shows the new command group and subcommands.
- `forge-cli inbox list --provider github --format json` can parse stubbed
  `gh search prs` and `gh search issues` output.
- `forge-cli inbox list --provider gitlab --format json` can parse stubbed
  `glab api user`, MR, issue, and todo output.
- Combined-provider mode returns partial success with provider-specific warnings
  when one provider is unavailable.
- Empty inbox fixtures produce a successful zero-item response.
- Duplicate item fixtures collapse reasons deterministically.
- `forge-cli inbox next --format json` ranks review-requested work ahead of
  authored or broad involved work unless the plan defines a different explicit
  ranking.
- Existing `forge-cli pr list` and issue lifecycle tests continue to pass
  without behavior changes.
- Docs explain how agents and scheduled jobs should consume the JSON contract.
- An optional `nils-alfredworkflow` follow-up can render the same JSON without
  re-implementing provider queries.

## Validation Plan

- Unit tests for provider command construction and parser normalization.
- Integration tests with stubbed `FORGE_CLI_GH_BIN` and `FORGE_CLI_GLAB_BIN`.
- Fixture lint through `bash scripts/ci/forge-cli-fixture-lint.sh --strict`.
- Targeted Rust tests for the new inbox module.
- `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast` before opening
  or merging the implementation PR.
- Optional live smoke, run manually and recorded separately:
  - `forge-cli inbox status --provider github --format json`
  - `forge-cli inbox status --provider gitlab --gitlab-host gitlab.gamania.com --format json`

## Risks And Guardrails

- GitLab `glab search` is not the right primitive for this inbox; it is project
  code search and should not drive personal work aggregation. `[A2]`
- `glab todo list` does not expose host selection in the observed help output;
  prefer `glab api todos` with `--hostname` for deterministic company-host
  behavior. `[A2][I5]`
- GitHub and GitLab do not share a perfect reason model. Preserve provider
  provenance and normalize only the fields needed for inbox decisions.
- Cross-provider queries can be slow or rate-limited. Add `--limit` and
  provider-level failure reporting before considering persistent caching.
- Future scheduled agents must remain read-only until a separate approval model
  exists for automated mutation.
- The implementation should not hardcode `terrylin`, user id `1435`, or
  `gitlab.gamania.com`; those are live probe examples, not portable defaults.

## Execution

Recommended plan: docs/plans/forge-cli-inbox/forge-cli-inbox-plan.md
Recommended execution state: docs/plans/forge-cli-inbox/forge-cli-inbox-execution-state.md

- Recommended plan type: standard implementation plan.
- Recommended first implementation slice: JSON-only `inbox list` with offline
  GitHub and GitLab fixtures.
- Recommended second slice: `status` / `next` ranking and provider partial
  failure semantics.
- Recommended third slice: docs, completion assets, and optional live smoke
  instructions.
- Alfred workflow integration should be a follow-up in `nils-alfredworkflow`
  after the CLI JSON contract is stable.

## Retention Intent

This document is an execution source artifact. Keep it while `forge-cli inbox`
is planned and implemented. After the plan closes, either delete the plan bundle
as completed coordination material or promote the durable command contract into
`crates/forge-cli/docs/`.

## Open Questions

- Should `inbox next` return exactly one item by default or a short ranked set?
  Default recommendation: return up to five items and let agents pick after
  inspection.
- Should GitHub notifications be included in v1? Default recommendation: no;
  start with explicit search qualifiers and add notifications later only if they
  produce better actionability.
- Should `forge-cli` emit Alfred JSON directly? Default recommendation: no for
  v1; keep Alfred rendering in `nils-alfredworkflow` unless duplicate mapping
  becomes a real maintenance problem.
- Should persistent caching live in `forge-cli` or only in the Alfred wrapper?
  Default recommendation: avoid persistent cache in v1 CLI and use provider
  limits plus scheduler-level cadence.

## Recommended Next Artifact

Create `docs/plans/forge-cli-inbox/forge-cli-inbox-plan.md` from this source and
link this document under that plan's `Read First` section.
