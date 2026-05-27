# forge-cli Inbox Latency Optimization Implementation Handoff

- Status: ready for plan generation
- Date: 2026-05-22
- Source: user report that `forge-cli inbox` feels slow from the Alfred
  workflow, local dry-run inspection, live timing probes, and current
  `forge-cli` inbox source review.
- Intended next step: create an implementation plan for reducing
  `forge-cli inbox` latency while preserving the current personal-inbox
  semantics.

## Purpose

Reduce the latency of `forge-cli inbox` so provider-specific and mixed-provider
personal work inbox reads are usable from interactive consumers such as Alfred
and from headless agent/scheduler calls.

This work is about query latency and redundant work in the existing personal
inbox implementation. It is not a request to broaden inbox coverage to every
open PR in maintained repositories.

## Source Tags

- `[U1]` User observed that both GitHub and GitLab inbox modes feel slow and
  suspected the original `forge-cli` implementation rather than the Alfred
  workflow wrapper.
- `[U2]` User later confirmed the `sympoies/nils-alfredworkflow#161` coverage
  question does not need handling in this optimization.
- `[F1]` `crates/forge-cli/src/ops/inbox.rs` collects selected providers in a
  serial loop through `collect_inbox`.
- `[F2]` `crates/forge-cli/src/ops/inbox.rs` runs GitHub query families in a
  serial loop through `query_github`.
- `[F3]` `crates/forge-cli/src/ops/inbox.rs` resolves GitLab identity with
  `glab api user --hostname <host>` and then runs GitLab query families in a
  serial loop through `query_gitlab`.
- `[F4]` `crates/forge-cli/src/ops/inbox.rs` defaults inbox reasons to
  `review`, `assigned`, `todo`, and `authored` when no `--kind` is supplied.
- `[F5]` `crates/forge-cli/src/backend.rs` uses `Command::new(...).output()` for
  each backend call, so every query family spawns a new `gh` or `glab`
  subprocess.
- `[F6]` `crates/forge-cli/src/ops/inbox.rs` implements `inbox status` and
  `inbox next` by calling the same collection path used by `inbox list`.
- `[F7]` `crates/forge-cli/src/cli.rs` currently exposes `--kind` as a reason
  filter, not as a PR-vs-issue item-type filter.
- `[A1]` `forge-cli --provider github --format json --dry-run inbox list --limit
  30` reports 5 planned backend calls.
- `[A2]` `forge-cli --provider gitlab --format json --dry-run inbox list
  --gitlab-host gitlab.com --limit 30` reports 7 planned backend calls.
- `[A3]` Live timing on 2026-05-22 with `forge-cli 0.17.2`, `gh 2.92.0`, and
  `glab 1.99.0`: GitHub-only default inbox list took about 6.62s, GitLab-only
  took about 1.37s, and mixed-provider list took about 8.15s.
- `[A4]` Live timing on 2026-05-22: GitHub `--kind review` took about 1.19s,
  while GitHub `--limit 5` default-kind list still took about 6.01s.
- `[I1]` Inference from `[F1]` through `[F5]` and `[A1]` through `[A4]`: the
  dominant cost is serial subprocess/network fan-out, not returned row count or
  Alfred JSON rendering.
- `[I2]` Inference from `[F7]`: Alfred PR-only or issue-only filtering cannot
  avoid unnecessary provider calls until `forge-cli` exposes an item-type filter.

## Confirmed Facts

- GitHub default inbox list currently performs 5 backend calls: review-requested
  PRs, assigned PRs, assigned issues, authored PRs, and authored issues. `[A1]`
- GitLab default inbox list currently performs 7 backend calls: one identity
  lookup, assigned MRs, assigned issues, reviewer MRs, authored MRs, authored
  issues, and pending todos. `[A2]`
- Mixed-provider mode runs both provider adapters in one invocation and currently
  pays roughly the sum of the selected provider costs. `[F1][A3]`
- `--limit` bounds per-query result count but does not reduce the number of query
  families. Lowering GitHub from `--limit 30` to `--limit 5` did not materially
  reduce latency in the local probe. `[A4]`
- Narrowing by reason can reduce backend calls. GitHub `--kind review` ran one
  GitHub search and was much faster than the default-kind query. `[A4]`
- The Alfred workflow is a downstream consumer. Its provider-specific wrappers
  can select GitHub-only or GitLab-only, but they cannot currently tell
  `forge-cli` to avoid PR or issue query families based on Alfred row filtering.
  `[F7][I2]`

## Decisions

- Optimize in `nils-cli` / `forge-cli`, because the measured latency sits in the
  CLI provider adapter path rather than in Alfred row rendering. `[U1][I1]`
- Preserve the current personal-inbox reason semantics by default:
  `review`, `assigned`, `todo`, and `authored` remain the default reason set
  unless a later plan deliberately changes UX defaults. `[F4]`
- Do not fold the `sympoies/nils-alfredworkflow#161` Dependabot PR visibility
  question into this optimization. That is a separate inbox-coverage or
  repo-scope feature, not a latency bug. `[U2]`
- Prefer reducing unnecessary work before adding persistent cache:
  item-type filtering, provider/query parallelism, and status-specific summaries
  should be evaluated before introducing cache invalidation, freshness, or
  durable storage concerns.
- Keep `forge-cli` as a wrapper around `gh` and `glab` for this optimization
  unless a future plan explicitly justifies direct GitHub/GitLab API clients.

## Scope

- Add a CLI-level way to restrict item type, such as PR/MR only, issue only, or
  all items, so consumers can avoid irrelevant provider query families.
- Parallelize independent provider and query-family calls where the backend
  runner contract can support it safely.
- Keep GitLab identity lookup ordered before GitLab queries that require
  username or user id, but avoid blocking unrelated GitHub work on that lookup.
- Preserve dry-run output so callers can still inspect the exact backend argv
  planned for the selected providers, reasons, and item types.
- Update tests and docs for new selection semantics and optimized collection
  behavior.
- Provide live-smoke guidance for latency measurement without making external
  timing a hard CI assertion.

## Non-Scope

- Do not add repo-maintainer, owner-wide, Dependabot, or arbitrary repo-scope PR
  discovery in this latency optimization.
- Do not add mutations such as marking todos done, assigning PRs, or updating
  review requests.
- Do not require Alfred for validation; Alfred remains a downstream consumer.
- Do not introduce a new auth model, token store, or provider credential cache.
- Do not make CI depend on live GitHub or GitLab latency.

## Implementation Boundaries

- `crates/forge-cli/src/ops/inbox.rs` owns query-family selection, collection,
  de-duplication, sorting, status summarization, and dry-run planning.
- `crates/forge-cli/src/cli.rs` owns any new user-facing flags and help text.
- `crates/forge-cli/src/backend.rs` owns the subprocess runner abstraction; widen
  it only as much as needed for safe parallel execution.
- `crates/forge-cli/tests/integration/inbox.rs` should keep provider behavior
  fixture-backed and offline.
- `crates/forge-cli/README.md` and `crates/forge-cli/docs/specs/forge-cli-spec-v1.md`
  should document new selection behavior and latency caveats.
- `nils-alfredworkflow` may later pass the new item-type flag, but that change is
  outside this `nils-cli` source document.

## Requirements

- Existing default `forge-cli inbox list --format json` output shape remains
  compatible unless a plan records an intentional schema revision.
- `--kind` continues to mean inbox reason (`review`, `assigned`, `todo`,
  `authored`, `involved`) and is not overloaded as PR-vs-issue selection.
- A new item-type selector, if added, must make GitHub PR-only mode avoid issue
  searches and issue-only mode avoid PR searches.
- GitLab PR/MR-only mode should avoid GitLab issue queries; issue-only mode
  should avoid GitLab MR queries where possible.
- GitLab todos need explicit classification because todos can point at merge
  requests, issues, or other target types.
- Parallel execution must preserve the final normalized output contract:
  deterministic de-duplication, reason merge behavior, provider status rows,
  warnings, and stable sort order.
- Partial provider failure semantics must remain unchanged: successful provider
  results survive, failed providers are represented in provider status and
  warnings, and all-provider failure exits through the normal error path.

## Acceptance Criteria

- Dry-run for GitHub default mode still reports the expected default query
  families, while PR-only and issue-only modes report only relevant families.
- Dry-run for GitLab mode still includes identity lookup when needed and reports
  only relevant query families for the selected item type.
- Stubbed integration tests cover default, PR-only, issue-only, reason-filtered,
  and mixed-provider cases.
- Tests verify that parallel or reordered execution does not change normalized
  output ordering, de-duplication, or warning behavior.
- A deterministic test or fake-runner harness demonstrates that independent
  query families are not forced through the old fully serial path.
- Manual live smoke records before/after timing for:
  - GitHub default list
  - GitHub PR-only list
  - GitLab default list
  - mixed-provider default list
- A downstream caller can request PR-only or issue-only data without post-filtering
  irrelevant item types after `forge-cli` has already fetched them.

## Validation Plan

- `cargo test -p nils-forge-cli inbox`
- `cargo test -p nils-forge-cli --test integration inbox`
- `forge-cli --provider github --format json --dry-run inbox list --limit 30`
- `forge-cli --provider github --format json --dry-run inbox list --limit 30 <new item-type flag>`
- `forge-cli --provider gitlab --format json --dry-run inbox list --gitlab-host gitlab.com --limit 30 <new item-type flag>`
- `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast`
- For final delivery, run the repository-required gate selected by
  `DEVELOPMENT.md`.

## Risks And Guardrails

- Parallel subprocess execution can make failure ordering non-deterministic; the
  final warnings and provider statuses need deterministic ordering independent of
  completion order.
- `BackendRunner` is used broadly by `forge-cli`; any trait-bound changes should
  stay localized or be justified with targeted tests.
- GitLab todos may not expose enough information to cheaply classify target type
  before fetching; implementation should document whether todos are included in
  PR-only, issue-only, both, or only all-items mode.
- Live timing is naturally noisy. CI should assert deterministic behavior and
  concurrency structure, not fixed external-service wall-clock thresholds.
- Caching can improve interactive UX but adds freshness and invalidation
  complexity. Treat persistent cache as a later option if filtering and
  parallelism do not meet the target.

## Execution

- Recommended plan: docs/plans/forge-cli-inbox-latency/forge-cli-inbox-latency-plan.md
- Recommended execution state: docs/plans/forge-cli-inbox-latency/forge-cli-inbox-latency-execution-state.md

## Retention Intent

This source document is execution coordination for a `forge-cli inbox` latency
optimization plan. It can be cleaned up after the tracking issue or plan is
closed, unless the latency findings are promoted into the `forge-cli` docs as
lasting provider-behavior guidance.

## Open Questions

- What should the item-type flag be named: `--item-type`, `--target`, or another
  term that avoids confusion with reason `--kind`?
- Should GitLab todos appear in PR-only and issue-only modes when their target
  URL can be classified, or should todos stay all-items only?
- Should status gain a cheaper count-only path, or should it continue to derive
  counts from fetched normalized items to preserve exact reason de-duplication?
- Should provider/query-family parallelism use scoped threads around the current
  blocking subprocess runner, or should the runner abstraction grow a more
  explicit batch API?
- What latency budget should be documented as a target for interactive use after
  provider and network variance are considered?

## Read First References

- `docs/plans/forge-cli-inbox/forge-cli-inbox-discussion-source.md`
- `docs/plans/forge-cli-inbox/forge-cli-inbox-plan.md`
- `crates/forge-cli/src/ops/inbox.rs`
- `crates/forge-cli/src/cli.rs`
- `crates/forge-cli/src/backend.rs`
- `crates/forge-cli/tests/integration/inbox.rs`
- `crates/forge-cli/README.md`
- `crates/forge-cli/docs/specs/forge-cli-spec-v1.md`

## Recommended Next Artifact

Create `docs/plans/forge-cli-inbox-latency/forge-cli-inbox-latency-plan.md`
from this source document, then use the plan to decide whether the first PR
should ship item-type filtering, parallel execution, or both.
