# Provider Exact Attention Correlation Execution State

<!-- plan-issue-record:v2 role=state profile=tracking -->
## Execution State

- Status: ready-to-start; implementation not started
- Target scope: one provider-neutral v1 invariant, runtime-selected Codex
  attention authority with app-server exact resolution, capability-selected
  Claude exact-or-limited Elicitation, and an explicit conservative limitation
  for generic permission dialogs.
- Execution window: Sprint 1 shared boundary -> Sprints 2 and 3 independent /
  parallel provider lanes -> Sprint 4 integration and delivery.
- Current task: none; tracker initialized and implementation not started.
- Next task: Task 1.1, freeze the shared invariant and authority-mode contract.
- Last updated: 2026-07-15
- Branch: `feat/provider-exact-attention-correlation`
- Source document:
  `docs/plans/2026-07-15-provider-exact-attention-correlation/provider-exact-attention-correlation-plan.md`
- Implementation source:
  `docs/plans/2026-07-15-provider-exact-attention-correlation/provider-exact-attention-correlation-discussion-source.md`
- Tracking issue: <https://github.com/sympoies/nils-cli/issues/1237>

## Validation Plan

- Validate the plan bundle and docs-only repository gate before opening the
  tracker.
- Preserve the already-green shared invariant, then capture meaningful
  provider-lane test-first failures before production edits.
- Re-audit the installed Codex typed request-id schema and sanitize Claude
  Elicitation fixtures before selecting exact capability.
- Run focused reducer/proxy/setup/doctor tests plus nils-cli local-fast.
- Require specialist review, provider checks, and pre-lifecycle-boundary live
  polling/SSE/Agent Console acceptance before final closeout.

## Task Ledger

| ID | Status | Task | Evidence | Notes |
| --- | --- | --- | --- | --- |
| 1.1 | pending | Freeze shared invariant and authority-mode contract | pending | No source pairing; provider lanes become independent. |
| 2.1 | pending | Verify Codex capability and capture test-first evidence | pending | Typed `string \| int64`, dedupe, loss, and schema-drift reds. |
| 2.2 | pending | Implement typed Codex projection and fail-closed reduction | pending | Exact ids survive semantic dedupe. |
| 2.3 | pending | Implement Codex runtime source arbitration and capability status | pending | One authority selected at create/resume; no mid-runtime switch. |
| 3.1 | pending | Verify Claude capability and capture branch-specific test-first evidence | pending | Select exact or conservative terminal branch. |
| 3.2 | pending | Implement selected Claude exact or conservative branch | pending | Selective rollback; no global setup removal. |
| 4.1 | pending | Publish capability status and run integration acceptance | pending | Same-id clear before `Stop` where exact is supported. |
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
- 2026-07-15: Pre-merge specialist review rejected uncorrelated Codex
  hook/protocol reconciliation as unimplementable. The plan now selects one
  attention authority per runtime, preserves typed exact ids through semantic
  deduplication, treats request/resolution loss asymmetrically, and makes
  Claude's conservative capability result a valid independent lane outcome.
- 2026-07-15: Opened L2 tracker #1237 from the committed bundle and initialized
  run `20260715T155230Z-issue-1237` at Sprint 1, Task 1.1. The tracker remains
  open for the later implementation session.
- 2026-07-15: API-contract follow-up clarified that a duplicate
  `PermissionRequest` hook in protocol-authoritative mode is diagnostic-only and
  must not advance `last_progress_at` or change pending presentation.

## Validation

| Command | Status | Summary | Artifact |
| --- | --- | --- | --- |
| `agent-docs session activate` for `project-dev,task-tools` | pass | Required repository and external-fact policies activated. | local preflight |
| `plan-archive search` for turn-state/activity/app-server plans | pass | Found app-server foundation; no exact-attention duplicate. | local catalog |
| Open nils-cli issue audit | pass | #1118 is related configuration work, not this scope. | GitHub issue list |
| Installed version audit | pass | Claude 2.1.210, Codex 0.144.3, agent-session 1.22.4. | local runtime |
| `plan-tooling validate` | pass | Repaired eight-task graph validates with zero errors. | local |
| `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only` | pass | Repaired three-file bundle passes all docs checks. | local |
| `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast` | pass | Repository correctly selected docs-only mode and passed. | local |

## Handoff

- Publish this linked bundle to `main`. In the later implementation session,
  resume tracker #1237 and begin with Task 1.1 in a fresh implementation
  worktree.
