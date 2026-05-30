# Provider-Neutral Plan-Tracking + Local Backend — Execution State

<!-- plan-issue-record:v2 role=state profile=tracking -->
## Execution State

- Status: ready-to-start; tracking issue not yet opened.
- Target scope: provider-neutral driver seam, a frozen local-provider contract
  spec, a `forge-cli Provider::Local` file-backed backend, plan-issue-cli local
  routing, a cross-provider conformance suite, a real GitLab e2e target, and a
  gated service-feasibility eval.
- Execution window: four serial sprints (L2, steady; no L3 dispatch).
- Current task: none (tracking issue not yet opened).
- Next task: Task 1.1 — driver provider-neutral seam.
- Last updated: 2026-05-31
- Branch/commit/PR: per-task PRs into the relevant repo; no branch yet.
- Source document: `docs/plans/2026-05-31-plan-tracking-local-provider/plan-tracking-local-provider-plan.md`
- Direct source-doc execution waiver: not applicable
- Tracking issue: tbd (to be opened by `create-plan-tracking-issue` against
  `sympoies/nils-cli`)
- Source snapshot: pending — posted by `create-plan-tracking-issue` at open
- Plan snapshot: pending — posted by `create-plan-tracking-issue` at open
- Initial state snapshot: pending — posted by `create-plan-tracking-issue` at open

## Validation Plan

- Per-task: the task's own `## Validation` command (cargo test for nils-cli
  crates; `run.sh` assert chains for the driver).
- Cross-provider: the conformance suite (Task 3.1) green across
  `{local, github, gitlab}`.
- Gate: GitHub e2e stays green from Task 1.1 onward.

## Task Ledger

| ID | Status | Task | Evidence | Notes |
| --- | --- | --- | --- | --- |
| 1.1 | done | Driver provider-neutral seam | graysurf/agent-runtime-kit#200 (merge 65aa344) | agent-runtime-kit; GitHub stays green. |
| 1.2 | done | Local-provider contract schema spec | sympoies/nils-cli@77399d7 (direct-to-main) | nils-cli docs/specs; resolves runbook drift. |
| 2.1 | pending | forge-cli Provider::Local backend | — | The meat; file-backed, issue half real + PR half seeded. Dep 1.2. |
| 2.2 | pending | plan-issue-cli local routing | — | Parameterize forge_cli_adapter.rs:127. Dep 2.1. |
| 3.1 | pending | Cross-provider conformance harness | — | local+github+gitlab. Dep 2.2. |
| 3.2 | pending | GitLab real e2e target | — | agent-runtime-kit + external GitLab. Dep 1.1. |
| 4.1 | pending | Service feasibility eval | — | Gated go/no-go; no build commitment. Dep 3.1. |
