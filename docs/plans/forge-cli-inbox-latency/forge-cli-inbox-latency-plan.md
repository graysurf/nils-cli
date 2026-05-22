# Plan: forge-cli Inbox Latency Optimization

## Overview

Reduce `forge-cli inbox` interactive latency by avoiding irrelevant backend
query families first, then by running independent provider/query-family calls
without changing the normalized output contract. The plan keeps the current
personal-inbox reasons, provider failure semantics, and `gh`/`glab`
subprocess-backed architecture. Persistent caching is deliberately deferred
until filtering and parallelism have been measured.

## Read First

- Primary source: docs/plans/forge-cli-inbox-latency/forge-cli-inbox-latency-discussion-source.md
- Source type: discussion-to-implementation-doc
- Open questions carried into execution: item-type flag naming, GitLab `todo`
  classification behavior, status-specific count path, and the exact
  parallel-runner shape

## Scope

- In scope:
  - Add an inbox item-type selector so downstream callers can request all items,
    PR/MR-only items, or issue-only items without paying for irrelevant query
    families.
  - Preserve `--kind` as the repeatable inbox reason filter; do not overload it
    for PR-vs-issue selection.
  - Prune GitHub and GitLab query plans in both live and dry-run modes based on
    the selected reasons and item type.
  - Parallelize selected independent providers and query families while keeping
    deterministic final providers, warnings, de-duplication, and sort order.
  - Keep GitLab identity lookup before GitLab queries that need id or username.
  - Add offline fixture-backed tests, docs/spec updates, completion coverage,
    and manual live-smoke guidance for before/after timing.
- Out of scope:
  - Repo-wide maintained-project discovery, Dependabot-specific coverage, or
    owner-wide PR search behavior.
  - Mutations such as marking `todos` done or changing review/assignee state.
  - Persistent caches, token storage, or direct GitHub/GitLab API clients.
  - Alfred workflow changes; it can adopt new flags in a later downstream PR.
  - CI assertions on live provider latency.

## Assumptions

1. The item-type selector should default to `all` so current invocations remain
   behavior-compatible.
2. The likely flag spelling is `--item-type all|pr|issue`, where `pr` includes
   GitHub pull requests and GitLab merge requests; execution may choose a
   clearer spelling if local CLI conventions point elsewhere.
3. GitLab pending `todos` should remain in `all` mode. In PR-only or issue-only
   mode, include only `todos` whose target type or URL can be classified
   deterministically.
4. The current `BackendRunner::run` contract can remain blocking; parallelism
   can be introduced around independent calls if trait bounds and tests stay
   localized.
5. `inbox status` continues to derive exact bounded counts from normalized
   items unless a cheaper status path can prove it preserves count semantics.

## Sprint 1: Item-Type Query Pruning

**Goal**: Add a user-facing item-type selector and use it to remove unnecessary
GitHub/GitLab query families before any backend call is planned or executed.

**Demo/Validation**:

- Commands:
  - `cargo test -p nils-forge-cli inbox_cli`
  - `cargo test -p nils-forge-cli inbox_item_type`
  - `forge-cli --provider github --format json --dry-run inbox list --limit 30`
  - `forge-cli --provider github --format json --dry-run inbox list --limit 30 --item-type pr`
- Verify: default dry-run output is unchanged, PR-only mode omits issue
  searches, issue-only mode omits PR/MR searches, and `--kind` remains a reason
  filter.

**PR grouping intent**: group
**Execution Profile**: serial

### Task 1.1: Add inbox item-type CLI surface

- **Location**:
  - `crates/forge-cli/src/cli.rs`
  - `crates/forge-cli/tests/integration/cli.rs`
- **Description**: Add a typed inbox item selector shared by `inbox list`,
  `inbox status`, and `inbox next`, defaulting to all items. Keep `--kind`
  repeatable for reason filtering and add help text that distinguishes reason
  filters from item-type filters.
- **Dependencies**:
  - none
- **Complexity**:
  - 3
- **Acceptance criteria**:
  - `forge-cli inbox list --help`, `status --help`, and `next --help` expose
    the new selector with an all-items default.
  - Parsing accepts all supported values and rejects unknown item types through
    the existing clap parse-error path.
  - Existing `--kind review|assigned|todo|authored|involved` parsing and help
    text remain intact.
- **Validation**:
  - `cargo test -p nils-forge-cli inbox_cli`

### Task 1.2: Prune GitHub query families by item type

- **Location**:
  - `crates/forge-cli/src/ops/inbox.rs`
  - `crates/forge-cli/tests/integration/inbox.rs`
- **Description**: Extend `QueryConfig` to carry the selected item type and make
  `github_queries` skip issue searches in PR-only mode and skip PR searches in
  issue-only mode.
- **Dependencies**:
  - Task 1.1
- **Complexity**:
  - 4
- **Acceptance criteria**:
  - Default GitHub dry-run still plans review-requested PRs, assigned PRs,
    assigned issues, authored PRs, and authored issues.
  - GitHub PR-only dry-run plans only PR search families for selected reasons.
  - GitHub issue-only dry-run plans only issue search families where a reason
    has an issue-backed query.
  - Normalized output schema remains `cli.forge-cli.inbox.*.v1`.
- **Validation**:
  - `cargo test -p nils-forge-cli inbox_item_type`
  - `forge-cli --provider github --format json --dry-run inbox list --limit 30 --item-type pr`
  - `forge-cli --provider github --format json --dry-run inbox list --limit 30 --item-type issue`

### Task 1.3: Prune GitLab MR, issue, and `todo` query families

- **Location**:
  - `crates/forge-cli/src/ops/inbox.rs`
  - `crates/forge-cli/tests/integration/inbox.rs`
- **Description**: Make `gitlab_queries` skip merge-request, issue, and `todo`
  calls that cannot produce the selected item type. Keep identity lookup only
  when at least one selected GitLab query requires it.
- **Dependencies**:
  - Task 1.1
  - Task 1.2
- **Complexity**:
  - 5
- **Acceptance criteria**:
  - Default GitLab dry-run remains compatible and includes the identity lookup
    plus selected default query families.
  - GitLab PR-only mode skips issue API calls and includes only MR queries and
    classifiable MR `todos`.
  - GitLab issue-only mode skips MR API calls and includes only issue queries
    and classifiable issue `todos`.
  - `todo` classification behavior is documented in tests so downstream callers
    know whether unclassified `todos` appear only in all-items mode.
- **Validation**:
  - `cargo test -p nils-forge-cli inbox_item_type`
  - `forge-cli --provider gitlab --format json --dry-run inbox list --gitlab-host gitlab.gamania.com --limit 30 --item-type pr`
  - `forge-cli --provider gitlab --format json --dry-run inbox list --gitlab-host gitlab.gamania.com --limit 30 --item-type issue`

## Sprint 2: Deterministic Parallel Collection

**Goal**: Reduce remaining latency by running independent provider and query
families concurrently while preserving deterministic output and failure
behavior.

**Demo/Validation**:

- Commands:
  - `cargo test -p nils-forge-cli inbox_parallel`
  - `cargo test -p nils-forge-cli inbox_contract`
  - `cargo test -p nils-forge-cli --test integration inbox`
- Verify: tests demonstrate concurrent-ready scheduling, stable output order,
  stable warning order, unchanged partial-success semantics, and unchanged
  all-provider-failure behavior.

**PR grouping intent**: group
**Execution Profile**: parallel-x2

### Task 2.1: Isolate query-plan construction from execution

- **Location**:
  - `crates/forge-cli/src/ops/inbox.rs`
  - `crates/forge-cli/tests/integration/inbox.rs`
- **Description**: Separate provider target resolution, query-family planning,
  execution, and normalization so dry-run planning and live execution consume
  the same ordered query plan.
- **Dependencies**:
  - Task 1.1
  - Task 1.2
  - Task 1.3
- **Complexity**:
  - 5
- **Acceptance criteria**:
  - Dry-run and live paths use the same provider/query plan builder.
  - Plan rows retain stable provider, host, reason, source, and argv metadata.
  - Existing list/status/next tests keep passing before parallel execution is
    enabled.
- **Validation**:
  - `cargo test -p nils-forge-cli inbox_contract`

### Task 2.2: Parallelize independent provider work

- **Location**:
  - `crates/forge-cli/src/ops/inbox.rs`
  - `crates/forge-cli/src/backend.rs`
  - `crates/forge-cli/tests/integration/inbox.rs`
- **Description**: Run selected provider adapters concurrently where safe. Keep
  GitHub and GitLab provider status rows in target order, and keep provider
  warnings deterministic regardless of completion order.
- **Dependencies**:
  - Task 2.1
- **Complexity**:
  - 6
- **Acceptance criteria**:
  - Mixed-provider mode does not wait for GitLab identity lookup before starting
    independent GitHub work.
  - Partial provider failure still emits successful provider rows, failed
    provider rows, and warnings in deterministic provider order.
  - All selected providers failing still returns the existing non-zero backend
    error path.
- **Validation**:
  - `cargo test -p nils-forge-cli inbox_parallel`
  - `cargo test -p nils-forge-cli inbox_contract`

### Task 2.3: Parallelize independent query families within a provider

- **Location**:
  - `crates/forge-cli/src/ops/inbox.rs`
  - `crates/forge-cli/tests/integration/inbox.rs`
- **Description**: Run independent GitHub searches and GitLab post-identity
  query families concurrently where backend runner trait bounds allow it. Merge
  results only through the existing deterministic normalization, de-duplication,
  and sorting path.
- **Dependencies**:
  - Task 2.1
  - Task 2.2
- **Complexity**:
  - 7
- **Acceptance criteria**:
  - Query-family completion order cannot affect normalized item order,
    provider item counts, or warning order.
  - A deterministic fake-runner or stub harness proves the old fully serial
    query path is not required for independent query families.
  - Backend error redaction and missing-binary behavior remain centralized in
    `BackendRunner`.
- **Validation**:
  - `cargo test -p nils-forge-cli inbox_parallel`
  - `cargo test -p nils-forge-cli --test integration inbox`

## Sprint 3: Docs, Smoke Guidance, And Delivery Gate

**Goal**: Document the new selection and latency behavior, add completion/test
coverage, and run the repo-required docs and implementation gates.

**Demo/Validation**:

- Commands:
  - `cargo test -p nils-forge-cli inbox`
  - `cargo test -p nils-forge-cli --test integration inbox`
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast`
- Verify: docs match help/output behavior, live-smoke guidance captures
  before/after commands without making external latency a CI assertion, and the
  changed-scope gate passes.

**PR grouping intent**: group
**Execution Profile**: serial

### Task 3.1: Update docs, specs, and completions

- **Location**:
  - `crates/forge-cli/README.md`
  - `crates/forge-cli/docs/specs/forge-cli-spec-v1.md`
  - `completions/bash/forge-cli`
  - `completions/zsh/_forge-cli`
  - `crates/forge-cli/tests/integration/cli.rs`
- **Description**: Document item-type filtering, reason filtering, dry-run
  query planning, GitLab `todo` classification behavior, and latency caveats.
  Regenerate or update completion assets according to the repo completion
  policy.
- **Dependencies**:
  - Task 1.1
  - Task 1.2
  - Task 1.3
  - Task 2.1
  - Task 2.2
  - Task 2.3
- **Complexity**:
  - 4
- **Acceptance criteria**:
  - README examples show PR-only or issue-only usage.
  - The v1 spec distinguishes item type from `--kind` reason.
  - Shell completions expose the new item-type flag and values.
  - Completion syntax checks pass for changed assets.
- **Validation**:
  - `cargo test -p nils-forge-cli inbox_cli`
  - `zsh -n completions/zsh/_forge-cli`
  - `bash -n completions/bash/forge-cli`

### Task 3.2: Add manual live-smoke timing guidance

- **Location**:
  - `crates/forge-cli/README.md`
  - `crates/forge-cli/docs/specs/forge-cli-spec-v1.md`
  - `docs/plans/forge-cli-inbox-latency/forge-cli-inbox-latency-execution-state.md`
- **Description**: Record the live-smoke command set implementers should run
  before final delivery and capture results in the execution ledger when the
  plan is executed. Do not make live timings part of CI.
- **Dependencies**:
  - Task 1.1
  - Task 1.2
  - Task 1.3
  - Task 2.1
  - Task 2.2
  - Task 2.3
- **Complexity**:
  - 3
- **Acceptance criteria**:
  - Smoke guidance covers GitHub default, GitHub PR-only, GitLab default, and
    mixed-provider default list timings.
  - Guidance states provider/network variance and treats wall-clock timings as
    delivery evidence, not deterministic CI assertions.
  - The execution-state path is referenced but not required to exist before
    plan execution starts.
- **Validation**:
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only`

### Task 3.3: Run final changed-scope gate and capture delivery evidence

- **Location**:
  - `scripts/ci/nils-cli-checks-entrypoint.sh`
  - `docs/plans/forge-cli-inbox-latency/forge-cli-inbox-latency-execution-state.md`
- **Description**: Run the repository-selected changed-scope gate for the final
  implementation PR, then capture the local command results and manual
  live-smoke summary in the execution ledger when execution begins.
- **Dependencies**:
  - Task 3.1
  - Task 3.2
- **Complexity**:
  - 3
- **Acceptance criteria**:
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast` passes for the
    implementation branch.
  - Final delivery records whether full required checks or coverage were run,
    and why if they were not.
  - Live-smoke results are reported as evidence, with exact dates and command
    versions when captured.
- **Validation**:
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast`

## Testing Strategy

- Unit: exercise item-type config, query-plan construction, GitLab `todo` target
  classification, de-duplication, ordering, and provider warning ordering.
- Integration: use `crates/forge-cli/tests/integration/inbox.rs` with
  `FORGE_CLI_GH_BIN` and `FORGE_CLI_GLAB_BIN` stubs for default, PR-only,
  issue-only, reason-filtered, mixed-provider, partial-success, and
  all-provider-failure cases.
- CLI/contract: cover clap parsing/help, JSON schema version stability, dry-run
  plan rows, completion assets, and docs/spec examples.
- Manual: run live timing smoke for GitHub default, GitHub PR-only, GitLab
  default, and mixed-provider default list after implementation; treat timing
  as delivery evidence only.

## Risks & gotchas

- Parallel backend calls can make error completion order nondeterministic; the
  output layer must sort or reassemble provider statuses and warnings by the
  original target/query plan order.
- `BackendRunner` is currently a simple blocking trait. Any new `Send`/`Sync`
  bound or batch helper must not leak into unrelated `forge-cli` operations
  without tests.
- GitLab identity lookup is a dependency bottleneck for GitLab queries but not
  for unrelated GitHub work.
- GitLab `todo` payloads may not always include enough target metadata to classify
  PR-vs-issue without extra calls; do not add hidden extra `todo` lookups unless
  the latency tradeoff is explicit.
- `--limit` bounds each query family, not global output rows. Item-type
  filtering should reduce query count; it should not silently reinterpret
  bounded-count semantics.
- Caching may be tempting if live timings remain high, but cache freshness and
  invalidation are separate product decisions and should be a later plan.

## Rollback plan

- Disable parallel collection first and keep item-type filtering if concurrency
  introduces nondeterministic failures.
- If item-type filtering itself causes compatibility issues, keep the parser
  behind documented default `all` behavior and revert only the query-pruning
  branch for the affected provider.
- If generated completions or docs drift, regenerate from the final clap surface
  and rerun the docs-only/completion gates before delivery.
