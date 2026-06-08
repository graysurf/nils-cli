# Plan: forge-cli GitLab Capabilities

## Overview

Improve the `forge-cli` GitLab provider so agent-owned GitLab delivery is
robust, provider-neutral, and not blocked by minor `glab` text-output drift
when GitLab exposes structured API data. The first reliability target is the MR
delivery chain (`pr checks`, `pr wait-checks`, `pr merge`, and `pr deliver`),
with a broader capability matrix to keep the overall GitLab surface honest.

## Read First

- Primary source:
  `docs/plans/2026-06-08-forge-cli-gitlab-capabilities/forge-cli-gitlab-capabilities-discussion-source.md`
- Source type: discussion-to-implementation-doc
- Repo anchors:
  - `crates/forge-cli/README.md`
  - `crates/forge-cli/docs/specs/forge-cli-spec-v1.md`
  - `crates/forge-cli/src/backend.rs`
  - `crates/forge-cli/src/provider.rs`
  - `crates/forge-cli/src/glab_version.rs`
  - `crates/forge-cli/src/ops/pr_checks_gitlab.rs`
  - `crates/forge-cli/src/ops/pr_checks.rs`
  - `crates/forge-cli/src/ops/pr_wait_checks.rs`
  - `crates/forge-cli/src/ops/pr_merge.rs`
  - `crates/forge-cli/src/macros/pr_deliver.rs`
  - `crates/forge-cli/tests/integration/`
  - `BINARY_DEPENDENCIES.md`
- External workflow anchors:
  - GitLab merge request API and pipeline/status APIs.
  - GitLab CLI authentication state used by `glab api`.
- Key decisions carried into execution:
  - L2 plan tracking is appropriate because this is a shared multi-step CLI
    reliability effort with state worth tracking.
  - Keep `forge-cli` provider-neutral envelopes and safety gates stable.
  - Prefer structured GitLab API data over fragile text parsing for MR delivery
    gates.
  - Retain `glab` where it is a stable helper, but avoid late minor-version
    blockers for operations that can be satisfied through the API.
- Open questions carried into execution:
  - Whether to implement a dedicated GitLab API runner abstraction in
    `forge-cli`, or route through existing `glab api` subprocess calls.
  - Whether a live GitLab sandbox MR is available for non-destructive smoke
    validation before PR closeout.

## Scope

In scope:

- Current-state audit of GitLab support across `forge-cli` PR/MR, issue, label,
  inbox, repo, auth, and unsupported command families.
- A documented GitLab capability matrix with explicit supported,
  intentionally unsupported, and fragile surfaces.
- GitLab MR checks/wait-checks hardening using structured API data or a stable
  fallback chain.
- GitLab MR merge hardening, including pre-merge gates, merge execution,
  branch cleanup intent, idempotency recovery, and post-merge SHA verification.
- Tests for `glab` minor-version drift, API success/failure shapes, self-hosted
  host/project resolution, provider parity, dry-run planning, and error
  contracts.
- Documentation updates in `forge-cli` README/specs and dependency guidance.
- PR delivery and release/sync follow-up if the operator needs the improved
  binary available in local runtime surfaces.

Out of scope:

- Replacing every existing JSON-backed `glab` operation in the same PR.
- Adding broad GitLab search support unless audit work identifies it as a
  required dependency for MR delivery.
- Changing GitHub behavior beyond shared abstractions and parity tests.
- Managing GitLab project settings, CI job definitions, or protected-branch
  policy outside `forge-cli`.

## Assumptions

1. `glab auth status` or `glab api` can use the operator's existing GitLab auth
   for GitLab-backed operations.
2. `forge-cli` can derive the GitLab host and project path from provider
   context, repo slug, remote URL, or MR web URL.
3. Existing `forge-cli` local and integration test stubs can be extended to
   model GitLab API responses without live GitLab access.
4. A live GitLab smoke is useful but optional unless a safe sandbox MR exists.
5. The final implementation should ship through normal nils-cli PR and release
   workflows before downstream runtime-kit pins are changed.

## Sprint 1: Capability Audit And Contract

**Goal**: Freeze the current GitLab capability matrix and lock the target
contract before changing live delivery behavior.

**PR grouping intent**: `group`
**Execution Profile**: serial

### Task 1.1: Audit the GitLab provider surface

- **Location**:
  - `crates/forge-cli/src/`
  - `crates/forge-cli/tests/integration/`
  - `crates/forge-cli/docs/specs/forge-cli-spec-v1.md`
- **Description**: Map every `forge-cli` command family to its GitLab behavior:
  supported with stable JSON/API, supported through fragile text parsing,
  unsupported by design, or untested. Include PR/MR, issue, label, inbox, repo,
  auth, activity, and search surfaces.
- **Dependencies**:
  - none
- **Complexity**: 2
- **Acceptance criteria**:
  - The plan tracker has a capability matrix checkpoint.
  - The `forge-cli` spec or crate docs document each GitLab surface status.
  - Fragile surfaces are explicitly named, with `pr checks` / `pr wait-checks`
    / `pr merge` prioritized first.
- **Validation**:
  - `cargo test -p nils-forge-cli cli`
  - `cargo test -p nils-forge-cli parity`

### Task 1.2: Choose and test the GitLab structured backend contract

- **Location**:
  - `crates/forge-cli/src/backend.rs`
  - `crates/forge-cli/src/provider.rs`
  - `crates/forge-cli/src/ops/pr_checks_gitlab.rs`
  - `crates/forge-cli/tests/integration/support.rs`
- **Description**: Decide whether GitLab API access should be modeled as a
  dedicated backend call type or as `glab api` subprocess calls. Add tests that
  prove host/project derivation, JSON parsing, authentication error handling,
  timeouts, and redaction behavior before replacing delivery atoms.
- **Dependencies**:
  - Task 1.1
- **Complexity**: 3
- **Acceptance criteria**:
  - Test stubs can model GitLab API responses without live GitLab access.
  - Self-hosted and nested project path examples are covered.
  - Auth failure and missing backend errors keep stable `error.kind` values.
  - The selected abstraction does not change GitHub envelope behavior.
- **Validation**:
  - `cargo test -p nils-forge-cli conformance`
  - `cargo test -p nils-forge-cli pr_checks_gitlab`

## Sprint 2: GitLab MR Delivery Reliability

**Goal**: Make GitLab MR checks, wait, merge, and deliver reliable across
supported auth states even when `glab` text output changes between minor
versions.

**PR grouping intent**: `group`
**Execution Profile**: serial after Sprint 1

### Task 2.1: Harden GitLab checks and wait-checks

- **Location**:
  - `crates/forge-cli/src/ops/pr_checks_gitlab.rs`
  - `crates/forge-cli/src/ops/pr_checks.rs`
  - `crates/forge-cli/src/ops/pr_wait_checks.rs`
  - `crates/forge-cli/tests/integration/pr_checks_gitlab.rs`
  - `crates/forge-cli/tests/integration/pr_wait_checks.rs`
- **Description**: Replace or supplement `glab ci status` text parsing with a
  structured GitLab API snapshot. Preserve canonical success, failure, pending,
  skipped/manual, required-only, timeout, and no-pipeline semantics.
- **Dependencies**:
  - Task 1.2
- **Complexity**: 4
- **Acceptance criteria**:
  - `glab_version_unsupported` no longer blocks checks when API data is
    available.
  - Existing all-success, failure, pending, no-pipeline, and manual-only tests
    still pass.
  - `pr wait-checks` reuses the same snapshot and keeps timeout/failure exit
    codes stable.
  - Dry-run continues to show the planned backend reads.
- **Validation**:
  - `cargo test -p nils-forge-cli pr_checks_gitlab`
  - `cargo test -p nils-forge-cli pr_wait_checks`

### Task 2.2: Harden GitLab merge and post-merge verification

- **Location**:
  - `crates/forge-cli/src/ops/pr_merge.rs`
  - `crates/forge-cli/src/ops/required_check_gate.rs`
  - `crates/forge-cli/src/macros/pr_deliver.rs`
  - `crates/forge-cli/tests/integration/pr_merge.rs`
  - `crates/forge-cli/tests/integration/pr_deliver_chain.rs`
- **Description**: Execute GitLab merges through a stable API path or reliable
  fallback chain after existing safety gates pass. Preserve method selection,
  branch cleanup intent, draft refusal, required-check refetch, idempotent
  recovery, and post-merge SHA extraction.
- **Dependencies**:
  - Task 2.1
- **Complexity**: 4
- **Acceptance criteria**:
  - Mergeable, green GitLab MRs can be merged without depending on unsupported
    `glab` minor text output.
  - Draft, blocked checks, unsupported method, non-default-base, and branch
    cleanup conflict refusals remain covered.
  - A backend non-zero after an actual merge is recovered only when GitLab
    reports the MR as merged.
  - `pr deliver` inherits the improved GitLab behavior through existing atom
    composition.
- **Validation**:
  - `cargo test -p nils-forge-cli pr_merge`
  - `cargo test -p nils-forge-cli pr_deliver_chain`
  - `cargo test -p nils-forge-cli required_check_gate`

### Task 2.3: Normalize diagnostics and version preflight behavior

- **Location**:
  - `crates/forge-cli/src/glab_version.rs`
  - `crates/forge-cli/src/error.rs`
  - `crates/forge-cli/tests/integration/validations.rs`
  - `crates/forge-cli/tests/integration/exit_codes_full.rs`
- **Description**: Make version diagnostics actionable without blocking API
  capable paths. Retain `glab_version_unsupported` only for operations that
  still truly require the pinned text parser.
- **Dependencies**:
  - Task 2.1
  - Task 2.2
- **Complexity**: 2
- **Acceptance criteria**:
  - Unsupported-version errors identify the specific operation and fallback
    availability.
  - API-backed paths do not call the version guard unnecessarily.
  - Exit-code and JSON error-envelope tests remain stable.
- **Validation**:
  - `cargo test -p nils-forge-cli validations`
  - `cargo test -p nils-forge-cli exit_codes_full`

## Sprint 3: Documentation, Release, And Runtime Consumption

**Goal**: Ship the improved GitLab contract with documentation, validation, and
an explicit downstream binary rollout path.

**PR grouping intent**: `group`
**Execution Profile**: serial after Sprint 2

### Task 3.1: Update documentation and dependency guidance

- **Location**:
  - `crates/forge-cli/README.md`
  - `crates/forge-cli/docs/specs/forge-cli-spec-v1.md`
  - `BINARY_DEPENDENCIES.md`
- **Description**: Document the GitLab capability matrix, API fallback behavior,
  `glab` dependency boundaries, diagnostics, and supported validation
  expectations.
- **Dependencies**:
  - Task 2.3
- **Complexity**: 2
- **Acceptance criteria**:
  - GitLab support status is visible from `forge-cli` docs.
  - Dependency guidance explains when `glab` is required and which paths use
    structured API access.
  - Docs do not contain local machine paths, tokens, or private project data.
- **Validation**:
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only`

### Task 3.2: Validate and deliver the nils-cli PR

- **Location**:
  - full repo
- **Description**: Run changed-scope validation, optionally smoke non-destructive
  GitLab paths against a sandbox MR, deliver the PR, and record any unsupported
  follow-up gaps discovered during capability audit.
- **Dependencies**:
  - Task 3.1
- **Complexity**: 2
- **Acceptance criteria**:
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast` passes.
  - Provider PR checks pass before merge.
  - The plan tracker records final validation and PR evidence.
  - Any remaining GitLab capability gaps are explicitly left as L0/L1 follow-up
    candidates rather than hidden in chat.
- **Validation**:
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast`
  - Optional sandbox smoke commands listed in the source document.

### Task 3.3: Release and runtime-surface follow-up

- **Location**:
  - nils-cli release workflow
  - downstream runtime-kit pin/surface only if needed
- **Description**: If the operator needs the improved `forge-cli` immediately
  in runtime surfaces, release nils-cli and update downstream pins/surfaces
  through the existing release and sync workflows.
- **Dependencies**:
  - Task 3.2
- **Complexity**: 2
- **Acceptance criteria**:
  - A released nils-cli version contains the improved GitLab behavior, or the
    tracker explicitly records why release is deferred.
  - Local smoke confirms the active `forge-cli --version` resolves to the
    intended release or local test binary.
  - Downstream runtime-kit changes, if any, are delivered through their own PR
    workflow.
- **Validation**:
  - nils-cli release validation, when release is requested.
  - runtime-kit version-alignment and sync validation, when downstream pins
    change.

## Done Criteria

- `forge-cli` has a documented GitLab capability matrix.
- GitLab `pr checks`, `pr wait-checks`, `pr merge`, and `pr deliver` no longer
  fail solely because `glab` minor text output drifted when structured GitLab
  API data can satisfy the operation.
- Safety gates and provider-neutral JSON envelopes remain stable.
- Targeted `nils-forge-cli` tests and `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast` pass.
- A PR lands the implementation, and release/runtime follow-up is completed or
  explicitly deferred in the tracker.

## Validation Plan

- `plan-tooling validate --file docs/plans/2026-06-08-forge-cli-gitlab-capabilities/forge-cli-gitlab-capabilities-plan.md --format text --explain`
- `bash scripts/ci/plan-bundle-validate.sh --strict --file docs/plans/2026-06-08-forge-cli-gitlab-capabilities/forge-cli-gitlab-capabilities-plan.md`
- `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only`
- `cargo fmt -p nils-forge-cli -- --check`
- `cargo test -p nils-forge-cli pr_checks_gitlab`
- `cargo test -p nils-forge-cli pr_wait_checks`
- `cargo test -p nils-forge-cli pr_merge`
- `cargo test -p nils-forge-cli pr_deliver_chain`
- `cargo test -p nils-forge-cli conformance`
- `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast`

## Risks And Guardrails

- **Risk**: API fallback changes the meaning of required checks on GitLab.
  **Guardrail**: preserve the existing canonical `ChecksSnapshot` semantics and
  add fixtures for success, failure, pending, manual/skipped, and no-pipeline
  states.
- **Risk**: GitLab host/project derivation is wrong for self-hosted or nested
  group repos.
  **Guardrail**: test web URL, remote URL, host override, and nested path
  encoding before using the abstraction in merge code.
- **Risk**: merge fallback becomes destructive before gates are verified.
  **Guardrail**: keep all existing `pr_merge` preflight gates ahead of the
  merge invocation and re-fetch provider state after any non-zero backend exit.
- **Risk**: the plan becomes an unbounded GitLab rewrite.
  **Guardrail**: the tracked delivery target is MR delivery reliability plus a
  visible capability matrix; unrelated gaps become separate follow-up records.
