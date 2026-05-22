# Plan: forge-cli GitHub Checks Compatibility and 0.17.0 Release

## Overview

Fix the `forge-cli` GitHub checks backend for `gh 2.92.0`, verify every
downstream checks consumer (`pr checks`, `pr wait-checks`, `pr merge`, and
`pr deliver`), then cut `nils-cli` `0.17.0` as the compatibility release needed
by `agent-runtime-kit` Plan 05 Sprint 6.

## Read First

- Primary source: docs/plans/forge-cli-gh-292-checks/forge-cli-gh-292-checks-discussion-source.md
- Source type: discussion-to-implementation-doc
- Open questions carried into execution:
  - Required-only gating implementation should preserve full check reporting
    when practical; default to separate all-checks and required-only calls.
  - Keep the `0.17.0` release focused on this compatibility fix unless a release
    blocker is discovered.

## Scope

- In scope:
  - Update `crates/forge-cli/src/ops/pr_checks.rs` GitHub backend field usage and
    parsing for the `gh 2.92.0` field set.
  - Keep `PrChecksPayload` stable where possible, deriving normalized state from
    `bucket` and `state` when `conclusion` is unavailable.
  - Preserve deterministic required-check gating for `pr wait-checks`,
    `required_check_gate`, `pr merge`, and `pr deliver`.
  - Add fixtures and tests for supported-field output, required-only output,
    no-required-check behavior, passing checks, failing checks, and pending
    checks.
  - Update docs or changelog entries needed for the `0.17.0` release.
  - Run the standard release workflow for `nils-cli 0.17.0`.
  - Post downstream handoff evidence back to `agent-runtime-kit` issue #26.
- Out of scope:
  - Replacing `gh` subprocess usage with REST or GraphQL clients.
  - Changing GitLab check parsing unless a shared abstraction requires a small
    test or comment update.
  - Updating `agent-runtime-kit` skill manifests or completing its Sprint 6 work.

## Assumptions

1. `gh 2.92.0` is the compatibility target because it is the currently observed
   version that broke released `forge-cli 0.16.0`.
2. Default automated tests must stay network-free through backend stubs.
3. The standard nils-cli release path is still the source of truth for version
   bump, tag, GitHub release, and local PATH verification.
4. Downstream `agent-runtime-kit` will update `required_clis` and close
   `P5-S5-G1` only after `forge-cli --version` reports the fixed release.

## Sprint 1: Characterize GitHub Checks Compatibility

**Goal**: Reproduce the `gh 2.92.0` field-set breakage in tests and pin the
expected backend command shapes before changing production behavior.

**Demo/Validation**:

- Commands:
  - `cargo test -p nils-forge-cli pr_checks_github`
  - `cargo test -p nils-forge-cli pr_wait_checks`
- Verify: tests fail against the old unsupported `conclusion` / `isRequired`
  field request and fixtures cover the supported `gh 2.92.0` fields.

**PR grouping intent**: group
**Execution Profile**: serial

### Task 1.1: Add gh 2.92.0 field-set fixtures

- **Location**:
  - `crates/forge-cli/tests/fixtures/github/pr_checks/`
  - `crates/forge-cli/tests/integration/pr_checks_github.rs`
- **Description**: Add fixtures for the `gh 2.92.0` supported field set,
  including passing, failing, pending, and no-required-check cases.
- **Dependencies**:
  - none
- **Complexity**: 3
- **Acceptance criteria**:
  - Fixtures omit `conclusion` and `isRequired`.
  - Tests assert the current implementation still requests unsupported fields.
  - The intended supported-field command shape is visible in test failure or
    assertion output.
- **Validation**:
  - `cargo test -p nils-forge-cli pr_checks_github`

### Task 1.2: Pin required-only behavior expectations

- **Location**:
  - `crates/forge-cli/tests/integration/pr_wait_checks.rs`
  - `crates/forge-cli/tests/integration/required_check_gate.rs`
  - `crates/forge-cli/tests/integration/pr_deliver_chain.rs`
- **Description**: Add or update tests showing how required-only gating should
  behave when GitHub exposes required checks through `gh pr checks --required`
  instead of an `isRequired` JSON field.
- **Dependencies**:
  - Task 1.1
- **Complexity**: 4
- **Acceptance criteria**:
  - Passing required checks produce `state=success`.
  - Failed required checks produce `checks_failed`.
  - Pending required checks remain pending or timeout through the existing wait
    path.
  - No-required-check behavior is classified deliberately.
- **Validation**:
  - `cargo test -p nils-forge-cli pr_wait_checks required_check_gate`

## Sprint 2: Fix forge-cli GitHub Checks Backend

**Goal**: Make the GitHub checks backend compatible with `gh 2.92.0` while
preserving the normalized payload and downstream gating behavior.

**Demo/Validation**:

- Commands:
  - `cargo test -p nils-forge-cli pr_checks_github`
  - `cargo test -p nils-forge-cli pr_wait_checks required_check_gate`
  - `cargo test -p nils-forge-cli pr_merge pr_deliver`
- Verify: all checks consumers use the fixed backend and no command requests
  unsupported GitHub JSON fields.

**PR grouping intent**: group
**Execution Profile**: serial

### Task 2.1: Replace unsupported GitHub checks fields

- **Location**:
  - `crates/forge-cli/src/ops/pr_checks.rs`
- **Description**: Change the GitHub backend command builder to request only
  fields supported by `gh 2.92.0`, and normalize payload state from `bucket` and
  `state`.
- **Dependencies**:
  - Task 1.1
- **Complexity**: 5
- **Acceptance criteria**:
  - No GitHub checks command requests `conclusion` or `isRequired`.
  - `state` normalization handles current `gh` values such as `SUCCESS`,
    `FAILURE`, `PENDING`, and `CANCELLED`.
  - Existing payload fields that cannot be sourced from `gh 2.92.0` are omitted
    or derived through documented logic, not guessed.
- **Validation**:
  - `cargo test -p nils-forge-cli pr_checks_github`

### Task 2.2: Implement required-only gating source

- **Location**:
  - `crates/forge-cli/src/ops/pr_checks.rs`
  - `crates/forge-cli/src/ops/pr_wait_checks.rs`
  - `crates/forge-cli/src/ops/required_check_gate.rs`
- **Description**: Route required-check gating through a `gh pr checks
  --required` snapshot or equivalent explicit path, while preserving all-check
  reporting for the normal payload when possible.
- **Dependencies**:
  - Task 2.1
- **Complexity**: 6
- **Acceptance criteria**:
  - `required_only=true` gates only required checks.
  - `required_only=false` gates all reported checks.
  - No-required-check cases have a clear success or no-checks classification
    backed by tests.
  - Backend non-zero statuses caused by no required checks do not leak as generic
    backend failures.
- **Validation**:
  - `cargo test -p nils-forge-cli pr_wait_checks required_check_gate`

### Task 2.3: Verify merge and deliver consumers

- **Location**:
  - `crates/forge-cli/src/ops/pr_merge.rs`
  - `crates/forge-cli/src/macros/pr_deliver.rs`
  - `crates/forge-cli/tests/integration/pr_merge.rs`
  - `crates/forge-cli/tests/integration/pr_deliver_chain.rs`
- **Description**: Ensure `pr merge` and `pr deliver` call the fixed checks
  backend and keep their existing lock-down semantics.
- **Dependencies**:
  - Task 2.2
- **Complexity**: 4
- **Acceptance criteria**:
  - Merge re-check no longer fails on unsupported fields.
  - Deliver macro wait step records fixed checks payloads.
  - Existing draft/base/method safeguards remain intact.
- **Validation**:
  - `cargo test -p nils-forge-cli pr_merge pr_deliver`

## Sprint 3: Workspace Gate And Release Preparation

**Goal**: Run the broader nils-cli validation stack, update release metadata, and
prepare a focused `0.17.0` release boundary.

**Demo/Validation**:

- Commands:
  - `cargo fmt -p nils-forge-cli`
  - `cargo clippy -p nils-forge-cli --all-targets -- -D warnings`
  - `cargo test -p nils-forge-cli`
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh`
- Verify: local gates pass and release notes describe the GitHub checks
  compatibility fix.

**PR grouping intent**: per-sprint
**Execution Profile**: serial

### Task 3.1: Update docs and changelog for the compatibility fix

- **Location**:
  - `crates/forge-cli/README.md`
  - `crates/forge-cli/CHANGELOG.md` if present
  - root release/changelog files used by the current release workflow
- **Description**: Record the `gh 2.92.0` compatibility fix and downstream
  reason for the `0.17.0` release.
- **Dependencies**:
  - Task 2.3
- **Complexity**: 2
- **Acceptance criteria**:
  - Release notes identify the checks backend compatibility fix.
  - Downstream `agent-runtime-kit` Plan 05 dependency is referenced without
    making `agent-runtime-kit` changes in this repo.
- **Validation**:
  - `rg -n "gh 2\\.92\\.0|0\\.17\\.0|checks" crates/forge-cli CHANGELOG.md docs`

### Task 3.2: Run full local gate

- **Location**:
  - workspace root
  - `scripts/ci/nils-cli-checks-entrypoint.sh`
- **Description**: Run targeted and full workspace validation before release.
- **Dependencies**:
  - Task 3.1
- **Complexity**: 3
- **Acceptance criteria**:
  - Targeted forge-cli tests pass.
  - The canonical workspace entrypoint passes.
  - Any skipped optional check is recorded with its reason.
- **Validation**:
  - `cargo test -p nils-forge-cli`
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh`

## Sprint 4: Release 0.17.0 And Downstream Handoff

**Goal**: Cut the focused `nils-cli 0.17.0` release and record the handoff back
to the downstream `agent-runtime-kit` issue.

**Demo/Validation**:

- Commands:
  - standard nils-cli release workflow for `0.17.0`
  - `forge-cli --version`
  - `gh release view v0.17.0 --repo sympoies/nils-cli`
- Verify: released binary reports `0.17.0`, GitHub release exists, and
  downstream issue #26 has the handoff comment.

**PR grouping intent**: per-sprint
**Execution Profile**: serial

### Task 4.1: Cut nils-cli 0.17.0

- **Location**:
  - workspace version files
  - release scripts and generated release artifacts
  - `sympoies/homebrew-tap` if the standard release workflow requires it
- **Description**: Execute the existing nils-cli release workflow for `0.17.0`.
- **Dependencies**:
  - Task 3.2
- **Complexity**: 5
- **Acceptance criteria**:
  - Source tag and GitHub release for `v0.17.0` exist.
  - Local installed `forge-cli --version` reports `0.17.0` after release
    verification.
  - Homebrew tap state is updated if that is part of the current release
    contract.
- **Validation**:
  - `forge-cli --version`
  - `gh release view v0.17.0 --repo sympoies/nils-cli`

### Task 4.2: Record downstream handoff

- **Location**:
  - `graysurf/agent-runtime-kit` issue #26
  - `graysurf/agent-runtime-kit` extraction backlog in a downstream follow-up,
    not this nils-cli plan
- **Description**: Comment on the downstream issue with the `0.17.0` release
  evidence and the next expected `agent-runtime-kit` action.
- **Dependencies**:
  - Task 4.1
- **Complexity**: 2
- **Acceptance criteria**:
  - Issue #26 links the nils-cli tracking issue.
  - Issue #26 links or names `nils-cli 0.17.0` as the fixed release once
    available.
  - Downstream work remains responsible for bumping skill floors and closing
    `P5-S5-G1`.
- **Validation**:
  - `gh issue view 26 --repo graysurf/agent-runtime-kit --comments`

## Risks And Gotchas

- Required-check semantics are the risky part of this fix. Removing `isRequired`
  from the JSON payload without adding an explicit required-check source would
  make gating unsafe.
- `gh pr checks --required` behavior may differ for merged PRs, branches with no
  required checks, and branches with pending checks. Tests should cover these as
  separate cases.
- Keep this release focused. Mixing unrelated fixes into `0.17.0` makes the
  downstream compatibility boundary harder to audit.
- Do not claim `agent-runtime-kit` Plan 05 Sprint 6 is unblocked until the
  released `forge-cli` binary is verified on PATH.

## Completion Criteria

- The GitHub checks compatibility fix is merged in `sympoies/nils-cli`.
- `nils-cli 0.17.0` is released and verified locally.
- The nils-cli tracking issue records validation and release evidence.
- `agent-runtime-kit` issue #26 records that the nils-cli fix issue and release
  handoff exist.
