# Plan: forge-cli Inbox Provider Timeout Resilience

## Overview

Bound `forge-cli inbox` waits for VPN-dependent GitLab hosts while preserving
the existing provider-neutral inbox contract. The implementation adds
subprocess timeout support, inbox-local timeout and strict-provider controls,
visible provider timeout reporting, and an opt-in stale-cache fallback that
never hides live GitLab failure.

## Read First

- Primary source: docs/plans/forge-cli-inbox-provider-timeouts/forge-cli-inbox-provider-timeouts-discussion-source.md
- Related prior plan: docs/plans/forge-cli-inbox-latency/forge-cli-inbox-latency-plan.md
- Source type: discussion-to-implementation-doc
- Open questions carried into execution: timeout flag spelling, default timeout
  value, strict-mode envelope shape, and stale-cache storage location

## Scope

- In scope:
  - Add inbox-local timeout configuration for VPN-dependent provider calls.
  - Add subprocess kill-on-timeout support in the `forge-cli` backend runner.
  - Surface provider timeouts through a distinct error kind and provider status
    row without hiding successful provider results.
  - Add a strict provider mode for automation that fails when any selected
    provider fails.
  - Add opt-in stale-cache fallback with explicit staleness metadata.
  - Update tests, specs, docs, and completion coverage for the new flags and
    provider failure states.
- Out of scope:
  - Streaming JSON, NDJSON, or incremental output events.
  - Direct REST clients or new credential storage.
  - Changing `pr`, `issue`, `repo`, or `auth` command semantics.
  - Mutating GitHub or GitLab work items.
  - CI assertions on live provider latency.

## Assumptions

1. `forge-cli inbox` should continue to be a thin wrapper around `gh` and
   `glab` subprocesses.
2. Mixed-provider interactive output should prefer partial success over
   fail-fast behavior.
3. A timeout must terminate the backend child process, not only stop waiting in
   the parent control flow.
4. Automation needs a stricter mode than Alfred-style interactive consumers.
5. Stale cache is useful only when it is opt-in and visibly stale.

## Sprint 1: Timeout Contract And Backend Primitive

**Goal**: Define the timeout surface and add the backend mechanism that can
terminate a slow or hung provider subprocess.

**Demo/Validation**:

- Commands:
  - `cargo test -p nils-forge-cli inbox_timeout_cli`
  - `cargo test -p nils-forge-cli backend_timeout`
  - `forge-cli --provider gitlab --format json --dry-run inbox list --gitlab-host gitlab.gamania.com`
- Verify: help/dry-run expose the timeout controls, timeout parsing is
  validated, and a stubbed backend child is killed near the configured
  deadline.

**PR grouping intent**: group
**Execution Profile**: serial

### Task 1.1: Add inbox timeout and strict-provider CLI surface

- **Location**:
  - `crates/forge-cli/src/cli.rs`
  - `crates/forge-cli/src/ops/inbox.rs`
  - `crates/forge-cli/tests/integration/cli.rs`
- **Description**: Add typed timeout controls to `inbox list`, `inbox status`,
  and `inbox next`, plus `--strict-providers` for automation. During execution,
  decide and document whether the timeout flag is `--gitlab-timeout`,
  `--provider-timeout`, or both.
- **Dependencies**:
  - none
- **Complexity**:
  - 4
- **Acceptance criteria**:
  - Inbox help lists the timeout flag and `--strict-providers`.
  - Invalid timeout values fail through the existing parse-error envelope path.
  - Dry-run output includes the effective timeout policy without running
    provider subprocesses.
  - Existing `--provider`, `--gitlab-host`, `--kind`, `--item-type`, and
    `--limit` behavior remains compatible.
- **Validation**:
  - `cargo test -p nils-forge-cli inbox_timeout_cli`
  - `cargo test -p nils-forge-cli inbox_cli`

### Task 1.2: Add subprocess kill-on-timeout support

- **Location**:
  - `crates/forge-cli/src/backend.rs`
  - `crates/forge-cli/src/error.rs`
  - `crates/forge-cli/tests/integration/inbox.rs`
- **Description**: Extend backend call execution so an optional deadline can
  terminate a spawned `gh` or `glab` process and return a distinct timeout
  error. Keep redaction, missing-binary handling, and auth classification
  centralized in the backend layer.
- **Dependencies**:
  - Task 1.1
- **Complexity**:
  - 7
- **Acceptance criteria**:
  - Timeout kills the child process and does not leave a sleeping stub alive.
  - Timeout errors use a stable kind such as `backend_timeout`.
  - Timeout maps to the existing `UNAVAILABLE 69` class.
  - Non-timeout backend errors retain their current kind and message behavior.
- **Validation**:
  - `cargo test -p nils-forge-cli backend_timeout`
  - `cargo test -p nils-forge-cli backend`

### Task 1.3: Apply effective timeout policy to inbox provider calls

- **Location**:
  - `crates/forge-cli/src/ops/inbox.rs`
  - `crates/forge-cli/tests/integration/inbox.rs`
- **Description**: Apply the configured timeout to GitLab inbox identity and API
  calls, and decide whether the same policy applies to GitHub or only to
  GitLab. If a total provider budget is chosen, pass the remaining budget to
  child calls so one provider cannot exceed the intended cap.
- **Dependencies**:
  - Task 1.1
  - Task 1.2
- **Complexity**:
  - 6
- **Acceptance criteria**:
  - A sleeping GitLab identity call fails around the configured timeout.
  - A sleeping GitLab query-family call fails around the configured timeout.
  - GitHub query execution remains independent from GitLab timeout behavior.
  - Provider status, warning order, item sort order, and de-duplication stay
    deterministic.
- **Validation**:
  - `cargo test -p nils-forge-cli inbox_timeout`
  - `cargo test -p nils-forge-cli inbox_parallel`

## Sprint 2: Partial Success And Strict Failure Semantics

**Goal**: Preserve fast interactive results while making timeout and strict
failure behavior explicit for machine consumers.

**Demo/Validation**:

- Commands:
  - `cargo test -p nils-forge-cli inbox_timeout`
  - `cargo test -p nils-forge-cli inbox_contract`
  - `cargo test -p nils-forge-cli --test integration inbox`
- Verify: mixed-provider mode can return GitHub results with a visible GitLab
  timeout, GitLab-only timeout is non-zero, and strict mode fails partial
  provider failure.

**PR grouping intent**: group
**Execution Profile**: serial

### Task 2.1: Surface timeout as provider-local partial failure

- **Location**:
  - `crates/forge-cli/src/ops/inbox.rs`
  - `crates/forge-cli/tests/integration/inbox.rs`
- **Description**: Convert timeout errors into failed provider rows and warnings
  in default mixed-provider mode. Keep successful provider items in the payload.
- **Dependencies**:
  - Task 1.3
- **Complexity**:
  - 5
- **Acceptance criteria**:
  - GitHub success plus GitLab timeout exits `0` by default.
  - JSON includes `data.providers[]` with GitHub `ok=true` and GitLab
    `ok=false`.
  - GitLab timeout provider error uses the timeout-specific kind.
  - Text output prints GitHub items and a GitLab timeout warning.
- **Validation**:
  - `cargo test -p nils-forge-cli inbox_timeout`
  - `cargo test -p nils-forge-cli inbox_contract`

### Task 2.2: Define and implement strict-provider mode

- **Location**:
  - `crates/forge-cli/src/ops/inbox.rs`
  - `crates/nils-common/src/cli_contract.rs`
  - `crates/forge-cli/tests/integration/inbox.rs`
  - `docs/specs/cli-output-contract-v1.md`
- **Description**: Make `--strict-providers` fail when any selected provider
  fails, including timeout. Define the failure envelope shape explicitly so
  automation can rely on it.
- **Dependencies**:
  - Task 2.1
- **Complexity**:
  - 6
- **Acceptance criteria**:
  - GitHub success plus GitLab timeout exits non-zero with
    `--strict-providers`.
  - The strict failure envelope includes machine-readable provider failure
    details.
  - All-provider-failed behavior remains compatible with current default mode.
  - Non-strict mixed-provider behavior remains partial success.
- **Validation**:
  - `cargo test -p nils-forge-cli inbox_strict_providers`
  - `cargo test -p nils-forge-cli inbox_contract`

### Task 2.3: Document operator guidance for VPN-on and VPN-off workflows

- **Location**:
  - `crates/forge-cli/docs/specs/forge-cli-spec-v1.md`
  - `BINARY_DEPENDENCIES.md`
  - `docs/plans/forge-cli-inbox-provider-timeouts/`
- **Description**: Document recommended invocations for mixed-provider use,
  GitHub-only use when VPN is known to be off, GitLab-only verification when VPN
  is on, and strict automation.
- **Dependencies**:
  - Task 2.1
  - Task 2.2
- **Complexity**:
  - 3
- **Acceptance criteria**:
  - Docs explain that `--provider github` means intentionally skipping GitLab.
  - Docs explain that timeout warnings mean GitLab was selected but unreachable.
  - Docs explain when to use `--strict-providers`.
  - Docs include short JSON and text examples without token-like values.
- **Validation**:
  - `bash scripts/ci/cli-output-contract-lint.sh --strict`
  - `bash scripts/ci/docs-hygiene-audit.sh --strict`

## Sprint 3: Opt-In Stale Cache Fallback

**Goal**: Allow interactive consumers to display recent GitLab context when
live GitLab times out, without pretending the live provider succeeded.

**Demo/Validation**:

- Commands:
  - `cargo test -p nils-forge-cli inbox_cache`
  - `cargo test -p nils-forge-cli inbox_timeout`
  - `cargo test -p nils-forge-cli --test integration inbox`
- Verify: successful provider reads can write cache, timeout can optionally read
  stale cache, and stale rows are visibly marked while the provider remains
  failed.

**PR grouping intent**: group
**Execution Profile**: serial

### Task 3.1: Define cache storage and metadata contract

- **Location**:
  - `crates/forge-cli/src/ops/inbox.rs`
  - `crates/forge-cli/docs/specs/forge-cli-spec-v1.md`
  - `BINARY_DEPENDENCIES.md`
- **Description**: Choose the cache location and JSON metadata shape for
  provider-scoped inbox snapshots. The cache must be local-only, contain no
  secrets, and expose creation time, provider, host, query scope, and age.
- **Dependencies**:
  - Task 2.1
- **Complexity**:
  - 5
- **Acceptance criteria**:
  - Cache files are provider/host/query-scope scoped.
  - Cache content stores normalized items and non-secret metadata only.
  - Cache paths are documented and can be disabled.
  - Fixture lint covers cache examples.
- **Validation**:
  - `cargo test -p nils-forge-cli inbox_cache`
  - `bash scripts/ci/forge-cli-fixture-lint.sh --strict`

### Task 3.2: Write cache after successful live provider reads

- **Location**:
  - `crates/forge-cli/src/ops/inbox.rs`
  - `crates/forge-cli/tests/integration/inbox.rs`
- **Description**: Persist successful provider snapshots after live reads. Do
  not cache failed, partial, or parse-error responses.
- **Dependencies**:
  - Task 3.1
- **Complexity**:
  - 4
- **Acceptance criteria**:
  - GitHub and GitLab successful provider snapshots can be cached separately.
  - Failed providers do not overwrite a previous good cache.
  - Cache write failures become warnings, not successful-provider failures.
  - `--no-cache` or equivalent disables cache writes and reads.
- **Validation**:
  - `cargo test -p nils-forge-cli inbox_cache`

### Task 3.3: Add opt-in stale-cache read on provider timeout

- **Location**:
  - `crates/forge-cli/src/ops/inbox.rs`
  - `crates/forge-cli/tests/integration/inbox.rs`
- **Description**: When a selected provider times out and cache fallback is
  explicitly enabled, include recent cached items with staleness metadata while
  leaving the provider status failed.
- **Dependencies**:
  - Task 3.2
- **Complexity**:
  - 6
- **Acceptance criteria**:
  - Cache fallback is opt-in and bounded by max age.
  - Timed-out provider status remains `ok=false`.
  - Cached items or provider metadata expose stale/cache origin.
  - Warnings distinguish live timeout from stale-cache fallback.
  - Strict mode still exits non-zero when a selected live provider times out.
- **Validation**:
  - `cargo test -p nils-forge-cli inbox_cache`
  - `cargo test -p nils-forge-cli inbox_strict_providers`

## Sprint 4: Completion, Specs, And Delivery Gate

**Goal**: Finish public contract documentation, completion coverage, and the
required repository validation gate.

**Demo/Validation**:

- Commands:
  - `cargo test -p nils-forge-cli inbox`
  - `cargo test -p nils-forge-cli --test integration inbox`
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast`
- Verify: help, completions, docs, tests, and local fast gate agree on timeout,
  strict, and cache behavior.

**PR grouping intent**: group
**Execution Profile**: serial

### Task 4.1: Update completions and CLI docs

- **Location**:
  - `completions/`
  - `crates/forge-cli/docs/specs/forge-cli-spec-v1.md`
  - `README.md`
- **Description**: Regenerate or update completion assets and ensure the
  user-facing docs describe the new flags and failure semantics.
- **Dependencies**:
  - Task 1.1
  - Task 2.2
  - Task 3.3
- **Complexity**:
  - 4
- **Acceptance criteria**:
  - Completion assets include timeout, strict-provider, and cache flags.
  - Docs describe default, strict, GitHub-only, GitLab-only, timeout, and
    stale-cache behavior.
  - Root/version/help checks remain compatible.
- **Validation**:
  - `bash scripts/ci/completion-asset-audit.sh --strict`
  - `bash scripts/ci/completion-flag-parity-audit.sh --strict`

### Task 4.2: Run final changed-scope and docs gates

- **Location**:
  - workspace
- **Description**: Run the required changed-scope gate and any targeted tests
  from earlier sprints.
- **Dependencies**:
  - Task 4.1
- **Complexity**:
  - 3
- **Acceptance criteria**:
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast` passes.
  - Targeted `nils-forge-cli` tests pass.
  - Docs/spec/completion audits pass.
  - The implementation PR body summarizes timeout, strict, and cache behavior
    from the actual diff.
- **Validation**:
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast`

## Risks And Mitigations

- **Risk**: Killing child processes differs across platforms.
  **Mitigation**: keep tests portable, prefer standard-library child management,
  and cover macOS/Linux CI behavior.
- **Risk**: A too-short default timeout harms VPN-on GitLab users.
  **Mitigation**: make the value configurable and pick the default from live
  smoke evidence before finalizing.
- **Risk**: Strict mode conflicts with the single-envelope success contract.
  **Mitigation**: define strict failure shape in specs before implementation.
- **Risk**: Stale cache misleads consumers.
  **Mitigation**: make cache fallback opt-in, age-bounded, warning-backed, and
  visibly stale in JSON/text output.

## Validation Summary

- Plan validation:
  - `plan-tooling validate --file docs/plans/forge-cli-inbox-provider-timeouts/forge-cli-inbox-provider-timeouts-plan.md --format text --explain`
- Docs-only validation:
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only`
- Implementation validation:
  - `cargo test -p nils-forge-cli inbox_timeout`
  - `cargo test -p nils-forge-cli inbox_strict_providers`
  - `cargo test -p nils-forge-cli inbox_cache`
  - `cargo test -p nils-forge-cli --test integration inbox`
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast`
