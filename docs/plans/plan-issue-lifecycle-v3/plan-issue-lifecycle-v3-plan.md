# Plan: Plan-Issue Lifecycle v3

## Overview

Rewrite `plan-issue` around one issue-backed lifecycle contract for
agent-runtime-kit and future dispatch work. The new surface removes legacy
compat markers, stops exposing closeout as a caller-assembled sequence, and
makes `plan-issue` own provider-backed issue record open, post, audit, repair,
and close operations.

## Read First

- Primary source: docs/plans/plan-issue-lifecycle-v3/plan-issue-lifecycle-v3-discussion-source.md
- Source type: discussion-to-implementation-doc
- Open questions carried into execution: none

## Scope

- In scope:
  - Define the breaking issue-backed plan record v3 contract.
  - Replace dual marker families with one canonical marker family.
  - Add structured lifecycle payload parsing for state, validation, review, and
    closeout evidence.
  - Add high-level live commands for opening, posting to, auditing, repairing,
    and closing issue-backed plan records.
  - Make closeout strict by default and provider-verify linked PR evidence.
  - Retire or isolate old Task Decomposition lifecycle commands from the
    primary `plan-issue` CLI surface.
  - Update docs, tests, completions, and output-contract coverage.
  - Add an agent-runtime-kit closeout fixture to prove the new CLI covers the
    workflow that exposed the current defects.
- Out of scope:
  - Migrating agent-runtime-kit skills before a nils-cli release exists.
  - Supporting legacy marker families.
  - Preserving old `record closeout-gate --require-*` behavior.
  - Changing `plan-tooling` plan parsing.
  - Releasing Homebrew tap updates in the implementation PR itself.

## Assumptions

1. The target consumer is agent-runtime-kit on GitHub.
2. GitHub support is sufficient for the first v3 implementation.
3. The old `start-plan` / `start-sprint` Task Decomposition runtime can be
   retired from the primary surface or moved behind an explicit legacy module.
4. The v3 CLI may break existing generated completions and docs because no
   backward compatibility is required.
5. Provider-verified PR state is mandatory for live closeout, while local
   fixture inputs remain available for deterministic tests.

## Sprint 1: Contract And Command Design

**Goal**: Land the v3 spec and CLI contract before touching the Rust command
model.

**PR grouping intent**: group
**Execution Profile**: serial

### Task 1.1: Replace the issue-backed record contract with v3

- **Location**:
  - `crates/plan-issue-cli/docs/specs/issue-backed-plan-record-contract-v1.md`
  - `crates/plan-issue-cli/docs/specs/issue-backed-plan-record-contract-v2.md`
  - `crates/plan-issue-cli/docs/specs/plan-issue-state-machine-v1.md`
  - `crates/plan-issue-cli/docs/README.md`
- **Description**: Replace the v1 local-rendering contract with a v3
  issue-backed lifecycle contract. Define the canonical marker, structured
  payload schema, live command responsibilities, strict closeout gate, and
  GitHub provider boundary.
- **Dependencies**:
  - none
- **Complexity**: 4
- **Acceptance criteria**:
  - The spec states that legacy compat markers are retired.
  - The spec defines one marker family for source, plan, state, session,
    validation, review, and closeout comments.
  - The spec defines the provider-backed command boundary for open, post,
    audit, repair-dashboard, and close.
  - The state machine no longer treats the Task Decomposition runtime as the
    primary agent-runtime-kit lifecycle.
- **Validation**:
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only`

### Task 1.2: Define the high-level CLI surface

- **Location**:
  - `crates/plan-issue-cli/src/commands/record.rs`
  - `crates/plan-issue-cli/docs/specs/issue-backed-plan-record-contract-v2.md`
  - `crates/plan-issue-cli/tests/integration/cli_contract.rs`
- **Description**: Define the new command tree and argument contract. The
  intended shape is `plan-issue record open`, `post`, `audit`,
  `repair-dashboard`, and `close`, with bundle-first inputs for normal use and
  fixture inputs for tests.
- **Dependencies**:
  - Task 1.1
- **Complexity**: 5
- **Acceptance criteria**:
  - `--marker-family` is removed.
  - `--require-complete`, `--require-session`, `--require-validation`,
    `--require-review`, and `--require-closeout` are removed from closeout.
  - `record close` accepts issue, linked PR, approval evidence, and optional
    fixture paths for deterministic tests.
  - `record open` accepts a plan bundle path and can derive source, plan, and
    execution-state paths.
- **Validation**:
  - `cargo test -p nils-plan-issue-cli cli_contract`

### Task 1.3: Specify structured lifecycle payloads

- **Location**:
  - `crates/plan-issue-cli/docs/specs/issue-backed-plan-record-contract-v2.md`
  - `crates/plan-issue-cli/src/lifecycle_record.rs`
  - `crates/plan-issue-cli/tests/integration/lifecycle_record.rs`
- **Description**: Define a structured JSON or YAML payload block for lifecycle
  comments so audit does not infer state from prose lines.
- **Dependencies**:
  - Task 1.1
- **Complexity**: 4
- **Acceptance criteria**:
  - State comments expose machine-readable `status`, `tasks`, `prs`, and
    `updated_at` fields.
  - Validation comments expose command rows with pass/fail status.
  - Review comments expose findings and decision.
  - Closeout comments expose final checks and merge evidence.
- **Validation**:
  - `cargo test -p nils-plan-issue-cli lifecycle_record_structured_payloads`

## Sprint 2: Rust Lifecycle Core Rewrite

**Goal**: Replace the marker/parser/rendering internals with the v3 data model.

**PR grouping intent**: group
**Execution Profile**: serial

### Task 2.1: Collapse marker parsing to the canonical family

- **Location**:
  - `crates/plan-issue-cli/src/lifecycle_record.rs`
  - `crates/plan-issue-cli/tests/integration/lifecycle_record.rs`
- **Description**: Remove compat marker rendering and parsing. Audit should
  recognize only the canonical v3 marker family and should reject quoted or
  legacy markers.
- **Dependencies**:
  - Task 1.3
- **Complexity**: 5
- **Acceptance criteria**:
  - `MarkerFamily` is removed.
  - Tracking compat review marker errors disappear because there is no compat
    branch.
  - Legacy markers are ignored or reported as unsupported, not accepted as
    current lifecycle evidence.
  - Latest-marker selection is deterministic by role and comment timestamp.
- **Validation**:
  - `cargo test -p nils-plan-issue-cli lifecycle_record`

### Task 2.2: Implement structured audit output

- **Location**:
  - `crates/plan-issue-cli/src/lifecycle_record.rs`
  - `crates/plan-issue-cli/src/execute.rs`
  - `crates/plan-issue-cli/tests/integration/lifecycle_record.rs`
- **Description**: Update audit to return typed source, plan, state, session,
  validation, review, closeout, dashboard, and PR-reference evidence from the
  structured payloads.
- **Dependencies**:
  - Task 2.1
- **Complexity**: 6
- **Acceptance criteria**:
  - Audit JSON exposes the latest URL, created timestamp, role, status, and
    parsed payload for each lifecycle role.
  - Missing required evidence is reported with stable machine-readable codes.
  - Prose Markdown can change without breaking closeout state detection.
  - Audit can run from live issue reads or fixture body/comments files.
- **Validation**:
  - `cargo test -p nils-plan-issue-cli lifecycle_record_audit`

### Task 2.3: Render dashboards from audit evidence

- **Location**:
  - `crates/plan-issue-cli/src/lifecycle_record.rs`
  - `crates/plan-issue-cli/tests/integration/lifecycle_record.rs`
- **Description**: Make final and current dashboards derive durable-record
  links from audit evidence instead of requiring callers to pass every URL.
- **Dependencies**:
  - Task 2.2
- **Complexity**: 4
- **Acceptance criteria**:
  - `record repair-dashboard` can render a complete dashboard from issue audit.
  - Missing optional evidence appears as `pending` with stable status.
  - Complete issues render `## Final Dashboard`; incomplete issues render
    `## Current Dashboard`.
  - Dashboard output remains human-readable and Markdown-safe.
- **Validation**:
  - `cargo test -p nils-plan-issue-cli lifecycle_record_dashboard`

## Sprint 3: Provider-Backed Lifecycle Commands

**Goal**: Make `plan-issue` own live issue-backed lifecycle mutations for the
agent-runtime-kit flow.

**PR grouping intent**: group
**Execution Profile**: serial

### Task 3.1: Implement `record open`

- **Location**:
  - `crates/plan-issue-cli/src/commands/record.rs`
  - `crates/plan-issue-cli/src/execute.rs`
  - `crates/plan-issue-cli/src/github.rs`
  - `crates/plan-issue-cli/tests/integration/live_issue_ops.rs`
- **Description**: Add a bundle-first command that validates the plan bundle,
  creates the provider issue, posts source/plan/initial-state comments, repairs
  the dashboard with exact comment URLs, and audits the final record.
- **Dependencies**:
  - Task 2.3
- **Complexity**: 7
- **Acceptance criteria**:
  - The command derives source, plan, and execution-state paths from
    `--bundle`.
  - Live mode requires committed and clean local source/plan files.
  - Dry-run mode renders every provider mutation plan without writing.
  - The result JSON includes issue URL and source, plan, and state comment
    URLs.
- **Validation**:
  - `cargo test -p nils-plan-issue-cli live_record_open`

### Task 3.2: Implement `record post`

- **Location**:
  - `crates/plan-issue-cli/src/commands/record.rs`
  - `crates/plan-issue-cli/src/execute.rs`
  - `crates/plan-issue-cli/tests/integration/live_issue_ops.rs`
- **Description**: Add a high-level append-only comment command for state,
  session, validation, and review evidence. It should render canonical markers
  and structured payloads before posting.
- **Dependencies**:
  - Task 2.2
- **Complexity**: 5
- **Acceptance criteria**:
  - Supported kinds are state, session, validation, review, and closeout.
  - Posted comments always include structured payloads.
  - Dry-run prints the exact comment body and provider action plan.
  - The result JSON includes the posted comment URL.
- **Validation**:
  - `cargo test -p nils-plan-issue-cli live_record_post`

### Task 3.3: Implement strict `record close`

- **Location**:
  - `crates/plan-issue-cli/src/commands/record.rs`
  - `crates/plan-issue-cli/src/execute.rs`
  - `crates/plan-issue-cli/src/github.rs`
  - `crates/plan-issue-cli/tests/integration/live_issue_ops.rs`
- **Description**: Add one close command that fetches issue evidence, verifies
  strict lifecycle readiness, verifies linked PR merge/check state through the
  provider, posts closeout, repairs dashboard, and closes the issue.
- **Dependencies**:
  - Task 2.2
  - Task 2.3
  - Task 3.2
- **Complexity**: 8
- **Acceptance criteria**:
  - Close requires source, plan, complete state, session, validation, review,
    approval evidence, and merged linked PRs.
  - Linked PR evidence is checked through provider state, not text search.
  - The command supports dry-run and fixture mode for deterministic tests.
  - The result JSON includes closeout URL, final dashboard state, closed issue
    URL, linked PR statuses, and merge SHAs.
- **Validation**:
  - `cargo test -p nils-plan-issue-cli live_record_close`

## Sprint 4: Retire Legacy Surface And Refresh Contracts

**Goal**: Remove old lifecycle entrypoints from the primary CLI and update
generated assets.

**PR grouping intent**: group
**Execution Profile**: serial

### Task 4.1: Remove or isolate Task Decomposition commands

- **Location**:
  - `crates/plan-issue-cli/src/commands/mod.rs`
  - `crates/plan-issue-cli/src/commands/plan.rs`
  - `crates/plan-issue-cli/src/commands/sprint.rs`
  - `crates/plan-issue-cli/src/execute.rs`
  - `crates/plan-issue-cli/tests/integration/cli_contract.rs`
- **Description**: Remove legacy `start-plan`, `status-plan`, `link-pr`,
  `ready-plan`, `close-plan`, `start-sprint`, `ready-sprint`, and
  `accept-sprint` from the primary `plan-issue` help surface, or place them
  behind an explicit legacy-only binary if removal creates too much churn.
- **Dependencies**:
  - Task 3.3
- **Complexity**: 7
- **Acceptance criteria**:
  - Root help leads with the issue-backed record lifecycle.
  - Old Task Decomposition commands no longer appear in the primary command
    list.
  - Tests no longer treat `## Task Decomposition` as the default
    agent-runtime-kit lifecycle truth.
  - Removed command docs are deleted or marked retired.
- **Validation**:
  - `cargo test -p nils-plan-issue-cli cli_contract`

### Task 4.2: Refresh completions and output contract fixtures

- **Location**:
  - `completions/bash/plan-issue`
  - `completions/bash/plan-issue-local`
  - `completions/zsh/_plan-issue`
  - `completions/zsh/_plan-issue-local`
  - `crates/plan-issue-cli/tests/integration/parity_guardrails.rs`
  - `docs/specs/cli-output-contract-v1.md`
- **Description**: Regenerate completion assets and update CLI output contract
  expectations for the new command surface.
- **Dependencies**:
  - Task 4.1
- **Complexity**: 5
- **Acceptance criteria**:
  - Bash and zsh completions include the new lifecycle commands.
  - Removed command completions are gone.
  - `-V, --version`, `--format`, and JSON envelope expectations still pass.
  - Completion parity audits pass.
- **Validation**:
  - `bash scripts/ci/completion-asset-audit.sh --strict`
  - `bash scripts/ci/completion-flag-parity-audit.sh --strict`
  - `zsh -n completions/zsh/_plan-issue`
  - `bash -n completions/bash/plan-issue`

### Task 4.3: Add agent-runtime-kit closeout fixture coverage

- **Location**:
  - `crates/plan-issue-cli/tests/fixtures/lifecycle/agent-runtime-kit-closeout.json`
  - `crates/plan-issue-cli/tests/integration/lifecycle_record.rs`
  - `crates/plan-issue-cli/tests/integration/live_issue_ops.rs`
- **Description**: Add a sanitized fixture modeled on the shared Heuristic
  System closeout flow. The fixture should prove that one command can audit,
  repair, close, and report the record that previously required manual
  stitching.
- **Dependencies**:
  - Task 3.3
- **Complexity**: 5
- **Acceptance criteria**:
  - The fixture includes source, plan, state, session, validation, review, and
    closeout evidence.
  - The test verifies provider PR merge evidence is required.
  - The test fails if callers need to mix marker families.
  - The test fails if dashboard links must be supplied manually.
- **Validation**:
  - `cargo test -p nils-plan-issue-cli agent_runtime_kit_lifecycle_fixture`

## Sprint 5: Gate, Release Readiness, And Consumer Handoff

**Goal**: Prove the breaking CLI is ready for release and prepare the
agent-runtime-kit migration lane.

**PR grouping intent**: group
**Execution Profile**: serial

### Task 5.1: Run full nils-cli validation

- **Location**:
  - `scripts/ci/nils-cli-checks-entrypoint.sh`
  - `crates/plan-issue-cli/CHANGELOG.md`
  - `README.md`
- **Description**: Run the full local gate, update plan-issue changelog notes,
  and document the breaking v3 command surface in the workspace CLI map.
- **Dependencies**:
  - Task 4.3
- **Complexity**: 4
- **Acceptance criteria**:
  - The full local gate passes.
  - Changelog documents removed commands and replacement lifecycle commands.
  - Workspace README points users to the v3 issue-backed workflow.
  - No completion, docs hygiene, or CLI output contract audit fails.
- **Validation**:
  - `NILS_CLI_TEST_RUNNER=nextest bash scripts/ci/nils-cli-checks-entrypoint.sh`

### Task 5.2: Prepare release and agent-runtime-kit handoff notes

- **Location**:
  - `docs/plans/plan-issue-lifecycle-v3/plan-issue-lifecycle-v3-execution-state.md`
  - `crates/plan-issue-cli/docs/specs/issue-backed-plan-record-contract-v2.md`
  - `crates/plan-issue-cli/CHANGELOG.md`
- **Description**: Record release readiness and the downstream agent-runtime-kit
  migration steps that should happen only after the nils-cli release is cut and
  installed.
- **Dependencies**:
  - Task 5.1
- **Complexity**: 3
- **Acceptance criteria**:
  - The execution state identifies the exact follow-up needed in
    agent-runtime-kit.
  - The spec gives example commands for replacing current generated skills.
  - The closeout notes say whether a separate agent-runtime-kit issue is still
    needed after release.
- **Validation**:
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only`

## Issue Closeout Gate

The tracking issue is complete when:

- All Sprint 1-5 tasks are done or explicitly deferred.
- The implementation PR is merged.
- Specialist review evidence is posted.
- Local and provider CI pass.
- The issue dashboard is repaired by the new `plan-issue record close`
  command itself.
- A release handoff exists for agent-runtime-kit consumer migration.

The issue does not require an agent-runtime-kit PR. That consumer migration
belongs after the nils-cli release unless the implementation discovers that
agent-runtime-kit source changes are needed to validate the CLI contract.
