# Plan-Issue Lifecycle v3 Execution State

<!-- execute-from-tracking-issue:state:v1 -->
## Execution State

- Status: tracking issue open; ready for Sprint 1 execution
- Target scope: breaking `plan-issue` issue-backed lifecycle v3 rewrite
- Execution window: next nils-cli implementation lane
- Current task: Sprint 1 ready
- Next task: execute Task 1.1, Task 1.2, and Task 1.3
- Last updated: 2026-05-23
- Branch/commit/PR: plan/plan-issue-lifecycle-v3; plan commit dc9aedc; PR pending
- Source document: docs/plans/plan-issue-lifecycle-v3/plan-issue-lifecycle-v3-plan.md
- Direct source-doc execution waiver: not applicable
- Tracking issue: [#448](https://github.com/sympoies/nils-cli/issues/448)
- Source snapshot: [source](https://github.com/sympoies/nils-cli/issues/448#issuecomment-4524856896)
- Plan snapshot: [plan](https://github.com/sympoies/nils-cli/issues/448#issuecomment-4524856962)
- Initial state snapshot: [state](https://github.com/sympoies/nils-cli/issues/448#issuecomment-4524857027)

## Validation Plan

- `plan-tooling validate --file docs/plans/plan-issue-lifecycle-v3/plan-issue-lifecycle-v3-plan.md --format text --explain`
- `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only`
- During implementation:
  `NILS_CLI_TEST_RUNNER=nextest bash scripts/ci/nils-cli-checks-entrypoint.sh`

## Task Ledger

| ID | Status | Task | Evidence | Notes |
| --- | --- | --- | --- | --- |
| 1.1 | pending | Replace issue-backed record contract with v3 | pending | Spec-first breaking change. |
| 1.2 | pending | Define high-level CLI surface | pending | Removes marker-family and optional closeout requirements. |
| 1.3 | pending | Specify structured lifecycle payloads | pending | Audit no longer parses prose status lines. |
| 2.1 | pending | Collapse marker parsing to canonical family | pending | Removes compat marker support. |
| 2.2 | pending | Implement structured audit output | pending | Provides typed evidence for closeout. |
| 2.3 | pending | Render dashboards from audit evidence | pending | Removes manual URL stitching. |
| 3.1 | pending | Implement `record open` | pending | Bundle-first live issue creation. |
| 3.2 | pending | Implement `record post` | pending | Append-only canonical lifecycle comments. |
| 3.3 | pending | Implement strict `record close` | pending | Provider-verifies linked PRs and closes issue. |
| 4.1 | pending | Remove or isolate Task Decomposition commands | pending | Primary help surface becomes issue-backed lifecycle. |
| 4.2 | pending | Refresh completions and output contract fixtures | pending | Required after command surface change. |
| 4.3 | pending | Add agent-runtime-kit closeout fixture coverage | pending | Proves the new CLI covers the discovered workflow. |
| 5.1 | pending | Run full nils-cli validation | pending | CI-equivalent local gate. |
| 5.2 | pending | Prepare release and agent-runtime-kit handoff notes | pending | Consumer migration happens after release. |

## Session Log

- 2026-05-23: User delegated whether to open the tracking issue in nils-cli,
  agent-runtime-kit, or both. Decision: open one nils-cli issue because the
  implementation owner is `crates/plan-issue-cli`; defer agent-runtime-kit
  migration until the CLI is released.
- 2026-05-23: Created the plan bundle for a breaking v3 rewrite of the
  issue-backed lifecycle surface.
- 2026-05-23: Opened tracking issue #448 with source, plan, and initial state
  snapshots. The plan branch was pushed to
  `origin/plan/plan-issue-lifecycle-v3`.

## Notes

- This plan intentionally does not preserve backwards compatibility.
- This plan does not mutate agent-runtime-kit source files.
- The implementation should stop and re-scope only if removing the legacy Task
  Decomposition commands would make unrelated nils-cli surfaces unusable.
