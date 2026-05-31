# Plan: Plan Tracking Issue Ref Synchronization

## Overview

Keep the durable plan-bundle state (`*-execution-state.md`) synchronized with
the runtime source of truth (`run-state.json` plus provider lifecycle comments)
at both ends of the workflow: when a tracking issue is opened, and when it is
closed out. Today the runtime layer is correct but the durable Markdown drifts
in two ways:

- After `record open`, `run-state.json` carries the issue number while
  `*-execution-state.md` can still say `Tracking issue: not yet opened` or omit
  the issue URL. That leaves completed bundles invisible to
  `plan-archive discover`, which intentionally infers provider refs only from
  local Markdown.
- After closeout (`record close`), the lifecycle comment posted to the issue
  reports the terminal state, but the local `*-execution-state.md` is never
  written back, so it freezes at a mid-flight status (e.g. `not yet opened`,
  an in-progress task, or a `pending` ledger row) even though the issue is
  closed. The local file only becomes terminal later, at
  `plan-archive migrate` time (the archived-header rewrite from
  nils-cli finding #47), which is too late for the in-repo copy.

This plan keeps runtime authority where it belongs: `run-state.json` plus
provider lifecycle comments remain the source of truth for execution. The
improvement is a contract and tooling update so that durable execution-state is
written from run-state at the open and close transitions, gated for consistency
in between, and repairable for legacy bundles. After closeout the in-repo
execution-state file must already be the final state, not a transient snapshot
waiting on archive migration.

## Read First

- Primary source:
  `docs/plans/2026-06-01-plan-tracking-issue-ref-sync/plan-tracking-issue-ref-sync-discussion-source.md`
- Source type: discussion-to-implementation-doc
- Repo anchors:
  - `crates/plan-issue/src/execute.rs` (`record open`, `record close`,
    `tracking run init`, live checkpoint issue inheritance, and checkpoint
    rendering)
  - `crates/plan-issue/src/tracking/run_state.rs` (`run-state.json` schema and
    issue field)
  - `crates/plan-tooling/src/ledger_update.rs` and `crates/plan-tooling/src/cli.rs`
    (existing surgical execution-state patcher; the new header/state sync helper
    follows this pattern)
  - `crates/plan-archive/src/discover/mod.rs` (`no-provider-refs` blocker and
    local Markdown ref inference)
  - `crates/plan-archive/src/migrate/` (existing archived-header rewrite from
    finding #47; closeout writeback must stay complementary, not conflicting)
  - `crates/plan-archive/README.md` (discover status classes and offline
    boundary)
  - `docs/plans/2026-05-31-forge-cli-search/` (failure case: missing issue
    URL for issue `https://github.com/sympoies/nils-cli/issues/716`)
  - `docs/plans/2026-05-31-git-cli-worktree-surface/` (healthy example carrying
    a tracking issue URL in execution-state)
  - agent-runtime-kit
    `build/codex/plugins/dispatch/skills/create-plan-tracking-issue/SKILL.md`,
    `build/codex/plugins/dispatch/skills/execute-plan-tracking-issue/SKILL.md`,
    and
    `build/codex/plugins/dispatch/skills/plan-tracking-issue-closeout/SKILL.md`
- Key decisions carried into execution:
  - Primary implementation lands in `sympoies/nils-cli`.
  - `run-state.json` stays the runtime source of truth.
  - `*-execution-state.md` must be synchronized as durable plan-bundle state at
    both open and close transitions.
  - A single execution-state durable-sync routine backs create-time URL writes,
    closeout terminal-state writeback, in-between self-heal, and legacy repair,
    so the surfaces cannot drift apart.
  - `plan-archive discover` remains offline; do not add provider lookup by
    default.
  - `plan-archive migrate` semantics are unchanged; closeout writeback produces
    the `complete`/`closed` terminal in-repo state, and migrate's existing
    archived-header rewrite remains the archive-time step.
  - agent-runtime-kit skill updates are required because the create/execute/
    closeout command sequence changes.
- Open questions carried into execution: resolved during this expansion — the
  shared sync routine (Task 1.2) owns the execution-state patch; the gate
  self-heals a missing/placeholder ref then hard-blocks only on a true issue
  mismatch (Task 1.4); legacy bundles get an on-demand repair command
  (Task 1.5).

## Scope

- In scope:
  - Add a shared, surgical execution-state durable-sync routine that writes the
    `Tracking issue` URL bullet and the terminal-state fields without disturbing
    the authored header, validation, or task ledger.
  - Wire create-time URL sync so live tracking issue creation records the issue
    ref in both `run-state.json` and canonical execution-state Markdown.
  - Wire closeout terminal-state writeback so `record close` (and the closeout
    handoff) leaves the local execution-state file at the final state
    (`Status: complete`/closed, final task ledger rows, linked PR, last-updated,
    issue URL present).
  - Add execute/checkpoint/close-ready consistency checks for run-state issue
    vs execution-state tracking issue URL, with an idempotent
    write-if-missing self-heal before any hard block.
  - Add regression coverage for the `forge-cli-search` / issue #716 missing-ref
    failure mode and for the closeout-stale local-state failure mode.
  - Implement a deterministic repair path for legacy plan bundles.
  - Update agent-runtime-kit skill instructions for the changed create/execute/
    closeout command sequence.
- Out of scope:
  - Making `plan-archive discover` call GitHub/GitLab by default.
  - Title-based provider ref guessing.
  - Changing `plan-archive migrate` semantics beyond consuming the already
    synchronized local provider refs and terminal state.
  - Making execution-state header text the runtime authority (run-state stays
    authoritative; the durable file is written from it, never read as truth for
    live comments).

## Assumptions

1. `plan-issue record open` / `tracking run init` can determine the full issue
   URL from provider repo identity and issue number.
2. `plan-issue record close` has access to the final run-state (terminal phase,
   linked PR, completed ledger) needed to compute the terminal execution-state.
3. The execution-state patch can be a narrow, byte-preserving Markdown rewrite
   of the `- Tracking issue:` bullet, the `- Status:` / state bullets, and the
   ledger rows, preserving the rest of the authored header and session log
   (same atomic temp+rename discipline as `plan-tooling ledger-update`).
4. Existing plan bundles may contain any of these placeholder values:
   `not yet opened`, `tbd`, `pending`, or no `Tracking issue` bullet, and may be
   frozen at a mid-flight status after a real closeout.
5. A follow-up commit may be required after live issue creation and after
   closeout if the local execution-state file is patched once the issue
   number / terminal state becomes known.

## Sprint 1: nils-cli Contract And Tooling

**Goal**: Make nils-cli write and enforce durable execution-state
synchronization at the open and close transitions without turning
`plan-archive discover` into a provider-aware command and without changing
`migrate` semantics.

**PR grouping intent**: `group`
**Execution Profile**: serial

### Task 1.1: Lock the failing cases and current invariants

- **Location**:
  - `crates/plan-archive/tests/discover.rs`
  - `crates/plan-issue/tests/integration/tracking_checkpoint_dry_run.rs`
  - `crates/plan-issue/src/execute.rs`
- **Description**: Add regression coverage that captures the two observed
  failures: (a) a completed plan bundle with no top-level provider URL is
  blocked by `plan-archive discover` as `no-provider-refs`, while a bundle with
  a `Tracking issue: <https://github.com/.../issues/N>` line is inferable; and
  (b) a bundle whose issue was closed out but whose local execution-state is
  still frozen at a mid-flight status. Preserve existing checkpoint behavior
  where live execution derives from run-state and does not trust stale
  execution-state header bullets.
- **Dependencies**:
  - none
- **Complexity**: 2
- **Acceptance criteria**:
  - Tests prove `discover` remains offline and does not guess issue refs.
  - Tests prove the healthy execution-state URL line is sufficient for
    provider-ref inference.
  - A test documents the closeout-stale local-state failure mode as the
    baseline the writeback in Task 1.3 must fix.
- **Validation**:
  - `cargo test -p plan-archive discover`
  - `cargo test -p plan-issue tracking_checkpoint`

### Task 1.2: Shared sync routine and create-time URL write

- **Location**:
  - `crates/plan-tooling/src/ledger_update.rs` (or a sibling
    `execution_state_sync` module)
  - `crates/plan-tooling/src/cli.rs`
  - `crates/plan-issue/src/execute.rs`
  - `crates/plan-issue/tests/integration/live_record_ops.rs`
- **Description**: Implement one shared, surgical execution-state durable-sync
  routine and have `record open` invoke it automatically once the live issue
  URL is known, writing or updating the `Tracking issue` bullet. The routine is
  the single owner of execution-state writes (resolves Open Question 1); it is
  reused by closeout writeback (1.3), self-heal (1.4), and the repair command
  (1.5). The standard `create-plan-tracking-issue` flow gets the URL written
  automatically, with a JSON result field reporting whether the file was already
  synced, patched, or skipped, plus any required follow-up commit.
- **Dependencies**:
  - Task 1.1
- **Complexity**: 3
- **Acceptance criteria**:
  - Live/open fixture tests show the execution-state file ends with the issue
    URL after creation/init.
  - The patch preserves the task ledger and all unrelated state bullets
    (byte-preserving, atomic temp+rename).
  - JSON output reports the sync action and any required follow-up commit.
- **Validation**:
  - `cargo test -p plan-issue live_record_ops`
  - `cargo test -p plan-tooling`

### Task 1.3: Closeout terminal-state writeback

- **Location**:
  - `crates/plan-issue/src/execute.rs` (`record close` path)
  - `crates/plan-tooling/src/` (shared sync routine from 1.2)
  - `crates/plan-issue/tests/integration/` (closeout writeback fixtures)
- **Description**: When `record close` runs (and via the closeout handoff),
  write the terminal state back into the local execution-state file using the
  shared sync routine: flip `- Status:` to the terminal `complete`/closed value,
  stamp `- Last updated:`, fill `- Branch/commit/PR:` from the linked PR, ensure
  the `- Tracking issue:` URL is present, and mark every task-ledger row at its
  final status. This is the feature that guarantees the in-repo file is the
  final state after the workflow finishes. It is complementary to the
  `plan-archive migrate` archived-header rewrite (finding #47): closeout
  produces `complete`/`closed`, migrate later produces `archived`; neither
  fights the other.
- **Dependencies**:
  - Task 1.2
- **Complexity**: 3
- **Acceptance criteria**:
  - After a simulated closeout, the local execution-state file shows the
    terminal status, the linked PR, the issue URL, and no remaining
    `pending`/`in-progress` ledger rows.
  - The writeback preserves the authored session log and validation table.
  - JSON output reports the writeback action and any required follow-up commit;
    a re-run is idempotent (no spurious diff).
- **Validation**:
  - `cargo test -p plan-issue` (closeout writeback)
  - `cargo test -p plan-tooling`

### Task 1.4: Consistency and self-heal gate across execute/checkpoint/close

- **Location**:
  - `crates/plan-issue/src/execute.rs`
  - `crates/plan-issue/src/tracking/reconcile.rs`
  - `crates/plan-issue/tests/integration/tracking_checkpoint_live.rs`
  - `crates/plan-issue/tests/integration/tracking_checkpoint_refusals.rs`
  - `crates/plan-issue/tests/integration/tracking_status.rs`
  - `crates/plan-issue/tests/integration/` (close-ready refusal fixtures)
- **Description**: When run-state carries an issue and an execution-state file
  is recorded, parse the execution-state tracking issue URL and compare it to
  the provider repo and issue number across `execute`, `tracking checkpoint`,
  and `close-ready`. Before refusing, attempt an idempotent
  write-if-missing self-heal via the shared sync routine (1.2); only a true
  mismatch (a different issue) is a hard block. This closes the gap where the
  earlier design gated checkpoints but left the close path — the exact moment
  archive readiness matters — unguarded.
- **Dependencies**:
  - Task 1.3
- **Complexity**: 3
- **Acceptance criteria**:
  - Missing or placeholder refs self-heal and proceed; genuine mismatches
    return stable machine-readable refusal codes on execute, checkpoint, and
    close-ready.
  - Matching refs pass without changing the existing state/session/validation
    lifecycle rules.
  - Error text tells the operator how to run the sync/repair path.
- **Validation**:
  - `cargo test -p plan-issue tracking_checkpoint`
  - `cargo test -p plan-issue tracking_status`

### Task 1.5: Legacy repair and documentation

- **Location**:
  - `crates/plan-tooling/src/cli.rs` (repair subcommand surfacing the shared
    routine)
  - `crates/plan-issue/docs/specs/issue-backed-plan-record-contract-v2.md`
  - `crates/plan-tooling/docs/specs/plan-source-bundle-contract-v1.md`
  - `docs/plans/2026-05-31-forge-cli-search/forge-cli-search-execution-state.md`
- **Description**: Expose a deterministic repair command (the shared sync
  routine, run on demand) for existing bundles whose issue exists but whose
  execution-state Markdown lacks the URL or is frozen at a mid-flight status.
  The `forge-cli-search` / issue #716 case is the motivating example.
- **Dependencies**:
  - Task 1.4
- **Complexity**: 2
- **Acceptance criteria**:
  - The repair path can update the `forge-cli-search` bundle from missing refs
    to discoverable refs without provider guessing.
  - Documentation states that `plan-archive discover` remains local-only and
    that closeout writeback and migrate's archived rewrite are complementary.
  - The repaired flow is covered by docs-only or integration validation.
- **Validation**:
  - `plan-archive discover --source-repo . --include-unknown --format json`
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast`

## Sprint 2: agent-runtime-kit Skill Surface

**Goal**: Reflect the nils-cli contract in agent-runtime-kit skill guidance.
This sprint is required, not optional: Task 1.2/1.3 change the create and
closeout command sequences (added URL-sync and terminal-state-writeback
postconditions plus follow-up commits), so the skill docs must change.

**PR grouping intent**: `group`
**Execution Profile**: serial after Sprint 1

### Task 2.1: Update create/execute/closeout skill instructions

- **Location**:
  - agent-runtime-kit `build/codex/plugins/dispatch/skills/create-plan-tracking-issue/SKILL.md`
  - agent-runtime-kit `build/codex/plugins/dispatch/skills/execute-plan-tracking-issue/SKILL.md`
  - agent-runtime-kit `build/codex/plugins/dispatch/skills/plan-tracking-issue-closeout/SKILL.md`
  - source skill definitions that render those files
- **Description**: Update the create flow to mention the execution-state URL
  sync postcondition and the required follow-up commit; update the execute flow
  to name the self-heal-then-refusal codes and remediation command; update the
  closeout flow to name the terminal-state writeback postcondition so the local
  file is final after `record close`. Wire the repair/sync command into the
  documented flows.
- **Dependencies**:
  - Task 1.5
- **Complexity**: 2
- **Acceptance criteria**:
  - Create/open skill text says run-state and execution-state issue refs must be
    left consistent, with a follow-up commit.
  - Execute skill text names the self-heal behavior, refusal codes, and
    remediation command.
  - Closeout skill text says the local execution-state must be written to its
    terminal state before close.
  - Runtime surfaces are synced and validated through agent-runtime-kit checks.
- **Validation**:
  - agent-runtime-kit project-dev validation for touched skill sources and
    rendered surfaces.

## Done Criteria

- `run-state.json` and canonical `*-execution-state.md` cannot drift silently
  after a tracking issue exists, on open or close.
- After closeout, the in-repo execution-state file already shows the terminal
  state (status, final ledger, linked PR, issue URL) without waiting on
  `plan-archive migrate`.
- A completed plan bundle produced through the repaired workflow is discoverable
  by `plan-archive discover` without provider lookup.
- The `forge-cli-search` / issue #716 missing-ref failure and the
  closeout-stale local-state failure are preserved as regression or repair
  fixtures.
- nils-cli validation passes locally and provider checks are green for the PR.
- agent-runtime-kit create/execute/closeout skill updates are delivered.

## Validation Plan

- Targeted Rust tests for the affected `plan-issue`, `plan-tooling`, and
  `plan-archive` behavior.
- `plan-tooling validate --file docs/plans/2026-06-01-plan-tracking-issue-ref-sync/plan-tracking-issue-ref-sync-plan.md --format text --explain`
- `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast`
- Provider PR checks for the nils-cli PR.
- agent-runtime-kit project-dev validation for skill-source and
  rendered-surface changes in Sprint 2.

## Risks And Guardrails

- **Risk**: `record open` / `record close` patch local files after the issue or
  terminal state is known but before the operator commits the updated bundle.
  **Guardrail**: JSON output must report the patch and the create/closeout
  skills must require a follow-up commit/push; self-heal at checkpoint catches a
  missed create-time sync.
- **Risk**: making execution-state mandatory as runtime truth reintroduces stale
  header bugs.
  **Guardrail**: continue deriving checkpoint payloads from run-state; the
  durable file is written *from* run-state at transitions, never read as
  authority for live comments.
- **Risk**: closeout writeback or repair clobbers authored notes, session log,
  or validation rows.
  **Guardrail**: a single byte-preserving, atomic, idempotent sync routine that
  touches only the targeted bullets and ledger cells, covered by re-run tests.
- **Risk**: closeout writeback fights `plan-archive migrate`'s archived-header
  rewrite (finding #47).
  **Guardrail**: closeout writes `complete`/`closed`; migrate writes `archived`;
  document the two-stage terminal model and test both.
- **Risk**: provider lookup in discover makes archive selection slow and
  non-deterministic.
  **Guardrail**: keep discover offline; repair the producer workflow instead.

## Future Work

- Add a bulk audit command for plan bundles whose tracking issue exists in
  provider history but whose local execution-state Markdown lacks a provider
  URL or terminal status.
- Add optional `plan-archive discover --include-unknown --explain-repair` style
  guidance if operators repeatedly hit `no-provider-refs`.
