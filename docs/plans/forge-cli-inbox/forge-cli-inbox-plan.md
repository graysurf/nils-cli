# Plan: forge-cli Inbox

## Overview

Add a read-only `forge-cli inbox` command group for cross-repo personal work
discovery across GitHub and GitLab. The implementation keeps existing
repo-local lifecycle commands unchanged, adds an inbox-local multi-provider
resolver, normalizes provider results into a stable JSON contract, and leaves
Alfred as a downstream consumer of the CLI output.

## Read First

- Primary source: docs/plans/forge-cli-inbox/forge-cli-inbox-discussion-source.md
- Source type: discussion-to-implementation-doc
- Open questions carried into execution: none

## Scope

- In scope:
  - Add `forge-cli inbox status`, `forge-cli inbox list`, and
    `forge-cli inbox next`.
  - Add inbox-local provider resolution where default mode queries all available
    providers and `--provider github|gitlab` narrows the query.
  - Add inbox-local `--gitlab-host <host>` and pass `--hostname <host>` to every
    GitLab inbox API call.
  - Normalize GitHub `gh search` and GitLab `glab api` output into stable JSON
    items with deterministic `reasons`.
  - Support bounded counts, partial provider success, all-provider-failure
    errors, and ranked next-item output.
  - Add offline fixtures, integration tests, docs, and completion coverage.
- Out of scope:
  - Mutating PRs, issues, MRs, or GitLab todos.
  - Adding a raw REST passthrough or direct token/auth storage.
  - Emitting Alfred JSON directly from `forge-cli`.
  - Adding persistent CLI cache files.
  - Changing existing `pr`, `issue`, `repo`, or `auth` provider detection.

## Assumptions

1. `forge-cli` should remain a thin wrapper around `gh` and `glab`
   subprocesses.
2. Offline tests should stub `FORGE_CLI_GH_BIN` and `FORGE_CLI_GLAB_BIN`
   instead of requiring live provider access.
3. The v1 `status` command reports bounded counts, not globally exact counts.
4. GitHub notifications stay out of v1 until explicit search qualifiers prove
   insufficient.
5. GitLab self-managed endpoint details may differ, so live smoke is optional
   and separate from the offline gate.

## Sprint 1: Command Surface And Contract Foundation

**Goal**: Add the `inbox` command group, data models, provider resolver, and
contract tests before provider adapters depend on them.

**Demo/Validation**:

- Commands:
  - `cargo test -p nils-forge-cli inbox_cli`
  - `cargo test -p nils-forge-cli inbox_contract`
- Verify: help lists the inbox commands, non-inbox provider behavior is
  unchanged, and the JSON contract models compile with fixture-free tests.

**PR grouping intent**: group
**Execution Profile**: serial

### Task 1.1: Add inbox CLI command group

- **Location**:
  - `crates/forge-cli/src/cli.rs`
  - `crates/forge-cli/src/ops/mod.rs`
  - `crates/forge-cli/src/ops/inbox.rs`
  - `crates/forge-cli/tests/integration/cli.rs`
- **Description**: Add `inbox status`, `inbox list`, and `inbox next`
  subcommands with shared flags for provider selection, kind filters, limit, and
  inbox-local GitLab host selection.
- **Dependencies**:
  - none
- **Complexity**:
  - 4
- **Acceptance criteria**:
  - `forge-cli inbox --help` lists `status`, `list`, and `next`.
  - `--gitlab-host <host>` is accepted only on inbox commands.
  - Existing top-level `--provider` parsing remains compatible for non-inbox
    commands.
  - Root `--help` and completion tests include the new command group.
- **Validation**:
  - `cargo test -p nils-forge-cli inbox_cli`

### Task 1.2: Implement inbox provider resolver

- **Location**:
  - `crates/forge-cli/src/provider.rs`
  - `crates/forge-cli/src/ops/inbox.rs`
  - `crates/forge-cli/tests/integration/inbox.rs`
- **Description**: Add an inbox-local resolver that can return multiple provider
  contexts for one invocation while preserving the existing single-provider
  resolver for lifecycle commands.
- **Dependencies**:
  - Task 1.1
- **Complexity**:
  - 5
- **Acceptance criteria**:
  - Default inbox mode selects all available providers.
  - `--provider github` selects GitHub only.
  - `--provider gitlab --gitlab-host <host>` selects GitLab with that host.
  - Non-inbox commands still resolve exactly one `ProviderContext`.
- **Validation**:
  - `cargo test -p nils-forge-cli inbox_provider_resolver`

### Task 1.3: Define inbox JSON models and error aggregation

- **Location**:
  - `crates/forge-cli/src/ops/inbox.rs`
  - `crates/forge-cli/tests/integration/inbox.rs`
- **Description**: Define provider status rows, warning rows, inbox items,
  deterministic `reasons`, bounded-count metadata, and partial-success result
  aggregation.
- **Dependencies**:
  - Task 1.1
- **Complexity**:
  - 5
- **Acceptance criteria**:
  - JSON item fields match the source contract.
  - Duplicate rows merge into one item with deterministic `reasons`.
  - Partial provider success emits `ok=true`, exit `0`, warnings, and
    `data.providers[]`.
  - All selected providers failing returns a non-zero error envelope.
- **Validation**:
  - `cargo test -p nils-forge-cli inbox_contract`

## Sprint 2: Provider Adapters And `inbox list`

**Goal**: Implement offline-tested GitHub and GitLab adapters and compose them
into the JSON-only `inbox list` behavior.

**Demo/Validation**:

- Commands:
  - `cargo test -p nils-forge-cli inbox_github`
  - `cargo test -p nils-forge-cli inbox_gitlab`
  - `cargo test -p nils-forge-cli inbox_list`
  - `bash scripts/ci/forge-cli-fixture-lint.sh --strict`
- Verify: stubbed provider commands produce normalized list output and no
  fixture leaks token-shaped values.

**PR grouping intent**: group
**Execution Profile**: parallel-x2

### Task 2.1: Implement GitHub inbox adapter

- **Location**:
  - `crates/forge-cli/src/ops/inbox.rs`
  - `crates/forge-cli/tests/fixtures/github/`
  - `crates/forge-cli/tests/integration/inbox.rs`
- **Description**: Build `gh search prs` and `gh search issues` calls for
  review, assigned, authored, and optional involved rows, then normalize the
  returned JSON.
- **Dependencies**:
  - Task 1.3
- **Complexity**:
  - 5
- **Acceptance criteria**:
  - Each GitHub query uses `--limit <limit>` and explicit `--json` fields.
  - PR and issue results normalize `repo`, `number`, `title`, `url`,
    `updated_at`, `author`, `kind`, `source`, and `reasons`.
  - Empty arrays return successful zero-item provider status.
  - Backend failures become provider failures when another selected provider
    succeeds.
- **Validation**:
  - `cargo test -p nils-forge-cli inbox_github`

### Task 2.2: Implement GitLab inbox adapter

- **Location**:
  - `crates/forge-cli/src/ops/inbox.rs`
  - `crates/forge-cli/tests/fixtures/gitlab/`
  - `crates/forge-cli/tests/integration/inbox.rs`
- **Description**: Resolve GitLab identity with `glab api user --hostname
  <host>`, then query MRs, issues, and todos through host-aware `glab api`
  endpoints.
- **Dependencies**:
  - Task 1.2
  - Task 1.3
- **Complexity**:
  - 6
- **Acceptance criteria**:
  - Every GitLab inbox API invocation includes `--hostname <host>`.
  - User id and username are discovered dynamically and cached only for the
    invocation.
  - Merge request, issue, and GitLab pending-action records normalize into the
    shared item model.
  - Empty arrays return successful zero-item provider status.
- **Validation**:
  - `cargo test -p nils-forge-cli inbox_gitlab`

### Task 2.3: Compose `inbox list`

- **Location**:
  - `crates/forge-cli/src/ops/inbox.rs`
  - `crates/forge-cli/tests/integration/inbox.rs`
- **Description**: Run selected adapters, merge and sort items, collapse
  duplicate reasons, and render text and JSON output for `inbox list`.
- **Dependencies**:
  - Task 2.1
  - Task 2.2
- **Complexity**:
  - 5
- **Acceptance criteria**:
  - Combined-provider mode returns one normalized list.
  - One provider failing does not hide successful provider results.
  - All selected providers failing exits non-zero.
  - Text output is concise and does not expose raw backend stderr.
- **Validation**:
  - `cargo test -p nils-forge-cli inbox_list`
  - `bash scripts/ci/forge-cli-fixture-lint.sh --strict`

## Sprint 3: `status`, `next`, Docs, And Gate

**Goal**: Finish the user-facing inbox workflows, document scheduler/agent use,
and run the local validation gate.

**Demo/Validation**:

- Commands:
  - `cargo test -p nils-forge-cli inbox_status inbox_next`
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast`
- Verify: `status` and `next` use the same bounded contract and the changed-scope
  gate passes.

**PR grouping intent**: group
**Execution Profile**: serial

### Task 3.1: Implement bounded `inbox status`

- **Location**:
  - `crates/forge-cli/src/ops/inbox.rs`
  - `crates/forge-cli/tests/integration/inbox.rs`
- **Description**: Aggregate bounded counts by provider, host, kind, and reason
  from normalized list results.
- **Dependencies**:
  - Task 2.3
- **Complexity**:
  - 4
- **Acceptance criteria**:
  - JSON includes the effective limit and limited/exactness metadata.
  - Empty provider results count as zero.
  - Partial provider failure status is visible in `data.providers[]` and
    warnings.
- **Validation**:
  - `cargo test -p nils-forge-cli inbox_status`

### Task 3.2: Implement ranked `inbox next`

- **Location**:
  - `crates/forge-cli/src/ops/inbox.rs`
  - `crates/forge-cli/tests/integration/inbox.rs`
- **Description**: Rank normalized items for next-action discovery, defaulting
  to up to five items with review-requested work ahead of authored or broad
  involved work.
- **Dependencies**:
  - Task 2.3
- **Complexity**:
  - 4
- **Acceptance criteria**:
  - Default output returns at most five items.
  - Review items rank ahead of authored and involved items.
  - Ranking is deterministic when timestamps or reasons tie.
  - JSON output keeps the full normalized item shape.
- **Validation**:
  - `cargo test -p nils-forge-cli inbox_next`

### Task 3.3: Update docs, completions, and release-facing notes

- **Location**:
  - `crates/forge-cli/README.md`
  - `crates/forge-cli/docs/`
  - `completions/zsh/_forge-cli`
  - `completions/bash/forge-cli`
  - `docs/plans/forge-cli-inbox/`
- **Description**: Document inbox semantics, provider caveats, scheduler/agent
  usage, partial-failure behavior, and completion assets.
- **Dependencies**:
  - Task 3.1
  - Task 3.2
- **Complexity**:
  - 3
- **Acceptance criteria**:
  - Docs explain `--provider`, `--gitlab-host`, `--limit`, partial success, and
    bounded counts.
  - Completion assets include inbox commands and flags.
  - Optional live-smoke commands are documented without making them mandatory in
    CI.
- **Validation**:
  - `bash scripts/ci/cli-output-contract-lint.sh --strict`
  - `bash scripts/ci/completion-asset-audit.sh --strict`
  - `bash scripts/ci/completion-flag-parity-audit.sh --strict`

### Task 3.4: Run final changed-scope gate

- **Location**:
  - workspace root
  - `scripts/ci/nils-cli-checks-entrypoint.sh`
- **Description**: Run targeted and changed-scope validation before opening the
  implementation PR.
- **Dependencies**:
  - Task 3.3
- **Complexity**:
  - 3
- **Acceptance criteria**:
  - Targeted inbox tests pass.
  - Fixture lint passes.
  - Local fast gate passes or records a concrete external blocker.
- **Validation**:
  - `cargo test -p nils-forge-cli inbox`
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast`

## Testing Strategy

- Unit: provider resolver, reason dedupe, ranking, bounded counts, and
  provider-status aggregation.
- Integration: stubbed GitHub and GitLab command adapters through
  `FORGE_CLI_GH_BIN` and `FORGE_CLI_GLAB_BIN`.
- Contract: JSON envelope shape for list, status, next, partial success,
  all-provider failure, empty results, and duplicate reasons.
- Manual/live: optional post-implementation smoke for GitHub and
  `gitlab.gamania.com` with `--format json`.

## Risks & gotchas

- The inbox resolver must stay local to `inbox`; changing global provider
  detection would risk existing lifecycle commands.
- GitLab API endpoint parameters may differ on the company self-managed host, so
  offline fixtures should cover the intended shape and live smoke should be
  recorded separately.
- Partial success can be misleading if text output hides warnings; both text and
  JSON should make provider failures visible.
- Bounded counts must not be described as exact global totals.
- New fixture files must pass token redaction lint.
- Completion assets can drift when new flags are added late in the
  implementation.

## Rollback plan

- Revert the inbox command group and adapter modules while leaving existing
  `pr`, `issue`, `repo`, and `auth` commands untouched.
- If only one provider adapter is faulty, disable that adapter behind provider
  selection checks while preserving the normalized contract tests for the
  working provider.
- If completion assets fail after code is otherwise sound, revert generated
  completion changes and regenerate them from the fixed CLI before merge.
