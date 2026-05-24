# Support Matrix Render Target And Existing-Issue Attach Execution State

<!-- plan-issue-record:v2 role=state profile=tracking -->
## Execution State

- Status: local implementation complete; downstream agent-runtime-kit
  verification complete
- Target scope: nils-cli support for agent-runtime-kit support matrix
  rendering and existing issue lifecycle attach
- Execution window: 2026-05-25
- Current task: prepare commit / PR handoff
- Next task: deliver the nils-cli branch, then consume the released binary in
  agent-runtime-kit when available
- Last updated: 2026-05-25
- Branch/commit/PR: feat/support-matrix-render-target; tracking issue
  <https://github.com/sympoies/nils-cli/issues/486>
- Source document: docs/plans/support-matrix-render-target/support-matrix-render-target-plan.md
- Direct source-doc execution waiver: not applicable
- Downstream issue: <https://github.com/graysurf/agent-runtime-kit/issues/69>

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
| 1.1 | done | Add shared target CLI routing | `crates/agent-runtime-cli/src/commands/render.rs` | `--target support-matrix` preserves default product render behavior. |
| 1.2 | done | Load `surfaces.yaml` | `crates/agent-runtime-cli/src/render/support_matrix.rs` | Closed product keys and actionable validation errors. |
| 1.3 | done | Render deterministic Markdown | `build/shared/SUPPORT_MATRIX.md` downstream check | Writes shared generated support matrix. |
| 1.4 | done | Add shared golden and determinism coverage | `cargo nextest run --profile ci -p agent-runtime-cli` | Shared target has integration and determinism coverage. |
| 2.1 | done | Define `record attach` | `crates/plan-issue-cli/src/commands/record.rs` | Existing issue source/plan/state attach. |
| 2.2 | done | Implement provider-backed attach | `crates/plan-issue-cli/src/execute.rs` | Posts canonical lifecycle comments then repairs/audits. |
| 2.3 | done | Refresh generated CLI surfaces | `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast` | Help/output contracts/completions/docs covered by local-fast. |
| 3.1 | done | Render support matrix in agent-runtime-kit | local `target/debug/agent-runtime` | Local binary rendered 17 surfaces / 34 rows. |
| 3.2 | done | Attach v3 lifecycle comments to #69 | <https://github.com/graysurf/agent-runtime-kit/issues/69#issuecomment-4529537599> | Read-back audit recognized source/plan/state with no missing required evidence. |

## Session Log

- 2026-05-25: Created plan bundle from agent-runtime-kit #69 audit results.
- 2026-05-25: Opened tracking issue #486 with v2 source, plan, and state
  lifecycle comments.
- 2026-05-25: Implemented `agent-runtime render --target support-matrix` and
  `plan-issue record attach`.
- 2026-05-25: Used local binaries to render agent-runtime-kit
  `SUPPORT_MATRIX.md` and attach v2 lifecycle comments to
  graysurf/agent-runtime-kit#69.
