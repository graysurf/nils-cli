# Support Matrix Render Target And Existing-Issue Attach Source

- Status: ready for tracking-plan creation
- Date: 2026-05-25
- Source: agent-runtime-kit issue #69 follow-up after auditing the current
  runtime-kit repository and the local nils-cli `plan-issue` / `agent-runtime`
  binaries.
- Intended next step: open one nils-cli tracking issue, implement the missing
  local CLI support, then use the local binaries to finish
  `graysurf/agent-runtime-kit#69`.

## Purpose

`graysurf/agent-runtime-kit#69` tracks making `SUPPORT_MATRIX.md` generated
from manifest data instead of hand-maintained Markdown. Sprint 1 can live in
agent-runtime-kit by adding the manifest and schema, but Sprint 2 is blocked by
nils-cli: the current `agent-runtime render` command renders only product
skill surfaces and has no shared `--target support-matrix` mode.

The same issue is an older tracking issue. Live audit with the current
`plan-issue record audit --profile tracking` recognized zero lifecycle
comments because the issue has older compatibility markers rather than
`plan-issue-record:v2` source, plan, and state comments. The local v3
`plan-issue record post` command can append state/session/validation/review
comments, but still rejects `source` and `plan` because those are owned by
`record open`. Existing provider issues therefore need a high-level attach
command that `plan-issue` owns, instead of hand-written GitHub comments.

## Source Tags

- `[U1]` User asked to do the agent-runtime-kit issue #69 state repair and
  Sprint 1 manifest/schema work first, then open a nils-cli
  `create-plan-tracking-issue`, and finally use local binaries to return to
  issue #69 and finish the remaining work.
- `[F1]` `graysurf/agent-runtime-kit#69` is open with title
  `SUPPORT_MATRIX rendered + acceptance-in-manifest` and still carries
  `state::needs-triage`.
- `[F2]` `agent-runtime render --help` currently exposes `--source-root`,
  `--product`, and `--update-golden`; it has no `--target` option.
- `[F3]` `crates/agent-runtime-cli/src/commands/render.rs` routes every render
  through `writer::write_product`, which writes only `build/<product>/`.
- `[F4]` `plan-issue record post --help` says `source` and `plan` kinds are
  owned by `record open` and rejected by `post`.
- `[A1]` Live audit of `graysurf/agent-runtime-kit#69` with
  `plan-issue record audit --profile tracking` returned
  `recognized_count=0` and missing required source, plan, and state evidence.
- `[A2]` Agent-runtime-kit Sprint 1 adds `manifests/surfaces.yaml`,
  `core/docs/schemas/surfaces.schema.json`, and a manifest validation script,
  so nils-cli can consume an explicit row registry instead of parsing
  `SUPPORT_MATRIX.md`.
- `[I1]` Inference from `[F2]`, `[F3]`, and `[A2]`: the render implementation
  belongs in `crates/agent-runtime-cli`, not agent-runtime-kit shell glue.
- `[I2]` Inference from `[F4]` and `[A1]`: attaching v3 source/plan/state
  comments to an existing provider issue needs a first-class `plan-issue`
  command, not ad hoc `gh` comment creation.

## Decisions

- Open this tracking issue in `sympoies/nils-cli`; agent-runtime-kit is the
  downstream consumer and acceptance target.
- Implement a shared render target for support matrix output:
  `agent-runtime render --target support-matrix`.
- Keep the existing product render path unchanged:
  `agent-runtime render --product codex|claude`.
- Read support matrix row data from `manifests/surfaces.yaml`.
- Do not parse `SUPPORT_MATRIX.md` as the source of truth.
- Add a first-class `plan-issue record attach` command for existing issues.
  It should derive source, plan, and execution-state paths from a bundle,
  post canonical v2 lifecycle comments, repair the dashboard, and audit the
  resulting record.
- Use the local nils-cli binaries against `graysurf/agent-runtime-kit#69` once
  both pieces are implemented.

## Scope

- Add the `support-matrix` render target to `agent-runtime`.
- Add typed `surfaces.yaml` loading for support matrix rows.
- Render a deterministic shared Markdown file under `build/shared/`.
- Support update-golden or an equivalent golden fixture path for the shared
  target.
- Add integration tests for successful rendering, invalid manifests, and
  deterministic output.
- Add a `plan-issue record attach` existing-issue lifecycle command.
- Add tests proving `record attach` posts source, plan, and initial state
  lifecycle comments and then repairs/audits the dashboard.
- Refresh help/output-contract fixtures and completions if the CLI surface
  changes.

## Non-Scope

- Do not ship a new nils-cli release in this plan.
- Do not mutate live runtime homes.
- Do not hand-author lifecycle comments through `gh issue comment`.
- Do not reintroduce retired marker families as accepted audit evidence.
- Do not make `SUPPORT_MATRIX.md` parsing part of the render contract.

## Execution

- Recommended plan: docs/plans/support-matrix-render-target/support-matrix-render-target-plan.md
- Recommended execution state: docs/plans/support-matrix-render-target/support-matrix-render-target-execution-state.md
