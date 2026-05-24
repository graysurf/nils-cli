# Support Matrix Render Target And Existing-Issue Attach Execution State

<!-- plan-issue-record:v2 role=state profile=tracking -->
## Execution State

- Status: plan bundle ready; provider tracking issue pending
- Target scope: nils-cli support for agent-runtime-kit support matrix
  rendering and existing issue lifecycle attach
- Execution window: 2026-05-25
- Current task: create tracking issue, then implement Sprint 1 and Sprint 2
- Next task: run `plan-issue record open` for this bundle, then start
  `agent-runtime render --target support-matrix`
- Last updated: 2026-05-25
- Branch/commit/PR: feat/support-matrix-render-target
- Source document: docs/plans/support-matrix-render-target/support-matrix-render-target-plan.md
- Direct source-doc execution waiver: not applicable
- Downstream issue: https://github.com/graysurf/agent-runtime-kit/issues/69

## Validation Plan

- `plan-tooling validate --file docs/plans/support-matrix-render-target/support-matrix-render-target-plan.md --format text --explain`
- `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only`
- `cargo test -p agent-runtime-cli render`
- `cargo test -p agent-runtime-cli render_determinism`
- `cargo test -p nils-plan-issue-cli cli_contract`
- `cargo test -p nils-plan-issue-cli live_record_attach`
- `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast`
- Consumer validation in agent-runtime-kit:
  `agent-runtime render --source-root <agent-runtime-kit> --target support-matrix`
- Consumer lifecycle validation in agent-runtime-kit:
  `plan-issue record audit --profile tracking --body-file <body> --comments-json <comments>`

## Task Ledger

| ID | Status | Task | Evidence | Notes |
| --- | --- | --- | --- | --- |
| 1.1 | pending | Add shared target CLI routing | pending | Preserve product render default. |
| 1.2 | pending | Load `surfaces.yaml` | pending | Closed product keys and actionable validation errors. |
| 1.3 | pending | Render deterministic Markdown | pending | Writes `build/shared/SUPPORT_MATRIX.md`. |
| 1.4 | pending | Add shared golden and determinism coverage | pending | Shared target must not touch product golden trees. |
| 2.1 | pending | Define `record attach` | pending | Existing issue source/plan/state attach. |
| 2.2 | pending | Implement provider-backed attach | pending | Posts canonical lifecycle comments then repairs/audits. |
| 2.3 | pending | Refresh generated CLI surfaces | pending | Help/output contracts/completions/docs. |
| 3.1 | pending | Render support matrix in agent-runtime-kit | pending | Local binary downstream verification. |
| 3.2 | pending | Attach v3 lifecycle comments to #69 | pending | Local binary downstream lifecycle repair. |

## Session Log

- 2026-05-25: Created plan bundle from agent-runtime-kit #69 audit results.
