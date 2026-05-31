# Plan: Plan Tracking Issue Ref Synchronization

## Overview

Ensure the plan-tracking workflow records the live tracking issue ref in both
runtime state and durable plan-bundle state. Today `run-state.json` can carry
the issue number that `execute-plan-tracking-issue` needs, while the canonical
`*-execution-state.md` can still say `Tracking issue: not yet opened` or omit
the issue URL entirely. That leaves completed bundles invisible to
`plan-archive discover`, which intentionally infers provider refs only from
local Markdown.

This plan keeps runtime authority where it belongs: `run-state.json` plus
provider lifecycle comments remain the source of truth for execution. The
improvement is a contract and tooling update: once a live tracking issue exists,
the canonical execution-state file must carry the matching issue URL before the
workflow can proceed through execute/checkpoint and later archive discovery.

## Read First

- Primary source:
  `docs/plans/2026-06-01-plan-tracking-issue-ref-sync/plan-tracking-issue-ref-sync-discussion-source.md`
- Source type: discussion-to-implementation-doc
- Repo anchors:
  - `crates/plan-issue/src/execute.rs` (`record open`, `tracking run init`,
    live checkpoint issue inheritance, and checkpoint rendering)
  - `crates/plan-issue/src/tracking/run_state.rs` (`run-state.json` schema and
    issue field)
  - `crates/plan-tooling/src/ledger_update.rs` and neighboring tooling command
    patterns if a local execution-state patch helper is added
  - `crates/plan-archive/src/discover/mod.rs` (`no-provider-refs` blocker and
    local Markdown ref inference)
  - `crates/plan-archive/README.md` (discover status classes and offline
    boundary)
  - `docs/plans/2026-05-31-forge-cli-search/` (failure case: missing issue
    URL for issue `https://github.com/sympoies/nils-cli/issues/716`)
  - `docs/plans/2026-05-31-git-cli-worktree-surface/` (healthy example carrying
    a tracking issue URL in execution-state)
  - agent-runtime-kit
    `build/codex/plugins/dispatch/skills/create-plan-tracking-issue/SKILL.md`
    and
    `build/codex/plugins/dispatch/skills/execute-plan-tracking-issue/SKILL.md`
- Key decisions carried into execution:
  - Primary implementation lands in `sympoies/nils-cli`.
  - `run-state.json` stays the runtime source of truth.
  - `*-execution-state.md` must be synchronized as durable plan-bundle state.
  - `plan-archive discover` remains offline; do not add provider lookup by
    default.
  - agent-runtime-kit skill updates follow after the CLI contract is settled.
- Open questions carried into execution: command ownership for the patch helper,
  exact hard-block vs strict-warning boundary, and repair command shape for
  legacy bundles.

## Scope

- In scope:
  - Add or adjust nils-cli tooling so live tracking issue creation/init records
    the issue ref in both `run-state.json` and canonical execution-state
    Markdown.
  - Add execute/checkpoint consistency checks for run-state issue vs
    execution-state tracking issue URL.
  - Add regression coverage for the `forge-cli-search` / issue #716 missing-ref
    failure mode.
  - Document or implement a repair path for legacy plan bundles.
  - Update agent-runtime-kit skill instructions if the create/execute command
    sequence changes.
- Out of scope:
  - Making `plan-archive discover` call GitHub/GitLab by default.
  - Title-based provider ref guessing.
  - Rewriting archived plan metadata outside the chosen repair path.
  - Changing plan-archive migrate semantics beyond consuming the already
    synchronized local provider refs.

## Assumptions

1. `plan-issue record open` or `tracking run init` can determine the full issue
   URL from provider repo and issue number.
2. The execution-state patch can be a narrow Markdown rewrite of the
   `- Tracking issue:` bullet, preserving the rest of the authored header and
   task ledger.
3. Existing plan bundles may contain any of these placeholder values:
   `not yet opened`, `tbd`, `pending`, or no `Tracking issue` bullet.
4. A follow-up commit may be required after live issue creation if the local
   execution-state file is patched after the issue number becomes known.

## Sprint 1: nils-cli Contract And Tooling

**Goal**: Make nils-cli enforce and repair tracking issue ref synchronization
without changing `plan-archive discover` into a provider-aware command.

**PR grouping intent**: `group`
**Execution Profile**: serial

### Task 1.1: Lock the failing case and current invariants

- **Location**:
  - `crates/plan-archive/tests/discover.rs`
  - `crates/plan-issue/tests/integration/tracking_checkpoint_dry_run.rs`
  - `crates/plan-issue/src/execute.rs`
- **Description**: Add regression coverage that captures the observed failure:
  a completed plan bundle with no top-level provider URL is blocked by
  `plan-archive discover` as `no-provider-refs`, while a bundle with a
  `Tracking issue: <https://github.com/.../issues/N>` line is inferable.
  Preserve existing checkpoint behavior where live execution derives from
  run-state and does not trust stale execution-state header bullets.
- **Dependencies**:
  - none
- **Complexity**: 2
- **Acceptance criteria**:
  - Tests prove `discover` remains offline and does not guess issue refs.
  - Tests prove the healthy execution-state URL line is sufficient for
    provider-ref inference.
- **Validation**:
  - `cargo test -p plan-archive discover`
  - `cargo test -p plan-issue tracking_checkpoint`

### Task 1.2: Synchronize tracking issue URL into execution-state

- **Location**:
  - `crates/plan-issue/src/execute.rs`
  - `crates/plan-tooling/src/ledger_update.rs`
  - `crates/plan-tooling/src/cli.rs`
  - `crates/plan-issue/tests/integration/live_record_ops.rs`
- **Description**: Choose the owner command and implement a narrow sync path
  that writes or updates the execution-state `Tracking issue` bullet once the
  live issue URL is known. The preferred behavior is automatic in the standard
  `create-plan-tracking-issue` flow, with a JSON result field showing whether
  the execution-state file was already synced, patched, or skipped.
- **Dependencies**:
  - Task 1.1
- **Complexity**: 3
- **Acceptance criteria**:
  - Live/open fixture tests show the execution-state file ends with the issue
    URL after creation/init.
  - The patch preserves the task ledger and all unrelated state bullets.
  - JSON output reports the sync action and any required follow-up commit.
- **Validation**:
  - `cargo test -p plan-issue live_record_ops`
  - `cargo test -p plan-tooling ledger_update`

### Task 1.3: Add execute/checkpoint consistency gate

- **Location**:
  - `crates/plan-issue/src/execute.rs`
  - `crates/plan-issue/src/tracking/reconcile.rs`
  - `crates/plan-issue/tests/integration/tracking_checkpoint_live.rs`
  - `crates/plan-issue/tests/integration/tracking_checkpoint_refusals.rs`
  - `crates/plan-issue/tests/integration/tracking_status.rs`
- **Description**: When run-state carries an issue and an execution-state file
  is recorded, parse the execution-state tracking issue URL and compare it to
  the provider repo and issue number. Missing placeholders or mismatches should
  block live execute/checkpoint with a clear remediation message. If the team
  chooses a warning phase, it must still be strict enough that close-ready and
  archive handoff cannot proceed with stale durable state.
- **Dependencies**:
  - Task 1.2
- **Complexity**: 3
- **Acceptance criteria**:
  - Missing, placeholder, and mismatched execution-state issue refs return
    stable machine-readable refusal codes.
  - Matching refs pass without changing the existing state/session/validation
    lifecycle rules.
  - Error text tells the operator how to run the sync/repair path.
- **Validation**:
  - `cargo test -p plan-issue tracking_checkpoint`
  - `cargo test -p plan-issue tracking_status`

### Task 1.4: Legacy repair and documentation

- **Location**:
  - `crates/plan-issue/docs/specs/issue-backed-plan-record-contract-v2.md`
  - `crates/plan-tooling/docs/specs/plan-source-bundle-contract-v1.md`
  - `docs/plans/2026-05-31-forge-cli-search/forge-cli-search-execution-state.md`
- **Description**: Document or implement a deterministic repair path for
  existing bundles whose issue exists but whose execution-state Markdown lacks
  the URL. The `forge-cli-search` / issue #716 case is the motivating example.
- **Dependencies**:
  - Task 1.3
- **Complexity**: 2
- **Acceptance criteria**:
  - The repair path can update the `forge-cli-search` bundle from missing refs
    to discoverable refs without provider guessing.
  - Documentation states that `plan-archive discover` remains local-only.
  - The repaired flow is covered by docs-only or integration validation.
- **Validation**:
  - `plan-archive discover --source-repo . --include-unknown --format json`
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast`

## Sprint 2: agent-runtime-kit Skill Surface

**Goal**: Reflect the nils-cli contract in agent-runtime-kit skill guidance
only after Sprint 1 establishes the exact command sequence.

**PR grouping intent**: `group`
**Execution Profile**: serial after Sprint 1

### Task 2.1: Update create/execute skill instructions

- **Location**:
  - agent-runtime-kit `build/codex/plugins/dispatch/skills/create-plan-tracking-issue/SKILL.md`
  - agent-runtime-kit `build/codex/plugins/dispatch/skills/execute-plan-tracking-issue/SKILL.md`
  - source skill definitions that render those files
- **Description**: Update the skill flow to mention the new execution-state ref
  sync postcondition and the execute consistency gate. If nils-cli exposes a
  dedicated repair/sync command, wire it into the documented create flow.
- **Dependencies**:
  - Task 1.4
- **Complexity**: 2
- **Acceptance criteria**:
  - Skill text says create/open must leave run-state and execution-state issue
    refs consistent.
  - Execute skill text names the refusal codes and remediation command.
  - Runtime surfaces are synced and validated through agent-runtime-kit checks.
- **Validation**:
  - agent-runtime-kit project-dev validation for touched skill sources and
    rendered surfaces.

## Done Criteria

- `run-state.json` and canonical `*-execution-state.md` cannot drift silently
  after a tracking issue exists.
- A completed plan bundle produced through the repaired workflow is discoverable
  by `plan-archive discover` without provider lookup.
- The `forge-cli-search` / issue #716 missing-ref failure is preserved as a
  regression or repair fixture.
- nils-cli validation passes locally and provider checks are green for the PR.
- agent-runtime-kit skill updates are either delivered or explicitly marked
  not needed because the command sequence did not change.

## Validation Plan

- Targeted Rust tests for the affected `plan-issue`, `plan-tooling`, and
  `plan-archive` behavior.
- `plan-tooling validate --file docs/plans/2026-06-01-plan-tracking-issue-ref-sync/plan-tracking-issue-ref-sync-plan.md --format text --explain`
- `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast`
- Provider PR checks for nils-cli PR1.
- If Sprint 2 runs, agent-runtime-kit project-dev validation for skill-source
  and rendered-surface changes.

## Risks And Guardrails

- **Risk**: `record open` patches local files after the issue exists but before
  the operator commits the updated bundle.
  **Guardrail**: JSON output must report the patch and the create skill must
  require a follow-up commit/push before execution continues.
- **Risk**: making execution-state mandatory as runtime truth reintroduces stale
  header bugs.
  **Guardrail**: continue deriving checkpoint payloads from run-state; use
  execution-state only for durable consistency checks and archive readiness.
- **Risk**: provider lookup in discover makes archive selection slow and
  non-deterministic.
  **Guardrail**: keep discover offline; repair producer workflow instead.

## Future Work

- Add a bulk audit command for plan bundles whose tracking issue exists in
  provider history but whose local execution-state Markdown lacks a provider
  URL.
- Add optional `plan-archive discover --include-unknown --explain-repair` style
  guidance if operators repeatedly hit `no-provider-refs`.
