# Provider Exact Attention Correlation Execution State

<!-- plan-issue-record:v2 role=state profile=tracking -->
## Execution State

- Status: ready-to-start; implementation not started
- Target scope: one provider-neutral v1 invariant with Codex app-server exact
  request resolution, capability-gated Claude Elicitation correlation, and an
  explicit conservative limitation for generic permission dialogs.
- Execution window: Sprint 1 evidence gates -> Sprint 2 Codex lane -> Sprint 3
  Claude lane -> Sprint 4 integration and delivery.
- Current task: none; plan bundle publication and tracker initialization.
- Next task: Task 1.1, declare the contract delta and capture test-first
  evidence.
- Last updated: 2026-07-15
- Branch: `feat/provider-exact-attention-correlation`
- Source document:
  `docs/plans/2026-07-15-provider-exact-attention-correlation/provider-exact-attention-correlation-plan.md`
- Implementation source:
  `docs/plans/2026-07-15-provider-exact-attention-correlation/provider-exact-attention-correlation-discussion-source.md`
- Tracking issue: pending

## Validation Plan

- Validate the plan bundle and docs-only repository gate before opening the
  tracker.
- Capture meaningful test-first failure evidence before production edits.
- Re-audit installed Codex schema and sanitize Claude Elicitation fixtures
  before enabling exact mappings.
- Run focused reducer/setup/doctor tests plus nils-cli local-fast.
- Require specialist review, provider checks, and live polling/SSE/Agent Console
  acceptance before final closeout.

## Task Ledger

| ID | Status | Task | Evidence | Notes |
| --- | --- | --- | --- | --- |
| 1.1 | pending | Declare contract delta and capture test-first evidence | pending | Freeze exact clear and progress-never-clears behavior. |
| 1.2 | pending | Verify installed provider capability shapes | pending | Codex schema and sanitized Claude form/URL fixtures. |
| 2.1 | pending | Project Codex app-server request/resolution metadata | pending | Keep v1 and metadata-only boundary. |
| 2.2 | pending | Reconcile duplicate Codex hook/protocol evidence | pending | Bounded runtime/turn ledger with resolved tombstones. |
| 3.1 | pending | Add capability-gated Claude Elicitation mapping | pending | Exact only with matching non-empty id. |
| 3.2 | pending | Publish provider capability/degradation status | pending | State generic permission limitation. |
| 4.1 | pending | Run repository and live provider acceptance | pending | Polling, SSE, and unchanged Agent Console. |
| 4.2 | pending | Deliver implementation PRs and close tracker | pending | Close only after completion audit. |

## Session Log

- 2026-07-15: Reassessment confirmed that the original producer, persistence,
  SSE, setup, and consumer work shipped under v1. The remaining issue is exact
  provider request correlation, not a v2 redesign.
- 2026-07-15: The user required the original Codex/Claude shared-design goal to
  remain explicit. The plan converged on one v1 invariant, separate provider
  evidence adapters, and no false exact-clear claim for generic permissions.
- 2026-07-15: Plan archive and open-issue audit found related foundations but no
  duplicate. Archived #1151/#1154 own app-server auto-resume; open #1118 owns
  Codex hook representation convergence.

## Validation

| Command | Status | Summary | Artifact |
| --- | --- | --- | --- |
| `agent-docs session activate` for `project-dev,task-tools` | pass | Required repository and external-fact policies activated. | local preflight |
| `plan-archive search` for turn-state/activity/app-server plans | pass | Found app-server foundation; no exact-attention duplicate. | local catalog |
| Open nils-cli issue audit | pass | #1118 is related configuration work, not this scope. | GitHub issue list |
| Installed version audit | pass | Claude 2.1.210, Codex 0.144.3, agent-session 1.22.4. | local runtime |
| `plan-tooling validate` | pass | Bundle structure and all eight tasks validate with zero errors. | local |
| `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only` | pass | Placement, hygiene, markdown, contract, and fixture checks passed. | local |
| `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast` | pass | Correctly selected docs-only mode for the three-file bundle and passed. | local |

## Handoff

- Publish this bundle to `main`, open one L2 tracking issue from the committed
  source/plan/state files, initialize the run controller, and begin with Task
  1.1 in a fresh implementation worktree.
