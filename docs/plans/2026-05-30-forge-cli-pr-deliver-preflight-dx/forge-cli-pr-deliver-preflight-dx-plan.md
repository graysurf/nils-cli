# Plan: forge-cli pr deliver preflight & DX

## Overview

Make `forge-cli pr deliver --dry-run` a faithful preflight: run the
existing non-mutating local validation rule-set (Rules 1a-5 in
`validations.rs`) during dry-run and report each rule's verdict
additively in the dry-run envelope, so a dry-run predicts whether the
real run's local gates will pass — without ever invoking a
provider-mutating backend step. Then two DX cleanups: aggregate the
body-section validation so a body missing both `## Summary` and
`## Test plan` returns one error listing both, and bring the
`agent-runtime pr-body` scaffold to parity with the six `pr deliver`
kinds while pointing the `body_missing_*` error at the scaffold.

Source: this bundle's discussion source doc (Read First, below). The
only open question (aggregated-error code shape) is resolved there to a
recommended default; no open questions are carried into execution.

## Read First

- Primary source:
  `docs/plans/forge-cli-pr-deliver-preflight-dx/forge-cli-pr-deliver-preflight-dx-discussion-source.md`
- Source type: discussion-to-implementation-doc
- Source issue: none (DX friction found 2026-05-30 while delivering a
  docs PR through `forge-cli pr deliver`)
- Open questions carried into execution: none (the aggregated body-error
  code shape is resolved to "keep per-section codes in `details`"; see
  the source doc).
- Implementation surface:
  - `crates/forge-cli/src/macros/pr_deliver.rs` (dry-run early return at
    `:109-110`; `emit_dry_run` at `:330`; deliver step sequence).
  - `crates/forge-cli/src/validations.rs` (Rules 1a-5: `branch_name`
    `:126`, `branch_kind_matches` `:194`, `body_summary` `:256`,
    `body_test_plan` `:273`, `title_length` `:225`, `worktree_clean`
    `:327`, `head_pushed` `:349`).
  - `crates/agent-runtime-cli` `pr-body render` (kind set).
  - `crates/forge-cli/src/cli.rs:180-187` (the six deliver kinds).
- Out of scope (tracked separately): changing the push contract /
  auto-pushing the head; any provider-mutating dry-run behavior;
  `wait_checks` / merge rework; an opt-in `--push` flag.

## Read First boundary

- Dry-run must stay provider-read-only: it may run local `git` / string
  validations but must never call a `gh pr create` / merge backend.
- Preserve existing error codes (`body_missing_summary`,
  `body_missing_test_plan`, `head_not_pushed`); the aggregated body
  error keeps per-section codes discoverable in `details`.
- No new third-party dependency, so `third-party-artifacts` and the
  `Cargo.lock` locked-build gate stay clean.

## Scope

- In scope:
  - A non-short-circuiting local-preflight runner over Rules 1a-5 that
    collects per-rule verdicts.
  - Wiring that runner into `pr deliver --dry-run` and adding the
    verdicts to the dry-run envelope additively.
  - Aggregating the body-section validation into one error listing all
    missing required sections.
  - `pr-body render` kind parity with the six deliver kinds (or a
    kind-agnostic Summary / Test-plan skeleton) and a scaffold pointer in
    the `body_missing_*` error `details`.
- Out of scope:
  - The push contract and an opt-in `--push` flag.
  - `wait_checks` / ready / merge behavior.
  - Changing the required heading defaults (`## Summary` /
    `## Test plan`).

## Assumptions

- Rules 1a-5 are all non-mutating (string inspection + local `git`
  queries), so running them in dry-run introduces no provider calls.
- The dry-run envelope (`cli.forge-cli.pr.deliver.v1`) can take an
  additive optional field without breaking existing JSON consumers.
- `agent-runtime pr-body render` is the intended body scaffold; bringing
  its kind set to parity does not change forge-cli's own validation.

## Sprint 1: dry-run becomes a faithful local preflight

**Goal**: `pr deliver --dry-run` runs Rules 1a-5 and reports each
verdict in the envelope, invoking no provider backend, so a single
dry-run surfaces a bad body and an unpushed head together.

**Demo/Validation**:

- Commands:
  - `cargo test -p forge-cli`
  - `forge-cli pr deliver --kind docs --title x --body "" --dry-run --format json`
- Verify: the dry-run JSON carries a local-preflight verdict block; a
  bad body plus an unpushed head both report `fail` in one run; no `gh`
  backend command is issued.

### Task 1.1: Non-short-circuiting local-preflight runner

- **Location**:
  - `crates/forge-cli/src/validations.rs` (new aggregating runner; reuse
    the existing Rule 1a-5 functions)
- **Description**: Add a runner that, given branch / kind / title / body
  / workdir, evaluates Rules 1a, 1b, 2a, 2b, 3, 4, 5 and returns a
  `Vec` of per-rule verdicts (`rule`, `ok`, optional `code`, optional
  `message`) without returning early on the first failure.
- **Dependencies**: none
- **Complexity**: 2
- **Acceptance criteria**:
  - A body missing both sections plus an unpushed head yields verdicts
    containing both `body_missing_*` and `head_not_pushed` failures.
  - All-green inputs yield all-`ok` verdicts.
- **Validation**:
  - `cargo test -p forge-cli`

### Task 1.2: Report verdicts in `pr deliver --dry-run`

- **Location**:
  - `crates/forge-cli/src/macros/pr_deliver.rs` (`emit_dry_run` and the
    dry-run envelope struct)
- **Description**: In `emit_dry_run`, run the Task 1.1 runner and add a
  `local_preflight` block to the dry-run envelope alongside
  `plan_steps[]`. The dry-run path must not call any provider backend
  and must not abort on a failing rule.
- **Dependencies**: Task 1.1
- **Complexity**: 2
- **Acceptance criteria**:
  - `pr deliver --dry-run --format json` includes the per-rule verdict
    block.
  - The envelope schema change is additive (existing fields unchanged).
- **Validation**:
  - `cargo test -p forge-cli`
  - manual `pr deliver --dry-run --format json` spot check

### Task 1.3: Regression guard + help text

- **Location**:
  - `crates/forge-cli` tests; `--dry-run` help text
- **Description**: Pin a test that dry-run issues no provider backend
  call while still producing verdicts, and update the `--dry-run` help
  to state it reports the local preflight.
- **Dependencies**: Task 1.2
- **Complexity**: 1
- **Acceptance criteria**:
  - A test fails if dry-run ever issues a backend `gh` invocation.
  - `--dry-run --help` documents the preflight reporting.
- **Validation**:
  - `cargo test -p forge-cli`

## Sprint 2: body-error aggregation + pr-body scaffold parity

**Goal**: A body missing both required sections returns one aggregated
error, and `pr-body render` can scaffold a valid body for every kind
`pr deliver` accepts, with the `body_missing_*` error pointing at the
scaffold.

**Demo/Validation**:

- Commands:
  - `cargo test -p forge-cli`
  - `cargo test -p agent-runtime-cli`
  - `agent-runtime pr-body render --kind docs ...`
- Verify: one error lists both missing sections; `pr-body render`
  succeeds for `docs|chore|ci|refactor`; existing per-section code tests
  stay green.

### Task 2.1: Aggregate body-section validation

- **Location**:
  - `crates/forge-cli/src/validations.rs` (aggregate over Rules 2a+2b)
    and the create-atom call site
- **Description**: Add an aggregate that runs `body_summary` and
  `body_test_plan` and, when more than one section is missing, returns a
  single error enumerating all missing sections while preserving the
  per-section codes in `details`. Route the create path through it.
- **Dependencies**: none
- **Complexity**: 2
- **Acceptance criteria**:
  - A body missing both sections returns one error whose `details`
    lists `## Summary` and `## Test plan`.
  - Existing tests matching `body_missing_summary` /
    `body_missing_test_plan` codes stay green.
- **Validation**:
  - `cargo test -p forge-cli`

### Task 2.2: pr-body kind parity + error cross-reference

- **Location**:
  - `crates/agent-runtime-cli` (`pr-body render` kind handling)
  - `crates/forge-cli/src/validations.rs` (the body-missing error
    `details`)
- **Description**: Extend `pr-body render --kind` to accept all six
  deliver kinds (or make the Summary / Test-plan skeleton kind-agnostic),
  and add a hint to the `body_missing_*` error `details` pointing at
  `agent-runtime pr-body render`.
- **Dependencies**: none
- **Complexity**: 2
- **Acceptance criteria**:
  - `agent-runtime pr-body render --kind docs|chore|ci|refactor`
    succeeds and emits a forge-cli-valid body.
  - The `body_missing_*` error references the scaffold.
- **Validation**:
  - `cargo test -p agent-runtime-cli`
  - `cargo test -p forge-cli`

### Task 2.3: Full required checks

- **Location**:
  - workspace
- **Description**: Run the full required-checks entrypoint and the
  completion audits to confirm no surface regressed and no `Cargo.lock`
  drift.
- **Dependencies**: Task 2.1, Task 2.2
- **Complexity**: 1
- **Acceptance criteria**:
  - `nils-cli-checks-entrypoint.sh --local-fast` passes with no new
    dependency and no `Cargo.lock` drift.
  - Completion flag-parity and asset audits pass.
- **Validation**:
  - `bash scripts/ci/completion-flag-parity-audit.sh --strict`
  - `bash scripts/ci/completion-asset-audit.sh --strict`
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast`

## Risks

- **R-1**: Aggregating the body error could change the first-returned
  error code and break consumers matching on it. Mitigation: keep
  per-section codes in `details` and pin a regression test (Task 2.1).
- **R-2**: The dry-run preflight could accidentally issue a provider
  backend call or mutate state. Mitigation: a guard test asserts dry-run
  makes no backend invocation (Task 1.3).
- **R-3**: Expanding `pr-body` kinds could regress the existing
  feature / bug templates. Mitigation: keep current templates and add
  per-kind tests (Task 2.2).
- **R-4**: An additive dry-run envelope field could surprise strict JSON
  consumers. Mitigation: keep the field optional and additive; document
  the schema in the envelope (Task 1.2).
