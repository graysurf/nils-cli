# Plan-Issue Lifecycle v3 Execution State

<!-- plan-issue-record:v2 role=state profile=tracking -->
## Execution State

- Status: implementation complete; ready for closeout
- Target scope: breaking `plan-issue` issue-backed lifecycle v3 rewrite
- Execution window: 2026-05-23 (Sprint 1 → Sprint 5)
- Current task: Sprint 5 complete; closeout pending user approval
- Next task: re-post v2-marker source/plan/state lifecycle comments on
  #448 (Sprint 1 + 2 were authored with the v1 binary), then run
  `plan-issue record close` to apply the strict v3 gate and close the
  issue.
- Last updated: 2026-05-23
- Branch/commit/PR: plan/plan-issue-lifecycle-v3; impl PRs #449, #453,
  #454, #455 (all merged); Sprint 5 PR pending
- Source document: docs/plans/plan-issue-lifecycle-v3/plan-issue-lifecycle-v3-plan.md
- Direct source-doc execution waiver: not applicable
- Tracking issue: [#448](https://github.com/sympoies/nils-cli/issues/448)
- Source snapshot: [source](https://github.com/sympoies/nils-cli/issues/448#issuecomment-4524856896)
- Plan snapshot: [plan](https://github.com/sympoies/nils-cli/issues/448#issuecomment-4524856962)
- Initial state snapshot: [state](https://github.com/sympoies/nils-cli/issues/448#issuecomment-4524857027)
- Latest state snapshot (v2): [state](https://github.com/sympoies/nils-cli/issues/448#issuecomment-4525591970)
- Latest session snapshot (v2): [session](https://github.com/sympoies/nils-cli/issues/448#issuecomment-4525592044)
- Latest validation snapshot (v2): [validation](https://github.com/sympoies/nils-cli/issues/448#issuecomment-4525592137)

## Validation Plan

- `plan-tooling validate --file docs/plans/plan-issue-lifecycle-v3/plan-issue-lifecycle-v3-plan.md --format text --explain`
- `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only`
- During implementation:
  `NILS_CLI_TEST_RUNNER=nextest bash scripts/ci/nils-cli-checks-entrypoint.sh`
- For Sprint 5 closeout:
  `plan-issue record audit --profile tracking --body-file <body> --comments-json <comments>`
  must return `recognized_count >= 6` and `missing_required = []` before
  `plan-issue record close` is invoked.

## Task Ledger

| ID | Status | Task | Evidence | Notes |
| --- | --- | --- | --- | --- |
| 1.1 | done | Replace issue-backed record contract with v3 | PR #449 (7aaf443) | Spec-first breaking change. |
| 1.2 | done | Define high-level CLI surface | PR #449 (7aaf443) | Removed marker-family and optional closeout requirements. |
| 1.3 | done | Specify structured lifecycle payloads | PR #449 (7aaf443) | Audit no longer parses prose status lines. |
| 2.1 | done | Collapse marker parsing to canonical family | PR #453 (eb6f383) | Removed compat marker support. |
| 2.2 | done | Implement structured audit output | PR #453 (eb6f383) | Provides typed evidence for closeout. |
| 2.3 | done | Render dashboards from audit evidence | PR #453 (eb6f383) | Removes manual URL stitching. |
| 3.1 | done | Implement `record open` | PR #454 (cef31ee) | Bundle-first live issue creation. |
| 3.2 | done | Implement `record post` | PR #454 (cef31ee) | Append-only canonical lifecycle comments. |
| 3.3 | done | Implement strict `record close` | PR #454 (cef31ee) | Provider-verifies linked PRs and closes issue. |
| 4.1 | done | Hide retired record subcommands from primary help | PR #455 (9fd3017) | Task Decomposition CLI retirement deferred to next major release; see "Deferred Follow-ups". |
| 4.2 | done | Refresh completions and output contract fixtures | PR #455 (9fd3017) | bash + zsh completions regenerated. |
| 4.3 | done | Add agent-runtime-kit closeout fixture coverage | PR #455 (9fd3017) | crates/plan-issue-cli/tests/fixtures/lifecycle/agent-runtime-kit-closeout. |
| 5.1 | done | Run full nils-cli validation | Sprint 5 PR | CI-equivalent local gate. |
| 5.2 | done | Prepare release and agent-runtime-kit handoff notes | Sprint 5 PR | CHANGELOG + v2 spec Consumer Migration section. |

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
- 2026-05-23: Sprint 1 landed via PR #449 (spec + scaffolded CLI surface).
- 2026-05-23: Sprint 2 landed via PR #453 (marker collapse, structured audit,
  dashboard from audit).
- 2026-05-23: Sprint 3 landed via PR #454 (live record open / post / close +
  strict v2 closeout gate + GitHubAdapter additions: comment_issue URL,
  issue_evidence, pr_merge_summary).
- 2026-05-23: Sprint 4 landed via PR #455 (hide retired record subcommands,
  regenerated completions, agent-runtime-kit closeout fixture).
- 2026-05-23: Sprint 5 lands the full local gate, CHANGELOG BREAKING entries
  for the v3 audit/role rename + retired subcommands, README v3 pointer,
  and agent-runtime-kit Consumer Migration section in the v2 spec.

## Deferred Follow-ups

Tracked here so the next major-release cycle can act on them:

- Retire the Task Decomposition CLI (`start-plan`, `status-plan`,
  `link-pr`, `ready-plan`, `close-plan`, `cleanup-worktrees`,
  `start-sprint`, `ready-sprint`, `accept-sprint`,
  `multi-sprint-guide`, `resolve-approval`, `build-task-spec`,
  `build-plan-task-spec`) from `plan-issue --help`. Sprint 4 hid only
  the retired Record subcommands; the broader Task Decomposition
  retirement is a high-blast-radius refactor that needs a coordinated
  consumer migration first.
- Result envelope normalization across `record` subcommands: today
  the result shape varies between fixture (`mode: "fixture"`),
  dry-run (`mode: "dry-run"` with `preview.*`), and live (top-level
  fields). Sprint 3 specialist review flagged this as api-contract
  drift; normalize in a follow-up so consumers don't have to branch
  on `mode`.
- `record close` rollback: if any provider call between issue create
  and dashboard repair fails, the issue is left in an inconsistent
  state. Add `--cleanup-on-failure` or surface a partial-state error
  with the partial URLs.
- `record close --body-file` mode without `--fixture` returns the
  misleading `linked-pr-not-merged` code when the caller did not
  provide PR evidence. Either reject the combination or surface a
  clearer `linked-pr-evidence-missing` code.
- `evaluate_strict_closeout_gate` dashboard-out-of-date branch is
  wired but unused. Expose it via a future `record audit --strict`
  surface or remove it.
- `build_initial_state_payload` uses hardcoded `Sprint 1 ready` /
  `execute Sprint 1 tasks` copy. Derive from the plan instead, or
  document the placeholder behavior.

## Notes

- This plan intentionally does not preserve backwards compatibility.
- This plan does not mutate agent-runtime-kit source files.
- The agent-runtime-kit consumer migration is documented in
  [`crates/plan-issue-cli/docs/specs/issue-backed-plan-record-contract-v2.md`](../../../crates/plan-issue-cli/docs/specs/issue-backed-plan-record-contract-v2.md)
  ("Consumer Migration" section). It happens after the next
  plan-issue-cli release cuts.
