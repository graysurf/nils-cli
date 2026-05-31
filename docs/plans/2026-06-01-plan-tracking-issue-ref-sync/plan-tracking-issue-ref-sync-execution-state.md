# Plan Tracking Issue Ref Sync Execution State

<!-- plan-issue-record:v2 role=state profile=tracking -->
## Execution State

- Status: complete; tracking issue closed
- Target scope: nils-cli durable execution-state synchronization at the open
  and close transitions across `plan-issue`, `plan-tooling`, and
  `plan-archive`, followed by agent-runtime-kit create/execute/closeout skill
  updates.
- Execution window: Sprint 1 (`nils-cli` contract: shared sync routine,
  create-time URL write, closeout terminal-state writeback, self-heal gate,
  legacy repair, tests, PR1) -> Sprint 2 (`agent-runtime-kit` skill guidance,
  PR2), serial.
- Current task: Task 1.1 - not started.
- Next task: Task 1.1 - lock the failing case and current invariants.
- Last updated: 2026-05-31
- Branch/commit/PR: sympoies/nils-cli#741 merged (<https://github.com/sympoies/nils-cli/pull/741>); graysurf/agent-runtime-kit#238 merged (<https://github.com/graysurf/agent-runtime-kit/pull/238>)
- Source document: docs/plans/2026-06-01-plan-tracking-issue-ref-sync/plan-tracking-issue-ref-sync-plan.md
- Direct source-doc execution waiver: not applicable
- Tracking issue: <https://github.com/sympoies/nils-cli/issues/738>
- Source snapshot: <https://github.com/sympoies/nils-cli/issues/738#issuecomment-4587861074>
- Plan snapshot: <https://github.com/sympoies/nils-cli/issues/738#issuecomment-4587861114>
- Initial state snapshot: <https://github.com/sympoies/nils-cli/issues/738#issuecomment-4587861188>

## Validation Plan

- Bundle creation: targeted plan-source bundle validation and docs-only
  validation before opening the tracker.
- Sprint 1: targeted Rust tests for `plan-issue` / `plan-tooling`
  execution-state sync, closeout terminal-state writeback,
  self-heal/consistency refusals, and `plan-archive discover` provider-ref
  inference; then `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast`
  and provider PR checks.
- Sprint 2: agent-runtime-kit project-dev validation for skill source and
  rendered skill surfaces.

## Task Ledger

| ID | Status | Task | Evidence | Notes |
| --- | --- | --- | --- | --- |
| 1.1 | done | Lock the failing cases and current invariants | <https://github.com/sympoies/nils-cli/pull/741> | Preserve the `forge-cli-search` / issue #716 `no-provider-refs` case and the closeout-stale local-state case; prove discover stays offline. |
| 1.2 | done | Shared sync routine and create-time URL write | <https://github.com/sympoies/nils-cli/pull/741> | One byte-preserving routine owns execution-state writes; `record open` invokes it to record the issue URL. |
| 1.3 | done | Closeout terminal-state writeback | <https://github.com/sympoies/nils-cli/pull/741> | `record close` writes terminal status, final ledger, linked PR, and URL into the local file; complementary to migrate's archived rewrite. |
| 1.4 | done | Consistency and self-heal gate across execute/checkpoint/close | <https://github.com/sympoies/nils-cli/pull/741> | Self-heal write-if-missing before any hard block; cover the previously unguarded close path. |
| 1.5 | done | Legacy repair and documentation | <https://github.com/sympoies/nils-cli/pull/741> | On-demand repair command over the shared routine for bundles such as `forge-cli-search`. |
| 2.1 | done | Update agent-runtime-kit create/execute/closeout skill instructions | <https://github.com/graysurf/agent-runtime-kit/pull/238> | Required: Task 1.2/1.3 change the create and closeout command sequences. |

## Session Log

- 2026-06-01: Authored this bundle after `plan-archive discover` found
  `docs/plans/2026-05-31-forge-cli-search/` blocked with `no-provider-refs`.
  Manual provider lookup confirmed the intended tracker was
  `https://github.com/sympoies/nils-cli/issues/716`, but the local plan folder
  did not contain that URL. Decision: fix the producer workflow in nils-cli
  first so run-state and execution-state issue refs stay synchronized, then
  update agent-runtime-kit skills only if the CLI contract changes. Do not make
  `plan-archive discover` perform default provider lookup.
- 2026-06-01: Expanded scope after review (issue #738 was opened from the
  original 4-task plan at `f34b082`; this is the refined 6-task plan). Added
  (a) closeout terminal-state writeback so the in-repo execution-state is the
  final state after `record close`, not transient-stale until
  `plan-archive migrate`; (b) a consistency/self-heal gate that also covers the
  close path; (c) a shared sync routine backing create, closeout, self-heal,
  and repair; and (d) Sprint 2 is now required because the create/execute/
  closeout command sequence changes. The expansion is delivered on
  `feat/plan-tracking-issue-ref-sync`; #738's frozen plan snapshot stays as the
  open-time record and the scope change is noted in a session checkpoint.
- 2026-06-01: Bootstrap note (chicken-and-egg). The create-time URL sync and
  closeout writeback do not exist yet, so this bundle's own execution-state was
  not auto-patched. The `Tracking issue` URL was recorded manually at open; its
  terminal state must likewise be written manually (or via the new repair
  command once Sprint 1 lands) at closeout.

## Validation

| Command | Status | Summary | Artifact |
| --- | --- | --- | --- |
| `plan-tooling validate --file docs/plans/2026-06-01-plan-tracking-issue-ref-sync/plan-tracking-issue-ref-sync-plan.md --format text --explain` | pass | Plan-source bundle validated with zero errors. | local |
| `bash scripts/ci/plan-bundle-validate.sh --strict --file docs/plans/2026-06-01-plan-tracking-issue-ref-sync/plan-tracking-issue-ref-sync-plan.md` | pass | Repository plan-bundle validator passed for this new bundle. | local |
| `agent-run exec --cwd /Users/terry/Project/sympoies/nils-cli -- bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only` | pass | Docs placement, hygiene, markdown lint, cli-output contract lint, and forge-cli fixture lint passed. | local |
| `agent-run exec --cwd /Users/terry/Project/sympoies/nils-cli -- bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast` | pass | Local-fast selected docs-only mode for this three-file plan bundle and passed. | local |
| `plan-issue --repo sympoies/nils-cli --format json --dry-run record open --profile tracking --bundle ...` | pass | Dry-run preview rendered source, plan, and state lifecycle comments from commit `f34b082`. | local |
| `plan-issue --repo sympoies/nils-cli --format json record open --profile tracking --bundle ...` | pass | Opened tracker issue #738 and posted source, plan, and state lifecycle comments. | <https://github.com/sympoies/nils-cli/issues/738> |
| `plan-issue --format json tracking run init --provider-repo sympoies/nils-cli --issue 738 --bundle ...` | pass | Initialized typed run state `20260531T193736Z-issue-738` for branch `feat/plan-tracking-issue-ref-sync`. | local |
| `plan-issue --format json record audit --profile tracking --expect-visible` | pass | Read-back audit recognized source, plan, and state roles with visible lint passing. | local |
| `plan-issue --format json tracking status --provider-repo sympoies/nils-cli --issue 738 --expect-visible` | pass | FSM is `RECORD_OPEN_INITIAL`; safe transition is `checkpoint_progress`; session/validation/review remain expected future evidence. | local |
