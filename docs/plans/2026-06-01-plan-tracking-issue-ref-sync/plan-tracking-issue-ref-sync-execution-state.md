# Plan Tracking Issue Ref Sync Execution State

<!-- plan-issue-record:v2 role=state profile=tracking -->
## Execution State

- Status: decisions settled; not yet implemented. This bundle is prepared so
  `create-plan-tracking-issue` can open the tracker with the observed
  `forge-cli-search` / issue #716 failure case attached.
- Target scope: nils-cli plan-tracking issue ref synchronization across
  `plan-issue`, `plan-tooling`, and `plan-archive`, followed by
  agent-runtime-kit skill updates only if the CLI workflow contract changes.
- Execution window: Sprint 1 (`nils-cli` contract, sync/repair tooling,
  consistency gate, tests, PR1) -> Sprint 2 (`agent-runtime-kit` skill guidance,
  PR2 if needed), serial.
- Current task: Task 1.1 - not started.
- Next task: Task 1.1 - lock the failing case and current invariants.
- Last updated: 2026-06-01
- Branch/commit/PR: not yet opened.
- Source document: docs/plans/2026-06-01-plan-tracking-issue-ref-sync/plan-tracking-issue-ref-sync-plan.md
- Direct source-doc execution waiver: not applicable
- Tracking issue: not yet opened
- Source snapshot: pending
- Plan snapshot: pending
- Initial state snapshot: pending

## Validation Plan

- Bundle creation: targeted plan-source bundle validation and docs-only
  validation before opening the tracker.
- Sprint 1: targeted Rust tests for `plan-issue` run-state / execution-state
  sync, execute/checkpoint consistency refusals, and `plan-archive discover`
  provider-ref inference; then `bash scripts/ci/nils-cli-checks-entrypoint.sh
  --local-fast` and provider PR checks.
- Sprint 2: agent-runtime-kit project-dev validation only if skill source or
  rendered skill surfaces change.

## Task Ledger

| ID | Status | Task | Evidence | Notes |
| --- | --- | --- | --- | --- |
| 1.1 | pending | Lock the failing case and current invariants | - | Preserve the `forge-cli-search` / issue #716 `no-provider-refs` case and prove discover remains offline. |
| 1.2 | pending | Synchronize tracking issue URL into execution-state | - | Choose owner command and patch `Tracking issue` once live issue URL is known. |
| 1.3 | pending | Add execute/checkpoint consistency gate | - | Block or strictly refuse missing, placeholder, or mismatched durable issue refs. |
| 1.4 | pending | Legacy repair and documentation | - | Provide deterministic repair for existing bundles such as `forge-cli-search`. |
| 2.1 | pending | Update agent-runtime-kit create/execute skill instructions if needed | - | Runs after nils-cli command contract is settled. |

## Session Log

- 2026-06-01: Authored this bundle after `plan-archive discover` found
  `docs/plans/2026-05-31-forge-cli-search/` blocked with `no-provider-refs`.
  Manual provider lookup confirmed the intended tracker was
  `https://github.com/sympoies/nils-cli/issues/716`, but the local plan folder
  did not contain that URL. Decision: fix the producer workflow in nils-cli
  first so run-state and execution-state issue refs stay synchronized, then
  update agent-runtime-kit skills only if the CLI contract changes. Do not make
  `plan-archive discover` perform default provider lookup.

## Validation

| Command | Status | Summary | Artifact |
| --- | --- | --- | --- |
| `plan-tooling validate --file docs/plans/2026-06-01-plan-tracking-issue-ref-sync/plan-tracking-issue-ref-sync-plan.md --format text --explain` | pass | Plan-source bundle validated with zero errors. | local |
| `bash scripts/ci/plan-bundle-validate.sh --strict --file docs/plans/2026-06-01-plan-tracking-issue-ref-sync/plan-tracking-issue-ref-sync-plan.md` | pass | Repository plan-bundle validator passed for this new bundle. | local |
| `agent-run exec --cwd /Users/terry/Project/sympoies/nils-cli -- bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only` | pass | Docs placement, hygiene, markdown lint, cli-output contract lint, and forge-cli fixture lint passed. | local |
| `agent-run exec --cwd /Users/terry/Project/sympoies/nils-cli -- bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast` | pass | Local-fast selected docs-only mode for this three-file plan bundle and passed. | local |
| `plan-issue record open` dry-run/live | pending | Tracker not yet opened. | - |
