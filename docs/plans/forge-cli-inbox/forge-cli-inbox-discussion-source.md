# forge-cli Inbox Implementation Handoff

- Status: ready for plan generation after pre-implementation review fixes
- Date: 2026-05-22
- Source: user discussion about cross-repo PR / issue visibility, GitHub `gh`
  search behavior, GitLab `glab` live probes, the current `forge-cli`
  provider-wrapper contract, and the pre-implementation specialist review on
  2026-05-22.
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
- `[F4]` `crates/forge-cli/src/provider.rs` currently resolves one
  `ProviderContext` per operation from the global `--provider` flag or git
  remote detection.
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
- `[A5]` Local `glab api --help` confirms non-git-directory API calls default to
  `gitlab.com` unless `--hostname` is supplied.
- `[A6]` Pre-implementation specialist review found five contract gaps:
  combined-provider default versus single-provider detection, GitLab host
  propagation, singular `reason` versus reason-list dedupe, partial-failure
  envelope shape, and pagination / limit semantics.
- `[I1]` Inference from `[F1]` and `[F2]`: personal cross-repo work discovery
  needs a separate command surface from repo-local lifecycle list operations.
- `[I2]` Inference from `[U3]`: read-only discovery should land before any
  automated action or mutation model.
- `[I3]` Inference from `[U1]` and `[U3]`: CLI JSON should be the durable
  contract, while Alfred should remain a consumer.
- `[I4]` Inference from `[A3]` and `[A5]`: non-repo agent and scheduler runs need
  explicit GitLab host selection, and every GitLab API call must receive the
  selected host.
- `[I5]` Inference from `[A2]`: GitLab todos should use host-aware API calls,
  not host-ambiguous high-level todo commands.
- `[I6]` Inference from `[A6]`: `forge-cli inbox` needs an inbox-local
  multi-provider resolver instead of directly reusing the existing single
  `ProviderContext` detection path.

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
- `glab api` supports `--hostname`; without it, non-git-directory calls default
  to `gitlab.com`, which is wrong for company-host scheduled jobs. `[A5]`
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
- Add an inbox-local provider resolver instead of changing existing lifecycle
  command detection:
  - `forge-cli inbox ...` with no `--provider` queries all available providers.
  - `--provider github` or `--provider gitlab` narrows inbox queries to that
    provider.
  - Existing non-inbox commands keep their single-provider `ProviderContext`
    behavior. `[F4][A6][I6]`
- Add `--gitlab-host <host>` as an inbox-only command flag. This is a scoped
  exception to the existing no-global-`--host` lifecycle policy; it does not add
  token handling, auth mutation, or host overrides to `pr`, `issue`, `repo`, or
  `auth` commands. `[F1][A5][I4]`
- Resolve GitLab identity per selected host with `glab api user --hostname
  <host>`, cache it only for the current invocation, and pass `--hostname
  <host>` on every GitLab inbox API call. `[A3][A5][I4]`
- Preserve existing `forge-cli` lifecycle semantics. `pr list` and
  `issue view/create/edit/comment/close/reopen` remain repo-local lifecycle
  commands. `[F1][F2]`
- `inbox next` returns up to five ranked items by default. Agents may inspect the
  returned short list before deciding whether to act. `[U3]`
- GitHub notifications are out of v1; start with explicit search qualifiers and
  add notifications later only if they provide better actionability. `[U1][U3]`
- `forge-cli` does not emit Alfred JSON in v1. Alfred rendering stays in
  `nils-alfredworkflow` unless duplicated mapping becomes a demonstrated
  maintenance problem. `[I3]`
- No persistent cache in v1 CLI. Use provider limits plus scheduler cadence;
  Alfred may add UI-local cache behavior later if needed.

## Scope

- Add `forge-cli inbox status` for aggregate counts and stale-work summary.
- Add `forge-cli inbox list` for normalized inbox item rows.
- Add `forge-cli inbox next` for up to five ranked candidate work items by
  default, with a flag such as `--limit <n>` to request a different bounded
  count.
- Support `--provider github`, `--provider gitlab`, and a combined default that
  queries both available providers.
- Support `--gitlab-host <host>` on inbox commands only.
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
- Add docs describing command semantics, provider caveats, partial-failure
  semantics, limit semantics, and agent/scheduler use.

## Non-Scope

- Do not implement automatic work execution or mutation in this feature.
- Do not mark GitLab todos as done.
- Do not approve, merge, close, assign, label, or comment on work items.
- Do not replace existing lifecycle commands.
- Do not introduce a direct token store, OAuth flow, or separate authentication
  surface; reuse existing `gh` and `glab` auth state.
- Do not add a raw REST passthrough command.
- Do not require Alfred for agent or scheduler use.
- Do not add persistent CLI cache files in v1.
- Do not add a global `--host` flag to `forge-cli`; GitLab host selection is
  inbox-local only.

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
- GitLab user identity lookup should happen through `glab api user --hostname
  <host>` per selected host and be cached only within a single invocation unless
  the later plan explicitly adds a cache file.
- The inbox provider resolver may call multiple provider adapters for one CLI
  invocation. This resolver is local to `inbox` and should not change existing
  `pr`, `issue`, `repo`, or `auth` dispatch.

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
  - `reasons`
  - `repo`
  - `number`
  - `title`
  - `url`
  - `updated_at`
  - `author`
  - `source`
- `reasons` is a deterministic, de-duplicated array of one or more reason
  values. Allowed v1 reason values are `review`, `assigned`, `todo`,
  `authored`, and `involved`. Text output may choose one display reason, but
  JSON must expose the full `reasons` array.
- `source` identifies the provider query family that produced the item, such as
  `github_search_prs`, `github_search_issues`, `gitlab_merge_requests`,
  `gitlab_issues`, or `gitlab_todos`.
- GitHub rows must be normalized from `gh search prs` and `gh search issues`
  JSON output.
- GitLab rows must be normalized from `glab api` endpoints and GitLab todo
  records.
- Duplicate rows from multiple reasons must collapse into one item with a
  deterministic `reasons` array, not appear as separate rows.
- Empty provider results are successful and should report zero counts, not
  runtime failures.
- Provider auth or availability failures must be represented per provider so one
  failing provider does not hide successful results from the other provider.
- Partial success is successful at the process/envelope level:
  - If at least one selected provider returns successfully, exit `0` and emit
    `ok = true`.
  - Include provider-specific warning entries for failed providers.
  - Include provider status entries in `data.providers[]` with `provider`,
    `host`, `ok`, `item_count`, and optional redacted `error`.
  - If all selected providers fail, exit non-zero through the existing
    `ForgeError` / `cli_contract` error path and do not present an empty inbox as
    a successful result.
- The default output must not include secrets, token fragments, or raw backend
  stderr beyond the existing redacted error detail policy.
- The default per-provider limit is `30`, matching the practical defaults of the
  backing CLIs. `--limit <n>` applies per provider and per query family unless
  the implementation plan chooses a narrower flag name.
- Counts in `status` are v1 bounded counts, not guaranteed global exact counts.
  JSON status output must indicate the effective limit and whether each provider
  or reason is `limited`.
- GitLab v1 may read the first page only by default. If pagination is added in
  the first implementation, it must stay bounded by the same effective limit.

## JSON Contract Sketch

The final implementation plan may adjust field ordering, but not these v1
semantics:

```json
{
  "schema_version": "cli.forge-cli.inbox.list.v1",
  "ok": true,
  "data": {
    "providers": [
      {
        "provider": "github",
        "host": "github.com",
        "ok": true,
        "item_count": 3
      },
      {
        "provider": "gitlab",
        "host": "gitlab.gamania.com",
        "ok": false,
        "item_count": 0,
        "error": {
          "kind": "backend_unauthenticated",
          "message": "backend reports authentication required"
        }
      }
    ],
    "limit": 30,
    "items": [
      {
        "provider": "github",
        "host": "github.com",
        "kind": "review",
        "reasons": ["review", "assigned"],
        "repo": "sympoies/nils-cli",
        "number": 440,
        "title": "Fix forge-cli GitHub checks for gh 2.92",
        "url": "https://github.com/sympoies/nils-cli/pull/440",
        "updated_at": "2026-05-22T11:30:29Z",
        "author": "graysurf",
        "source": "github_search_prs"
      }
    ]
  },
  "warnings": [
    {
      "kind": "provider_failed",
      "provider": "gitlab",
      "host": "gitlab.gamania.com",
      "message": "GitLab inbox query failed; GitHub results are still shown."
    }
  ]
}
```

## Provider Query Sketch

GitHub:

```text
gh search prs --review-requested @me --state open --sort updated --order desc --limit <limit> --json number,url,title,updatedAt,author,repository
gh search prs --author @me --state open --sort updated --order desc --limit <limit> --json number,url,title,updatedAt,author,repository
gh search prs --assignee @me --state open --sort updated --order desc --limit <limit> --json number,url,title,updatedAt,author,repository
gh search issues --assignee @me --state open --sort updated --order desc --limit <limit> --json number,url,title,updatedAt,author,repository
gh search issues --author @me --state open --sort updated --order desc --limit <limit> --json number,url,title,updatedAt,author,repository
```

GitLab:

```text
glab api user --hostname <host>
glab api --hostname <host> 'merge_requests?scope=assigned_to_me&state=opened&order_by=updated_at&sort=desc&per_page=<limit>'
glab api --hostname <host> 'merge_requests?reviewer_username=<username>&state=opened&order_by=updated_at&sort=desc&per_page=<limit>'
glab api --hostname <host> 'merge_requests?author_id=<user_id>&state=opened&order_by=updated_at&sort=desc&per_page=<limit>'
glab api --hostname <host> 'issues?scope=assigned_to_me&state=opened&order_by=updated_at&sort=desc&per_page=<limit>'
glab api --hostname <host> 'issues?author_id=<user_id>&state=opened&order_by=updated_at&sort=desc&per_page=<limit>'
glab api --hostname <host> 'todos?state=pending&order_by=updated_at&sort=desc&per_page=<limit>'
```

The final implementation plan should verify exact GitLab endpoint parameters
against the target host before coding because self-managed GitLab versions can
lag public GitLab behavior.

## Acceptance Criteria

- `forge-cli inbox --help` shows the new command group and subcommands.
- `forge-cli inbox list --provider github --format json` can parse stubbed
  `gh search prs` and `gh search issues` output.
- `forge-cli inbox list --provider gitlab --gitlab-host gitlab.gamania.com
  --format json` can parse stubbed `glab api user`, MR, issue, and todo output,
  and every GitLab API invocation includes `--hostname gitlab.gamania.com`.
- Combined-provider mode returns partial success with provider-specific warnings
  when one provider is unavailable.
- All-selected-providers-failed mode exits non-zero through the normal error
  envelope path.
- Empty inbox fixtures produce a successful zero-item response.
- Duplicate item fixtures collapse `reasons` deterministically.
- `forge-cli inbox status --format json` reports bounded counts with the
  effective limit and limited/exactness metadata.
- `forge-cli inbox next --format json` returns up to five items by default and
  ranks review-requested work ahead of authored or broad involved work.
- Existing `forge-cli pr list` and issue lifecycle tests continue to pass
  without behavior changes.
- Existing global `--provider` behavior for non-inbox commands remains
  single-provider.
- Docs explain how agents and scheduled jobs should consume the JSON contract.
- An optional `nils-alfredworkflow` follow-up can render the same JSON without
  re-implementing provider queries.

## Validation Plan

- Unit tests for inbox provider resolver behavior:
  - default selects all available providers;
  - `--provider github` selects GitHub only;
  - `--provider gitlab --gitlab-host <host>` selects GitLab only with the
    explicit host;
  - non-inbox commands still use the existing single-provider resolver.
- Unit tests for provider command construction and parser normalization.
- Integration tests with stubbed `FORGE_CLI_GH_BIN` and `FORGE_CLI_GLAB_BIN`.
- Fixture lint through `bash scripts/ci/forge-cli-fixture-lint.sh --strict`.
- Targeted Rust tests for the new inbox module.
- Contract tests for partial success, all-provider failure, empty results,
  duplicate reason collapse, and bounded status counts.
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
  behavior. `[A2][A5][I5]`
- GitHub and GitLab do not share a perfect reason model. Preserve provider
  provenance and normalize only the fields needed for inbox decisions.
- Cross-provider queries can be slow or rate-limited. Keep bounded provider
  limits in v1 and report provider-level failures before considering persistent
  caching.
- Future scheduled agents must remain read-only until a separate approval model
  exists for automated mutation.
- The implementation should not hardcode `terrylin`, user id `1435`, or
  `gitlab.gamania.com`; those are live probe examples, not portable defaults.
- The inbox-local `--gitlab-host` flag must not become a hidden global host
  override for lifecycle commands.

## Execution

Recommended plan: docs/plans/forge-cli-inbox/forge-cli-inbox-plan.md
Recommended execution state: docs/plans/forge-cli-inbox/forge-cli-inbox-execution-state.md

- Recommended plan type: standard implementation plan.
- Recommended first implementation slice: JSON-only `inbox list` with offline
  GitHub and GitLab fixtures, inbox-local provider resolution, GitLab host
  propagation, partial-success envelope shape, and deterministic `reasons`.
- Recommended second slice: `status` / `next` bounded counts, ranking, and
  all-provider-failure behavior.
- Recommended third slice: docs, completion assets, and optional live smoke
  instructions.
- Alfred workflow integration should be a follow-up in `nils-alfredworkflow`
  after the CLI JSON contract is stable.

## Retention Intent

This document is an execution source artifact. Keep it while `forge-cli inbox`
is planned and implemented. After the plan closes, either delete the plan bundle
as completed coordination material or promote the durable command contract into
`crates/forge-cli/docs/`.

## Closed Questions

- `inbox next` returns up to five items by default, not exactly one.
- GitHub notifications are not included in v1.
- `forge-cli` does not emit Alfred JSON in v1.
- Persistent cache does not live in v1 CLI.
- `reason` is not a singular JSON field; v1 JSON uses deterministic `reasons`.
- Partial success exits `0` with `ok = true` when at least one selected provider
  succeeds; all-provider failure exits non-zero.

## Recommended Next Artifact

Create `docs/plans/forge-cli-inbox/forge-cli-inbox-plan.md` from this source and
link this document under that plan's `Read First` section.
