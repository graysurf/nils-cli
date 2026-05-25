# Plan: plan-issue `record close` Non-Required Check Gate Fix

## Overview

Land a focused fix in `plan-issue-cli` so that `record close --linked-pr ...`
no longer treats non-required GitHub `statusCheckRollup` failures as hard
close blockers. The gate must:

- Only fail with `linked-pr-not-merged` when the PR is truly unmerged.
- Fail with the contract-listed `linked-pr-checks-failed` code when required
  checks fail (or required-check state is unknown).
- Treat non-required failures as informational evidence in the closeout
  comment, never as a blocker.
- Expose an explicit, evidence-emitting override
  (`--allow-non-required-check-failure`) for the degraded-provider edge case.

Issue: sympoies/nils-cli#502.

## Read First

- Primary source:
  `docs/plans/plan-issue-close-non-required-checks/plan-issue-close-non-required-checks-discussion-source.md`
- Source type: discussion-to-implementation-doc
- Open questions carried into execution: none
- Upstream contract:
  `crates/plan-issue-cli/docs/specs/issue-backed-plan-record-contract-v2.md:318`
  (`linked-pr-missing` / `linked-pr-not-merged` / `linked-pr-checks-failed`)
- Existing required-only classifier reused as-is:
  `crates/forge-cli/src/ops/required_check_gate.rs:46-98`

## Scope

- In scope:
  - `CheckStatus` / `LinkedPrEvidence` shape updates in
    `crates/plan-issue-cli/src/lifecycle_record.rs`.
  - Strict-closeout-gate logic update in the same file.
  - `record close` linked-PR resolution in
    `crates/plan-issue-cli/src/execute.rs` (live + fixture paths).
  - GitHub and GitLab adapter return-shape changes
    (`crates/plan-issue-cli/src/github.rs`,
    `crates/plan-issue-cli/src/forge_cli_adapter.rs`).
  - New `--allow-non-required-check-failure` and
    `--allow-non-required-check-failure-reason` flags on `RecordCloseArgs`
    in `crates/plan-issue-cli/src/commands/record.rs`.
  - Fixture additions under
    `crates/plan-issue-cli/tests/fixtures/lifecycle/` and integration
    coverage in
    `crates/plan-issue-cli/tests/integration/live_record_ops.rs`.
  - Closeout-comment evidence block addition when the override is used.
  - CHANGELOG and `--help` updates.
- Out of scope:
  - Any change to `forge-cli pr_checks` or `required_check_gate`.
  - Any change to `pr_merge_summary` callers other than `record close`.
  - Reworking the GitLab pipeline-vs-required mapping (GitLab has no
    first-class required concept; we fall back to aggregate state when
    `required_count` is unresolvable and document it).
  - Backfilling historical closeout comments.

## Sprint 1: Required-check-aware close gate

**Goal**: Make `record close` gate on required-check status, distinguish
`linked-pr-checks-failed` from `linked-pr-not-merged`, and ship the
override flag with explicit evidence emission.

**Demo/Validation**:

- Commands:
  - `cargo test -p plan-issue-cli`
  - `cargo build --release -p plan-issue-cli`
  - Fixture-mode rerun of `record close` against a
    "merged + required pass + non-required fail" PR snapshot.
- Verify: All three fixture scenarios (PR merged + zero required +
  non-required fail; PR merged + required pass + non-required fail;
  PR merged + required fail) gate as documented.

### Task 1.1: Widen `CheckStatus` / `LinkedPrEvidence` to carry required-check detail

- **Location**:
  - `crates/plan-issue-cli/src/lifecycle_record.rs:1066-1080`
- **Description**: Replace the single-state `CheckStatus` with a struct (or
  expand `LinkedPrEvidence`) that records: aggregate state, required-check
  state, required count, and the list of failing non-required check names.
  Keep `Default` semantics equivalent to today's `CheckStatus::None`.
- **Dependencies**: none
- **Complexity**: 2
- **Acceptance criteria**:
  - Existing call sites compile (callers that only know aggregate state
    pass `None` for required fields).
  - Unit tests cover serde + display for the new shape.
- **Validation**:
  - `cargo test -p plan-issue-cli lifecycle_record`

### Task 1.2: Rewrite the linked-PR branch of `evaluate_strict_closeout_gate`

- **Location**:
  - `crates/plan-issue-cli/src/lifecycle_record.rs:1755-1782`
- **Description**: Replace the single
  `!matches!(pr.checks, CheckStatus::Pass | CheckStatus::None)` test with the
  required-only rule: block when `merge_sha` is missing
  (`linked-pr-not-merged`); block when required-check state is `Fail` or
  unknown-with-required-count>0 (`linked-pr-checks-failed`); pass otherwise.
  Honor the override flag by skipping the required-state check when set,
  while still recording the observed failures.
- **Dependencies**: Task 1.1
- **Complexity**: 2
- **Acceptance criteria**:
  - Gate emits `linked-pr-not-merged` only for unmerged PRs.
  - Gate emits `linked-pr-checks-failed` for required failures.
  - Non-required failures alone never block.
  - Override path emits a `record-close-allow-non-required` evidence entry.
- **Validation**:
  - `cargo test -p plan-issue-cli lifecycle_record`

### Task 1.3: Expand provider adapters to return required-check detail

- **Location**:
  - `crates/plan-issue-cli/src/github.rs:64,348-383,477-518`
  - `crates/plan-issue-cli/src/forge_cli_adapter.rs:305-349`
- **Description**: Extend `PrMergeSummary` (or add a sibling type) with
  `required_state: Option<String>`, `required_count: Option<u32>`, and
  `non_required_failures: Vec<String>`. The GitHub adapter calls
  `forge-cli`'s required-only path (or replicates the `required: bool`
  filter against GitHub's GraphQL output) to populate these. The GitLab
  adapter populates what it can from `forge-cli pr checks` and leaves the
  required fields `None` when unavailable.
- **Dependencies**: Task 1.1
- **Complexity**: 3
- **Acceptance criteria**:
  - GitHub adapter returns populated required fields when checks exist.
  - GitLab adapter leaves required fields `None` and is documented as such
    in the spec comment.
  - Unit tests cover the parse paths for: zero required + one non-required
    fail; one required pass + one non-required fail; one required fail.
- **Validation**:
  - `cargo test -p plan-issue-cli github`
  - `cargo test -p plan-issue-cli forge_cli_adapter`

### Task 1.4: Wire `record close` and fixture snapshot to the new shape

- **Location**:
  - `crates/plan-issue-cli/src/execute.rs:488-534,1160-1204`
- **Description**: Update `read_fixture_pr_snapshot` to read the new
  required-check fields from PR fixtures and update the live linked-PR
  loop to consume the adapter's extended return type. Pass the override
  flag plus reason string into the `StrictCloseoutGateInput`.
- **Dependencies**: Task 1.2, Task 1.3
- **Complexity**: 2
- **Acceptance criteria**:
  - Live and fixture paths produce equivalent `LinkedPrEvidence` shapes.
  - `record close` JSON envelope includes the new fields under linked-PR
    evidence.
- **Validation**:
  - `cargo test -p plan-issue-cli execute`

### Task 1.5: Add CLI flag and closeout-comment evidence emission

- **Location**:
  - `crates/plan-issue-cli/src/commands/record.rs:238-286`
  - Closeout-comment renderer in `crates/plan-issue-cli/src/lifecycle_record.rs`
- **Description**: Add
  `--allow-non-required-check-failure` (bool) and
  `--allow-non-required-check-failure-reason` (string, required when the
  bool is set) to `RecordCloseArgs`. Wire the values into the gate input
  (Task 1.4). On override, the rendered closeout comment includes a
  dedicated `Override:` block listing the reason and the observed
  non-required failures. Missing reason fails with
  `record-close-override-reason-missing`.
- **Dependencies**: Task 1.2, Task 1.4
- **Complexity**: 1
- **Acceptance criteria**:
  - `plan-issue --help record close` documents both flags.
  - Override exercise emits the dedicated evidence block in the comment.
- **Validation**:
  - `cargo test -p plan-issue-cli commands::record`

### Task 1.6: Fixtures and integration coverage

- **Location**:
  - `crates/plan-issue-cli/tests/fixtures/lifecycle/...` (add new fixtures)
  - `crates/plan-issue-cli/tests/integration/live_record_ops.rs:648,718`
- **Description**: Add three PR fixtures covering: (a) merged +
  `required_count = 0` + one non-required fail; (b) merged + one required
  pass + one non-required fail; (c) merged + one required fail. Add three
  integration tests:
  - `record_close_passes_with_non_required_failure_when_zero_required`
  - `record_close_passes_with_non_required_failure_when_required_pass`
  - `record_close_blocks_with_linked_pr_checks_failed_when_required_fail`
  - `record_close_override_emits_allow_non_required_evidence`
- **Dependencies**: Task 1.4, Task 1.5
- **Complexity**: 2
- **Acceptance criteria**:
  - All four new integration tests pass.
  - Existing close-gate tests remain unmodified except for any required
    field additions to their fixtures.
- **Validation**:
  - `cargo test -p plan-issue-cli --test integration`

### Task 1.7: CHANGELOG, help text, and spec wiring

- **Location**:
  - `crates/plan-issue-cli/CHANGELOG.md`
  - `crates/plan-issue-cli/docs/specs/issue-backed-plan-record-contract-v2.md:318`
- **Description**: Add a CHANGELOG entry referencing issue #502 and the new
  `linked-pr-checks-failed` wiring. Tighten the spec line to explicitly state
  that `linked-pr-not-merged` is reserved for `merge_sha`-missing PRs and
  that `linked-pr-checks-failed` covers required-check failures (or
  unknown required state with `required_count > 0`). Mention the override
  flag and the override evidence block.
- **Dependencies**: Task 1.6
- **Complexity**: 1
- **Acceptance criteria**:
  - CHANGELOG and spec lines reflect the new behavior.
  - `plan-issue record close --help` matches the spec wording.
- **Validation**:
  - `cargo test -p plan-issue-cli` (covers contract-spec consistency tests
    if any).

### Task 1.8: Build, install, and re-run the original closeout scenario

- **Location**:
  - `~/.local/nils-cli/plan-issue` (install target)
  - `target/release/plan-issue` (build artifact)
- **Description**: Rebuild `plan-issue-cli` with the new gate, install
  locally, and re-run the `graysurf/agent-runtime-kit#69` closeout shape in
  fixture mode using a snapshot built from #103. The original failure must
  no longer surface, without using the override flag.
- **Dependencies**: Task 1.7
- **Complexity**: 1
- **Acceptance criteria**:
  - Fixture-mode close runs with `ok=true` and an empty
    `blocked_codes` list for the original PR shape.
- **Validation**:
  - `cargo test -p nils-plan-issue-cli record_close_fixture_passes_with_non_required_failure_when_zero_required`
