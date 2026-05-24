# Plan-Issue Lifecycle v3 Implementation Source

- Status: ready for tracking-plan creation
- Date: 2026-05-23
- Source: user discussion after closing the `agent-runtime-kit` shared
  Heuristic System tracking issue and identifying that the current
  `plan-issue record` surface still forces skills to stitch together low-level
  render, provider mutation, audit, and closeout commands.
- Intended next step: open one nils-cli tracking issue and execute the v3
  breaking rewrite there. Do not open a parallel agent-runtime-kit issue until
  the CLI contract is implemented and released.

## Purpose

Rewrite `plan-issue` around the issue-backed lifecycle that agent-runtime-kit
actually needs: one canonical marker family, high-level provider-backed
commands, strict closeout gates, provider-verified PR evidence, and automatic
dashboard repair from durable issue comments.

This should be a breaking rewrite. No migration layer is required for retired
pre-v2 marker families.

## Source Tags

- `[U1]` User asked whether the follow-up tracking issue belongs in
  `agent-runtime-kit`, `nils-cli`, or both, and delegated the decision.
- `[U2]` User asked for a complete, final rewrite of the nils-cli
  `plan-issue` surface, without backward-version support.
- `[F1]` `crates/plan-issue-cli/src/commands/record.rs` currently exposes
  low-level issue-record helper commands rather than one provider-backed
  lifecycle owner.
- `[F2]` `crates/plan-issue-cli/src/commands/record.rs` currently exposes
  multiple marker-family variants.
- `[F3]` `crates/plan-issue-cli/src/lifecycle_record.rs` currently errors when
  asked to render a tracking-profile review marker for the retired family.
- `[F4]` `crates/plan-issue-cli/src/lifecycle_record.rs` currently extracts
  lifecycle status from visible Markdown lines such as `- Status: complete`.
- `[F5]` The prior issue-backed plan record contract says `plan-issue record`
  never mutates provider issues and provider CRUD remains outside the record
  command.
- `[A1]` The shared Heuristic System closeout in agent-runtime-kit required a
  manual chain of `plan-issue record`, `forge-cli issue`, `gh issue view`, and
  dashboard repair commands.
- `[A2]` The same closeout found that `forge-cli issue close --reason completed`
  is not a valid current command shape.
- `[I1]` Inference from `[F1]`, `[F5]`, and `[A1]`: agent-runtime-kit needs a
  high-level lifecycle owner, not more low-level rendering primitives.
- `[I2]` Inference from `[F2]` and `[F3]`: retaining multiple marker families
  keeps migration complexity in the exact place where the new workflow needs
  determinism.
- `[I3]` Inference from `[F4]`: audit and closeout should parse a structured
  lifecycle payload rather than prose Markdown.

## Decisions

- Open the tracking issue in `sympoies/nils-cli`, not in agent-runtime-kit.
  The implementation owner is `crates/plan-issue-cli`; agent-runtime-kit is the
  downstream consumer and validation target.
- Do not open a second agent-runtime-kit issue now. After the nils-cli release,
  open a separate agent-runtime-kit migration issue only if the generated skill
  and smoke-test changes are non-trivial.
- Treat this as a breaking `plan-issue` v3 contract.
- Keep `plan-tooling` as the plan parser and validator.
- Make `plan-issue` the owner of issue-backed lifecycle state, provider issue
  comments, dashboard repair, closeout gating, and issue closure.
- Keep `forge-cli` as the general provider lifecycle tool, but do not require
  agent-runtime-kit skills to compose `forge-cli` directly for plan issue
  lifecycle operations.
- Keep dispatch as a profile or mode of the same issue-backed lifecycle model,
  not as a separate marker family.

## Scope

- Replace the issue-backed record contract with a single canonical marker and
  structured payload model.
- Add high-level live commands that can open, post, audit, repair, and close
  issue-backed plan records from a plan bundle.
- Make closeout strict by default.
- Verify linked PRs through provider state, including merge state and check
  status where available.
- Update docs, tests, completions, and output-contract fixtures.
- Add a sanitized fixture that represents the agent-runtime-kit closeout flow
  that exposed the current defects.

## Non-Scope

- Do not migrate agent-runtime-kit skills in this nils-cli plan.
- Do not preserve retired marker support.
- Do not preserve optional retired closeout helper flags.
- Do not make `forge-cli` issue close accept `--reason` as the primary fix.
- Do not mutate live agent-runtime-kit runtime homes.
- Do not ship an unreleased debug binary as the agent-runtime-kit consumer
  contract.

## Execution

- Recommended plan: docs/plans/plan-issue-lifecycle-v3/plan-issue-lifecycle-v3-plan.md
- Recommended execution state: docs/plans/plan-issue-lifecycle-v3/plan-issue-lifecycle-v3-execution-state.md
