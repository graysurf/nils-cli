# Plan: Support Matrix Render Target And Existing-Issue Attach

## Overview

Add the nils-cli support required to finish
`graysurf/agent-runtime-kit#69`: a shared `agent-runtime` render target for
the generated support matrix, and a `plan-issue` lifecycle command that can
attach v3 source, plan, and state comments to an already-open provider issue.

## Read First

- Primary source: docs/plans/support-matrix-render-target/support-matrix-render-target-discussion-source.md
- Source type: discussion-to-implementation-doc
- Open questions carried into execution: none

## Scope

- In scope:
  - Add `agent-runtime render --target support-matrix`.
  - Load and validate `manifests/surfaces.yaml` as typed data.
  - Render deterministic support matrix Markdown into `build/shared/`.
  - Add shared-target golden and determinism coverage.
  - Add `plan-issue record attach` for existing provider issues.
  - Use the local binaries to finish agent-runtime-kit issue #69 after the
    nils-cli changes are validated locally.
- Out of scope:
  - Releasing nils-cli.
  - Mutating live runtime homes.
  - Accepting retired marker families as current lifecycle evidence.
  - Parsing `SUPPORT_MATRIX.md` as a data source.
  - Changing `plan-tooling` validation semantics.

## Assumptions

1. GitHub is the only provider needed for the first `record attach`
   implementation.
2. `manifests/surfaces.yaml` is the authoritative source for support matrix
   row data.
3. Existing product renders must keep their current CLI behavior and output
   tree.
4. The agent-runtime-kit consumer can temporarily use a local nils-cli binary
   before a formal release.

## Sprint 1: Support Matrix Render Target

**Goal**: Teach `agent-runtime` to render the shared support matrix from a
manifest-backed source of truth.

**PR grouping intent**: group
**Execution Profile**: serial

### Task 1.1: Add shared target CLI routing

- **Location**:
  - `crates/agent-runtime-cli/src/commands/render.rs`
  - `crates/agent-runtime-cli/src/render/mod.rs`
- **Description**: Add a `--target support-matrix` option that routes to a
  shared render path while preserving the current product render default.
- **Dependencies**:
  - none
- **Complexity**: 3
- **Acceptance criteria**:
  - `agent-runtime render --product codex` behaves as before.
  - `agent-runtime render --target support-matrix` does not require a product
    argument for output location.
  - Help text documents the target and output path.
- **Validation**:
  - `cargo test -p agent-runtime-cli render_help`

### Task 1.2: Load `surfaces.yaml`

- **Location**:
  - `crates/agent-runtime-cli/src/render/manifest.rs`
  - `crates/agent-runtime-cli/src/render/support_matrix.rs`
- **Description**: Add typed deserialization for the support surfaces
  manifest used by the shared render target.
- **Dependencies**:
  - Task 1.1
- **Complexity**: 5
- **Acceptance criteria**:
  - Schema version is checked.
  - Unknown fields are rejected.
  - Product keys are closed to `codex` and `claude`.
  - Invalid acceptance entries fail with actionable errors.
- **Validation**:
  - `cargo test -p agent-runtime-cli support_matrix_manifest`

### Task 1.3: Render deterministic Markdown

- **Location**:
  - `crates/agent-runtime-cli/src/render/support_matrix.rs`
  - `crates/agent-runtime-cli/tests/integration/render.rs`
- **Description**: Render support matrix Markdown into
  `build/shared/SUPPORT_MATRIX.md` from the typed manifest.
- **Dependencies**:
  - Task 1.2
- **Complexity**: 6
- **Acceptance criteria**:
  - Output has a generated-file header naming `manifests/surfaces.yaml`.
  - Row ordering is deterministic.
  - The generated table includes both product rows for each surface.
  - Invalid source paths cannot escape the source root.
- **Validation**:
  - `cargo test -p agent-runtime-cli render_support_matrix`

### Task 1.4: Add shared golden and determinism coverage

- **Location**:
  - `crates/agent-runtime-cli/tests/integration/render.rs`
  - `crates/agent-runtime-cli/tests/integration/render_determinism.rs`
  - `crates/agent-runtime-cli/tests/fixtures/`
- **Description**: Add fixture coverage that proves the shared target is
  stable across repeated runs and can refresh expected output.
- **Dependencies**:
  - Task 1.3
- **Complexity**: 4
- **Acceptance criteria**:
  - Shared target render writes the expected Markdown.
  - Repeated renders are byte-identical.
  - Golden update does not touch product golden trees.
- **Validation**:
  - `cargo test -p agent-runtime-cli render`
  - `cargo test -p agent-runtime-cli render_determinism`

## Sprint 2: Existing-Issue Lifecycle Attach

**Goal**: Let `plan-issue` own source/plan/state lifecycle backfill for
already-open issues such as agent-runtime-kit #69.

**PR grouping intent**: group
**Execution Profile**: serial

### Task 2.1: Define `record attach`

- **Location**:
  - `crates/plan-issue-cli/src/commands/record.rs`
  - `crates/plan-issue-cli/docs/specs/issue-backed-plan-record-contract-v2.md`
  - `crates/plan-issue-cli/tests/integration/cli_contract.rs`
- **Description**: Add `plan-issue record attach --issue ISSUE --bundle DIR`
  for existing provider issues.
- **Dependencies**:
  - none
- **Complexity**: 4
- **Acceptance criteria**:
  - The command derives source, plan, and execution-state paths from the
    bundle naming convention.
  - Dry-run prints every planned provider mutation.
  - Live mode can target an existing GitHub issue.
  - Source and plan still remain rejected by generic `record post`.
- **Validation**:
  - `cargo test -p nils-plan-issue-cli cli_contract`

### Task 2.2: Implement provider-backed attach

- **Location**:
  - `crates/plan-issue-cli/src/execute.rs`
  - `crates/plan-issue-cli/src/github.rs`
  - `crates/plan-issue-cli/src/lifecycle_record.rs`
  - `crates/plan-issue-cli/tests/integration/live_issue_ops.rs`
- **Description**: Reuse the v3 lifecycle rendering used by `record open`,
  but post comments to an existing issue and then repair/audit the dashboard.
- **Dependencies**:
  - Task 2.1
- **Complexity**: 7
- **Acceptance criteria**:
  - Live attach posts canonical source, plan, and initial state comments.
  - Dashboard repair uses the recognized comment URLs.
  - Result JSON includes issue URL and source/plan/state comment URLs.
  - Audit after attach reports no missing required tracking evidence.
- **Validation**:
  - `cargo test -p nils-plan-issue-cli live_record_attach`

### Task 2.3: Refresh generated CLI surfaces

- **Location**:
  - `crates/plan-issue-cli/tests/fixtures/`
  - `completions/`
  - `docs/`
- **Description**: Update output contracts, docs, and shell completions for
  the new attach command and render target.
- **Dependencies**:
  - Task 1.1
  - Task 2.1
- **Complexity**: 3
- **Acceptance criteria**:
  - Help/output contract tests pass.
  - bash and zsh completions include `record attach`.
  - Documentation explains when to use `record open` versus `record attach`.
- **Validation**:
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only`

## Sprint 3: Agent-Runtime-Kit Consumer Verification

**Goal**: Prove the nils-cli changes unblock agent-runtime-kit issue #69 with
local binaries.

**PR grouping intent**: group
**Execution Profile**: serial

### Task 3.1: Render support matrix in agent-runtime-kit

- **Location**:
  - `graysurf/agent-runtime-kit` checkout
- **Description**: Run the local `agent-runtime` binary against the
  agent-runtime-kit checkout and verify `build/shared/SUPPORT_MATRIX.md`.
- **Dependencies**:
  - Task 1.4
- **Complexity**: 3
- **Acceptance criteria**:
  - Local render exits successfully.
  - Generated Markdown is deterministic across repeated runs.
  - Agent-runtime-kit can copy or promote the generated output into the root
    support matrix without manual row editing.
- **Validation**:
  - `agent-runtime render --source-root $HOME.codex/worktrees/f43c/agent-runtime-kit --target support-matrix`

### Task 3.2: Attach v3 lifecycle comments to #69

- **Location**:
  - `graysurf/agent-runtime-kit#69`
- **Description**: Use local `plan-issue record attach` to add v3 source,
  plan, and state lifecycle comments to the existing issue.
- **Dependencies**:
  - Task 2.3
- **Complexity**: 3
- **Acceptance criteria**:
  - The command mutates #69 through `plan-issue`, not raw `gh` comments.
  - `record audit` recognizes source, plan, and state evidence.
  - The dashboard points at the generated lifecycle comment URLs.
- **Validation**:
  - `plan-issue record audit --profile tracking --body-file "$ARKIT_ISSUE_BODY" --comments-json "$ARKIT_ISSUE_JSON"`

## Validation Summary

- `plan-tooling validate --file docs/plans/support-matrix-render-target/support-matrix-render-target-plan.md --format text --explain`
- `cargo test -p agent-runtime-cli render`
- `cargo test -p agent-runtime-cli render_determinism`
- `cargo test -p nils-plan-issue-cli cli_contract`
- `cargo test -p nils-plan-issue-cli live_record_attach`
- `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast`

## Rollback

- If the shared render target fails consumer verification, remove the
  `--target support-matrix` routing and keep product rendering unchanged.
- If `record attach` fails live provider verification, keep the command hidden
  behind failing tests and do not use it on agent-runtime-kit #69.
